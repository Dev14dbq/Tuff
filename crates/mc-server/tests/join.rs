//! End-to-end: a protocol bot pings the server, logs in (offline and online
//! mode, against a fake session server), configures, spawns, receives chunks
//! and chats. Needs extracted data (`cargo xtask extract-data`); skipped otherwise.
//!
//! The bot is written from the protocol documentation, independently of the
//! server's encoders, so layout mistakes on either side show up as failures.

use std::path::Path;
use std::time::Duration;

use mc_nbt::Tag;
use mc_protocol::crypto::{Cfb8, server_hash};
use mc_protocol::frame::{decode_frame_body, split_frame, write_frame};
use mc_protocol::{Direction, PacketIds, Reader, State, Write as _};
use mc_server::{Config, Server, find_data_dir};
use rsa::pkcs8::DecodePublicKey;
use rsa::rand_core::OsRng;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct Bot {
    stream: TcpStream,
    buf: Vec<u8>,
    compression: Option<usize>,
    /// (to server, from server) once encryption is on.
    ciphers: Option<(Cfb8, Cfb8)>,
    ids: PacketIds,
    state: State,
}

impl Bot {
    async fn connect(addr: std::net::SocketAddr, ids: PacketIds) -> Self {
        Self {
            stream: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
            compression: None,
            ciphers: None,
            ids,
            state: State::Handshake,
        }
    }

    async fn send(&mut self, name: &str, body: Vec<u8>) {
        let id = self
            .ids
            .id(self.state, Direction::Serverbound, name)
            .unwrap_or_else(|| panic!("no serverbound {name} in {:?}", self.state));
        let mut out = Vec::new();
        write_frame(&mut out, id, &body, self.compression);
        if let Some((encrypt, _)) = &mut self.ciphers {
            encrypt.encrypt(&mut out);
        }
        self.stream.write_all(&out).await.unwrap();
    }

    /// Next clientbound packet as (name, body).
    async fn recv(&mut self) -> (String, Vec<u8>) {
        loop {
            if let Some((header, len)) = split_frame(&self.buf).unwrap() {
                let inner =
                    decode_frame_body(&self.buf[header..header + len], self.compression).unwrap();
                self.buf.drain(..header + len);
                let mut r = Reader::new(&inner);
                let id = r.varint().unwrap();
                let name = self
                    .ids
                    .name(self.state, Direction::Clientbound, id)
                    .unwrap_or_else(|| panic!("unknown clientbound id {id} in {:?}", self.state))
                    .to_owned();
                return (name, r.rest().to_vec());
            }
            let start = self.buf.len();
            let n =
                tokio::time::timeout(Duration::from_secs(10), self.stream.read_buf(&mut self.buf))
                    .await
                    .expect("server answered in time")
                    .unwrap();
            assert!(n > 0, "server closed the connection");
            if let Some((_, decrypt)) = &mut self.ciphers {
                decrypt.decrypt(&mut self.buf[start..]);
            }
        }
    }

    async fn recv_until(&mut self, wanted: &str) -> (Vec<String>, Vec<u8>) {
        let mut seen = Vec::new();
        loop {
            let (name, body) = self.recv().await;
            if name == wanted {
                return (seen, body);
            }
            seen.push(name);
        }
    }

    async fn handshake(&mut self, intent: i32) {
        let mut body = Vec::new();
        body.put_varint(mc_protocol::PROTOCOL_VERSION);
        body.put_string("localhost");
        body.put_u16(25565);
        body.put_varint(intent);
        self.send("minecraft:intention", body).await;
        self.state = if intent == 1 {
            State::Status
        } else {
            State::Login
        };
    }

    async fn login(&mut self, name: &str) {
        self.handshake(2).await;
        let mut body = Vec::new();
        body.put_string(name);
        body.put_uuid(0);
        self.send("minecraft:hello", body).await;
    }
}

