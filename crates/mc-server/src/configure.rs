//! The configuration phase of a connection: brand and features, known
//! packs, registries and tags, server links, the resource pack and the code
//! of conduct, until the client finishes configuring. It has no time limit
//! of its own, as in vanilla: a player may take as long as they like over a
//! pack download or the code of conduct, while keep-alives (and the read
//! timeout) catch a client that is gone.

use anyhow::{Result, bail};
use mc_protocol::packets::configuration;
use mc_protocol::{State, Write as _, text};
use tracing::{debug, info, trace};

use super::{Packet, PacketReader, PacketWriter, Session, note_client_info};
use crate::guard::{KeepAlive, KeepAliveStep};
use crate::util::uuid_string;

/// The next configuration packet worth handling. Keep-alives are sent and
/// checked on the way; a client that leaves one unanswered for a whole
/// interval is disconnected with `disconnect.timeout`, as vanilla does.
async fn next(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    keep_alive: &mut KeepAlive,
) -> Result<configuration::Serverbound> {
    loop {
        let due = tokio::time::Instant::from_std(keep_alive.due());
        tokio::select! {
            packet = reader.next(State::Configuration) => {
                let Packet::Configuration(packet) = packet? else {
                    unreachable!("configuration state decodes configuration packets")
                };
                match packet {
                    configuration::Serverbound::KeepAlive(id) => keep_alive.answered(id),
                    other => return Ok(other),
                }
            }
            () = tokio::time::sleep_until(due) => {
                match keep_alive.step(std::time::Instant::now()) {
                    KeepAliveStep::Send(id) => {
                        writer.send(&configuration::KeepAlive { id }).await?;
                    }
                    KeepAliveStep::TimedOut => {
                        let reason = text::translate("disconnect.timeout", vec![]);
                        writer.send(&configuration::Disconnect { reason }).await?;
                        bail!("Timed out");
                    }
                    KeepAliveStep::Wait => {}
                }
            }
        }
    }
}

/// Runs configuration; `Ok(false)` if the client was refused on the way.
pub(super) async fn configure(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    session: &mut Session,
) -> Result<bool> {
    let ctx = writer.ctx.clone();
    let mut keep_alive = KeepAlive::new(std::time::Instant::now());
    let mut brand = Vec::new();
    brand.put_string(crate::BRAND);
    writer.queue(&configuration::CustomPayload {
        channel: "minecraft:brand".into(),
        data: brand,
    })?;
    if let Some(links) = crate::serverlist::server_links(ctx.config.server_dir.as_deref()) {
        writer.queue(&configuration::ServerLinks { links })?;
    }
    let flags = ctx.packs.lock().expect("packs lock").enabled_features();
    writer.queue(&configuration::UpdateEnabledFeatures { flags })?;
    writer.queue(&configuration::SelectKnownPacks {
        packs: vec![ctx.core_pack.clone()],
    })?;
    writer.flush().await?;

    let max_view = i32::from(ctx.view_distance.load(std::sync::atomic::Ordering::Relaxed));
    let client_knows_core = loop {
        match next(reader, writer, &mut keep_alive).await? {
            configuration::Serverbound::SelectKnownPacks(packs) => {
                break packs.contains(&ctx.core_pack);
            }
            configuration::Serverbound::ClientInformation(info) => {
                note_client_info(session, &info, max_view);
            }
            other => trace!("configuration: {other:?}"),
        }
    };
    // Clients with the vanilla data pack only need entry names; others get
    // every entry's data as NBT.
    let registries = if client_knows_core {
        &ctx.registries
    } else {
        debug!("client does not know minecraft:core, sending registry data as NBT");
        &ctx.registries_with_data
    };
    for registry in registries {
        writer.queue(registry)?;
    }
    writer.queue(&ctx.tags)?;
    // Vanilla runs the code of conduct task before the resource pack task.
    if let Some(conduct) = ctx.config.code_of_conduct.clone() {
        let text = conduct.for_language(&session.language).to_owned();
        writer.send(&configuration::CodeOfConduct { text }).await?;
        loop {
            match next(reader, writer, &mut keep_alive).await? {
                configuration::Serverbound::AcceptCodeOfConduct => break,
                configuration::Serverbound::ClientInformation(info) => {
                    note_client_info(session, &info, max_view);
                }
                other => trace!("configuration: {other:?}"),
            }
        }
    }
    if let Some(pack) = &ctx.config.resource_pack {
        writer.queue(&configuration::ResourcePackPush {
            id: pack.id,
            url: pack.url.clone(),
            hash: pack.hash.clone(),
            forced: pack.required,
            prompt: pack.prompt.clone(),
        })?;
        writer.flush().await?;
        // The client answers as it goes; joining waits for it to be done.
        loop {
            match next(reader, writer, &mut keep_alive).await? {
                configuration::Serverbound::ResourcePack { id, result } if id == pack.id => {
                    let (done, refused) = crate::serverlist::pack_outcome(result);
                    if refused && pack.required {
                        info!(
                            "Disconnecting {} due to resource pack {} rejection",
                            session.player.profile.name,
                            uuid_string(pack.id)
                        );
                        writer
                            .send(&configuration::Disconnect {
                                reason: text::translate(
                                    "multiplayer.requiredTexturePrompt.disconnect",
                                    vec![],
                                ),
                            })
                            .await?;
                        return Ok(false);
                    }
                    if done {
                        break;
                    }
                }
                configuration::Serverbound::ClientInformation(info) => {
                    note_client_info(session, &info, max_view);
                }
                other => trace!("configuration: {other:?}"),
            }
        }
    }
    writer.queue(&configuration::FinishConfiguration)?;
    writer.flush().await?;

    loop {
        match next(reader, writer, &mut keep_alive).await? {
            configuration::Serverbound::FinishConfiguration => return Ok(true),
            configuration::Serverbound::ClientInformation(info) => {
                note_client_info(session, &info, max_view);
            }
            other => trace!("configuration: {other:?}"),
        }
    }
}
