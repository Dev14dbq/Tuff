//! One client connection: handshake → status, or login → configuration → play.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use mc_protocol::crypto::{Cfb8, server_hash};
use mc_protocol::frame::{decode_frame_body, split_frame};
use mc_protocol::packets::{GameProfile, configuration, handshake, login, play, status};
use mc_protocol::text;
use mc_protocol::{Direction, Encode, Position, Reader, State, Write as _};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast;
use tracing::{debug, info, trace, warn};

use crate::ServerContext;
use crate::auth;
use crate::commands::Source;
use crate::context::{Event, OnlinePlayer, Pose};
use crate::interact::{Actor, Hands, Outcome};
use crate::tick::{Action, Request};

#[path = "windows.rs"]
mod windows;
use crate::util::{offline_uuid, random_u64, random_uuid, uuid_string, valid_player_name};

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
const CREATIVE: i32 = 1;

pub async fn handle(stream: TcpStream, peer: SocketAddr, ctx: Arc<ServerContext>) -> Result<()> {
    let (read, write) = stream.into_split();
    let mut reader = PacketReader::new(read, ctx.clone());
    let mut writer = PacketWriter::new(write, ctx.clone());

    // Pre-1.7 clients (and modern ones after a failed ping) send 0xFE first.
    if reader.peek_byte().await? == 0xFE {
        bail!("legacy server list ping");
    }
    let intention = match reader.next(State::Handshake).await? {
        Packet::Handshake(handshake::Serverbound::Intention(intention)) => intention,
        other => bail!("expected a handshake, got {other:?}"),
    };
    match intention.intent {
        // With `enable-status` off the server does not answer, as if it
        // were not there.
        1 if !ctx.config.enable_status => Ok(()),
        1 => status(&mut reader, &mut writer).await,
        2 | 3 => {
            let login = login(&mut reader, &mut writer, &intention, peer);
            let session = tokio::time::timeout(LOGIN_TIMEOUT, login)
                .await
                .context("login timed out")??;
            let Some(player) = session else {
                return Ok(());
            };
            let name = player.player.profile.name.clone();
            let uuid = player.player.profile.uuid;
            let _ = uuid_string;
            let result = play(&mut reader, &mut writer, &player).await;
            {
                let ctx = ctx.clone();
                tokio::task::spawn_blocking(move || {
                    crate::stats::custom(&ctx, uuid, "leave_game", 1);
                    crate::save_player(&ctx, uuid);
                    crate::stats::forget(&ctx, uuid);
                    crate::criteria::forget(uuid);
                })
                .await?;
            }
            // What they rode leaves with them (their file keeps it).
            let dimension = ctx
                .players
                .lock()
                .expect("players lock")
                .get(&uuid)
                .map(|p| p.dimension.clone());
            if let Some(d) = dimension.and_then(|d| ctx.in_dimension(&d)) {
                crate::persist::left_with(&d, player.player.entity_id);
            }
            crate::containers::player_left(&ctx, uuid);
            ctx.players.lock().expect("players lock").remove(&uuid);
            ctx.management_bus
                .publish(&crate::notify::Note::PlayerLeft(crate::notify::Player {
                    id: Some(crate::util::uuid_string(uuid)),
                    name: Some(name.clone()),
                }));
            ctx.inventories
                .lock()
                .expect("inventories lock")
                .remove(&uuid);
            let entity_id = player.player.entity_id;
            let _ = ctx.events.send(Event::PlayerLeft { uuid, entity_id });
            let left = text::translate_colored(
                "multiplayer.player.left",
                vec![text::plain(&name)],
                "yellow",
            );
            let _ = ctx.events.send(Event::Chat(left));
            let reason = result
                .as_ref()
                .err()
                .map_or("Disconnected".to_owned(), |e| format!("{e:#}"));
            info!("{name} lost connection: {reason}");
            info!("{name} left the game");
            result
        }
        other => bail!("unknown handshake intent {other}"),
    }
}

// ---------------------------------------------------------------------------
// Status