fn data_dir() -> Option<std::path::PathBuf> {
    find_data_dir(Path::new(env!("CARGO_MANIFEST_DIR"))).ok()
}

#[tokio::test]
async fn ping_login_spawn_and_chat() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 3;
    config.online_mode = false;
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ids = server.context().ids.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    // --- status
    let mut bot = Bot::connect(addr, ids.clone()).await;
    bot.handshake(1).await;
    bot.send("minecraft:status_request", Vec::new()).await;
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:status_response");
    let json: serde_json::Value =
        serde_json::from_str(&Reader::new(&body).string(32767).unwrap()).unwrap();
    assert_eq!(json["version"]["protocol"], 777);
    assert_eq!(json["players"]["online"], 0);
    let mut ping = Vec::new();
    ping.put_i64(123456789);
    bot.send("minecraft:ping_request", ping).await;
    let (name, body) = bot.recv().await;
    assert_eq!(
        (name.as_str(), Reader::new(&body).i64().unwrap()),
        ("minecraft:pong_response", 123456789)
    );

    // --- login
    let mut bot = Bot::connect(addr, ids.clone()).await;
    bot.login("Bot").await;
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:login_compression");
    bot.compression = Some(Reader::new(&body).varint().unwrap() as usize);
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:login_finished");
    let mut r = Reader::new(&body);
    let uuid = r.uuid().unwrap();
    assert_eq!(r.string(16).unwrap(), "Bot");
    assert_eq!(r.length_prefix().unwrap(), 0, "no profile properties");
    assert_ne!(r.uuid().unwrap(), 0, "session id");
    assert_eq!(uuid >> 76 & 0xF, 3, "offline UUIDs are version 3");
    bot.send("minecraft:login_acknowledged", Vec::new()).await;
    bot.state = State::Configuration;

    // --- configuration
    let mut info = Vec::new();
    info.put_string("en_us");
    info.put_i8(3);
    info.put_varint(0);
    info.put_bool(true);
    info.put_u8(0x7f);
    info.put_varint(1);
    info.put_bool(false);
    info.put_bool(true);
    info.put_varint(0);
    bot.send("minecraft:client_information", info).await;
    let (_, packs) = bot.recv_until("minecraft:select_known_packs").await;
    bot.send("minecraft:select_known_packs", packs).await; // we know exactly the same packs
    let (seen, _) = bot.recv_until("minecraft:finish_configuration").await;
    assert_eq!(
        seen.iter()
            .filter(|n| *n == "minecraft:registry_data")
            .count(),
        mc_data::SYNCHRONIZED_REGISTRIES.len()
    );
    assert!(seen.contains(&"minecraft:update_tags".to_owned()));
    bot.send("minecraft:finish_configuration", Vec::new()).await;
    bot.state = State::Play;

    // --- play: spawn and chunks
    let mut teleport = None;
    let mut chunks = 0;
    let mut started_waiting = false;
    let mut listed_self = false;
    let mut skin_layers = None;
    loop {
        let (name, body) = bot.recv().await;
        match name.as_str() {
            "minecraft:player_position" => teleport = Some(body),
            "minecraft:set_entity_data" => {
                // entity id, then (index 16, type Byte, value) and the 0xff end marker
                let mut r = Reader::new(&body);
                r.varint().unwrap();
                assert_eq!((r.u8().unwrap(), r.varint().unwrap()), (16, 0));
                skin_layers = Some(r.u8().unwrap());
                assert_eq!(r.u8().unwrap(), 0xff);
            }
            "minecraft:player_info_update" => {
                let mut r = Reader::new(&body);
                assert_eq!(r.u8().unwrap(), 0xff, "all player info actions");
                assert_eq!(r.length_prefix().unwrap(), 1);
                assert_eq!(r.uuid().unwrap(), uuid);
                assert_eq!(r.string(16).unwrap(), "Bot");
                listed_self = true;
            }
            "minecraft:game_event" => started_waiting |= body[0] == 13,
            "minecraft:level_chunk_with_light" => {
                check_flat_chunk(&body);
                chunks += 1;
            }
            "minecraft:chunk_batch_finished" => {
                assert_eq!(Reader::new(&body).varint().unwrap(), chunks);
                break;
            }
            _ => {}
        }
    }
    assert!(started_waiting, "game event 13 (start waiting for chunks)");
    assert!(listed_self, "the player list contains ourselves (own skin)");
    assert_eq!(
        skin_layers,
        Some(0x7f),
        "skin layers echo Client Information"
    );
    assert_eq!(chunks, 7 * 7, "view distance 3 around spawn");
    // Confirm Teleportation (26.3): teleport id, resulting x, y, z, yaw, pitch.
    let teleport = teleport.expect("spawn teleport");
    let mut r = Reader::new(&teleport);
    let mut confirm = Vec::new();
    confirm.put_varint(r.varint().unwrap());
    for _ in 0..3 {
        confirm.put_f64(r.f64().unwrap());
    }
    r.bytes(24).unwrap(); // velocity
    confirm.put_f32(r.f32().unwrap());
    confirm.put_f32(r.f32().unwrap());
    bot.send("minecraft:accept_teleportation", confirm).await;

    // Walk 3 chunks east: new column of chunks arrives, old one is forgotten.
    let mut moved = Vec::new();
    for v in [48.5, -60.0, 0.5] {
        moved.put_f64(v);
    }
    moved.put_u8(0x01);
    bot.send("minecraft:move_player_pos", moved).await;
    let (seen, _) = bot.recv_until("minecraft:chunk_batch_finished").await;
    assert_eq!(
        seen.iter()
            .filter(|n| *n == "minecraft:level_chunk_with_light")
            .count(),
        3 * 7
    );
    assert_eq!(
        seen.iter()
            .filter(|n| *n == "minecraft:forget_level_chunk")
            .count(),
        3 * 7
    );

    // --- chat comes back as a system message
    let mut chat = Vec::new();
    chat.put_string("hello world");
    chat.put_i64(0);
    chat.put_i64(0);
    chat.put_bool(false);
    chat.put_varint(0);
    chat.put_slice(&[0, 0, 0]);
    chat.put_u8(0);
    bot.send("minecraft:chat", chat).await;
    loop {
        let (_, body) = bot.recv_until("minecraft:system_chat").await;
        let (tag, _) = mc_nbt::read_network(&body).unwrap();
        if tag == Tag::from("<Bot> hello world") {
            break;
        }
    }

    // --- the same name cannot log in twice
    let mut twin = Bot::connect(addr, ids.clone()).await;
    twin.login("Bot").await;
    let (name, body) = twin.recv().await;
    assert_eq!(name, "minecraft:login_disconnect");
    assert!(
        Reader::new(&body)
            .string(32767)
            .unwrap()
            .contains("already connected")
    );

    drop(bot);
    stop.send(()).unwrap();
    running.await.unwrap().unwrap();
}

