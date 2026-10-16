//! End-to-end: a protocol bot pings the server, logs in, configures, spawns,
//! receives chunks and chats. Needs extracted data (`cargo xtask extract-data`);
//! skipped otherwise.
//!
//! The bot is written from the protocol documentation, independently of the
//! server's encoders, so layout mistakes on either side show up as failures.

use std::path::Path;
use std::time::Duration;

use mc_nbt::Tag;
use mc_protocol::frame::{decode_frame_body, split_frame, write_frame};
use mc_protocol::{Direction, PacketIds, Reader, State, Write as _};
use mc_server::{Config, Server, find_data_dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Bot {
    stream: TcpStream,
    buf: Vec<u8>,
    compression: Option<usize>,
    ids: PacketIds,
    state: State,
}

impl Bot {
    async fn connect(addr: std::net::SocketAddr, ids: PacketIds) -> Self {
        Self {
            stream: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
            compression: None,
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
            let n =
                tokio::time::timeout(Duration::from_secs(10), self.stream.read_buf(&mut self.buf))
                    .await
                    .expect("server answered in time")
                    .unwrap();
            assert!(n > 0, "server closed the connection");
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
        29
    );
    assert!(seen.contains(&"minecraft:update_tags".to_owned()));
    bot.send("minecraft:finish_configuration", Vec::new()).await;
    bot.state = State::Play;

    // --- play: spawn and chunks
    let mut teleport_id = None;
    let mut chunks = 0;
    let mut started_waiting = false;
    loop {
        let (name, body) = bot.recv().await;
        match name.as_str() {
            "minecraft:player_position" => teleport_id = Some(Reader::new(&body).varint().unwrap()),
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
    assert_eq!(chunks, 7 * 7, "view distance 3 around spawn");
    let mut confirm = Vec::new();
    confirm.put_varint(teleport_id.expect("spawn teleport"));
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
    for _ in 0..4 {
        let longs = r.length_prefix().unwrap();
        r.bytes(longs * 8).unwrap();
    }
    let sky = r.length_prefix().unwrap();
    assert_eq!(sky, 26, "24 sections + 1 below + 1 above");
    for _ in 0..sky {
        assert_eq!(r.length_prefix().unwrap(), 2048);
        r.bytes(2048).unwrap();
    }
    assert_eq!(r.length_prefix().unwrap(), 0, "no block light arrays");
    assert_eq!(r.remaining(), 0);
}