async fn status(reader: &mut PacketReader, writer: &mut PacketWriter) -> Result<()> {
    let ctx = writer.ctx.clone();
    loop {
        match reader.next(State::Status).await? {
            Packet::Status(status::Serverbound::StatusRequest) => {
                let response = status::StatusResponse {
                    json: crate::serverlist::status(&ctx).to_string(),
                };
                writer.send(&response).await?;
            }
            Packet::Status(status::Serverbound::PingRequest(timestamp)) => {
                writer.send(&status::PongResponse { timestamp }).await?;
                return Ok(());
            }
            other => bail!("unexpected status packet {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Login and configuration

struct Session {
    player: OnlinePlayer,
    /// What the player file says, if they played here before.
    saved: Option<crate::players::Saved>,
    view_distance: i32,
    /// Displayed skin parts from Client Information (cape, jacket, sleeves, ...).
    skin_parts: u8,
    /// Client Information `text_filtering`. Kept for the client; chat is not
    /// passed to an external filter.
    text_filtering: bool,
    /// Client Information `locale` (`en_us`), which picks the code of conduct.
    language: String,
    /// Where the client connects from.
    peer: SocketAddr,
}

/// "You are banned from this server. Reason: ...", with the end of a
/// temporary ban.
fn ban_message(key: &str, ban: &crate::lists::Ban) -> serde_json::Value {
    let mut v = serde_json::json!({"translate": key, "with": [ban.reason]});
    if !ban.expires.is_empty() && ban.expires != "forever" {
        v["extra"] = serde_json::json!([{"translate": "multiplayer.disconnect.banned.expiration", "with": [ban.expires]}]);
    }
    v
}

fn refusal(reason: serde_json::Value) -> login::LoginDisconnect {
    login::LoginDisconnect {
        reason_json: reason.to_string(),
    }
}

/// Returns `None` if the client was refused (it has been told why).
async fn login(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    intention: &handshake::Intention,
    mut peer: SocketAddr,
) -> Result<Option<Session>> {
    let ctx = writer.ctx.clone();
    let name = match reader.next(State::Login).await? {
        Packet::Login(login::Serverbound::Hello { name, .. }) => name,
        other => bail!("expected Login Start, got {other:?}"),
    };

    if intention.protocol_version != mc_protocol::PROTOCOL_VERSION {
        let key = if intention.protocol_version < mc_protocol::PROTOCOL_VERSION {
            "multiplayer.disconnect.outdated_client"
        } else {
            "multiplayer.disconnect.outdated_server"
        };
        let reason = serde_json::json!({"translate": key, "with": [ctx.data.version.name]});
        writer.send(&refusal(reason)).await?;
        return Ok(None);
    }
    if !valid_player_name(&name) {
        let reason = format!("Invalid player name {name:?}");
        writer
            .send(&refusal(serde_json::json!({ "text": reason })))
            .await?;
        return Ok(None);
    }

    let profile = match &ctx.config.proxy {
        crate::proxy::Mode::Velocity { secret } => {
            match velocity_forwarding(reader, writer, secret).await? {
                Some(fwd) => {
                    peer = crate::proxy::player_socket(&fwd.address, peer);
                    fwd.profile
                }
                None => {
                    let reason = serde_json::json!({
                        "text": "This server requires Velocity modern forwarding."
                    });
                    writer.send(&refusal(reason)).await?;
                    return Ok(None);
                }
            }
        }
        crate::proxy::Mode::Bungee { tokens } => {
            match crate::proxy::verify_bungee(&intention.server_address, &name, tokens) {
                Ok(fwd) => {
                    peer = crate::proxy::player_socket(&fwd.address, peer);
                    fwd.profile
                }
                Err(reason) => {
                    writer
                        .send(&refusal(serde_json::json!({ "text": reason })))
                        .await?;
                    return Ok(None);
                }
            }
        }
        crate::proxy::Mode::Off => match &ctx.key {
            Some(key) => match authenticate(reader, writer, key, &name).await? {
                Some(profile) => profile,
                None => {
                    let reason = serde_json::json!(
                        {"translate": "multiplayer.disconnect.unverified_username"}
                    );
                    writer.send(&refusal(reason)).await?;
                    return Ok(None);
                }
            },
            // Offline mode: the UUID is derived from the name, as vanilla does.
            None => GameProfile {
                uuid: offline_uuid(&name),
                name,
                properties: Vec::new(),
            },
        },
    };
    if !valid_player_name(&profile.name) {
        let reason = format!("Invalid player name {:?}", profile.name);
        writer
            .send(&refusal(serde_json::json!({ "text": reason })))
            .await?;
        return Ok(None);
    }

    let saved = match &ctx.player_files {
        Some(store) => store.load(profile.uuid).unwrap_or_else(|err| {
            warn!("player file of {}: {err:#}", profile.name);
            None
        }),
        None => None,
    };
    let player = OnlinePlayer {
        profile,
        game_mode: saved
            .as_ref()
            .map_or(ctx.default_game_mode(), |s| s.game_mode),
        entity_id: ctx.next_entity_id(),
        skin_parts: 0,
        pose: None,
        dimension: saved
            .as_ref()
            .map_or_else(|| "minecraft:overworld".to_owned(), |s| s.dimension.clone()),
        main_hand: None,
        flying: saved.as_ref().is_some_and(|s| s.flying),
        address: Some(peer.ip().to_string()),
        extra: saved
            .as_ref()
            .map(|s| s.extra.clone())
            .unwrap_or_else(|| crate::players::Extra {
                enchantment_seed: crate::util::random_u64() as i32,
                ..Default::default()
            }),
    };
    let uuid = player.profile.uuid;
    // Bans and the whitelist (operators are always let in).
    let refused_by_lists = {
        let mut lists = ctx.lists.lock().expect("lists lock");
        let ip = peer.ip().to_string();
        let reason = if let Some(ban) = lists.player_ban(uuid, &player.profile.name) {
            Some(ban_message("multiplayer.disconnect.banned.reason", ban))
        } else if let Some(ban) = lists.ip_ban(&ip) {
            Some(ban_message("multiplayer.disconnect.banned_ip.reason", ban))
        } else if ctx.white_list()
            && !lists.whitelisted(uuid, &player.profile.name)
            && lists.op(uuid, &player.profile.name).is_none()
        {
            Some(serde_json::json!({"translate": "multiplayer.disconnect.not_whitelisted"}))
        } else {
            None
        };
        if reason.is_none() {
            lists.seen(uuid, &player.profile.name);
        }
        reason
    };
    if let Some(reason) = refused_by_lists {
        writer.send(&refusal(reason)).await?;
        return Ok(None);
    }
    // Already online: the old connection goes, as vanilla does.
    if ctx
        .players
        .lock()
        .expect("players lock")
        .contains_key(&uuid)
    {
        let _ = ctx.events.send(Event::Kick {
            uuid,
            reason: text::translate("multiplayer.disconnect.duplicate_login", Vec::new()),
        });
        for _ in 0..100 {
            if !ctx
                .players
                .lock()
                .expect("players lock")
                .contains_key(&uuid)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    // The lock guard must not live across an await.
    let refused = {
        let mut players = ctx.players.lock().expect("players lock");
        if players.contains_key(&uuid) {
            Some("multiplayer.disconnect.duplicate_login")
        } else if players.len() >= ctx.player_limit.load(std::sync::atomic::Ordering::Relaxed)
            && !ctx
                .lists
                .lock()
                .expect("lists lock")
                .op(uuid, &player.profile.name)
                .is_some_and(|o| o.bypasses_player_limit)
        {
            Some("multiplayer.disconnect.server_full")
        } else {
            players.insert(uuid, player.clone());
            None
        }
    };
    if let Some(reason) = refused {
        writer
            .send(&refusal(serde_json::json!({ "translate": reason })))
            .await?;
        return Ok(None);
    }

    let mut session = Session {
        player,
        saved,
        view_distance: i32::from(ctx.view_distance.load(std::sync::atomic::Ordering::Relaxed)),
        skin_parts: 0,
        text_filtering: false,
        language: "en_us".into(),
        peer,
    };
    // From here on the player is registered; remove them if login fails.
    let result = configure(reader, writer, &mut session).await;
    match result {
        Ok(true) => Ok(Some(session)),
        Ok(false) | Err(_) => {
            ctx.players.lock().expect("players lock").remove(&uuid);
            result.map(|_| None)
        }
    }
}

/// Velocity answers `velocity:player_info` with an HMAC'd profile instead
/// of an encryption response. `None` means the connection is not the proxy.
async fn velocity_forwarding(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    secret: &str,
) -> Result<Option<crate::proxy::Forwarded>> {
    writer
        .send(&login::CustomQuery {
            transaction_id: 0,
            channel: "velocity:player_info".into(),
            data: crate::proxy::velocity_query(),
        })
        .await?;
    let data = match reader.next(State::Login).await? {
        Packet::Login(login::Serverbound::CustomQueryAnswer {
            id: 0,
            data: Some(data),
        }) => data,
        other => {
            warn!("velocity forwarding expected a plugin response, got {other:?}");
            return Ok(None);
        }
    };
    match crate::proxy::verify_velocity(secret, &data) {
        Ok(fwd) => Ok(Some(fwd)),
        Err(reason) => {
            warn!("velocity forwarding rejected: {reason}");
            Ok(None)
        }
    }
}

/// Online mode: RSA key exchange, then both directions switch to AES/CFB8 and we
/// ask the session server who this is. `None` if the session server says no.
async fn authenticate(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    key: &auth::ServerKey,
    name: &str,
) -> Result<Option<GameProfile>> {
    let token = auth::verify_token();
    writer
        .send(&login::EncryptionRequest {
            server_id: String::new(),
            public_key: key.public_der.clone(),
            verify_token: token.to_vec(),
            should_authenticate: true,
        })
        .await?;
    let (encrypted_secret, encrypted_token) = match reader.next(State::Login).await? {
        Packet::Login(login::Serverbound::Key {
            shared_secret,
            verify_token,
        }) => (shared_secret, verify_token),
        other => bail!("expected Encryption Response, got {other:?}"),
    };
    if key.decrypt(&encrypted_token)? != token {
        bail!("verify token mismatch");
    }
    let secret: [u8; 16] = key
        .decrypt(&encrypted_secret)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("shared secret is not 16 bytes"))?;
    reader.enable_encryption(&secret);
    writer.enable_encryption(&secret);

    let hash = server_hash("", &secret, &key.public_der);
    let profile = auth::has_joined(&writer.ctx.config.session_server, name, &hash).await?;
    if profile.is_none() {
        warn!("session server did not verify {name}");
    }
    Ok(profile)
}

fn note_client_info(session: &mut Session, info: &configuration::ClientInformation, max_view: i32) {
    session.view_distance = i32::from(info.view_distance).clamp(2, max_view);
    session.skin_parts = info.skin_parts;
    session.text_filtering = info.text_filtering;
    session.language = info.locale.clone();
}

/// Returns `Ok(false)` if the client was refused during configuration.
async fn configure(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    session: &mut Session,
) -> Result<bool> {
    let ctx = writer.ctx.clone();
    if let Some(threshold) = ctx.config.compression_threshold {
        let compression = login::LoginCompression {
            threshold: threshold as i32,
        };
        writer.send(&compression).await?;
        writer.compression = Some(threshold);
        reader.compression = Some(threshold);
    }
    writer
        .send(&login::LoginFinished {
            profile: session.player.profile.clone(),
            session_id: random_uuid(),
        })
        .await?;
    loop {
        match reader.next(State::Login).await? {
            Packet::Login(login::Serverbound::LoginAcknowledged) => break,
            other => debug!("ignoring {other:?} before Login Acknowledged"),
        }
    }

    // Configuration.
    let mut brand = Vec::new();
    brand.put_string(crate::BRAND);
    writer.queue(&configuration::CustomPayload {
        channel: "minecraft:brand".into(),
        data: brand,
    })?;
    if let Some(links) = crate::serverlist::server_links(ctx.config.server_dir.as_deref()) {
        writer.queue(&configuration::ServerLinks { links })?;
    }
    writer.queue(&configuration::UpdateEnabledFeatures {
        flags: vec!["minecraft:vanilla".into()],
    })?;
    writer.queue(&configuration::SelectKnownPacks {
        packs: vec![ctx.core_pack.clone()],
    })?;
    writer.flush().await?;

    let max_view = i32::from(ctx.view_distance.load(std::sync::atomic::Ordering::Relaxed));
    let client_knows_core = loop {
        match reader.next(State::Configuration).await? {
            Packet::Configuration(configuration::Serverbound::SelectKnownPacks(packs)) => {
                break packs.contains(&ctx.core_pack);
            }
            Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
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
            match reader.next(State::Configuration).await? {
                Packet::Configuration(configuration::Serverbound::AcceptCodeOfConduct) => break,
                Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                    note_client_info(session, &info, max_view);
                }
                Packet::Configuration(configuration::Serverbound::KeepAlive(id)) => {
                    writer.send(&configuration::KeepAlive { id }).await?;
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
            match reader.next(State::Configuration).await? {
                Packet::Configuration(configuration::Serverbound::ResourcePack { id, result })
                    if id == pack.id =>
                {
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
                Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                    note_client_info(session, &info, max_view);
                }
                other => trace!("configuration: {other:?}"),
            }
        }
    }
    writer.queue(&configuration::FinishConfiguration)?;
    writer.flush().await?;

    loop {
        match reader.next(State::Configuration).await? {
            Packet::Configuration(configuration::Serverbound::FinishConfiguration) => {
                return Ok(true);
            }
            Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                note_client_info(session, &info, max_view);
            }
            other => trace!("configuration: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Play

async fn play(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    session: &Session,
) -> Result<()> {
    let me = &session.player;
    let (uuid, entity_id) = (me.profile.uuid, me.entity_id);
    // Where the player was last (the overworld for a new one).
    let mut ctx = writer.ctx.of_player(uuid);
    // Subscribe before reading the player list, so nobody slips through.
    let mut events = ctx.events.subscribe();
    crate::commands::load_progress(&ctx, uuid, &me.extra.recipes, &me.extra.book);
    crate::stats::load(&ctx, uuid);
    let mut pose = session
        .saved
        .as_ref()
        .map_or_else(|| world_spawn_pose(&ctx, me.game_mode), |s| s.pose);
    let (x, y, z) = (pose.x, pose.y, pose.z);

    writer.queue(&play::Login {
        entity_id,
        hardcore: false,
        dimensions: crate::context::DIMENSIONS
            .iter()
            .map(|d| (*d).to_owned())
            .collect(),
        max_players: ctx.player_limit.load(std::sync::atomic::Ordering::Relaxed) as i32,
        view_distance: i32::from(ctx.view_distance.load(std::sync::atomic::Ordering::Relaxed)),
        simulation_distance: i32::from(ctx.config.simulation_distance),
        reduced_debug_info: ctx.game_rule_bool("reduced_debug_info"),
        enable_respawn_screen: !ctx.game_rule_bool("immediate_respawn"),
        do_limited_crafting: ctx.game_rule_bool("limited_crafting"),
        dimension_type: ctx.world.dimension_type_id,
        dimension: ctx.world.dimension.clone(),
        hashed_seed: 0,
        game_mode: me.game_mode,
        previous_game_mode: None,
        is_debug: false,
        is_flat: ctx.world.is_flat,
        death_location: None,
        portal_cooldown: 0,
        sea_level: ctx.world.sea_level,
        online_mode: ctx.config.online_mode,
        enforces_secure_chat: ctx.config.enforce_secure_profile,
    })?;
    let mut flying = me.flying || me.game_mode == 3;
    writer.queue(&abilities(me.game_mode, flying))?;
    send_permissions(writer, &ctx, uuid, &me.profile.name, entity_id)?;
    writer.queue(&ctx.recipes.get().update_packet())?;
    writer.queue(&crate::commands::settings_packet(&ctx, uuid))?;
    writer.queue(&crate::commands::initial_book(&ctx, uuid))?;
    writer.queue(&crate::commands::initial_advancements(&ctx, uuid))?;
    let mut awaiting_teleport = Some(1);
    let mut next_teleport = 1;
    writer.queue(&play::PlayerPosition {
        teleport_id: 1,
        x,
        y,
        z,
        velocity: [0.0; 3],
        yaw: pose.yaw,
        pitch: pose.pitch,
        flags: 0,
    })?;

    // Player list first (the client takes skins, including its own, from it),
    // then the players who are already in the world.
    let everyone: Vec<OnlinePlayer> = ctx
        .players
        .lock()
        .expect("players lock")
        .values()
        .cloned()
        .collect();
    writer.queue(&play::PlayerInfoUpdate {
        actions: play::player_info::ALL,
        players: everyone.iter().map(OnlinePlayer::info_entry).collect(),
    })?;
    let _ = ctx.events.send(Event::PlayerJoined(me.clone()));
    ctx.management_bus
        .publish(&crate::notify::Note::PlayerJoined(crate::notify::Player {
            id: Some(crate::util::uuid_string(me.profile.uuid)),
            name: Some(me.profile.name.clone()),
        }));
    // A rejoin should see the whole map again, not a rectangle of what
    // changed while the previous session still had it.
    crate::maps::forget(&ctx, uuid);
    let mut visible: HashSet<u128> = HashSet::new();
    let mut dots: HashMap<u128, [i32; 3]> = HashMap::new();
    for other in everyone
        .iter()
        .filter(|p| p.profile.uuid != uuid && p.dimension == ctx.world.dimension)
    {
        if let Some(other_pose) = other.pose {
            spawn_player(writer, &ctx, other, other_pose)?;
            visible.insert(other.profile.uuid);
        }
    }

    let time = ctx.time.lock().expect("time lock").packet(true);
    writer.queue(&time)?;
    let ticking = ctx.ticking.lock().expect("ticking lock").clone();
    writer.queue(&ticking)?;
    writer.queue(&play::TickingStep { steps: 0 })?;
    writer.queue(&spawn_packet(&ctx))?;
    writer.queue(&ctx.border.lock().expect("border lock").packet())?;
    {
        let w = ctx.weather.lock().expect("weather lock");
        if w.rain_level > 0.0 {
            let changes = crate::weather::Changes {
                raining: Some(true),
                rain_level: Some(w.rain_level),
                thunder_level: Some(w.thunder_level),
            };
            send_weather(writer, &changes)?;
        }
    }
    writer.queue(&play::GameEvent {
        event: play::game_event::START_WAITING_FOR_CHUNKS,
        value: 0.0,
    })?;
    // Which skin layers to draw comes back from the server as entity metadata;
    // without it the client shows only the base layer.
    writer.queue(&skin_parts_metadata(entity_id, session.skin_parts))?;
    // Out before the chunks, which take a while.
    writer.flush().await?;
    let mut view = ChunkView::new(session.view_distance, ctx.clone());
    view.move_to(
        writer,
        chunk_of(x),
        chunk_of(z),
        loads_new_chunks(&ctx, me.game_mode),
    )
    .await?;
    writer.queue(&play::SystemChat {
        content: text::colored(
            &format!(
                "Welcome to {}! You are in {}.",
                crate::BRAND,
                ctx.world.description
            ),
            "gold",
        ),
        overlay: false,
    })?;
    writer.flush().await?;

    // Now others can see us.
    update_player(&ctx, uuid, |p| {
        p.pose = Some(pose);
        p.skin_parts = session.skin_parts;
    });
    send_bars(writer, &ctx, uuid, &mut dots, false)?;
    let _ = ctx.events.send(Event::PlayerMoved {
        uuid,
        entity_id,
        pose,
    });
    info!(
        "{}[/{}] logged in with entity id {} at ({}, {}, {})",
        me.profile.name, session.peer, me.entity_id, pose.x, pose.y, pose.z
    );
    info!("{} joined the game", me.profile.name);
    let joined = text::translate_colored(
        "multiplayer.player.joined",
        vec![text::plain(&me.profile.name)],
        "yellow",
    );
    let _ = ctx.events.send(Event::Chat(joined));

    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    let mut pending_keep_alive: Option<(i64, Instant)> = None;
    // The inventory from the player file, sent whole as vanilla does on join.
    let hands = session
        .saved
        .as_ref()
        .map_or_else(Hands::default, |s| s.hands.clone());
    writer.queue(&play::ContainerSetContent {
        window: 0,
        state_id: 1,
        slots: hands.slots.clone(),
        carried: None,
    })?;
    writer.queue(&play::SetHeldSlot {
        slot: hands.selected as i32,
    })?;
    let main_hand = hands.main_hand().cloned();
    ctx.inventories
        .lock()
        .expect("inventories lock")
        .insert(uuid, hands);
    update_player(&ctx, uuid, |p| p.main_hand = main_hand);
    let mut game_mode = me.game_mode;
    let mut sneaking = false;
    // The vehicle the player sits in.
    let mut riding: Option<i32> = None;
    let mut floating = crate::guard::Floating::default();
    let mut spam = crate::guard::Spam::default();
    let mut rate = crate::guard::Rate::new(ctx.config.rate_limit, Instant::now());
    let mut windows = windows::Windows::new();
    let vitals = session
        .saved
        .as_ref()
        .map(|s| s.vitals.clone())
        .unwrap_or_default();
    writer.queue(&vitals.packet())?;
    writer.queue(&vitals.experience_packet())?;
    let (effects, effect_data, attributes) =
        crate::effects::join_packets(&ctx, &vitals, me.entity_id);
    for p in effects {
        writer.queue(&p)?;
    }
    if let Some(p) = effect_data {
        writer.queue(&p)?;
    }
    if let Some(p) = attributes {
        writer.queue(&p)?;
    }
    ctx.vitals.lock().expect("vitals lock").insert(uuid, vitals);
    let bars = ctx
        .bossbars
        .lock()
        .expect("bossbars lock")
        .join_packets(uuid);
    for p in bars {
        writer.queue(&p)?;
    }
    let scores = ctx
        .scoreboard
        .lock()
        .expect("scoreboard lock")
        .join_packets();
    for p in &scores {
        send_scoreboard(writer, p)?;
    }
    let mut text_filtering = session.text_filtering;
    let (replies_tx, mut replies) = tokio::sync::mpsc::unbounded_channel::<Outcome>();
    // Non-player entities this client has been told about.
    let mut seen_entities: HashSet<i32> = HashSet::new();
    // Back on what they rode when they left.
    if let Some(vehicle) = &me.extra.root_vehicle {
        crate::persist::ride_again(&ctx, entity_id, vehicle);
    }
    let existing: Vec<mc_entity::Entity> = ctx.entities.lock().expect("entities lock").clone();
    for entity in existing {
        let (x, _, z) = entity.block_pos();
        if view.has_block(Position::new(x, 0, z)) {
            spawn_entity(writer, &ctx, &entity)?;
            seen_entities.insert(entity.id);
        }
    }
    loop {
        tokio::select! {
                Some(mut outcome) = replies.recv() => {
                    if let Some((position, kind)) = outcome.open.take() {
                        windows.open_container(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, position, kind)?;
                    }
                    if let Some((mount, columns)) = outcome.open_mount.take() {
                        windows.open_mount(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, mount, columns)?;
                    }
                    if let Some((position, front)) = outcome.open_sign.take() {
                        writer.queue(&play::OpenSignEditor { position, front })?;
                    }
                    if let Some((position, menu)) = outcome.open_menu.take() {
                        windows.open_menu(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, position, menu)?;
                    }
                    if std::mem::take(&mut outcome.open_crafting) {
                        windows.open_crafting(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose })?;
                    }
                    answer(writer, &ctx, outcome)?;
                }
                packet = reader.next(State::Play) => {
                    let Packet::Play(packet) = packet? else {
                        unreachable!("play state decodes play packets")
                    };
                    if rate.received(Instant::now()) {
                        let reason = text::translate("disconnect.exceeded_packet_rate", vec![]);
                        writer.send(&play::Disconnect { reason }).await?;
                        bail!("{} exceeded the packet rate limit", me.profile.name);
                    }
                    let chatting = matches!(packet, play::Serverbound::Chat(_) | play::Serverbound::ChatCommand { .. });
                    if chatting && spam.said(Instant::now()) && crate::tick::lock(&ctx.lists).op(uuid, &me.profile.name).is_none() {
                        let reason = text::translate("disconnect.spam", vec![]);
                        writer.send(&play::Disconnect { reason }).await?;
                        bail!("{} was kicked for spamming", me.profile.name);
                    }
                    // Movement sent before the client saw our teleport is stale.
                    let settled = awaiting_teleport.is_none() && riding.is_none();
                    let mut moved = false;
                    let before = pose;
                    match packet {
                        play::Serverbound::AcceptTeleportation { teleport_id, .. } => {
                            if awaiting_teleport == Some(teleport_id) {
                                awaiting_teleport = None;
                            }
                        }
                        play::Serverbound::MovePlayerPos { x, y, z, flags } if settled => {
                            moved = set_position(&mut pose, x, y, z, flags);
                        }
                        play::Serverbound::MovePlayerPosRot { x, y, z, yaw, pitch, flags } if settled => {
                            moved = set_position(&mut pose, x, y, z, flags);
                            if moved && yaw.is_finite() && pitch.is_finite() {
                                (pose.yaw, pose.pitch) = (yaw, pitch);
                            }
                        }
                        // Landing or leaving the ground without moving: the client
                        // sends only the flags (the dig check needs them right).
                        play::Serverbound::MovePlayerStatusOnly { flags } if settled => {
                            pose.on_ground = flags & 1 != 0;
                            moved = true;
                        }
                        play::Serverbound::MovePlayerRot { yaw, pitch, flags } if settled => {
                            if yaw.is_finite() && pitch.is_finite() {
                                (pose.yaw, pose.pitch, pose.on_ground) = (yaw, pitch, flags & 1 != 0);
                                moved = true;
                            }
                        }
                        play::Serverbound::KeepAlive(id) => {
                            if pending_keep_alive.is_some_and(|(expected, _)| expected == id) {
                                pending_keep_alive = None;
                            }
                        }
                        play::Serverbound::Chat(chat) => {
                            if ctx.config.enforce_secure_profile && chat.signature.is_none() {
                                info!("{} tried to chat without a signature", me.profile.name);
                                let line = text::translate("multiplayer.disconnect.unsigned_chat", vec![]);
                                let _ = ctx.events.send(Event::Message { uuid, line, overlay: false });
                            } else {
                                // `text_filtering` asks for a profanity pass. There is
                                // no external filter; the line is delivered as written.
                                trace!(text_filtering, "chat delivered as written");
                                info!("<{}> {}", me.profile.name, chat.message);
                                let line = text::translate("chat.type.text", vec![text::plain(&me.profile.name), text::plain(&chat.message)]);
                                let _ = ctx.events.send(Event::Chat(line));
                            }
                        }
                        play::Serverbound::ChatCommand { command } => {
                            info!("{} issued server command: /{command}", me.profile.name);
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let source = Source::Player { uuid, name: me.profile.name.clone(), pose };
                            let action = Action::Command { line: command, source };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::CommandSuggestion { id, text } => {
                            let source = Source::Player { uuid, name: me.profile.name.clone(), pose };
                            if let Some(answer) = crate::commands::suggest(&ctx, &source, id, &text) {
                                writer.queue(&answer)?;
                            }
                        }
                        play::Serverbound::SetCreativeModeSlot { slot: -1, item: Some(stack) } if game_mode == CREATIVE => {
                            // Dropped out of the creative inventory.
                            if stack.count > 0 {
                                crate::stats::threw(&ctx, uuid, &stack);
                                crate::tick::throw_item_aged(&ctx, pose, stack, crate::dropped::LIFETIME - 1200);
                            }
                        }
                        play::Serverbound::SetCreativeModeSlot { slot, item } if game_mode == CREATIVE => {
                            if let Ok(slot) = usize::try_from(slot) {
                                change_hands(&ctx, uuid, entity_id, |hands| {
                                    if let Some(s) = hands.slots.get_mut(slot) {
                                        *s = item;
                                    }
                                });
                                if slot == 6 {
                                    crate::worn::settle_gliding(&ctx, uuid);
                                }
                            }
                        }
                        play::Serverbound::SetCarriedItem { slot } => {
                            crate::vitals::stop_using(&ctx, uuid);
                            crate::combat::shield(&ctx, uuid, false);
                            crate::combat::reset_strength(&ctx, uuid);
                            if let Ok(slot @ 0..=8) = usize::try_from(slot) {
                                change_hands(&ctx, uuid, entity_id, |hands| hands.selected = slot);
                            }
                        }
                        play::Serverbound::ContainerClick { window, slot, button, mode, changed, carried, .. } => {
                            let click = windows::Click { window, slot, button, mode, changed, carried };
                            windows.click(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, click)?;
                        }
                        play::Serverbound::ContainerButtonClick { window, button } => {
                            windows.button(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, window, button)?;
                        }
                        play::Serverbound::PlaceRecipe { window, recipe, all } => {
                            windows.place_recipe(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, window, recipe, all)?;
                        }
                        play::Serverbound::RecipeBookChangeSettings { book, open, filter } => {
                            crate::commands::book_settings(&ctx, uuid, book, open, filter);
                        }
                        play::Serverbound::RecipeBookSeenRecipe { recipe } => {
                            crate::commands::recipe_seen(&ctx, uuid, recipe);
                        }
                        play::Serverbound::SeenAdvancements { tab } => {
                            if let Some(p) = crate::commands::seen_tab(tab) {
                                writer.queue(&p)?;
                            }
                        }
                        play::Serverbound::ContainerSlotStateChanged { slot, window, enabled } => {
                            windows.slot_state(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, window, slot, enabled)?;
                        }
                        play::Serverbound::EditBook { slot, pages, title } => {
                            let index = match slot {
                                0..=8 => 36 + slot as usize,
                                40 => 45,
                                _ => usize::MAX,
                            };
                            let mut changed = None;
                            change_hands(&ctx, uuid, entity_id, |hands| {
                                let Some(Some(stack)) = hands.slots.get_mut(index) else { return };
                                if crate::books::edit(&ctx, stack, &me.profile.name, pages, title) {
                                    changed = Some(stack.clone());
                                }
                            });
                            if let Some(stack) = changed {
                                windows.inventory_slot(writer, index as i16, Some(stack))?;
                            }
                        }
                        play::Serverbound::RenameItem { name } => {
                            windows.rename(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, &name)?;
                        }
                        play::Serverbound::SetBeacon { primary, secondary } => {
                            windows.set_beacon(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, primary, secondary)?;
                        }
                        play::Serverbound::Interact { entity_id: target, .. }
                            if game_mode != 3 && crate::trading::is_merchant(&ctx, target) =>
                        {
                            let near = ctx.entities.lock().expect("entities lock").iter()
                                .find(|e| e.id == target)
                                .is_some_and(|e| {
                                    let (dx, dy, dz) = (e.pos[0] - pose.x, e.pos[1] - pose.y, e.pos[2] - pose.z);
                                    dx * dx + dy * dy + dz * dz <= 36.0
                                });
                            if near {
                                windows.open_merchant(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, target)?;
                            }
                        }
                        play::Serverbound::SelectTrade { index } => {
                            windows.select_trade(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, index)?;
                        }
                        play::Serverbound::ContainerClose { window } => {
                            windows.close(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, window)?;
                        }
                        play::Serverbound::PlayerAction { status: 5, position, face, sequence } => {
                            crate::vitals::stop_using(&ctx, uuid);
                            crate::combat::shield(&ctx, uuid, false);
                            // Letting go of use: a drawn bow, trident or crossbow
                            // shoots on the tick thread (where the draw was stored).
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::Dig { status: 5, position, face, sequence };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::PlayerCommand { action: 5, .. } => {
                            // The inventory key while riding: a boat's chest, or a mount's.
                            if let Some((position, kind)) = crate::vehicles::ridden_inventory(&ctx, entity_id) {
                                windows.open_container(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, position, kind)?;
                            } else {
                                let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action: Action::MountInventory, reply: replies_tx.clone() });
                            }
                        }
                        play::Serverbound::PlayerCommand { action: 3, data, .. } => {
                            // The jump key let go while riding (a horse's jump, a nautilus's dash).
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action: Action::MountJump { power: data }, reply: replies_tx.clone() });
                        }
                        play::Serverbound::PlayerCommand { action: action @ (1 | 2), .. } => {
                            crate::combat::set_sprinting(&ctx, uuid, action == 1);
                            crate::stats::set_sprinting(&ctx, uuid, action == 1);
                        }
                        play::Serverbound::PlayerCommand { action: 0, .. } => {
                            // The leave bed button.
                            crate::useblocks::wake(&ctx, uuid);
                        }
                        play::Serverbound::PlayerCommand { action: 6, .. } => {
                            crate::worn::start_gliding(&ctx, uuid);
                        }
                        play::Serverbound::PlayerAction { status: status @ (3 | 4), .. } => {
                            // Q: drop one (4) or the whole stack (3) from the main hand.
                            let received = std::time::Instant::now();
                            debug!(%uuid, status, "drop key: packet received");
                            let mut thrown = None;
                            let mut changed_slot = None;
                            change_hands(&ctx, uuid, entity_id, |hands| {
                                let slot = hands.hand_slot(0);
                                if let Some(stack) = &mut hands.slots[slot] {
                                    let n = if status == 3 { stack.count } else { 1 };
                                    thrown = Some(play::ItemStack { count: n, ..stack.clone() });
                                    stack.count -= n;
                                    if stack.count <= 0 {
                                        hands.slots[slot] = None;
                                    }
                                    changed_slot = Some((slot, hands.slots[slot].clone()));
                                }
                            });
                            debug!(
                                %uuid,
                                status,
                                thrown = thrown.as_ref().map(|s| s.count),
                                took_us = received.elapsed().as_micros() as u64,
                                "drop key: handled",
                            );
                            if let Some(stack) = thrown {
                                crate::stats::threw(&ctx, uuid, &stack);
                                crate::tick::throw_item(&ctx, pose, stack);
                            }
                            if let Some((slot, item)) = changed_slot {
                                windows.inventory_slot(writer, slot as i16, item)?;
                            }
                        }
                        play::Serverbound::PlayerAction { status: 6, .. } if game_mode != 3 => {
                            // F: the main hand and the off hand trade places.
                            let mut swapped = None;
                            change_hands(&ctx, uuid, entity_id, |hands| {
                                let main = hands.hand_slot(0);
                                hands.slots.swap(main, 45);
                                swapped = Some((main, hands.slots[main].clone(), hands.slots[45].clone()));
                            });
                            if let Some((main, in_main, in_off)) = swapped {
                                windows.inventory_slot(writer, main as i16, in_main)?;
                                windows.inventory_slot(writer, 45, in_off.clone())?;
                                let _ = ctx.events.send(Event::Equipment { uuid, entity_id, slots: vec![(1, in_off)] });
                            }
                        }
                        play::Serverbound::ClientCommand { action: 0 } => {
                            let dead = ctx.vitals.lock().expect("vitals lock").get(&uuid).is_some_and(|v| v.dead);
                            if dead {
                                let health = crate::vitals::respawn(&ctx, uuid);
                                let (dimension, target) = respawn_target(&ctx, uuid);
                                if let Some(there) = ctx.in_dimension(&dimension) {
                                    crate::tickets::player_spawn(&there, target.x, target.z);
                                }
                                if dimension != ctx.world.dimension {
                                    // Another dimension: the switch sends it all.
                                    let _ = ctx.events.send(Event::ChangeDimension {
                                        uuid,
                                        dimension,
                                        pose: target,
                                        after_death: true,
                                    });
                                    continue;
                                }
                                writer.queue(&play::Respawn {
                                    dimension_type: ctx.world.dimension_type_id,
                                    dimension: ctx.world.dimension.clone(),
                                    hashed_seed: 0,
                                    game_mode,
                                    previous_game_mode: None,
                                    is_debug: false,
                                    is_flat: ctx.world.is_flat,
                                    death_location: None,
                                    portal_cooldown: 0,
                                    sea_level: ctx.world.sea_level,
                                    data_kept: 0,
                                })?;
                                // The client makes a new player at op level 0:
                                // without this F3+F4 is refused.
                                send_permissions(writer, &ctx, uuid, &me.profile.name, entity_id)?;
                                flying = game_mode == 3;
                                update_player(&ctx, uuid, |p| p.flying = flying);
                                writer.queue(&abilities(game_mode, flying))?;
                                writer.queue(&health)?;
                                if let Some(xp) = ctx.vitals.lock().expect("vitals lock").get(&uuid).map(crate::vitals::Vitals::experience_packet) {
                                    writer.queue(&xp)?;
                                }
                                pose = target;
                                next_teleport += 1;
                                awaiting_teleport = Some(next_teleport);
                                writer.queue(&play::PlayerPosition {
                                    teleport_id: next_teleport,
                                    x: pose.x,
                                    y: pose.y,
                                    z: pose.z,
                                    velocity: [0.0; 3],
                                    yaw: pose.yaw,
                                    pitch: pose.pitch,
                                    flags: 0,
                                })?;
                                writer.queue(&play::GameEvent {
                                    event: play::game_event::START_WAITING_FOR_CHUNKS,
                                    value: 0.0,
                                })?;
                                // The client starts a fresh world: everything again.
                                view = ChunkView::new(session.view_distance, ctx.clone());
                                seen_entities.clear();
                                view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z), loads_new_chunks(&ctx, game_mode)).await?;
                                let hands = ctx.inventories.lock().expect("inventories lock").get(&uuid).cloned().unwrap_or_default();
                                windows.state_id += 1;
                                writer.queue(&play::ContainerSetContent {
                                    window: 0,
                                    state_id: windows.state_id,
                                    slots: hands.slots.clone(),
                                    carried: None,
                                })?;
                                writer.queue(&skin_parts_metadata(entity_id, session.skin_parts))?;
                                update_player(&ctx, uuid, |p| p.pose = Some(pose));
                                let _ = ctx.events.send(Event::PlayerMoved { uuid, entity_id, pose });
                                dots.clear();
                                send_bars(writer, &ctx, uuid, &mut dots, false)?;
                            }
                        }
                        play::Serverbound::PlayerInput { flags } => {
                            sneaking = flags & 0x20 != 0;
                            crate::stats::set_sneaking(&ctx, uuid, sneaking);
                            if sneaking && riding.is_some() {
                                let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                let action = Action::Vehicle(crate::vehicles::Act::Dismount);
                                let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                            }
                        }
                        play::Serverbound::PaddleBoat { left, right } if riding.is_some() => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::Vehicle(crate::vehicles::Act::Paddle { left, right });
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::MoveVehicle { x, y, z, yaw, pitch, on_ground } if riding.is_some() && awaiting_teleport.is_none() => {
                            if [x, y, z].iter().all(|c| c.is_finite()) {
                                pose = Pose { x, y, z, yaw, pitch, on_ground: false };
                                update_player(&ctx, uuid, |p| p.pose = Some(pose));
                                view.move_to(writer, chunk_of(x), chunk_of(z), loads_new_chunks(&ctx, game_mode)).await?;
                                let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                let action = Action::Vehicle(crate::vehicles::Act::Move { pos: [x, y, z], yaw, on_ground });
                                let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                            }
                        }
                        play::Serverbound::PlayerAbilities { flags } if matches!(game_mode, 1 | 3) => {
                            flying = flags & play::abilities::FLYING != 0 || game_mode == 3;
                            update_player(&ctx, uuid, |p| p.flying = flying);
                        }
                        play::Serverbound::Attack { entity_id: target } if game_mode != 3 => {
                            // Taken now: the landing packet often follows in the
                            // same breath and clears the fall before the blow runs.
                            let fall = crate::tick::lock(&ctx.vitals).get(&uuid).map(|v| v.fall_distance);
                            let victim = ctx.players.lock().expect("players lock").values()
                                .find(|p| p.entity_id == target && p.dimension == ctx.world.dimension)
                                .and_then(|p| p.pose.map(|at| (p.profile.uuid, at)));
                            if let Some((victim, at)) = victim {
                                let (dx, dy, dz) = (at.x - pose.x, at.y - pose.y, at.z - pose.z);
                                // Reach 3 from the docs, with some room for lag.
                                if dx * dx + dy * dy + dz * dz <= 36.0 {
                                    let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                    crate::combat::player_hits_player(&ctx, &actor, &me.profile.name, victim, fall);
                                }
                            } else {
                                let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                let action = Action::UseEntity { entity_id: target, hand: 0, attack: true, at: None, fall };
                                let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                            }
                        }
                        play::Serverbound::Interact { entity_id: target, hand, location } if game_mode != 3 => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::UseEntity { entity_id: target, hand, attack: false, at: location, fall: None };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::Punch => {
                            let _ = ctx.events.send(Event::Swing { uuid, entity_id, times: 1 });
                            if game_mode != 3 {
                                let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action: Action::Punch, reply: replies_tx.clone() });
                            }
                        }
                        // Block changes happen on the tick thread; the answer comes back
                        // through `replies`.
                        play::Serverbound::PlayerAction { status, position, face, sequence } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::Dig { status, position, face, sequence };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::UseItemOn { hand, position, face, cursor, sequence, .. } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::UseItemOn { hand, position, face, cursor, sequence };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::UseItem { hand, sequence, yaw, pitch } => {
                            writer.queue(&play::BlockChangedAck { sequence })?;
                            let held = ctx.inventories.lock().expect("inventories lock")
                                .get(&uuid).and_then(|h| h.held(hand).map(|s| s.item));
                            if held.is_some_and(|i| ctx.items.name(i) == Some("minecraft:written_book")) {
                                // Selectors and scores in the text are filled in for the reader.
                                let mut resolved = None;
                                change_hands(&ctx, uuid, entity_id, |hands| {
                                    let slot = hands.hand_slot(hand);
                                    if let Some(Some(stack)) = hands.slots.get_mut(slot)
                                        && crate::books::resolve(&ctx, uuid, stack)
                                    {
                                        resolved = Some((slot, stack.clone()));
                                    }
                                });
                                if let Some((slot, stack)) = resolved {
                                    windows.inventory_slot(writer, slot as i16, Some(stack))?;
                                }
                                writer.queue(&play::OpenBook { hand })?;
                            }
                            if !crate::spear::start(&ctx, uuid, entity_id, hand, game_mode) {
                                crate::vitals::start_using(&ctx, uuid, entity_id, hand, game_mode);
                            }
                            if crate::combat::holds_shield(&ctx, uuid, hand) {
                                crate::combat::shield(&ctx, uuid, true);
                            }
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::UseItem { hand, yaw, pitch };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::ChangeGameMode { mode: mode @ 0..=3 } => {
                            // As `/gamemode`, for those allowed to use it; the
                            // others are ignored.
                            if crate::commands::level(&ctx, uuid, &me.profile.name) >= 2 {
                                let name = ["survival", "creative", "adventure", "spectator"][mode as usize];
                                let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                                let source = Source::Player { uuid, name: me.profile.name.clone(), pose };
                                let action = Action::Command { line: format!("gamemode {name}"), source };
                                let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                            } else {
                                warn!("Player {} tried to change game mode without required permissions", me.profile.name);
                            }
                        }
                        play::Serverbound::PickItemFromBlock { position, .. } if game_mode != 3 => {
                            let reach = if game_mode == CREATIVE { 5.0 } else { 4.5 } + 1.0 + 0.87;
                            let (dx, dy, dz) = (
                                f64::from(position.x) + 0.5 - pose.x,
                                f64::from(position.y) + 0.5 - pose.y - 1.62,
                                f64::from(position.z) + 0.5 - pose.z,
                            );
                            let item = (dx * dx + dy * dy + dz * dz <= reach * reach)
                                .then(|| ctx.world.block(position, &ctx.data.blocks))
                                .flatten()
                                .and_then(|state| ctx.data.blocks.block_name(state))
                                .and_then(|name| crate::interact::clone_item(name, &ctx.items));
                            if let Some(item) = item {
                                let mut changed = Vec::new();
                                let mut selected = None;
                                change_hands(&ctx, uuid, entity_id, |hands| {
                                    let was = hands.selected;
                                    changed = hands
                                        .pick(item, game_mode == CREATIVE)
                                        .into_iter()
                                        .map(|i| (i, hands.slots[i].clone()))
                                        .collect();
                                    if hands.selected != was {
                                        selected = Some(hands.selected);
                                    }
                                });
                                for (slot, stack) in changed {
                                    windows.inventory_slot(writer, slot as i16, stack)?;
                                }
                                if let Some(slot) = selected {
                                    writer.queue(&play::SetHeldSlot { slot: slot as i32 })?;
                                }
                            }
                        }
                        play::Serverbound::PickItemFromEntity { entity_id: target, .. } if game_mode != 3 => {
                            // The tick thread has the entities out while it moves
                            // them: look again a little later.
                            let mut item = None;
                            for _ in 0..5 {
                                let found = ctx.entities.lock().expect("entities lock").iter()
                                    .find(|e| e.id == target && !e.removed)
                                    .map(|e| {
                                        let (dx, dy, dz) = (e.pos[0] - pose.x, e.pos[1] - pose.y, e.pos[2] - pose.z);
                                        (dx * dx + dy * dy + dz * dz <= 64.0, crate::mobs::eggs::pick(&ctx, e))
                                    });
                                if let Some((near, egg)) = found {
                                    item = egg.filter(|_| near);
                                    break;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                            if let Some(item) = item {
                                let mut changed = Vec::new();
                                let mut selected = None;
                                change_hands(&ctx, uuid, entity_id, |hands| {
                                    let was = hands.selected;
                                    changed = hands
                                        .pick(item, game_mode == CREATIVE)
                                        .into_iter()
                                        .map(|i| (i, hands.slots[i].clone()))
                                        .collect();
                                    if hands.selected != was {
                                        selected = Some(hands.selected);
                                    }
                                });
                                for (slot, stack) in changed {
                                    windows.inventory_slot(writer, slot as i16, stack)?;
                                }
                                if let Some(slot) = selected {
                                    writer.queue(&play::SetHeldSlot { slot: slot as i32 })?;
                                }
                            }
                        }
                        play::Serverbound::ClientCommand { action: 1 } => {
                            // Everything counted so far (the menu waits for it).
                            writer.queue(&crate::stats::packet(&ctx, uuid))?;
                        }
                        play::Serverbound::BlockEntityTagQuery { transaction, position } => {
                            if crate::commands::level(&ctx, uuid, &me.profile.name) >= 2 {
                                let data = ctx.world.block_entity(position, &ctx.data.blocks).map(|be| be.data);
                                writer.queue(&play::TagQuery { transaction, data })?;
                            }
                        }
                        play::Serverbound::TeleportToEntity { target } if game_mode == 3 => {
                            let to = ctx.players.lock().expect("players lock").get(&target).and_then(|p| p.pose);
                            if let Some(to) = to {
                                let _ = ctx.events.send(Event::Teleport { uuid, pose: to });
                            }
                        }
                        play::Serverbound::SetCommandBlock { position, command, mode, flags } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::CommandBlock { position, command, mode, flags };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::SetJigsawBlock { position, name, target, pool, final_state, joint, selection_priority, placement_priority } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::SetJigsaw { position, name, target, pool, final_state, joint, selection_priority, placement_priority };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::SetTestBlock { position, mode, message } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::SetTestBlock { position, mode, message };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::TestInstanceBlockAction { position, action, test, size, rotation, ignore_entities } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::TestInstance { position, action, test, size, rotation, ignore_entities };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::JigsawGenerate { position, levels, keep_jigsaws } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::GenerateJigsaw { position, levels, keep_jigsaws };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::SetStructureBlock { position, action, mode, name, offset, size, mirror, rotation, metadata, integrity, seed, flags } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::SetStructure { position, action, mode, name, offset, size, mirror, rotation, metadata, integrity, seed, flags };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::SetCommandMinecart { entity_id: cart, command, track_output } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let action = Action::CommandMinecart { entity_id: cart, command, track_output };
                            let _ = ctx.actions.send(Request { dim: ctx.dim, actor, action, reply: replies_tx.clone() });
                        }
                        play::Serverbound::SignUpdate { position, front, lines } => {
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            if let Some(event) = crate::interact::sign_update(&ctx, actor, position, front, lines) {
                                let _ = ctx.events.send(event);
                            }
                        }
                        play::Serverbound::ClientInformation(info) => {
                            text_filtering = info.text_filtering;
                            writer.queue(&skin_parts_metadata(entity_id, info.skin_parts))?;
                            update_player(&ctx, uuid, |p| p.skin_parts = info.skin_parts);
                            let _ = ctx.events.send(Event::SkinParts {
                                uuid,
                                entity_id,
                                parts: info.skin_parts,
                            });
                        }
                        play::Serverbound::Other(name) => debug!("{}: unhandled {name}", me.profile.name),
                        other => trace!("play: {other:?}"),
                    }
                    if moved && settled {
                        let (gliding, levitating) = crate::tick::lock(&ctx.vitals)
                            .get(&uuid)
                            .map_or((false, false), |v| (v.gliding, v.effects.has("levitation")));
                        if crate::guard::too_far(&ctx, before, pose, gliding) {
                            warn!("{} moved too quickly! {},{},{}", me.profile.name, pose.x - before.x, pose.y - before.y, pose.z - before.z);
                            pose = before;
                            moved = false;
                            next_teleport += 1;
                            awaiting_teleport = Some(next_teleport);
                            writer.queue(&play::PlayerPosition {
                                teleport_id: next_teleport,
                                x: pose.x,
                                y: pose.y,
                                z: pose.z,
                                velocity: [0.0; 3],
                                yaw: pose.yaw,
                                pitch: pose.pitch,
                                flags: 0,
                            })?;
                        } else {
                            // Survival and adventure players held up by nothing.
                            let afloat = !ctx.config.allow_flight
                                && matches!(game_mode, 0 | 2)
                                && !gliding
                                && !levitating
                                && pose.y - before.y >= -0.03125
                                && crate::guard::nothing_around(&ctx, pose);
                            if floating.moved(afloat, Instant::now()) {
                                warn!("{} was kicked for floating too long!", me.profile.name);
                                let reason = text::translate("multiplayer.disconnect.flying", vec![]);
                                writer.send(&play::Disconnect { reason }).await?;
                                bail!("{} was flying", me.profile.name);
                            }
                        }
                    }
                    if moved {
                        let feet = Position::new(pose.x.floor() as i32, pose.y.floor() as i32, pose.z.floor() as i32);
                        let in_water = ctx
                            .world
                            .block(feet, &ctx.data.blocks)
                            .is_some_and(|s| ctx.data.blocks.is_fluid(s));
                        crate::vitals::moved(&ctx, uuid, before, pose, in_water);
                        view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z), loads_new_chunks(&ctx, game_mode)).await?;
                        sync_entities(writer, &ctx, &view, &mut seen_entities)?;
                        update_player(&ctx, uuid, |p| p.pose = Some(pose));
                        let _ = ctx.events.send(Event::PlayerMoved { uuid, entity_id, pose });
                    }
                }
                _ = keep_alive.tick() => {
                    if let Some((_, sent)) = pending_keep_alive {
                        if sent.elapsed() > KEEP_ALIVE_TIMEOUT {
                            let disconnect = play::Disconnect { reason: text::plain("Timed out") };
                            writer.send(&disconnect).await?;
                            bail!("keep-alive timeout");
                        }
                    } else {
                        let id = (random_u64() >> 1) as i64;
                        pending_keep_alive = Some((id, Instant::now()));
                        writer.queue(&play::KeepAlive { id })?;
                    }
                }
                event = events.recv() => {
                    match event {
                        Ok(Event::Chat(content)) => {
                            writer.queue(&play::SystemChat { content, overlay: false })?;
                        }
                        Ok(Event::Time(packet)) => writer.queue(&packet)?,
                        Ok(Event::LightChanged(chunks)) => {
                            for (x, z) in chunks {
                                if view.has_block(Position::new(x * 16, 0, z * 16))
                                    && let Some(light) = ctx.world.light_data(x, z)
                                {
                                    writer.queue(&play::LightUpdate { x, z, light })?;
                                }
                            }
                        }
                        Ok(Event::EntitySpawned(entity)) => {
                            let (x, _, z) = entity.block_pos();
                            if view.has_block(Position::new(x, 0, z)) && !seen_entities.contains(&entity.id) {
                                spawn_entity(writer, &ctx, &entity)?;
                                seen_entities.insert(entity.id);
                            }
                        }
                        Ok(Event::EntitiesMoved(moves)) => {
                            for (id, pos, on_ground) in moves {
                                if seen_entities.contains(&id) {
                                    writer.queue(&play::EntityPositionSync {
                                        entity_id: id,
                                        x: pos[0],
                                        y: pos[1],
                                        z: pos[2],
                                        yaw: 0.0,
                                        pitch: 0.0,
                                        on_ground,
                                    })?;
                                }
                            }
                        }
                        Ok(Event::MobsMoved(moves)) => {
                            for m in moves {
                                let at = Position::new(m.pos[0].floor() as i32, 0, m.pos[2].floor() as i32);
                                let in_view = view.has_block(at);
                                if seen_entities.contains(&m.id) {
                                    if !in_view {
                                        seen_entities.remove(&m.id);
                                        writer.queue(&play::RemoveEntities { entity_ids: vec![m.id] })?;
                                        continue;
                                    }
                                    writer.queue(&play::EntityPositionSync {
                                        entity_id: m.id,
                                        x: m.pos[0],
                                        y: m.pos[1],
                                        z: m.pos[2],
                                        yaw: m.yaw,
                                        pitch: m.pitch,
                                        on_ground: m.on_ground,
                                    })?;
                                    if let Some(velocity) = m.motion {
                                        writer.queue(&play::SetEntityMotion {
                                            entity_id: m.id,
                                            velocity,
                                        })?;
                                    }
                                    writer.queue(&play::RotateHead { entity_id: m.id, head_yaw: m.head_yaw })?;
                                } else if in_view {
                                    // A mob walked into view.
                                    let found = ctx.entities.lock().expect("entities lock").iter().find(|e| e.id == m.id).cloned();
                                    if let Some(entity) = found {
                                        spawn_entity(writer, &ctx, &entity)?;
                                        seen_entities.insert(entity.id);
                                    }
                                }
                            }
                        }
                        Ok(Event::MobsStatus(statuses)) => {
                            for (id, status) in statuses {
                                if seen_entities.contains(&id) {
                                    writer.queue(&play::EntityEvent { entity_id: id, status: status as i8 })?;
                                }
                            }
                        }
                        Ok(Event::EntitySync(rows)) => {
                            for r in rows {
                                // The vehicle carries the player along.
                                if riding == Some(r.id) && (pose.x, pose.y, pose.z) != (r.pos[0], r.pos[1], r.pos[2]) {
                                    pose = Pose { x: r.pos[0], y: r.pos[1], z: r.pos[2], ..pose };
                                    update_player(&ctx, uuid, |p| p.pose = Some(pose));
                                    view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z), loads_new_chunks(&ctx, game_mode)).await?;
                                }
                                if seen_entities.contains(&r.id) {
                                    writer.queue(&play::EntityPositionSync {
                                        entity_id: r.id,
                                        x: r.pos[0],
                                        y: r.pos[1],
                                        z: r.pos[2],
                                        yaw: r.yaw,
                                        pitch: r.pitch,
                                        on_ground: r.on_ground,
                                    })?;
                                    writer.queue(&play::SetEntityMotion {
                                        entity_id: r.id,
                                        velocity: r.vel,
                                    })?;
                                }
                            }
                        }
                        Ok(Event::Fx(fx)) => {
                            use crate::fx::Fx;
                            if let Fx::Passengers(p) = &fx {
                                if p.passengers.contains(&entity_id) {
                                    riding = Some(p.vehicle);
                                } else if riding == Some(p.vehicle) {
                                    riding = None;
                                }
                            }
                            let wanted = match (fx.entity(), fx.at()) {
                                (Some(id), _) => seen_entities.contains(&id) || visible_entity(&ctx, id, &visible),
                                (None, Some(at)) => {
                                    let (dx, dy, dz) = (pose.x - at[0], pose.y - at[1], pose.z - at[2]);
                                    dx * dx + dy * dy + dz * dz < 4096.0
                                }
                                _ => true,
                            };
                            if wanted {
                                match fx {
                                    Fx::Sound(p) => writer.queue(&p)?,
                                    Fx::Level(p) => writer.queue(&p)?,
                                    Fx::Particles(p) => writer.queue(&p)?,
                                    Fx::Status(p) => writer.queue(&p)?,
                                    Fx::Equip(p) => writer.queue(&p)?,
                                    Fx::Link(p) => writer.queue(&p)?,
                                    Fx::Cracks(p) => writer.queue(&p)?,
                                    Fx::Passengers(p) => writer.queue(&p)?,
                                    Fx::Motion(p) => writer.queue(&p)?,
                                    Fx::Damage(p) => writer.queue(&p)?,
                                    Fx::Swing(p) => writer.queue(&p)?,
                                    Fx::Data(p) => writer.queue(&p)?,
                                    Fx::Heard { event, at, dim } => writer.queue(&play::LevelEvent {
                                        event,
                                        position: crate::fx::heard_at([pose.x, pose.y, pose.z], ctx.dim, at, dim),
                                        data: 0,
                                        global: true,
                                    })?,
                                }
                            }
                        }
                        Ok(Event::EntitiesRemoved(ids)) => {
                            let gone: Vec<i32> = ids.into_iter().filter(|id| seen_entities.remove(id)).collect();
                            if !gone.is_empty() {
                                writer.queue(&play::RemoveEntities { entity_ids: gone })?;
                            }
                        }
                        Ok(Event::TickingState(packet)) => writer.queue(&packet)?,
                        Ok(Event::Health { uuid: who, health }) if who == uuid => writer.queue(&health)?,
                        Ok(Event::EntityData { uuid: who, packet })
                            if who == uuid || visible.contains(&who) =>
                        {
                            writer.queue(&packet)?;
                        }
                        Ok(Event::Attributes { uuid: who, packet })
                            if who == uuid || visible.contains(&who) =>
                        {
                            writer.queue(&packet)?;
                        }
                        Ok(Event::Hurt { uuid: who, entity_id: hurt, damage_type, direct }) if who == uuid || visible.contains(&who) => {
                            writer.queue(&play::DamageEvent { entity_id: hurt, damage_type, direct, cause: direct })?;
                        }
                        Ok(Event::WorldSound { pos, sound, category, volume, pitch }) => {
                            let reach = 16.0 * f64::from(volume.max(1.0));
                            let (dx, dy, dz) = (pose.x - pos[0], pose.y - pos[1], pose.z - pos[2]);
                            if dx * dx + dy * dy + dz * dz <= reach * reach
                                && let Some(id) = ctx.data.builtin.id("minecraft:sound_event", sound)
                            {
                                writer.queue(&play::Sound {
                                    id: Some(id),
                                    name: String::new(),
                                    category,
                                    x: pos[0],
                                    y: pos[1],
                                    z: pos[2],
                                    volume,
                                    pitch,
                                    seed: crate::util::random_u64() as i64,
                                })?;
                            }
                        }
                        Ok(Event::EntitiesData(packets)) => {
                            for packet in packets {
                                if seen_entities.contains(&packet.entity_id) {
                                    writer.queue(&packet)?;
                                }
                            }
                        }
                        Ok(Event::HandBroke { uuid: who, entity_id: id, status })
                            if who == uuid || visible.contains(&who) =>
                        {
                            writer.queue(&play::EntityEvent { entity_id: id, status })?;
                        }
                        Ok(Event::Explosion { center, radius, block_count, particle, sound, knockback }) => {
                            let (dx, dy, dz) = (pose.x - center[0], pose.y - center[1], pose.z - center[2]);
                            if dx * dx + dy * dy + dz * dz < 4096.0 {
                                writer.queue(&play::Explode {
                                    center,
                                    radius,
                                    block_count,
                                    knockback: knockback.iter().find(|(who, _)| *who == uuid).map(|&(_, v)| v),
                                    particle,
                                    sound,
                                })?;
                            }
                        }
                        Ok(Event::Died { uuid: who, entity_id: dead, message }) => {
                            if who == uuid {
                                let shown = ctx.game_rule_bool("show_death_messages");
                                let screen = if shown { message.clone() } else { mc_nbt::Tag::String(String::new()) };
                                writer.queue(&play::PlayerCombatKill { player: entity_id, message: screen })?;
                                // Everything carried falls out where the player died.
                                let keep = ctx.game_rule_bool("keep_inventory");
                                let lost: Vec<play::ItemStack> = if keep { Vec::new() } else {
                                    let mut inventories = ctx.inventories.lock().expect("inventories lock");
                                    inventories.get_mut(&uuid).map_or_else(Vec::new, |hands| {
                                        let mut all: Vec<play::ItemStack> = hands.slots.iter_mut().filter_map(Option::take).collect();
                                        all.extend(hands.carried.take());
                                        all
                                    })
                                };
                                // Curse of vanishing: gone with the death.
                                let lost = lost.into_iter()
                                    .filter(|s| !crate::enchant_effects::has_effect(&ctx, s, "minecraft:prevent_equipment_drop"));
                                for stack in lost {
                                    crate::tick::scatter_item(&ctx, pose, stack);
                                }
                                if !keep {
                                    update_player(&ctx, uuid, |p| p.main_hand = None);
                                    crate::vitals::drop_death_xp(&ctx, uuid, game_mode, [pose.x, pose.y, pose.z]);
                                }
                                if shown {
                                    crate::vitals::announce_death(&ctx, uuid, message);
                                }
                                crate::vitals::drop_pearls(&ctx, uuid);
                            } else {
                                if let Some(packet) = crate::locator::forget(who, &mut dots) {
                                    writer.queue(&packet)?;
                                }
                                if visible.remove(&who) {
                                    writer.queue(&play::RemoveEntities { entity_ids: vec![dead] })?;
                                }
                            }
                        }
                        Ok(Event::GameMode { uuid: who, mode }) => {
                            if who == uuid {
                                game_mode = mode;
                                writer.queue(&play::GameEvent {
                                    event: play::game_event::CHANGE_GAME_MODE,
                                    value: mode as f32,
                                })?;
                                // Creative keeps flying, spectators always fly,
                                // the other modes land.
                                flying = match mode {
                                    1 => flying,
                                    3 => true,
                                    _ => false,
                                };
                                update_player(&ctx, uuid, |p| p.flying = flying);
                                writer.queue(&abilities(mode, flying))?;
                            }
                            let known = ctx.players.lock().expect("players lock").get(&who).cloned();
                            if who != uuid
                                && let Some(player) = known.clone()
                                && let Some(at) = player.pose
                                && let Some(packet) = crate::locator::offer(&ctx, uuid, who, at, &mut dots, true)
                            {
                                writer.queue(&packet)?;
                            }
                            if let Some(player) = known {
                                writer.queue(&play::PlayerInfoUpdate {
                                    actions: play::player_info::UPDATE_GAME_MODE,
                                    players: vec![player.info_entry()],
                                })?;
                            }
                        }
                        Ok(Event::ContainerSlots { position, viewers, slots }) if viewers.contains(&uuid) => {
                            windows.container_slots(writer, position, slots)?;
                        }
                        Ok(Event::ContainerGone { position }) => windows.container_gone(writer, position)?,
                        Ok(Event::BlockEntity { position, kind, data }) if view.has_block(position) => {
                            writer.queue(&play::BlockEntityData { position, kind, data })?;
                        }
                        Ok(Event::BlockAction { position, action, param, block }) if view.has_block(position) => {
                            writer.queue(&play::BlockEvent { position, action, param, block })?;
                        }
                        Ok(Event::FurnaceProgress { position, values }) => {
                            windows.container_data(writer, position, &values)?;
                        }
                        Ok(Event::InventorySlot { uuid: who, slot, item }) if who == uuid => {
                            windows.inventory_slot(writer, slot, item)?;
                        }
                        Ok(Event::ItemTaken { item_entity, collector, count, slots }) => {
                            if seen_entities.contains(&item_entity) {
                                writer.queue(&play::TakeItemEntity {
                                    collected: item_entity,
                                    collector,
                                    count,
                                })?;
                            }
                            if collector == entity_id {
                                for (slot, item) in slots {
                                    windows.inventory_slot(writer, slot, item)?;
                                }
                            }
                        }
                        Ok(Event::TickingStep(packet)) => writer.queue(&packet)?,
                        Ok(Event::Permissions { uuid: who }) if who == uuid => {
                            send_permissions(writer, &ctx, uuid, &me.profile.name, entity_id)?;
                        }
                        Ok(Event::Message { uuid: who, line, overlay }) if who == uuid => {
                            writer.queue(&play::SystemChat { content: line, overlay })?;
                        }
                        Ok(Event::AdminFeedback { from, line }) if from != uuid => {
                            if crate::commands::level(&ctx, uuid, &me.profile.name) >= 2 {
                                writer.queue(&play::SystemChat { content: line, overlay: false })?;
                            }
                        }
                        Ok(Event::Weather(changes)) => send_weather(writer, &changes)?,
                        Ok(Event::BiomesChanged { chunks }) => {
                            let chunks: Vec<(i32, i32, Vec<u8>)> = chunks
                                .into_iter()
                                // Not to the chunks on the edge of the view.
                                .filter(|c| view.inside(*c))
                                .filter_map(|(x, z)| {
                                    Some((x, z, ctx.world.biome_data(x, z, &ctx.data.blocks)?))
                                })
                                .collect();
                            if !chunks.is_empty() {
                                writer.queue(&play::ChunksBiomes { chunks })?;
                            }
                        }
                        Ok(Event::Direct { uuid: who, packet }) if who == uuid => {
                            use crate::context::Direct;
                            match packet {
                                Direct::Title(p) => writer.queue(&p)?,
                                Direct::Subtitle(p) => writer.queue(&p)?,
                                Direct::ActionBar(p) => writer.queue(&p)?,
                                Direct::Times(p) => writer.queue(&p)?,
                                Direct::Clear(p) => writer.queue(&p)?,
                                Direct::Sound(p) => writer.queue(&p)?,
                                Direct::Experience(p) => writer.queue(&p)?,
                                Direct::StopSound(p) => writer.queue(&p)?,
                                Direct::MobEffect(p) => writer.queue(&p)?,
                                Direct::RemoveMobEffect(p) => writer.queue(&p)?,
                                Direct::DefaultSpawn(p) => writer.queue(&p)?,
                                Direct::Particles(p) => writer.queue(&p)?,
                                Direct::Camera(p) => writer.queue(&p)?,
                                Direct::Boss(p) => writer.queue(&p)?,
                                Direct::Transfer(p) => writer.queue(&p)?,
                                Direct::DisguisedChat(p) => writer.queue(&p)?,
                                Direct::ShowDialog(p) => writer.queue(&p)?,
                                Direct::ClearDialog(p) => writer.queue(&p)?,
                                Direct::PostEffects(p) => writer.queue(&p)?,
                                Direct::GameEvent(p) => writer.queue(&p)?,
                                Direct::BlockEntityData(p) => writer.queue(&p)?,
                                Direct::EntityEvent(p) => writer.queue(&p)?,
                                Direct::TestStatus(p) => writer.queue(&p)?,
                                Direct::TestHighlight(p) => writer.queue(&p)?,
                                Direct::Motion(p) => writer.queue(&p)?,
                                Direct::Map(p) => writer.queue(&p)?,
                                Direct::RecipeAdd(p) => writer.queue(&p)?,
                                Direct::RecipeRemove(p) => writer.queue(&p)?,
                                Direct::Advancements(p) => writer.queue(&p)?,
                            }
                        }
                        Ok(Event::Scoreboard(p)) => send_scoreboard(writer, &p)?,
                        Ok(Event::Border) => {
                            let packet = ctx.border.lock().expect("border lock").packet();
                            writer.queue(&packet)?;
                        }
                        Ok(Event::Difficulty(d)) => {
                            writer.queue(&play::ChangeDifficulty { difficulty: d as u8, locked: false })?;
                        }
                        Ok(Event::Kick { uuid: who, reason }) if who == uuid => {
                            let said = match &reason {
                                mc_nbt::Tag::String(s) => s.clone(),
                                mc_nbt::Tag::Compound(c) => match c.get("translate") {
                                    Some(mc_nbt::Tag::String(k)) => k.clone(),
                                    _ => String::new(),
                                },
                                _ => String::new(),
                            };
                            writer.send(&play::Disconnect { reason }).await?;
                            bail!("{said}");
                        }
                        Ok(Event::Teleport { uuid: who, pose: to }) if who == uuid => {
                            next_teleport += 1;
                            awaiting_teleport = Some(next_teleport);
                            pose = Pose { on_ground: false, ..to };
                            writer.queue(&play::PlayerPosition {
                                teleport_id: next_teleport,
                                x: pose.x,
                                y: pose.y,
                                z: pose.z,
                                velocity: [0.0; 3],
                                yaw: pose.yaw,
                                pitch: pose.pitch,
                                flags: 0,
                            })?;
                            view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z), loads_new_chunks(&ctx, game_mode)).await?;
                            update_player(&ctx, uuid, |p| p.pose = Some(pose));
                            let _ = ctx.events.send(Event::PlayerMoved { uuid, entity_id, pose });
                        }
                        Ok(Event::PlayerGone { uuid: other, entity_id: other_entity }) if other != uuid => {
                            if let Some(packet) = crate::locator::forget(other, &mut dots) {
                                writer.queue(&packet)?;
                            }
                            if visible.remove(&other) {
                                writer.queue(&play::RemoveEntities { entity_ids: vec![other_entity] })?;
                            }
                        }
                        Ok(Event::ChangeDimension { uuid: who, dimension, pose: to, after_death }) if who == uuid => {
                            let Some(next) = ctx.in_dimension(&dimension) else {
                                continue;
                            };
                            // Others here stop seeing us; the new place starts afresh.
                            let _ = ctx.events.send(Event::PlayerGone { uuid, entity_id });
                            let left = (ctx.world.dimension.clone(), [pose.x, pose.y, pose.z]);
                            ctx = next;
                            events = ctx.events.subscribe();
                            visible.clear();
                            dots.clear();
                            seen_entities.clear();
                            pose = Pose { on_ground: false, ..to };
                            update_player(&ctx, uuid, |p| {
                                p.dimension = ctx.world.dimension.clone();
                                p.pose = Some(pose);
                            });
                            if !after_death {
                                crate::criteria::changed_dimension(&ctx, uuid, &left.0, &ctx.world.dimension, left.1);
                            }
                            crate::vitals::stop_using(&ctx, uuid);
                            writer.queue(&play::Respawn {
                                dimension_type: ctx.world.dimension_type_id,
                                dimension: ctx.world.dimension.clone(),
                                hashed_seed: 0,
                                game_mode,
                                previous_game_mode: None,
                                is_debug: false,
                                is_flat: ctx.world.is_flat,
                                death_location: None,
                                portal_cooldown: 0,
                                sea_level: ctx.world.sea_level,
                                // Attributes and entity data stay, as through a portal.
                                data_kept: if after_death { 0 } else { 3 },
                            })?;
                            send_permissions(writer, &ctx, uuid, &me.profile.name, entity_id)?;
                            if after_death {
                                flying = game_mode == 3;
                                update_player(&ctx, uuid, |p| p.flying = flying);
                            }
                            writer.queue(&abilities(game_mode, flying))?;
                            next_teleport += 1;
                            awaiting_teleport = Some(next_teleport);
                            writer.queue(&play::PlayerPosition {
                                teleport_id: next_teleport,
                                x: pose.x,
                                y: pose.y,
                                z: pose.z,
                                velocity: [0.0; 3],
                                yaw: pose.yaw,
                                pitch: pose.pitch,
                                flags: 0,
                            })?;
                            let time = ctx.time.lock().expect("time lock").packet(true);
                            writer.queue(&time)?;
                            writer.queue(&spawn_packet(&ctx))?;
                            writer.queue(&ctx.border.lock().expect("border lock").packet())?;
                            {
                                let w = ctx.weather.lock().expect("weather lock");
                                if w.rain_level > 0.0 {
                                    let changes = crate::weather::Changes {
                                        raining: Some(true),
                                        rain_level: Some(w.rain_level),
                                        thunder_level: Some(w.thunder_level),
                                    };
                                    send_weather(writer, &changes)?;
                                }
                            }
                            writer.queue(&play::GameEvent {
                                event: play::game_event::START_WAITING_FOR_CHUNKS,
                                value: 0.0,
                            })?;
                            writer.queue(&skin_parts_metadata(entity_id, session.skin_parts))?;
                            writer.flush().await?;
                            view = ChunkView::new(session.view_distance, ctx.clone());
                            view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z), loads_new_chunks(&ctx, game_mode)).await?;
                            // What the client forgot with the old dimension.
                            let state = ctx.vitals.lock().expect("vitals lock").get(&uuid).cloned().unwrap_or_default();
                            writer.queue(&state.packet())?;
                            writer.queue(&state.experience_packet())?;
                            let (effects, effect_data, attributes) =
                                crate::effects::join_packets(&ctx, &state, entity_id);
                            for p in effects {
                                writer.queue(&p)?;
                            }
                            if let Some(p) = effect_data {
                                writer.queue(&p)?;
                            }
                            if let Some(p) = attributes {
                                writer.queue(&p)?;
                            }
                            let hands = ctx.inventories.lock().expect("inventories lock").get(&uuid).cloned().unwrap_or_default();
                            windows.state_id += 1;
                            writer.queue(&play::ContainerSetContent {
                                window: 0,
                                state_id: windows.state_id,
                                slots: hands.slots.clone(),
                                carried: None,
                            })?;
                            writer.queue(&play::SetHeldSlot { slot: hands.selected as i32 })?;
                            let others: Vec<OnlinePlayer> = ctx.players.lock().expect("players lock").values()
                                .filter(|p| p.profile.uuid != uuid && p.dimension == ctx.world.dimension)
                                .cloned()
                                .collect();
                            for other in others {
                                if let Some(other_pose) = other.pose {
                                    spawn_player(writer, &ctx, &other, other_pose)?;
                                    visible.insert(other.profile.uuid);
                                }
                            }
                            send_bars(writer, &ctx, uuid, &mut dots, false)?;
                            // Back on what they rode when they left.
        if let Some(vehicle) = &me.extra.root_vehicle {
            crate::persist::ride_again(&ctx, entity_id, vehicle);
        }
        let existing: Vec<mc_entity::Entity> = ctx.entities.lock().expect("entities lock").clone();
                            for entity in existing {
                                let (x, _, z) = entity.block_pos();
                                if view.has_block(Position::new(x, 0, z)) {
                                    spawn_entity(writer, &ctx, &entity)?;
                                    seen_entities.insert(entity.id);
                                }
                            }
                            let _ = ctx.events.send(Event::PlayerMoved { uuid, entity_id, pose });
                        }
                        Ok(Event::PlayerJoined(other)) if other.profile.uuid != uuid => {
                            writer.queue(&play::PlayerInfoUpdate {
                                actions: play::player_info::ALL,
                                players: vec![other.info_entry()],
                            })?;
                        }
                        Ok(Event::PlayerMoved { uuid: other, entity_id: other_entity, pose: other_pose })
                            if other != uuid =>
                        {
                            if visible.contains(&other) {
                                writer.queue(&play::EntityPositionSync {
                                    entity_id: other_entity,
                                    x: other_pose.x,
                                    y: other_pose.y,
                                    z: other_pose.z,
                                    yaw: other_pose.yaw,
                                    pitch: other_pose.pitch,
                                    on_ground: other_pose.on_ground,
                                })?;
                                writer.queue(&play::RotateHead {
                                    entity_id: other_entity,
                                    head_yaw: other_pose.yaw,
                                })?;
                            } else {
                                let known = ctx.players.lock().expect("players lock").get(&other).cloned();
                                if let Some(player) = known {
                                    spawn_player(writer, &ctx, &player, other_pose)?;
                                    visible.insert(other);
                                }
                            }
                            if let Some(packet) = crate::locator::offer(&ctx, uuid, other, other_pose, &mut dots, false) {
                                writer.queue(&packet)?;
                            }
                        }
                        Ok(Event::SkinParts { uuid: other, entity_id: other_entity, parts })
                            if other != uuid && visible.contains(&other) =>
                        {
                            writer.queue(&skin_parts_metadata(other_entity, parts))?;
                        }
                        Ok(Event::BlocksChanged { by, changes }) if by != uuid => {
                            send_block_changes(writer, &view, changes)?;
                        }
                        Ok(Event::BlockBroken { by, position, state }) if by != uuid && view.has_block(position) => {
                            writer.queue(&play::LevelEvent {
                                event: play::level_event::BLOCK_BROKEN,
                                position,
                                data: state as i32,
                                global: false,
                            })?;
                        }
                        Ok(Event::UsingItem { uuid: other, entity_id: other_entity, hand })
                            if other == uuid || visible.contains(&other) =>
                        {
                            let flags = hand.map_or(0, |h| 1 | if h == 1 { 2 } else { 0 });
                            writer.queue(&play::SetEntityData {
                                entity_id: other_entity,
                                entries: vec![(play::metadata::HAND_STATES, play::MetadataValue::Byte(flags))],
                            })?;
                        }
                        Ok(Event::UsedItem { uuid: other, entity_id: other_entity }) if other == uuid => {
                            writer.queue(&play::EntityEvent { entity_id: other_entity, status: 9 })?;
                        }
                        Ok(Event::Swing { uuid: other, entity_id: other_entity, times })
                            if other != uuid && visible.contains(&other) =>
                        {
                            for _ in 0..times {
                                writer.queue(&play::SwingAnimation {
                                    entity_id: other_entity,
                                    hand: 0,
                                    kind: 1,
                                    duration: 6,
                                })?;
                            }
                        }
                        Ok(Event::Animate { uuid: other, entity_id: other_entity, hand, kind, duration })
                            if other == uuid || visible.contains(&other) =>
                        {
                            writer.queue(&play::SwingAnimation {
                                entity_id: other_entity,
                                hand,
                                kind,
                                duration,
                            })?;
                        }
                        Ok(Event::Equipment { uuid: other, entity_id: other_entity, slots })
                            if other != uuid && visible.contains(&other) =>
                        {
                            writer.queue(&play::SetEquipment {
                                entity_id: other_entity,
                                slots,
                            })?;
                        }
                        Ok(Event::PlayerLeft { uuid: other, entity_id: other_entity }) if other != uuid => {
                            if let Some(packet) = crate::locator::forget(other, &mut dots) {
                                writer.queue(&packet)?;
                            }
                            if visible.remove(&other) {
                                writer.queue(&play::RemoveEntities { entity_ids: vec![other_entity] })?;
                            }
                            writer.queue(&play::PlayerInfoRemove { uuids: vec![other] })?;
                        }
                        Ok(Event::LocatorRefresh) => {
                            send_bars(writer, &ctx, uuid, &mut dots, true)?;
                        }
                        Ok(Event::ContentRefresh) => {
                            writer.queue(&ctx.recipes.get().update_packet())?;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            debug!("{} missed {missed} events", me.profile.name);
                        }
                        Err(broadcast::error::RecvError::Closed) => bail!("server shutting down"),
                    }
                }
            }
        writer.flush().await?;
    }
}

