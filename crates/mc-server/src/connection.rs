//! One client connection: handshake → status, or login → configuration → play.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use mc_protocol::frame::{decode_frame_body, split_frame};
use mc_protocol::packets::{GameProfile, configuration, handshake, login, play, status};
use mc_protocol::text;
use mc_protocol::{Direction, Encode, Position, Reader, State, Write as _};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast;
use tracing::{debug, info, trace};

use crate::ServerContext;
use crate::util::{offline_uuid, random_u64, random_uuid, uuid_string, valid_player_name};

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);

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
            let session =
                tokio::time::timeout(LOGIN_TIMEOUT, login(&mut reader, &mut writer, &intention))
                    .await
                    .context("login timed out")??;
            let Some(player) = session else {
                return Ok(());
            };
            info!(
                "{} ({}) joined from {peer}",
                player.name,
                uuid_string(player.uuid)
            );
            let result = play(&mut reader, &mut writer, &player).await;
            ctx.players
                .lock()
                .expect("players lock")
                .remove(&player.uuid);
            let _ = ctx.chat.send(text::colored(
                &format!("{} left the game", player.name),
                "yellow",
            ));
            info!(
                "{} left: {}",
                player.name,
                result
                    .as_ref()
                    .err()
                    .map_or("disconnected".to_owned(), |e| format!("{e:#}"))
            );
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
                    .iter()
                    .take(12)
                    .map(|(uuid, name)| serde_json::json!({"name": name, "id": uuid_string(*uuid)}))
                    .collect();
                let json = serde_json::json!({
                    "version": {"name": ctx.data.version.name, "protocol": mc_protocol::PROTOCOL_VERSION},
                    "players": {"max": ctx.config.max_players, "online": ctx.online_count(), "sample": sample},
                    "description": {"text": ctx.config.motd},
                    "enforcesSecureChat": false,
                });
                writer
                    .send(&status::StatusResponse {
                        json: json.to_string(),
                    })
                    .await?;
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

struct Player {
    uuid: u128,
    name: String,
    view_distance: i32,
}

async fn login(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    intention: &handshake::Intention,
) -> Result<Option<Player>> {
    let ctx = writer.ctx.clone();
    let (name, _client_uuid) = match reader.next(State::Login).await? {
        Packet::Login(login::Serverbound::Hello { name, uuid }) => (name, uuid),
        other => bail!("expected Login Start, got {other:?}"),
    };

    let refuse = |reason: String| login::LoginDisconnect {
        reason_json: serde_json::json!({ "text": reason }).to_string(),
    };
    if intention.protocol_version != mc_protocol::PROTOCOL_VERSION {
        let reason = format!(
            "This server runs Minecraft {} (protocol {}), you have protocol {}",
            ctx.data.version.name,
            mc_protocol::PROTOCOL_VERSION,
            intention.protocol_version
        );
        writer.send(&refuse(reason)).await?;
        return Ok(None);
    }
    if !valid_player_name(&name) {
        writer
            .send(&refuse(format!("Invalid player name {name:?}")))
            .await?;
        return Ok(None);
    }

    // Offline mode: the UUID is derived from the name, as vanilla does.
    let uuid = offline_uuid(&name);
    // The lock guard must not live across an await.
    let refusal = {
        let mut players = ctx.players.lock().expect("players lock");
        if players.contains_key(&uuid) {
            Some("You are already connected to this server")
        } else if players.len() >= ctx.config.max_players {
            Some("The server is full")
        } else {
            players.insert(uuid, name.clone());
            None
        }
    };
    if let Some(reason) = refusal {
        writer.send(&refuse(reason.to_owned())).await?;
        return Ok(None);
    }
    let mut player = Player {
        uuid,
        name,
        view_distance: i32::from(ctx.config.view_distance),
    };
    // From here on the player is registered; remove them if login fails.
    let result = configure(reader, writer, &mut player).await;
    match result {
        Ok(true) => Ok(Some(player)),
        Ok(false) | Err(_) => {
            ctx.players
                .lock()
                .expect("players lock")
                .remove(&player.uuid);
            result.map(|_| None)
        }
    }
}