#[tokio::test]
async fn client_without_known_packs_gets_registry_data_as_nbt() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ids = server.context().ids.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let mut bot = Bot::connect(addr, ids).await;
    bot.login("NoPacks").await;
    let (_, body) = bot.recv_until("minecraft:login_compression").await;
    bot.compression = Some(Reader::new(&body).varint().unwrap() as usize);
    bot.recv_until("minecraft:login_finished").await;
    bot.send("minecraft:login_acknowledged", Vec::new()).await;
    bot.state = State::Configuration;
    bot.recv_until("minecraft:select_known_packs").await;
    let mut none = Vec::new();
    none.put_varint(0);
    bot.send("minecraft:select_known_packs", none).await;

    let (mut registries, mut entries) = (0, 0);
    loop {
        let (name, body) = bot.recv().await;
        match name.as_str() {
            "minecraft:registry_data" => {
                registries += 1;
                let mut r = Reader::new(&body);
                r.identifier().unwrap();
                for _ in 0..r.length_prefix().unwrap() {
                    r.identifier().unwrap();
                    assert!(r.bool().unwrap(), "every entry carries NBT");
                    // Usually a compound; some registries use other root tags,
                    // exactly as vanilla sends them (see `cargo xtask registry-diff`).
                    r.nbt().unwrap();
                    entries += 1;
                }
                assert_eq!(r.remaining(), 0);
            }
            "minecraft:finish_configuration" => break,
            _ => {}
        }
    }
    assert_eq!(registries, mc_data::SYNCHRONIZED_REGISTRIES.len());
    assert!(entries > 400, "{entries} entries");
    bot.send("minecraft:finish_configuration", Vec::new()).await;
    bot.state = State::Play;
    bot.recv_until("minecraft:login").await;
    let _ = stop.send(());
}