/// Applies a movement packet's position; ignores non-finite values.
fn set_position(pose: &mut Pose, x: f64, y: f64, z: f64, flags: u8) -> bool {
    if !(x.is_finite() && y.is_finite() && z.is_finite()) {
        return false;
    }
    (pose.x, pose.y, pose.z, pose.on_ground) = (x, y, z, flags & 1 != 0);
    true
}

/// Rain starting or stopping and the levels the client draws.
fn send_weather(writer: &mut PacketWriter, c: &crate::weather::Changes) -> Result<()> {
    use play::game_event::{BEGIN_RAINING, END_RAINING, RAIN_LEVEL, THUNDER_LEVEL};
    if let Some(r) = c.raining {
        let event = if r { BEGIN_RAINING } else { END_RAINING };
        writer.queue(&play::GameEvent { event, value: 0.0 })?;
    }
    if let Some(v) = c.rain_level {
        writer.queue(&play::GameEvent {
            event: RAIN_LEVEL,
            value: v,
        })?;
    }
    if let Some(v) = c.thunder_level {
        writer.queue(&play::GameEvent {
            event: THUNDER_LEVEL,
            value: v,
        })?;
    }
    Ok(())
}

/// The player's permission level (Entity Event 24 + level) and the
/// commands they may use.
fn send_permissions(
    writer: &mut PacketWriter,
    ctx: &ServerContext,
    uuid: u128,
    name: &str,
    entity_id: i32,
) -> Result<()> {
    let level = crate::commands::level(ctx, uuid, name);
    writer.queue(&play::EntityEvent {
        entity_id,
        status: 24 + level as i8,
    })?;
    if let Some(body) = ctx.commands.packet(level) {
        writer.queue(&play::Commands {
            body: body.to_vec(),
        })?;
    }
    Ok(())
}

