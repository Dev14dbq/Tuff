//! One client connection: handshake → status, or login → configuration → play.

use std::collections::HashSet;
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
                tokio::task::spawn_blocking(move || crate::save_player(&ctx, uuid)).await?;
            }
            crate::containers::player_left(&ctx, uuid);
            ctx.players.lock().expect("players lock").remove(&uuid);
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
                let sample: Vec<_> = ctx
                    .players
                    .lock()
                    .expect("players lock")
                    .values()
                    .take(12)
                    .map(|p| {
                        serde_json::json!({"name": p.profile.name, "id": uuid_string(p.profile.uuid)})
                    })
                    .collect();
                let json = serde_json::json!({
                    "version": {"name": ctx.data.version.name, "protocol": mc_protocol::PROTOCOL_VERSION},
                    "players": {"max": ctx.config.max_players, "online": ctx.online_count(), "sample": sample},
                    "description": {"text": ctx.config.motd},
                    "enforcesSecureChat": false,
                });
                let response = status::StatusResponse {
                    json: json.to_string(),
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
    peer: SocketAddr,
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

    let profile = match &ctx.key {
        Some(key) => match authenticate(reader, writer, key, &name).await? {
            Some(profile) => profile,
            None => {
                let reason =
                    serde_json::json!({"translate": "multiplayer.disconnect.unverified_username"});
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
    };

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
            .map_or(ctx.default_game_mode, |s| s.game_mode),
        entity_id: ctx.next_entity_id(),
        skin_parts: 0,
        pose: None,
        main_hand: None,
        flying: saved.as_ref().is_some_and(|s| s.flying),
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
        } else if players.len() >= ctx.config.max_players
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
        view_distance: i32::from(ctx.config.view_distance),
        skin_parts: 0,
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
    brand.put_string("minecraft-rewrite");
    writer.queue(&configuration::CustomPayload {
        channel: "minecraft:brand".into(),
        data: brand,
    })?;
    writer.queue(&configuration::UpdateEnabledFeatures {
        flags: vec!["minecraft:vanilla".into()],
    })?;
    writer.queue(&configuration::SelectKnownPacks {
        packs: vec![ctx.core_pack.clone()],
    })?;
    writer.flush().await?;

    let view_distance = |info: &configuration::ClientInformation| {
        i32::from(info.view_distance).clamp(2, i32::from(ctx.config.view_distance))
    };
    let client_knows_core = loop {
        match reader.next(State::Configuration).await? {
            Packet::Configuration(configuration::Serverbound::SelectKnownPacks(packs)) => {
                break packs.contains(&ctx.core_pack);
            }
            Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                session.view_distance = view_distance(&info);
                session.skin_parts = info.skin_parts;
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
    writer.queue(&configuration::FinishConfiguration)?;
    writer.flush().await?;

    loop {
        match reader.next(State::Configuration).await? {
            Packet::Configuration(configuration::Serverbound::FinishConfiguration) => {
                return Ok(true);
            }
            Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                session.view_distance = view_distance(&info);
                session.skin_parts = info.skin_parts;
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
    let ctx = writer.ctx.clone();
    let world = &ctx.world;
    let me = &session.player;
    let (uuid, entity_id) = (me.profile.uuid, me.entity_id);
    // Subscribe before reading the player list, so nobody slips through.
    let mut events = ctx.events.subscribe();
    let (sx, sy, sz) = world.spawn;
    let mut pose = session.saved.as_ref().map_or(
        Pose {
            x: sx,
            y: sy,
            z: sz,
            yaw: world.spawn_yaw,
            pitch: 0.0,
            on_ground: true,
        },
        |s| s.pose,
    );
    let (x, y, z) = (pose.x, pose.y, pose.z);

    writer.queue(&play::Login {
        entity_id,
        hardcore: false,
        dimensions: vec![world.dimension.clone()],
        max_players: ctx.config.max_players as i32,
        view_distance: i32::from(ctx.config.view_distance),
        simulation_distance: i32::from(ctx.config.view_distance),
        reduced_debug_info: false,
        enable_respawn_screen: true,
        do_limited_crafting: false,
        dimension_type: world.dimension_type_id,
        dimension: world.dimension.clone(),
        hashed_seed: 0,
        game_mode: me.game_mode,
        previous_game_mode: None,
        is_debug: false,
        is_flat: world.is_flat,
        death_location: None,
        portal_cooldown: 0,
        sea_level: world.sea_level,
        online_mode: ctx.config.online_mode,
        enforces_secure_chat: false,
    })?;
    let mut flying = me.flying || me.game_mode == 3;
    writer.queue(&abilities(me.game_mode, flying))?;
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
    let mut visible: HashSet<u128> = HashSet::new();
    for other in everyone.iter().filter(|p| p.profile.uuid != uuid) {
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
    writer.queue(&play::SetDefaultSpawnPosition {
        dimension: world.dimension.clone(),
        position: Position::new(x.floor() as i32, y as i32, z.floor() as i32),
        yaw: 0.0,
        pitch: 0.0,
    })?;
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
    view.move_to(writer, chunk_of(x), chunk_of(z)).await?;
    writer.queue(&play::SystemChat {
        content: text::colored(
            &format!(
                "Welcome to minecraft-rewrite! You are in {}.",
                world.description
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
    let mut windows = windows::Windows::new();
    let vitals = session
        .saved
        .as_ref()
        .map(|s| s.vitals.clone())
        .unwrap_or_default();
    writer.queue(&vitals.packet())?;
    ctx.vitals.lock().expect("vitals lock").insert(uuid, vitals);
    let (replies_tx, mut replies) = tokio::sync::mpsc::unbounded_channel::<Outcome>();
    // Non-player entities this client has been told about.
    let mut seen_entities: HashSet<i32> = HashSet::new();
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
                if std::mem::take(&mut outcome.open_crafting) {
                    windows.open_crafting(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose })?;
                }
                answer(writer, &ctx, outcome)?;
            }
            packet = reader.next(State::Play) => {
                let Packet::Play(packet) = packet? else {
                    unreachable!("play state decodes play packets")
                };
                // Movement sent before the client saw our teleport is stale.
                let settled = awaiting_teleport.is_none();
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
                    play::Serverbound::Chat { message } => {
                        info!("<{}> {}", me.profile.name, message);
                        let line = text::translate("chat.type.text", vec![text::plain(&me.profile.name), text::plain(&message)]);
                        let _ = ctx.events.send(Event::Chat(line));
                    }
                    play::Serverbound::ChatCommand { command } => {
                        let op = ctx.lists.lock().expect("lists lock").op(uuid, &me.profile.name).is_some();
                        if op {
                            info!("{} issued server command: /{command}", me.profile.name);
                            let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                            let source = Source::Player { uuid, name: me.profile.name.clone(), pose };
                            let action = Action::Command { line: command, source };
                            let _ = ctx.actions.send(Request { actor, action, reply: replies_tx.clone() });
                        } else {
                            writer.queue(&play::SystemChat {
                                content: text::colored("You do not have permission to use commands", "red"),
                                overlay: false,
                            })?;
                        }
                    }
                    play::Serverbound::SetCreativeModeSlot { slot, item } if game_mode == CREATIVE => {
                        if let Ok(slot) = usize::try_from(slot) {
                            change_hands(&ctx, uuid, entity_id, |hands| {
                                if let Some(s) = hands.slots.get_mut(slot) {
                                    *s = item;
                                }
                            });
                        }
                    }
                    play::Serverbound::SetCarriedItem { slot } => {
                        crate::vitals::stop_using(&ctx, uuid);
                        if let Ok(slot @ 0..=8) = usize::try_from(slot) {
                            change_hands(&ctx, uuid, entity_id, |hands| hands.selected = slot);
                        }
                    }
                    play::Serverbound::ContainerClick { window, slot, button, mode, changed, carried, .. } => {
                        let click = windows::Click { window, slot, button, mode, changed, carried };
                        windows.click(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, click)?;
                    }
                    play::Serverbound::ContainerClose { window } => {
                        windows.close(writer, windows::Who { ctx: &ctx, uuid, entity_id, pose }, window)?;
                    }
                    play::Serverbound::PlayerAction { status: 5, .. } => crate::vitals::stop_using(&ctx, uuid),
                    play::Serverbound::PlayerAction { status: status @ (3 | 4), .. } => {
                        // Q: drop one (4) or the whole stack (3) from the main hand.
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
                                    hands.components.remove(&slot);
                                }
                                changed_slot = Some((slot, hands.slots[slot].clone()));
                            }
                        });
                        if let Some(stack) = thrown {
                            crate::tick::throw_item(&ctx, pose, stack);
                        }
                        if let Some((slot, item)) = changed_slot {
                            windows.inventory_slot(writer, slot as i16, item)?;
                        }
                    }
                    play::Serverbound::ClientCommand { action: 0 } => {
                        let dead = ctx.vitals.lock().expect("vitals lock").get(&uuid).is_some_and(|v| v.dead);
                        if dead {
                            let health = crate::vitals::respawn(&ctx, uuid);
                            writer.queue(&play::Respawn {
                                dimension_type: world.dimension_type_id,
                                dimension: world.dimension.clone(),
                                hashed_seed: 0,
                                game_mode,
                                previous_game_mode: None,
                                is_debug: false,
                                is_flat: world.is_flat,
                                death_location: None,
                                portal_cooldown: 0,
                                sea_level: world.sea_level,
                                data_kept: 0,
                            })?;
                            flying = game_mode == 3;
                            update_player(&ctx, uuid, |p| p.flying = flying);
                            writer.queue(&abilities(game_mode, flying))?;
                            writer.queue(&health)?;
                            let (sx, sy, sz) = world.spawn;
                            pose = Pose { x: sx, y: sy, z: sz, yaw: world.spawn_yaw, pitch: 0.0, on_ground: true };
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
                            view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z)).await?;
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
                        }
                    }
                    play::Serverbound::PlayerInput { flags } => sneaking = flags & 0x20 != 0,
                    play::Serverbound::PlayerAbilities { flags } if matches!(game_mode, 1 | 3) => {
                        flying = flags & play::abilities::FLYING != 0 || game_mode == 3;
                        update_player(&ctx, uuid, |p| p.flying = flying);
                    }
                    play::Serverbound::Attack { entity_id: target } if game_mode != 3 => {
                        let victim = ctx.players.lock().expect("players lock").values()
                            .find(|p| p.entity_id == target)
                            .and_then(|p| p.pose.map(|at| (p.profile.uuid, at)));
                        if let Some((victim, at)) = victim {
                            let (dx, dy, dz) = (at.x - pose.x, at.y - pose.y, at.z - pose.z);
                            // Reach 3 from the docs, with some room for lag.
                            if dx * dx + dy * dy + dz * dz <= 36.0 {
                                let held = ctx.inventories.lock().expect("inventories lock").get(&uuid)
                                    .and_then(|h| h.main_hand().map(|s| s.item));
                                let damage = crate::vitals::melee_damage(&ctx, held);
                                crate::vitals::hurt_by(&ctx, victim, damage, "minecraft:player_attack", "death.attack.player", Some(&me.profile.name));
                            }
                        }
                    }
                    play::Serverbound::Punch => {
                        let _ = ctx.events.send(Event::Swing { uuid, entity_id, times: 1 });
                    }
                    // Block changes happen on the tick thread; the answer comes back
                    // through `replies`.
                    play::Serverbound::PlayerAction { status, position, sequence, .. } => {
                        let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                        let action = Action::Dig { status, position, sequence };
                        let _ = ctx.actions.send(Request { actor, action, reply: replies_tx.clone() });
                    }
                    play::Serverbound::UseItemOn { hand, position, face, cursor, sequence, .. } => {
                        let actor = Actor { uuid, entity_id, game_mode, pose, sneaking };
                        let action = Action::UseItemOn { hand, position, face, cursor, sequence };
                        let _ = ctx.actions.send(Request { actor, action, reply: replies_tx.clone() });
                    }
                    play::Serverbound::UseItem { hand, sequence, .. } => {
                        writer.queue(&play::BlockChangedAck { sequence })?;
                        crate::vitals::start_using(&ctx, uuid, entity_id, hand, game_mode);
                    }
                    play::Serverbound::ClientInformation(info) => {
                        writer.queue(&skin_parts_metadata(entity_id, info.skin_parts))?;
                        update_player(&ctx, uuid, |p| p.skin_parts = info.skin_parts);
                        let _ = ctx.events.send(Event::SkinParts {
                            uuid,
                            entity_id,
                            parts: info.skin_parts,
                        });
                    }
                    other => trace!("play: {other:?}"),
                }
                if moved {
                    let feet = Position::new(pose.x.floor() as i32, pose.y.floor() as i32, pose.z.floor() as i32);
                    let in_water = ctx
                        .world
                        .block(feet, &ctx.data.blocks)
                        .is_some_and(|s| ctx.data.blocks.is_fluid(s));
                    crate::vitals::moved(&ctx, uuid, before, pose, in_water);
                    view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z)).await?;
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
                        if view.has_block(Position::new(x, 0, z)) {
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
                    Ok(Event::EntitiesRemoved(ids)) => {
                        let gone: Vec<i32> = ids.into_iter().filter(|id| seen_entities.remove(id)).collect();
                        if !gone.is_empty() {
                            writer.queue(&play::RemoveEntities { entity_ids: gone })?;
                        }
                    }
                    Ok(Event::TickingState(packet)) => writer.queue(&packet)?,
                    Ok(Event::Health { uuid: who, health }) if who == uuid => writer.queue(&health)?,
                    Ok(Event::Hurt { uuid: who, entity_id: hurt, damage_type }) if who == uuid || visible.contains(&who) => {
                        writer.queue(&play::DamageEvent { entity_id: hurt, damage_type })?;
                    }
                    Ok(Event::Died { uuid: who, entity_id: dead, message }) => {
                        if who == uuid {
                            writer.queue(&play::PlayerCombatKill { player: entity_id, message: message.clone() })?;
                            // Everything carried falls out where the player died.
                            let lost: Vec<play::ItemStack> = {
                                let mut inventories = ctx.inventories.lock().expect("inventories lock");
                                inventories.get_mut(&uuid).map_or_else(Vec::new, |hands| {
                                    hands.components.clear();
                                    let mut all: Vec<play::ItemStack> = hands.slots.iter_mut().filter_map(Option::take).collect();
                                    all.extend(hands.carried.take());
                                    all
                                })
                            };
                            for stack in lost {
                                crate::tick::scatter_item(&ctx, pose, stack);
                            }
                            update_player(&ctx, uuid, |p| p.main_hand = None);
                            let _ = ctx.events.send(Event::Chat(message));
                        } else if visible.remove(&who) {
                            writer.queue(&play::RemoveEntities { entity_ids: vec![dead] })?;
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
                    Ok(Event::BlockAction { position, action, param, block }) if view.has_block(position) => {
                        writer.queue(&play::BlockEvent { position, action, param, block })?;
                    }
                    Ok(Event::FurnaceProgress { position, values }) => {
                        windows.container_data(writer, position, &values)?;
                    }
                    Ok(Event::InventorySlot { uuid: who, slot, item }) if who == uuid => {
                        windows.inventory_slot(writer, slot, item)?;
                    }
                    Ok(Event::ItemTaken { item_entity, collector, count }) => {
                        if seen_entities.contains(&item_entity) {
                            writer.queue(&play::TakeItemEntity {
                                collected: item_entity,
                                collector,
                                count,
                            })?;
                        }
                    }
                    Ok(Event::TickingStep(packet)) => writer.queue(&packet)?,
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
                        view.move_to(writer, chunk_of(pose.x), chunk_of(pose.z)).await?;
                        update_player(&ctx, uuid, |p| p.pose = Some(pose));
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
                    Ok(Event::Equipment { uuid: other, entity_id: other_entity, main_hand })
                        if other != uuid && visible.contains(&other) =>
                    {
                        writer.queue(&play::SetEquipment {
                            entity_id: other_entity,
                            slots: vec![(0, main_hand)],
                        })?;
                    }
                    Ok(Event::PlayerLeft { uuid: other, entity_id: other_entity }) if other != uuid => {
                        if visible.remove(&other) {
                            writer.queue(&play::RemoveEntities { entity_ids: vec![other_entity] })?;
                        }
                        writer.queue(&play::PlayerInfoRemove { uuids: vec![other] })?;
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
            content: text::plain(&line),
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
    type Section = ((i32, i32, i32), Vec<(Position, u32)>);
    let mut sections: Vec<Section> = Vec::new();
    for (position, state) in changes {
        if !view.has_block(position) {
            continue;
        }
        let key = (position.x >> 4, position.y >> 4, position.z >> 4);
        match sections.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push((position, state)),
            None => sections.push((key, vec![(position, state)])),
        }
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
            main_hand: now,
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
    if player.main_hand.is_some() {
        writer.queue(&play::SetEquipment {
            entity_id: player.entity_id,
            slots: vec![(0, player.main_hand.clone())],
        })?;
    }
    Ok(())
}

/// Add Entity for a non-player entity.
fn spawn_entity(
    writer: &mut PacketWriter,
    ctx: &ServerContext,
    entity: &mc_entity::Entity,
) -> Result<()> {
    let (kind, data) = match entity.kind {
        mc_entity::Kind::FallingBlock { state } => (ctx.entity_types.falling_block, state as i32),
        mc_entity::Kind::Item { .. } => (ctx.entity_types.item, 0),
    };
    writer.queue(&play::AddEntity {
        entity_id: entity.id,
        uuid: entity.uuid,
        kind,
        x: entity.pos[0],
        y: entity.pos[1],
        z: entity.pos[2],
        pitch: 0.0,
        yaw: 0.0,
        head_yaw: 0.0,
        data,
    })?;
    if let mc_entity::Kind::Item { item, count, .. } = entity.kind {
        writer.queue(&play::SetEntityData {
            entity_id: entity.id,
            entries: vec![(
                play::metadata::ITEM,
                play::MetadataValue::Item(Some(play::ItemStack {
                    count,
                    item,
                    has_component_changes: false,
                })),
            )],
        })?;
    }
    Ok(())
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

/// Chunks the client has, kept to a square of `radius` around its chunk.
/// The world counts this client as a viewer of each of them.
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
    fn has_block(&self, p: Position) -> bool {
        self.loaded.contains(&(p.x >> 4, p.z >> 4))
    }

    async fn move_to(&mut self, writer: &mut PacketWriter, cx: i32, cz: i32) -> Result<()> {
        if self.center == Some((cx, cz)) {
            return Ok(());
        }
        self.center = Some((cx, cz));
        let r = self.radius;
        let in_range = |(x, z): (i32, i32)| (x - cx).abs() <= r && (z - cz).abs() <= r;

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

        let mut missing: Vec<_> = (cx - r..=cx + r)
            .flat_map(|x| (cz - r..=cz + r).map(move |z| (x, z)))
            .filter(|c| !self.loaded.contains(c))
            .collect();
        missing.sort_by_key(|&(x, z)| (x - cx).pow(2) + (z - cz).pow(2));

        writer.queue(&play::SetChunkCacheCenter { x: cx, z: cz })?;
        // Nearest first, in batches that grow, each sent as soon as it is
        // loaded: the player sees the ground at once.
        let mut rest = &missing[..];
        let mut size = FIRST_BATCH;
        while !rest.is_empty() {
            let (batch, later) = rest.split_at(size.min(rest.len()));
            let frames = load_chunks(&self.ctx, batch).await?;
            writer.queue(&play::ChunkBatchStart)?;
            for (&(x, z), frame) in batch.iter().zip(&frames) {
                writer.queue_frame(frame);
                self.loaded.insert((x, z));
            }
            writer.queue(&play::ChunkBatchFinished {
                batch_size: batch.len() as i32,
            })?;
            writer.flush().await?;
            rest = later;
            size = (size * 2).min(MAX_BATCH);
        }
        Ok(())
    }
}

/// Chunks in the first batch a player gets after moving (a 5×5 square).
const FIRST_BATCH: usize = 25;
const MAX_BATCH: usize = 256;

/// Loads (or takes from memory) the framed packets of `coords`, in order, on
/// the blocking pool: up to one task per core, so a joining player's chunks
/// are read, decoded and compressed in parallel and connection tasks never
/// block.
async fn load_chunks(ctx: &Arc<ServerContext>, coords: &[(i32, i32)]) -> Result<Vec<Arc<[u8]>>> {
    const PER_TASK: usize = 8;
    let tasks = std::thread::available_parallelism()
        .map_or(4, usize::from)
        .min(coords.len().div_ceil(PER_TASK))
        .max(1);
    // Task i takes every tasks-th chunk, so all of them start near the player.
    let handles: Vec<_> = (0..tasks)
        .map(|i| {
            let ctx = ctx.clone();
            let mine: Vec<(i32, i32)> = coords.iter().copied().skip(i).step_by(tasks).collect();
            tokio::task::spawn_blocking(move || {
                let compression = ctx.config.compression_threshold;
                let frame = |packet: &play::LevelChunkWithLight| {
                    let mut out = Vec::new();
                    ctx.ids
                        .write(packet, &mut out, compression)
                        .expect("chunk data is a known packet");
                    out
                };
                mine.into_iter()
                    .map(|(x, z)| ctx.world.watch(x, z, &ctx.data.blocks, &frame))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut parts = Vec::with_capacity(tasks);
    for handle in handles {
        parts.push(
            handle
                .await
                .context("chunk loading task failed")?
                .into_iter(),
        );
    }
    Ok((0..coords.len())
        .map(|i| parts[i % tasks].next().expect("one packet per chunk"))
        .collect())
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
                        Packet::Handshake(handshake::Serverbound::decode(name, &mut r)?)
                    }
                    State::Status => Packet::Status(status::Serverbound::decode(name, &mut r)?),
                    State::Login => Packet::Login(login::Serverbound::decode(name, &mut r)?),
                    State::Configuration => {
                        Packet::Configuration(configuration::Serverbound::decode(name, &mut r)?)
                    }
                    State::Play => Packet::Play(play::Serverbound::decode(name, &mut r)?),
                };
                return Ok(packet);
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