/// Logs an offline-mode bot all the way into play (first chunk batch received,
/// spawn teleport confirmed) and returns it with its spawn position.
async fn join_offline(
    addr: std::net::SocketAddr,
    ids: PacketIds,
    name: &str,
) -> (Bot, (f64, f64, f64)) {
    let mut bot = Bot::connect(addr, ids).await;
    bot.login(name).await;
    let (_, body) = bot.recv_until("minecraft:login_compression").await;
    bot.compression = Some(Reader::new(&body).varint().unwrap() as usize);
    bot.recv_until("minecraft:login_finished").await;
    bot.send("minecraft:login_acknowledged", Vec::new()).await;
    bot.state = State::Configuration;
    let (_, packs) = bot.recv_until("minecraft:select_known_packs").await;
    bot.send("minecraft:select_known_packs", packs).await;
    bot.recv_until("minecraft:finish_configuration").await;
    bot.send("minecraft:finish_configuration", Vec::new()).await;
    bot.state = State::Play;
    let (_, teleport) = bot.recv_until("minecraft:player_position").await;
    let mut r = Reader::new(&teleport);
    let mut confirm = Vec::new();
    confirm.put_varint(r.varint().unwrap());
    let position = (r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap());
    for v in [position.0, position.1, position.2] {
        confirm.put_f64(v);
    }
    confirm.put_f32(0.0);
    confirm.put_f32(0.0);
    bot.send("minecraft:accept_teleportation", confirm).await;
    bot.recv_until("minecraft:chunk_batch_finished").await;
    (bot, position)
}

#[tokio::test]
async fn players_see_each_other_spawn_move_and_leave() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ids = server.context().ids.clone();
    let player_type = server.context().player_entity_type;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let (mut alice, _) = join_offline(addr, ids.clone(), "Alice").await;
    let (mut bob, spawn) = join_offline(addr, ids.clone(), "Bob").await;

    // Alice learns about Bob, then sees his entity appear.
    let (_, info) = alice.recv_until("minecraft:player_info_update").await;
    let mut r = Reader::new(&info);
    r.u8().unwrap();
    r.length_prefix().unwrap();
    let bob_uuid = r.uuid().unwrap();
    assert_eq!(r.string(16).unwrap(), "Bob");
    let (_, spawned) = alice.recv_until("minecraft:add_entity").await;
    let mut r = Reader::new(&spawned);
    let bob_entity = r.varint().unwrap();
    assert_eq!(r.uuid().unwrap(), bob_uuid);
    assert_eq!(r.varint().unwrap(), player_type);
    assert_eq!(
        (r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap()),
        spawn
    );

    // Bob walks and turns; Alice sees where he went.
    let mut moved = Vec::new();
    for v in [spawn.0 + 3.0, spawn.1, spawn.2 - 2.0] {
        moved.put_f64(v);
    }
    moved.put_f32(90.0);
    moved.put_f32(-15.0);
    moved.put_u8(0x01);
    bob.send("minecraft:move_player_pos_rot", moved).await;
    let (_, sync) = alice.recv_until("minecraft:entity_position_sync").await;
    let mut r = Reader::new(&sync);
    assert_eq!(r.varint().unwrap(), bob_entity);
    assert_eq!(r.varint().unwrap(), 0, "linear path");
    assert_eq!(
        (r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap()),
        (spawn.0 + 3.0, spawn.1, spawn.2 - 2.0)
    );
    assert_eq!(
        (r.f32().unwrap(), r.f32().unwrap(), r.bool().unwrap()),
        (90.0, -15.0, true)
    );

    // Bob leaves: his entity and list entry go away.
    drop(bob);
    let (_, removed) = alice.recv_until("minecraft:remove_entities").await;
    let mut r = Reader::new(&removed);
    assert_eq!(
        (r.length_prefix().unwrap(), r.varint().unwrap()),
        (1, bob_entity)
    );
    let (_, gone) = alice.recv_until("minecraft:player_info_remove").await;
    let mut r = Reader::new(&gone);
    assert_eq!(
        (r.length_prefix().unwrap(), r.uuid().unwrap()),
        (1, bob_uuid)
    );
    let _ = stop.send(());
}