fn send_scoreboard(writer: &mut PacketWriter, p: &crate::scoreboard::Packet) -> Result<()> {
    use crate::scoreboard::Packet;
    match p {
        Packet::Objective(p) => writer.queue(p),
        Packet::Display(p) => writer.queue(p),
        Packet::Score(p) => writer.queue(p),
        Packet::Reset(p) => writer.queue(p),
        Packet::Team(p) => writer.queue(p),
    }
}

/// Sends the player what changed for them and tells everyone else.
fn answer(writer: &mut PacketWriter, ctx: &ServerContext, outcome: Outcome) -> Result<()> {
    for (position, state) in outcome.updates {
        writer.queue(&play::BlockUpdate { position, state })?;
    }
    if let Some(sequence) = outcome.ack {
        writer.queue(&play::BlockChangedAck { sequence })?;
    }
    for line in outcome.messages {
        writer.queue(&play::SystemChat {
            content: line,
            overlay: false,
        })?;
    }
    for event in outcome.events {
        let _ = ctx.events.send(event);
    }
    Ok(())
}

/// Block changes the client has chunks for: one Block Update for a lone change
/// in a section, one Update Section Blocks for several.
fn send_block_changes(
    writer: &mut PacketWriter,
    view: &ChunkView,
    changes: Vec<(Position, u32)>,
) -> Result<()> {
    // Sections in the order their first change came.
    let mut index: mc_tick::fast::FastMap<(i32, i32, i32), usize> = Default::default();
    type Section = ((i32, i32, i32), Vec<(Position, u32)>);
    let mut sections: Vec<Section> = Vec::new();
    for (position, state) in changes {
        if !view.has_block(position) {
            continue;
        }
        let key = (position.x >> 4, position.y >> 4, position.z >> 4);
        let at = *index.entry(key).or_insert_with(|| {
            sections.push((key, Vec::new()));
            sections.len() - 1
        });
        sections[at].1.push((position, state));
    }
    for (section, list) in sections {
        if let [(position, state)] = list[..] {
            writer.queue(&play::BlockUpdate { position, state })?;
        } else {
            writer.queue(&play::SectionBlocksUpdate {
                section,
                blocks: list
                    .into_iter()
                    .map(|(p, state)| {
                        (
                            ((p.x & 15) as u8, (p.y & 15) as u8, (p.z & 15) as u8),
                            state,
                        )
                    })
                    .collect(),
            })?;
        }
    }
    Ok(())
}

