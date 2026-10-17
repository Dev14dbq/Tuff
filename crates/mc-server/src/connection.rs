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
use crate::context::{Event, OnlinePlayer, Pose};
use crate::interact::{self, Actor, Hands, Outcome};
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
            let login = login(&mut reader, &mut writer, &intention);
            let session = tokio::time::timeout(LOGIN_TIMEOUT, login)
                .await
                .context("login timed out")??;
            let Some(player) = session else {
                return Ok(());
            };
            let name = player.player.profile.name.clone();
            let uuid = player.player.profile.uuid;
            info!("{name} ({}) joined from {peer}", uuid_string(uuid));
            let result = play(&mut reader, &mut writer, &player).await;
            ctx.players.lock().expect("players lock").remove(&uuid);
            let entity_id = player.player.entity_id;
            let _ = ctx.events.send(Event::PlayerLeft { uuid, entity_id });
            let left = text::colored(&format!("{name} left the game"), "yellow");
            let _ = ctx.events.send(Event::Chat(left));
            let reason = result
                .as_ref()
                .err()
                .map_or("disconnected".to_owned(), |e| format!("{e:#}"));
            info!("{name} left: {reason}");
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
    view_distance: i32,
    /// Displayed skin parts from Client Information (cape, jacket, sleeves, ...).
    skin_parts: u8,
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
) -> Result<Option<Session>> {
    let ctx = writer.ctx.clone();
    let name = match reader.next(State::Login).await? {
        Packet::Login(login::Serverbound::Hello { name, .. }) => name,
        other => bail!("expected Login Start, got {other:?}"),
    };

    if intention.protocol_version != mc_protocol::PROTOCOL_VERSION {
        let reason = format!(
            "This server runs Minecraft {} (protocol {}), you have protocol {}",
            ctx.data.version.name,
            mc_protocol::PROTOCOL_VERSION,
            intention.protocol_version
        );
        writer
            .send(&refusal(serde_json::json!({ "text": reason })))
            .await?;
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

    let player = OnlinePlayer {
        profile,
        game_mode: CREATIVE,
        entity_id: ctx.next_entity_id(),
        skin_parts: 0,
        pose: None,
        main_hand: None,
    };
    let uuid = player.profile.uuid;
    // The lock guard must not live across an await.
    let refused = {
        let mut players = ctx.players.lock().expect("players lock");
        if players.contains_key(&uuid) {
            Some("You are already connected to this server")
        } else if players.len() >= ctx.config.max_players {
            Some("The server is full")
        } else {
            players.insert(uuid, player.clone());
            None
        }
    };
    if let Some(reason) = refused {
        writer
            .send(&refusal(serde_json::json!({ "text": reason })))
            .await?;
        return Ok(None);
    }

    let mut session = Session {
        player,
        view_distance: i32::from(ctx.config.view_distance),
        skin_parts: 0,
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
    let (x, y, z) = world.spawn;
    let mut pose = Pose {
        x,
        y,
        z,
        yaw: world.spawn_yaw,
        pitch: 0.0,
        on_ground: true,
    };

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
    {
        use play::abilities::*;
        writer.queue(&play::PlayerAbilities {
            flags: INVULNERABLE | ALLOW_FLYING | INSTANT_BREAK,
            flying_speed: 0.05,
            fov_modifier: 0.1,
        })?;
    }
    let mut awaiting_teleport = Some(1);
    writer.queue(&play::PlayerPosition {
        teleport_id: 1,
        x,
        y,
        z,
        velocity: [0.0; 3],
        yaw: world.spawn_yaw,
        pitch: 0.0,
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
    let joined = text::colored(&format!("{} joined the game", me.profile.name), "yellow");
    let _ = ctx.events.send(Event::Chat(joined));

    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    let mut pending_keep_alive: Option<(i64, Instant)> = None;
    let mut hands = Hands::default();
    loop {
        tokio::select! {
            packet = reader.next(State::Play) => {
                let Packet::Play(packet) = packet? else {
                    unreachable!("play state decodes play packets")
                };
                // Movement sent before the client saw our teleport is stale.
                let settled = awaiting_teleport.is_none();
                let mut moved = false;
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
                        let line = format!("<{}> {}", me.profile.name, message);
                        info!("{line}");
                        let _ = ctx.events.send(Event::Chat(text::plain(&line)));
                    }
                    play::Serverbound::ChatCommand { command } => {
                        writer.queue(&play::SystemChat {
                            content: text::colored(&format!("Unknown command: /{command}"), "red"),
                            overlay: false,
                        })?;
                    }
                    play::Serverbound::SetCreativeModeSlot { slot, item } if me.game_mode == CREATIVE => {
                        if let Ok(slot) = usize::try_from(slot)
                            && slot < hands.slots.len()
                        {
                            let before = hands.main_hand().cloned();
                            hands.slots[slot] = item;
                            announce_main_hand(&ctx, uuid, entity_id, before, &hands);
                        }
                    }
                    play::Serverbound::SetCarriedItem { slot } => {
                        if let Ok(slot @ 0..=8) = usize::try_from(slot) {
                            let before = hands.main_hand().cloned();
                            hands.selected = slot;
                            announce_main_hand(&ctx, uuid, entity_id, before, &hands);
                        }
                    }
                    play::Serverbound::Punch => {
                        let _ = ctx.events.send(Event::Swing { uuid, entity_id, times: 1 });
                    }
                    play::Serverbound::PlayerAction { status, position, sequence, .. } => {
                        let actor = Actor { uuid, entity_id, game_mode: me.game_mode, pose };
                        let outcome = interact::dig(&ctx, actor, status, position, sequence);
                        answer(writer, &ctx, outcome)?;
                    }
                    play::Serverbound::UseItemOn { hand, position, face, cursor, sequence, .. } => {
                        let actor = Actor { uuid, entity_id, game_mode: me.game_mode, pose };
                        let outcome = interact::use_item_on(
                            &ctx, actor, &hands, hand, position, face, cursor, sequence,
                        );
                        answer(writer, &ctx, outcome)?;
                    }
                    play::Serverbound::UseItem { sequence, .. } => {
                        writer.queue(&play::BlockChangedAck { sequence })?;
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
                        for (position, state) in changes {
                            if view.has_block(position) {
                                writer.queue(&play::BlockUpdate { position, state })?;
                            }
                        }
                    }
                    Ok(Event::BlockBroken { by, position, state }) if by != uuid && view.has_block(position) => {
                        writer.queue(&play::LevelEvent {
                            event: play::level_event::BLOCK_BROKEN,
                            position,
                            data: state as i32,
                            global: false,
                        })?;
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
    for event in outcome.events {
        let _ = ctx.events.send(event);
    }
    Ok(())
}

/// Tells others when the item in the main hand changes.
fn announce_main_hand(
    ctx: &ServerContext,
    uuid: u128,
    entity_id: i32,
    before: Option<play::ItemStack>,
    hands: &Hands,
) {
    let now = hands.main_hand().cloned();
    if now == before {
        return;
    }
    update_player(ctx, uuid, |p| p.main_hand.clone_from(&now));
    let _ = ctx.events.send(Event::Equipment {
        uuid,
        entity_id,
        main_hand: now,
    });
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