/// Returns `Ok(false)` if the client was refused during configuration.
async fn configure(
    reader: &mut PacketReader,
    writer: &mut PacketWriter,
    player: &mut Player,
) -> Result<bool> {
    let ctx = writer.ctx.clone();
    if let Some(threshold) = ctx.config.compression_threshold {
        writer
            .send(&login::LoginCompression {
                threshold: threshold as i32,
            })
            .await?;
        writer.compression = Some(threshold);
        reader.compression = Some(threshold);
    }
    writer
        .send(&login::LoginFinished {
            profile: GameProfile {
                uuid: player.uuid,
                name: player.name.clone(),
                properties: Vec::new(),
            },
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
                    writer
                        .send(&configuration::Disconnect {
                            reason: text::plain(&reason),
                        })
                        .await?;
                    return Ok(false);
                }
                break;
            }
            Packet::Configuration(configuration::Serverbound::ClientInformation(info)) => {
                player.view_distance =
                    i32::from(info.view_distance).clamp(2, i32::from(ctx.config.view_distance));
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
                player.view_distance =
                    i32::from(info.view_distance).clamp(2, i32::from(ctx.config.view_distance));
            }
            other => trace!("configuration: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Play

async fn play(reader: &mut PacketReader, writer: &mut PacketWriter, player: &Player) -> Result<()> {
    let ctx = writer.ctx.clone();
    let world = &ctx.world;
    let mut chat = ctx.chat.subscribe();
    let (x, y, z) = world.spawn;

    writer.queue(&play::Login {
        entity_id: ctx.next_entity_id(),
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
        game_mode: 1, // creative
        previous_game_mode: None,
        is_debug: false,
        is_flat: true,
        death_location: None,
        portal_cooldown: 0,
        sea_level: world.sea_level,
        online_mode: false,
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
    writer.queue(&play::SetDefaultSpawnPosition {
        dimension: world.dimension.clone(),
        position: Position::new(x.floor() as i32, y as i32, z.floor() as i32),
        yaw: 0.0,
        pitch: 0.0,
    })?;
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
    writer.queue(&play::GameEvent {
        event: play::game_event::START_WAITING_FOR_CHUNKS,
        value: 0.0,
    })?;
    let mut view = ChunkView::new(player.view_distance);
    view.move_to(writer, chunk_of(x), chunk_of(z))?;
    writer.queue(&play::SystemChat {
        content: text::colored(
            "Welcome to minecraft-rewrite! This is a superflat test world.",
            "gold",
        ),
        overlay: false,
    })?;
    writer.flush().await?;
    let _ = ctx.chat.send(text::colored(
        &format!("{} joined the game", player.name),
        "yellow",
    ));

    let mut keep_alive = tokio::time::interval(KEEP_ALIVE_INTERVAL);
    let mut pending_keep_alive: Option<(i64, Instant)> = None;
    loop {
        tokio::select! {
            packet = reader.next(State::Play) => {
                let Packet::Play(packet) = packet? else { unreachable!("play state decodes play packets") };
                match packet {
                    play::Serverbound::AcceptTeleportation(id) => {
                        if awaiting_teleport == Some(id) {
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
                        let line = format!("<{}> {}", player.name, message);
                        info!("{line}");
                        let _ = ctx.chat.send(text::plain(&line));
                    }
                    play::Serverbound::ChatCommand { command } => {
                        writer.queue(&play::SystemChat {
                            content: text::colored(&format!("Unknown command: /{command}"), "red"),
                            overlay: false,
                        })?;
                    }
                    other => trace!("play: {other:?}"),
                }
            }
            _ = keep_alive.tick() => {
                if let Some((_, sent)) = pending_keep_alive {
                    if sent.elapsed() > KEEP_ALIVE_TIMEOUT {
                        writer.send(&play::Disconnect { reason: text::plain("Timed out") }).await?;
                        bail!("keep-alive timeout");
                    }
                } else {
                    let id = (random_u64() >> 1) as i64;
                    pending_keep_alive = Some((id, Instant::now()));
                    writer.queue(&play::KeepAlive { id })?;
                }
            }
            message = chat.recv() => {
                match message {
                    Ok(content) => writer.queue(&play::SystemChat { content, overlay: false })?,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => bail!("server shutting down"),
                }
            }
        }
        writer.flush().await?;
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
    ctx: Arc<ServerContext>,
}

impl PacketReader {
    fn new(half: OwnedReadHalf, ctx: Arc<ServerContext>) -> Self {
        Self {
            half,
            buf: Vec::with_capacity(4096),
            compression: None,
            ctx,
        }
    }

    async fn fill(&mut self) -> Result<()> {
        // `read_buf` is cancel-safe, so `next` can sit in a `select!`.
        if self.half.read_buf(&mut self.buf).await? == 0 {
            bail!("connection closed");
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
    ctx: Arc<ServerContext>,
}

impl PacketWriter {
    fn new(half: OwnedWriteHalf, ctx: Arc<ServerContext>) -> Self {
        Self {
            half,
            out: Vec::with_capacity(4096),
            compression: None,
            ctx,
        }
    }

    fn queue<P: Encode>(&mut self, packet: &P) -> Result<()> {
        self.ctx
            .ids
            .write(packet, &mut self.out, self.compression)?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<()> {
        if !self.out.is_empty() {
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