/// What a game mode lets the player do (survival 0, creative 1, adventure 2,
/// spectator 3).
fn abilities(mode: i32, flying: bool) -> play::PlayerAbilities {
    use play::abilities::*;
    let mut flags = match mode {
        1 => INVULNERABLE | ALLOW_FLYING | INSTANT_BREAK,
        3 => INVULNERABLE | FLYING | ALLOW_FLYING,
        _ => 0,
    };
    if flying && flags & ALLOW_FLYING != 0 {
        flags |= FLYING;
    }
    play::PlayerAbilities {
        flags,
        flying_speed: 0.05,
        fov_modifier: 0.1,
    }
}

/// Changes a player's inventory and tells others if the main hand changed.
fn change_hands(ctx: &ServerContext, uuid: u128, entity_id: i32, change: impl FnOnce(&mut Hands)) {
    let (before, now) = {
        let mut inventories = ctx.inventories.lock().expect("inventories lock");
        let Some(hands) = inventories.get_mut(&uuid) else {
            return;
        };
        let before = hands.main_hand().cloned();
        change(hands);
        (before, hands.main_hand().cloned())
    };
    if now != before {
        update_player(ctx, uuid, |p| p.main_hand.clone_from(&now));
        let _ = ctx.events.send(Event::Equipment {
            uuid,
            entity_id,
            slots: vec![(0, now)],
        });
    }
}

