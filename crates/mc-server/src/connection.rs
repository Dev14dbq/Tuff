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
use crate::context::{Event, OnlinePlayer};
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
            let _ = ctx.events.send(Event::PlayerLeft(uuid));
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
    loop {
        match reader.next(State::Configuration).await? {
            Packet::Configuration(configuration::Serverbound::SelectKnownPacks(packs)) => {
                if !packs.contains(&ctx.core_pack) {
                    // Without the vanilla pack on the client we would have to send
                    // every registry entry as NBT; not implemented yet.
                    let reason = format!(
                        "This server needs the Minecraft {} client",
                        ctx.data.version.name
                    );
                    let disconnect = configuration::Disconnect {
                        reason: text::plain(&reason),
                    };
                    writer.send(&disconnect).await?;
                    return Ok(false);
                }
                break;
            }
            Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                session.view_distance = view_distance(&info);
                session.skin_parts = info.skin_parts;
            }
            other => trace!("configuration: {other:?}"),
        }
    }
    for registry in &ctx.registries {
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
    let mut events = ctx.events.subscribe();
    let (x, y, z) = world.spawn;

    let entity_id = ctx.next_entity_id();
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
        is_flat: true,
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
        yaw: 0.0,
        pitch: 0.0,
        flags: 0,
    })?;

    // Player list: everyone already online, including ourselves (the client
    // takes its own skin from this list).
    let everyone: Vec<_> = ctx
        .players
        .lock()
        .expect("players lock")
        .values()
        .map(OnlinePlayer::info_entry)
        .collect();
    writer.queue(&play::PlayerInfoUpdate {
        actions: play::player_info::ALL,
        players: everyone,
    })?;
    let _ = ctx.events.send(Event::PlayerJoined(me.clone()));

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
    let mut view = ChunkView::new(session.view_distance);
    view.move_to(writer, chunk_of(x), chunk_of(z))?;
    writer.queue(&play::SystemChat {
        content: text::colored(
            "Welcome to minecraft-rewrite! This is a superflat test world.",
            "gold",
        ),
        overlay: false,
    })?;
    writer.flush().await?;
    let joined = text::colored(&format!("{} joined the game", me.profile.name), "yellow");
    let _ = ctx.events.send(Event::Chat(joined));

    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    let mut pending_keep_alive: Option<(i64, Instant)> = None;
    loop {
        tokio::select! {
            packet = reader.next(State::Play) => {
                let Packet::Play(packet) = packet? else {
                    unreachable!("play state decodes play packets")
                };
                match packet {
                    play::Serverbound::AcceptTeleportation { teleport_id, .. } => {
                        if awaiting_teleport == Some(teleport_id) {
                            awaiting_teleport = None;
                        }
                    }
                    play::Serverbound::MovePlayerPos { x, z, .. }
                    | play::Serverbound::MovePlayerPosRot { x, z, .. } => {
                        // Movement sent before the client saw our teleport is stale.
                        if awaiting_teleport.is_none() && x.is_finite() && z.is_finite() {
                            view.move_to(writer, chunk_of(x), chunk_of(z))?;
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
                    play::Serverbound::ClientInformation(info) => {
                        writer.queue(&skin_parts_metadata(entity_id, info.skin_parts))?;
                    }
                    other => trace!("play: {other:?}"),
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
                    Ok(Event::PlayerJoined(other)) if other.profile.uuid != me.profile.uuid => {
                        writer.queue(&play::PlayerInfoUpdate {
                            actions: play::player_info::ALL,
                            players: vec![other.info_entry()],
                        })?;
                    }
                    Ok(Event::PlayerLeft(uuid)) if uuid != me.profile.uuid => {
                        writer.queue(&play::PlayerInfoRemove { uuids: vec![uuid] })?;
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => bail!("server shutting down"),
                }
            }
        }
        writer.flush().await?;
    }
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
struct ChunkView {
    radius: i32,
    center: Option<(i32, i32)>,
    loaded: HashSet<(i32, i32)>,
}

impl ChunkView {
    fn new(radius: i32) -> Self {
        Self {
            radius,
            center: None,
            loaded: HashSet::new(),
        }
    }

    fn move_to(&mut self, writer: &mut PacketWriter, cx: i32, cz: i32) -> Result<()> {
        if self.center == Some((cx, cz)) {
            return Ok(());
        }
        let ctx = writer.ctx.clone();
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
            writer.queue(&play::ForgetLevelChunk { x, z })?;
        }

        let mut missing: Vec<_> = (cx - r..=cx + r)
            .flat_map(|x| (cz - r..=cz + r).map(move |z| (x, z)))
            .filter(|c| !self.loaded.contains(c))
            .collect();
        missing.sort_by_key(|&(x, z)| (x - cx).pow(2) + (z - cz).pow(2));

        writer.queue(&play::SetChunkCacheCenter { x: cx, z: cz })?;
        if missing.is_empty() {
            return Ok(());
        }
        writer.queue(&play::ChunkBatchStart)?;
        for &(x, z) in &missing {
            writer.queue(&ctx.world.chunk_packet(x, z))?;
            self.loaded.insert((x, z));
        }
        writer.queue(&play::ChunkBatchFinished {
            batch_size: missing.len() as i32,
        })?;
        Ok(())
    }
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