/// Stands in for sessionserver.mojang.com: answers one HTTP request with
/// `response` and hands back the request line it received.
async fn fake_session_server(response: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        stream.write_all(response.as_bytes()).await.unwrap();
        let _ = stream.shutdown().await;
        String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    });
    (base, handle)
}

/// Starts an online-mode server using `session_server`, logs a bot in up to
/// the key exchange, and returns the bot (encryption on), the shared secret
/// and the server's public key.
async fn online_bot(
    session_server: String,
) -> (Bot, [u8; 16], Vec<u8>, tokio::sync::oneshot::Sender<()>) {
    let mut config = Config::new(data_dir().unwrap());
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = true;
    config.session_server = session_server;
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ids = server.context().ids.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let mut bot = Bot::connect(addr, ids).await;
    bot.login("Bot").await;
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:hello", "Encryption Request");
    let mut r = Reader::new(&body);
    assert_eq!(r.string(20).unwrap(), "", "empty server id");
    let public_der = r.byte_array(1024).unwrap().to_vec();
    let token = r.byte_array(64).unwrap().to_vec();
    assert!(r.bool().unwrap(), "should authenticate");

    let public = RsaPublicKey::from_public_key_der(&public_der).unwrap();
    let secret = [0x5a; 16];
    let mut response = Vec::new();
    response.put_byte_array(
        &public
            .encrypt(&mut OsRng, Pkcs1v15Encrypt, &secret)
            .unwrap(),
    );
    response.put_byte_array(&public.encrypt(&mut OsRng, Pkcs1v15Encrypt, &token).unwrap());
    bot.send("minecraft:key", response).await;
    bot.ciphers = Some((Cfb8::new(&secret), Cfb8::new(&secret)));
    (bot, secret, public_der, stop)
}

fn http_response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn online_login_uses_session_server_profile() {
    if data_dir().is_none() {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    }
    let profile = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Bot","properties":[{"name":"textures","value":"dGV4dHVyZXM=","signature":"c2lnbmF0dXJl"}]}"#;
    let (session_server, request) = fake_session_server(http_response("200 OK", profile)).await;
    let (mut bot, secret, public_der, stop) = online_bot(session_server).await;

    // Everything from here on is encrypted.
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:login_compression");
    bot.compression = Some(Reader::new(&body).varint().unwrap() as usize);
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:login_finished");
    let mut r = Reader::new(&body);
    assert_eq!(
        r.uuid().unwrap(),
        0x069a79f444e94726a5befca90e38aaf5,
        "UUID from the session server"
    );
    assert_eq!(r.string(16).unwrap(), "Bot");
    assert_eq!(r.length_prefix().unwrap(), 1, "skin textures property");
    assert_eq!(r.string(64).unwrap(), "textures");
    assert_eq!(r.string(32767).unwrap(), "dGV4dHVyZXM=");
    assert!(r.bool().unwrap(), "signed");
    assert_eq!(r.string(1024).unwrap(), "c2lnbmF0dXJl");

    // The server asked about exactly this login.
    let line = request.await.unwrap();
    let hash = server_hash("", &secret, &public_der);
    assert!(
        line.starts_with("GET /session/minecraft/hasJoined?"),
        "{line}"
    );
    assert!(line.contains("username=Bot"), "{line}");
    assert!(line.contains(&format!("serverId={hash}")), "{line}");

    // And the encrypted connection keeps working into configuration.
    bot.send("minecraft:login_acknowledged", Vec::new()).await;
    bot.state = State::Configuration;
    bot.recv_until("minecraft:select_known_packs").await;
    let _ = stop.send(());
}