fn update_player(ctx: &ServerContext, uuid: u128, change: impl FnOnce(&mut OnlinePlayer)) {
    if let Some(player) = ctx.players.lock().expect("players lock").get_mut(&uuid) {
        change(player);
    }
}

/// Makes another player appear: the entity, its skin layers and head rotation.
/// Their Player Info entry must already have been sent.
fn spawn_player(
    writer: &mut PacketWriter,
    ctx: &ServerContext,
    player: &OnlinePlayer,
    pose: Pose,
) -> Result<()> {
    writer.queue(&play::AddEntity {
        entity_id: player.entity_id,
        uuid: player.profile.uuid,
        kind: ctx.player_entity_type,
        x: pose.x,
        y: pose.y,
        z: pose.z,
        velocity: [0.0; 3],
        pitch: pose.pitch,
        yaw: pose.yaw,
        head_yaw: pose.yaw,
        data: 0,
    })?;
    writer.queue(&skin_parts_metadata(player.entity_id, player.skin_parts))?;
    writer.queue(&play::RotateHead {
        entity_id: player.entity_id,
        head_yaw: pose.yaw,
    })?;
    let slots: Vec<(u8, Option<play::ItemStack>)> =
        crate::worn::equipment(ctx, player.profile.uuid)
            .into_iter()
            .enumerate()
            .filter(|(_, s)| s.is_some())
            .map(|(i, s)| (i as u8, s))
            .collect();
    if !slots.is_empty() {
        writer.queue(&play::SetEquipment {
            entity_id: player.entity_id,
            slots,
        })?;
    }
    Ok(())
}