#[tokio::test]
async fn online_login_refused_when_session_server_does_not_know_the_player() {
    if data_dir().is_none() {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    }
    let (session_server, request) = fake_session_server(http_response("204 No Content", "")).await;
    let (mut bot, _, _, stop) = online_bot(session_server).await;
    let (name, body) = bot.recv().await;
    assert_eq!(name, "minecraft:login_disconnect");
    let reason = Reader::new(&body).string(32767).unwrap();
    assert!(
        reason.contains("multiplayer.disconnect.unverified_username"),
        "{reason}"
    );
    request.await.unwrap();
    let _ = stop.send(());
}

/// Parses a flat-world chunk packet following the documented layout.
fn check_flat_chunk(body: &[u8]) {
    let mut r = Reader::new(body);
    let (_x, _z) = (r.i32().unwrap(), r.i32().unwrap());
    let heightmaps = r.length_prefix().unwrap();
    for _ in 0..heightmaps {
        r.varint().unwrap();
        let longs = r.length_prefix().unwrap();
        assert_eq!(longs, 37, "9-bit heightmap of 256 columns");
        r.bytes(longs * 8).unwrap();
    }
    let data = r.byte_array(1 << 21).unwrap().to_vec();

    let mut sections = Reader::new(&data);
    let mut non_air = Vec::new();
    for _ in 0..24 {
        non_air.push(sections.i16().unwrap());
        assert_eq!(sections.i16().unwrap(), 0, "no fluids");
        for (entries, min_indirect) in [(4096usize, 4u8), (64, 1)] {
            let bits = sections.u8().unwrap();
            if bits == 0 {
                sections.varint().unwrap();
                continue;
            }
            assert!(bits >= min_indirect);
            let palette = sections.length_prefix().unwrap();
            for _ in 0..palette {
                sections.varint().unwrap();
            }
            let per_long = 64 / bits as usize;
            sections.bytes(entries.div_ceil(per_long) * 8).unwrap();
        }
    }
    assert_eq!(sections.remaining(), 0, "sections fill the data exactly");
    // bedrock + 2 dirt + grass = 4 full layers in the bottom section
    assert_eq!(non_air[0], 4 * 256);
    assert!(non_air[1..].iter().all(|&n| n == 0));

    assert_eq!(r.length_prefix().unwrap(), 0, "no block entities");
    // Four light masks, each a byte bit set (26.3).
    let masks: Vec<Vec<u8>> = (0..4)
        .map(|_| r.byte_array(1024).unwrap().to_vec())
        .collect();
    assert_eq!(
        masks[0],
        [0xff, 0xff, 0xff, 0x03],
        "sky data for all 26 sections"
    );
    assert!(masks[1].is_empty() && masks[2].is_empty());
    let sky = r.length_prefix().unwrap();
    assert_eq!(sky, 26, "24 sections + 1 below + 1 above");
    for _ in 0..sky {
        assert_eq!(r.length_prefix().unwrap(), 2048);
        r.bytes(2048).unwrap();
    }
    assert_eq!(r.length_prefix().unwrap(), 0, "no block light arrays");
    assert_eq!(r.remaining(), 0);
}