/// Shows the entities in the chunks the client has and hides those that
/// are no longer in them.
fn sync_entities(
    writer: &mut PacketWriter,
    ctx: &ServerContext,
    view: &ChunkView,
    seen: &mut HashSet<i32>,
) -> Result<()> {
    let (mut show, mut hide) = (Vec::new(), Vec::new());
    for entity in ctx.entities.lock().expect("entities lock").iter() {
        let (x, _, z) = entity.block_pos();
        let in_view = view.has_block(Position::new(x, 0, z));
        if in_view && !seen.contains(&entity.id) {
            show.push(entity.clone());
        } else if !in_view && seen.contains(&entity.id) {
            hide.push(entity.id);
        }
    }
    for entity in show {
        spawn_entity(writer, ctx, &entity)?;
        seen.insert(entity.id);
    }
    for id in &hide {
        seen.remove(id);
    }
    if !hide.is_empty() {
        writer.queue(&play::RemoveEntities { entity_ids: hide })?;
    }
    Ok(())
}

/// Add Entity for a non-player entity.
fn spawn_entity(
    writer: &mut PacketWriter,
    ctx: &ServerContext,
    entity: &mc_entity::Entity,
) -> Result<()> {
    if let Some(d) = crate::fx::describe(ctx, entity) {
        let Some(kind) = crate::fx::type_id(ctx, &d.kind) else {
            return Ok(());
        };
        let at = d.at.unwrap_or(entity.pos);
        writer.queue(&play::AddEntity {
            entity_id: entity.id,
            uuid: entity.uuid,
            kind,
            x: at[0],
            y: at[1],
            z: at[2],
            velocity: d.vel,
            pitch: d.pitch,
            yaw: d.yaw,
            head_yaw: d.yaw,
            data: d.data,
        })?;
        if !d.meta.is_empty() {
            writer.queue(&play::SetEntityData {
                entity_id: entity.id,
                entries: d.meta,
            })?;
        }
        if let mc_entity::Kind::Vehicle(v) = &entity.kind
            && !v.riders.is_empty()
        {
            writer.queue(&play::SetPassengers {
                vehicle: entity.id,
                passengers: v.riders.clone(),
            })?;
        }
        if !d.gear.is_empty() {
            writer.queue(&play::SetEquipment {
                entity_id: entity.id,
                slots: d.gear,
            })?;
        }
        return Ok(());
    }
    let (kind, data) = match &entity.kind {
        mc_entity::Kind::FallingBlock { state } => (ctx.entity_types.falling_block, *state as i32),
        mc_entity::Kind::Item { .. } => (ctx.entity_types.item, 0),
        mc_entity::Kind::Tnt { .. } => (ctx.entity_types.tnt, 0),
        mc_entity::Kind::Merchant(m) if m.wandering => (ctx.entity_types.wandering_trader, 0),
        mc_entity::Kind::Merchant(_) => (ctx.entity_types.villager, 0),
        mc_entity::Kind::Mob(m) => (crate::mobs::type_id(ctx, m.species), 0),
        mc_entity::Kind::ExperienceOrb { .. } => (ctx.entity_types.experience_orb, 0),
        _ => return Ok(()),
    };
    let (yaw, pitch, head_yaw) = match &entity.kind {
        mc_entity::Kind::Mob(m) => (m.yaw, m.pitch, m.head_yaw),
        _ => (0.0, 0.0, 0.0),
    };
    writer.queue(&play::AddEntity {
        entity_id: entity.id,
        uuid: entity.uuid,
        kind,
        x: entity.pos[0],
        y: entity.pos[1],
        z: entity.pos[2],
        velocity: entity.vel,
        pitch,
        yaw,
        head_yaw,
        data,
    })?;
    if let Some(packet) = crate::mobs::metadata_packet(ctx, entity) {
        writer.queue(&packet)?;
    }
    match entity.kind {
        mc_entity::Kind::Mob(ref m) => {
            let slots = crate::mobs::meta::worn(m);
            if !slots.is_empty() {
                writer.queue(&play::SetEquipment {
                    entity_id: entity.id,
                    slots,
                })?;
            }
            if !m.riders.is_empty() {
                writer.queue(&play::SetPassengers {
                    vehicle: entity.id,
                    passengers: m.riders.clone(),
                })?;
            }
        }
        mc_entity::Kind::Item { .. } => {
            writer.queue(&play::SetEntityData {
                entity_id: entity.id,
                entries: vec![(
                    play::metadata::ITEM,
                    play::MetadataValue::Item(entity.kind.as_stack()),
                )],
            })?;
        }
        mc_entity::Kind::ExperienceOrb { value, .. } => {
            writer.queue(&play::SetEntityData {
                entity_id: entity.id,
                entries: vec![(
                    play::metadata::EXPERIENCE_ORB_VALUE,
                    play::MetadataValue::VarInt(value),
                )],
            })?;
        }
        // Only a fuse other than the default is sent.
        mc_entity::Kind::Tnt { fuse, .. } if fuse != crate::explosion::TNT_FUSE => {
            writer.queue(&crate::explosion::fuse_data(entity.id, fuse))?;
        }
        mc_entity::Kind::Merchant(ref m) if !m.wandering => {
            writer.queue(&crate::trading::villager_data(ctx, entity.id, m))?;
            if m.walk.asleep
                && let Some(bed) = m.walk.bed
            {
                writer.queue(&crate::mobs::blocks::sleep_data(entity.id, Some(bed)))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether `id` is the entity of a player this one sees (or themselves).
fn visible_entity(ctx: &ServerContext, id: i32, visible: &HashSet<u128>) -> bool {
    ctx.players
        .lock()
        .expect("players lock")
        .values()
        .any(|p| p.entity_id == id && visible.contains(&p.profile.uuid))
}

fn skin_parts_metadata(entity_id: i32, skin_parts: u8) -> play::SetEntityData {
    play::SetEntityData {
        entity_id,
        entries: vec![(
            play::metadata::PLAYER_SKIN_PARTS,
            play::MetadataValue::Byte(skin_parts as i8),
        )],
    }
}

fn chunk_of(coordinate: f64) -> i32 {
    (coordinate.floor() as i32) >> 4
}

/// Whether the chunk `dx`, `dz` chunks from the player's is sent for a view
/// distance of `radius`. Since 1.20.2 that is a rounded area, not a square:
/// the squared distance, with the two nearest rings of each axis not
/// counted (the client keeps the ring around the view for its light and
/// neighbours), is below `radius` squared. The counts match what a vanilla
/// server sends (`docs/BENCHMARKS.md`, `docs/ROADMAP.md`): 473 chunks for
/// 10, 1057 for 16, 3725 for 32; and 49 for 2, 81 for 3.
fn in_view(radius: i32, dx: i32, dz: i32) -> bool {
    let beyond = |d: i32| i64::from((d.abs() - 2).max(0));
    beyond(dx).pow(2) + beyond(dz).pow(2) < i64::from(radius).pow(2)
}

/// The chunks of a view of `radius` around `(cx, cz)`, nearest first.
fn view_chunks(radius: i32, cx: i32, cz: i32) -> Vec<(i32, i32)> {
    let reach = radius + 1;
    let mut chunks: Vec<_> = (cx - reach..=cx + reach)
        .flat_map(|x| (cz - reach..=cz + reach).map(move |z| (x, z)))
        .filter(|&(x, z)| in_view(radius, x - cx, z - cz))
        .collect();
    chunks.sort_by_key(|&(x, z)| ((x - cx).pow(2) + (z - cz).pow(2), x, z));
    chunks
}

/// A spectator loads new chunks only while `spectators_generate_chunks` is on.
fn loads_new_chunks(ctx: &ServerContext, game_mode: i32) -> bool {
    game_mode != 3 || ctx.game_rule_bool("spectators_generate_chunks")
}

/// Chunks the client has, kept to the rounded area of `radius` around its
/// chunk (see [`in_view`]). The world counts this client as a viewer of
/// each of them.
struct ChunkView {
    radius: i32,
    center: Option<(i32, i32)>,
    loaded: HashSet<(i32, i32)>,
    ctx: Arc<ServerContext>,
}

impl Drop for ChunkView {
    fn drop(&mut self) {
        for &(x, z) in &self.loaded {
            self.ctx.world.unwatch(x, z);
        }
    }
}

impl ChunkView {
    fn new(radius: i32, ctx: Arc<ServerContext>) -> Self {
        Self {
            radius,
            center: None,
            loaded: HashSet::new(),
            ctx,
        }
    }

    /// Whether the client has the chunk with this block.
    /// Has the chunk and all around it.
    fn inside(&self, (x, z): (i32, i32)) -> bool {
        (-1..=1).all(|dz| (-1..=1).all(|dx| self.loaded.contains(&(x + dx, z + dz))))
    }

    fn has_block(&self, p: Position) -> bool {
        self.loaded.contains(&(p.x >> 4, p.z >> 4))
    }

    async fn move_to(
        &mut self,
        writer: &mut PacketWriter,
        cx: i32,
        cz: i32,
        generate: bool,
    ) -> Result<()> {
        if self.center == Some((cx, cz)) {
            return Ok(());
        }
        self.center = Some((cx, cz));
        let r = self.radius;
        let in_range = |(x, z): (i32, i32)| in_view(r, x - cx, z - cz);

        let stale: Vec<_> = self
            .loaded
            .iter()
            .copied()
            .filter(|&c| !in_range(c))
            .collect();
        for (x, z) in stale {
            self.loaded.remove(&(x, z));
            self.ctx.world.unwatch(x, z);
            writer.queue(&play::ForgetLevelChunk { x, z })?;
        }

        // Nearest first.
        let mut missing = view_chunks(r, cx, cz);
        missing.retain(|c| !self.loaded.contains(c));
        if !generate {
            missing.retain(|&(x, z)| self.ctx.world.present(x, z));
        }

        writer.queue(&play::SetChunkCacheCenter { x: cx, z: cz })?;
        // All of them are worked on at once, nearest first; each batch sent
        // holds what is ready by then, so the player sees the ground at once.
        let mut ready = load_chunks(&self.ctx, missing);
        while let Some(first) = ready.recv().await {
            let mut batch = vec![first];
            while let Ok(more) = ready.try_recv() {
                batch.push(more);
            }
            writer.queue(&play::ChunkBatchStart)?;
            for ((x, z), frame) in &batch {
                writer.queue_frame(frame);
                self.loaded.insert((*x, *z));
            }
            writer.queue(&play::ChunkBatchFinished {
                batch_size: batch.len() as i32,
            })?;
            writer.flush().await?;
        }
        Ok(())
    }
}

/// Loads (or takes from memory) the framed packets of `coords`, in order, on
/// the blocking pool: up to one task per core, so a joining player's chunks
/// are read, decoded and compressed in parallel and connection tasks never
/// block.
/// A chunk's position and its Chunk Data packet.
type ReadyChunk = ((i32, i32), Arc<[u8]>);

/// Loads (or generates) chunks on several threads, nearest first as given;
/// each one's packet comes out of the channel as soon as it is ready.
fn load_chunks(
    ctx: &Arc<ServerContext>,
    coords: Vec<(i32, i32)>,
) -> tokio::sync::mpsc::UnboundedReceiver<ReadyChunk> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let workers = mc_features::worker_threads().min(coords.len()).max(1);
    let coords = Arc::new(coords);
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for _ in 0..workers {
        let (ctx, coords, next, tx) = (ctx.clone(), coords.clone(), next.clone(), tx.clone());
        tokio::task::spawn_blocking(move || {
            let compression = ctx.config.compression_threshold;
            let frame = |packet: &play::LevelChunkWithLight| {
                let mut out = Vec::new();
                ctx.ids
                    .write(packet, &mut out, compression)
                    .expect("chunk data is a known packet");
                out
            };
            loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&(x, z)) = coords.get(i) else { break };
                let bytes = ctx.world.watch(x, z, &ctx.data.blocks, &frame);
                if tx.send(((x, z), bytes)).is_err() {
                    // The player left: nobody has this one after all.
                    ctx.world.unwatch(x, z);
                    break;
                }
            }
        });
    }
    rx
}

// ---------------------------------------------------------------------------
// Framed IO

#[derive(Debug)]
enum Packet {
    Handshake(handshake::Serverbound),
    Status(status::Serverbound),
    Login(login::Serverbound),
    Configuration(configuration::Serverbound),
    Play(play::Serverbound),
}

struct PacketReader {
    half: OwnedReadHalf,
    buf: Vec<u8>,
    compression: Option<usize>,
    cipher: Option<Cfb8>,
    ctx: Arc<ServerContext>,
}

impl PacketReader {
    fn new(half: OwnedReadHalf, ctx: Arc<ServerContext>) -> Self {
        Self {
            half,
            buf: Vec::with_capacity(4096),
            compression: None,
            cipher: None,
            ctx,
        }
    }

    /// Everything after the Encryption Response is encrypted, including bytes
    /// already buffered.
    fn enable_encryption(&mut self, secret: &[u8; 16]) {
        let mut cipher = Cfb8::new(secret);
        cipher.decrypt(&mut self.buf);
        self.cipher = Some(cipher);
    }

    async fn fill(&mut self) -> Result<()> {
        // `read_buf` is cancel-safe, and decryption runs right after it without
        // another await, so `next` can sit in a `select!`.
        let start = self.buf.len();
        if self.half.read_buf(&mut self.buf).await? == 0 {
            bail!("connection closed");
        }
        if let Some(cipher) = &mut self.cipher {
            cipher.decrypt(&mut self.buf[start..]);
        }
        Ok(())
    }

    async fn peek_byte(&mut self) -> Result<u8> {
        while self.buf.is_empty() {
            self.fill().await?;
        }
        Ok(self.buf[0])
    }

    async fn next(&mut self, state: State) -> Result<Packet> {
        loop {
            if let Some((header, len)) = split_frame(&self.buf)? {
                let body = decode_frame_body(&self.buf[header..header + len], self.compression)?;
                self.buf.drain(..header + len);
                let mut r = Reader::new(&body);
                let id = r.varint()?;
                let name = self
                    .ctx
                    .ids
                    .name(state, Direction::Serverbound, id)
                    .with_context(|| format!("unknown {state:?} packet id {id}"))?;
                let packet = match state {
                    State::Handshake => {
                        handshake::Serverbound::decode(name, &mut r).map(Packet::Handshake)
                    }
                    State::Status => status::Serverbound::decode(name, &mut r).map(Packet::Status),
                    State::Login => login::Serverbound::decode(name, &mut r).map(Packet::Login),
                    State::Configuration => {
                        configuration::Serverbound::decode(name, &mut r).map(Packet::Configuration)
                    }
                    State::Play => play::Serverbound::decode(name, &mut r).map(Packet::Play),
                };
                // Which packet a client sent that we could not read, and its
                // bytes, so the layout can be fixed.
                return packet.with_context(|| {
                    let hex: String = body.iter().take(512).map(|b| format!("{b:02x}")).collect();
                    format!("{name} ({} bytes: {hex})", body.len())
                });
            }
            self.fill().await?;
        }
    }
}

struct PacketWriter {
    half: OwnedWriteHalf,
    out: Vec<u8>,
    compression: Option<usize>,
    cipher: Option<Cfb8>,
    ctx: Arc<ServerContext>,
}

impl PacketWriter {
    fn new(half: OwnedWriteHalf, ctx: Arc<ServerContext>) -> Self {
        Self {
            half,
            out: Vec::with_capacity(4096),
            compression: None,
            cipher: None,
            ctx,
        }
    }

    /// Called right after the Encryption Response; nothing may be queued yet.
    fn enable_encryption(&mut self, secret: &[u8; 16]) {
        assert!(self.out.is_empty(), "unflushed plaintext before encryption");
        self.cipher = Some(Cfb8::new(secret));
    }

    fn queue<P: Encode>(&mut self, packet: &P) -> Result<()> {
        self.ctx
            .ids
            .write(packet, &mut self.out, self.compression)?;
        Ok(())
    }

    /// A packet already framed with this connection's compression setting.
    fn queue_frame(&mut self, frame: &[u8]) {
        self.out.extend_from_slice(frame);
    }

    async fn flush(&mut self) -> Result<()> {
        if !self.out.is_empty() {
            if let Some(cipher) = &mut self.cipher {
                cipher.encrypt(&mut self.out);
            }
            self.half.write_all(&self.out).await?;
            self.out.clear();
        }
        Ok(())
    }

    async fn send<P: Encode>(&mut self, packet: &P) -> Result<()> {
        self.queue(packet)?;
        self.flush().await
    }
}

/// Dots for everyone else in this dimension. `force` resends a colour or
/// style that did not move.
fn send_bars(
    writer: &mut PacketWriter,
    ctx: &ServerContext,
    viewer: u128,
    bars: &mut HashMap<u128, [i32; 3]>,
    force: bool,
) -> Result<()> {
    let here: Vec<(u128, Pose)> = {
        let players = ctx.players.lock().expect("players lock");
        players
            .values()
            .filter(|p| p.profile.uuid != viewer && p.dimension == ctx.world.dimension)
            .filter_map(|p| p.pose.map(|pose| (p.profile.uuid, pose)))
            .collect()
    };
    let live: HashSet<u128> = here.iter().map(|(id, _)| *id).collect();
    let stale: Vec<u128> = bars
        .keys()
        .copied()
        .filter(|id| !live.contains(id))
        .collect();
    for id in stale {
        if let Some(packet) = crate::locator::forget(id, bars) {
            writer.queue(&packet)?;
        }
    }
    for (id, pose) in here {
        if let Some(packet) = crate::locator::offer(ctx, viewer, id, pose, bars, force) {
            writer.queue(&packet)?;
        }
    }
    Ok(())
}

/// Where new players appear: the middle of the world spawn's block, or,
/// except in adventure mode, the top of a random column within
/// `respawn_radius` of it, as vanilla picks one.
fn world_spawn_pose(ctx: &ServerContext, game_mode: i32) -> Pose {
    let spawn = ctx.spawn.lock().expect("spawn lock").clone();
    let mut pose = Pose {
        x: f64::from(spawn.pos.0) + 0.5,
        y: f64::from(spawn.pos.1),
        z: f64::from(spawn.pos.2) + 0.5,
        yaw: spawn.yaw,
        pitch: spawn.pitch,
        on_ground: true,
    };
    if game_mode == 2 || spawn.dimension != "minecraft:overworld" {
        return pose;
    }
    let radius = ctx
        .game_rules
        .lock()
        .expect("rules lock")
        .int("respawn_radius")
        .max(0);
    let side = i64::from(radius) * 2 + 1;
    let count = (side * side).min(i64::from(i32::MAX));
    // Every column once, in a random order: a fixed step coprime to the count.
    let step = (count / 2 + 1..=count)
        .chain(1..=count / 2)
        .find(|&n| gcd(n, count) == 1)
        .unwrap_or(1);
    let start = (crate::util::random_u64() % count as u64) as i64;
    for i in 0..count {
        let q = (start + step * i) % count;
        let (x, z) = (
            spawn.pos.0 + (q % side) as i32 - radius,
            spawn.pos.2 + (q / side) as i32 - radius,
        );
        if let Some(y) = respawn_surface(ctx, x, z) {
            (pose.x, pose.y, pose.z) = (f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5);
            break;
        }
    }
    pose
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The height a player can stand at on top of column x, z: above its
/// highest block, which is not a fluid, with room for the player.
fn respawn_surface(ctx: &ServerContext, x: i32, z: i32) -> Option<i32> {
    let blocks = &ctx.data.blocks;
    let world = &ctx.world;
    let top = world.min_y + world.height as i32 - 1;
    let state = |y: i32| world.block(Position::new(x, y, z), blocks);
    let mut y = top;
    while y >= world.min_y && blocks.is_air(state(y)?) {
        y -= 1;
    }
    if y < world.min_y || blocks.is_fluid(state(y)?) {
        return None;
    }
    let clear = |y: i32| state(y).is_none_or(|s| blocks.is_air(s));
    (clear(y + 1) && clear(y + 2)).then_some(y + 1)
}

/// Where a player respawns and in which dimension: their own point if
/// `/spawnpoint` set one, else the world spawn.
fn respawn_target(ctx: &ServerContext, uuid: u128) -> (String, Pose) {
    let own = ctx
        .players
        .lock()
        .expect("players lock")
        .get(&uuid)
        .and_then(|p| p.extra.respawn.clone());
    match own {
        Some(r) if r.forced => (
            r.dimension.clone(),
            Pose {
                x: f64::from(r.pos.0) + 0.5,
                y: f64::from(r.pos.1),
                z: f64::from(r.pos.2) + 0.5,
                yaw: r.yaw,
                pitch: r.pitch,
                on_ground: true,
            },
        ),
        // A bed or a charged anchor that is still there, with room beside
        // it; if not, the player is told and the point is forgotten.
        Some(r) => crate::respawn::find(ctx, &r).unwrap_or_else(|| {
            update_player(ctx, uuid, |p| p.extra.respawn = None);
            let _ = ctx.events.send(Event::Message {
                uuid,
                line: mc_protocol::text::translate("block.minecraft.spawn.not_valid", Vec::new()),
                overlay: false,
            });
            spawn_target(ctx, uuid)
        }),
        None => spawn_target(ctx, uuid),
    }
}

fn spawn_target(ctx: &ServerContext, uuid: u128) -> (String, Pose) {
    let game_mode = ctx
        .players
        .lock()
        .expect("players lock")
        .get(&uuid)
        .map_or(0, |p| p.game_mode);
    // The lock is let go before `world_spawn_pose` takes it again.
    let dimension = ctx.spawn.lock().expect("spawn lock").dimension.clone();
    (dimension, world_spawn_pose(ctx, game_mode))
}

/// Tells a client where the world spawn is (the compass points there).
pub fn spawn_packet(ctx: &ServerContext) -> play::SetDefaultSpawnPosition {
    let spawn = ctx.spawn.lock().expect("spawn lock").clone();
    play::SetDefaultSpawnPosition {
        dimension: spawn.dimension,
        position: Position::new(spawn.pos.0, spawn.pos.1, spawn.pos.2),
        yaw: spawn.yaw,
        pitch: spawn.pitch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_has_as_many_chunks_as_a_vanilla_server_sends() {
        // Counts vanilla sent in `cargo xtask bench` / `chunk-speed`.
        for (radius, count) in [(10, 473), (16, 1057), (32, 3725)] {
            assert_eq!(
                view_chunks(radius, 0, 0).len(),
                count,
                "view distance {radius}"
            );
        }
        // And the small ones, where it is bigger than the square.
        for (radius, count) in [(2, 49), (3, 81), (4, 117)] {
            assert_eq!(
                view_chunks(radius, 5, -7).len(),
                count,
                "view distance {radius}"
            );
        }
    }

    #[test]
    fn a_missing_bed_clears_the_spawn_point_and_tells_the_player() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let uuid = 0x7701;
        crate::testkit::join(&ctx, uuid, 7801, [0.5, -60.0, 0.5], 0);
        let mut events = ctx.events.subscribe();
        update_player(&ctx, uuid, |p| {
            p.extra.respawn = Some(crate::players::Respawn {
                dimension: ctx.world.dimension.clone(),
                pos: (60, -60, 60),
                yaw: 0.0,
                pitch: 0.0,
                forced: false,
            });
        });
        let (dimension, _) = respawn_target(&ctx, uuid);
        assert_eq!(dimension, ctx.world.dimension);
        assert!(ctx.players.lock().unwrap()[&uuid].extra.respawn.is_none());
        let mut told = false;
        while let Some(event) = events.try_changed() {
            if let Event::Message { uuid: to, line, .. } = event {
                told |=
                    to == uuid && format!("{line:?}").contains("block.minecraft.spawn.not_valid");
            }
        }
        assert!(told);
    }

    #[test]
    fn the_view_is_round_and_loads_nearest_first() {
        let chunks = view_chunks(16, 0, 0);
        assert_eq!(chunks[0], (0, 0));
        let far = |&(x, z): &(i32, i32)| x * x + z * z;
        assert!(chunks.windows(2).all(|w| far(&w[0]) <= far(&w[1])));
        // The straight edge reaches one chunk past the radius, the corner
        // of the square does not.
        assert!(chunks.contains(&(17, 0)) && chunks.contains(&(0, -17)));
        assert!(!chunks.contains(&(18, 0)));
        assert!(!chunks.contains(&(17, 17)) && !chunks.contains(&(-14, 14)));
        assert!(chunks.contains(&(12, 12)));
        // Moving one chunk east: the new edge arrives, the old one goes.
        let moved: HashSet<_> = view_chunks(16, 1, 0).into_iter().collect();
        let before: HashSet<_> = chunks.into_iter().collect();
        assert!(moved.contains(&(18, 0)) && !before.contains(&(18, 0)));
        assert!(before.contains(&(-17, 0)) && !moved.contains(&(-17, 0)));
    }
}
