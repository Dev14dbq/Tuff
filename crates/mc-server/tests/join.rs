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
    let (mut chunks, mut batched) = (0, 0);
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
            // Nearest chunks come first, in growing batches.
            "minecraft:chunk_batch_finished" => {
                assert_eq!(Reader::new(&body).varint().unwrap(), chunks - batched);
                batched = chunks;
                if chunks == 7 * 7 {
                    break;
                }
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
        // chat.type.text with the sender and the message.
        let text = format!("{tag:?}");
        if text.contains("chat.type.text") && text.contains("hello world") {
            break;
        }
    }

    // --- logging in again kicks the old session (duplicate_login)
    let mut twin = Bot::connect(addr, ids.clone()).await;
    twin.login("Bot").await;
    let (_, body) = bot.recv_until("minecraft:disconnect").await;
    let (tag, _) = mc_nbt::read_network(&body).unwrap();
    assert!(format!("{tag:?}").contains("multiplayer.disconnect.duplicate_login"));
    twin.recv_until("minecraft:login_compression").await;

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

#[tokio::test]
async fn placed_and_broken_blocks_are_seen_by_everyone() {
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
    let ctx = server.context().clone();
    let ids = ctx.ids.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let blocks = &ctx.data.blocks;
    let stone = blocks.default_state("minecraft:stone").unwrap() as i32;
    let air = blocks.default_state("minecraft:air").unwrap() as i32;
    let stone_item = ctx
        .data
        .builtin
        .id("minecraft:item", "minecraft:stone")
        .unwrap();

    let (mut alice, _) = join_offline(addr, ids.clone(), "Alice").await;
    let (mut bob, spawn) = join_offline(addr, ids.clone(), "Bob").await;
    assert_eq!(spawn, (0.5, -60.0, 0.5));
    let (_, spawned) = alice.recv_until("minecraft:add_entity").await;
    let bob_entity = Reader::new(&spawned).varint().unwrap();

    // Bob takes stone from the creative inventory into his first hotbar slot.
    let mut slot = Vec::new();
    slot.put_i16(36);
    slot.put_varint(1);
    slot.put_varint(stone_item);
    slot.put_varint(0);
    slot.put_varint(0);
    bob.send("minecraft:set_creative_mode_slot", slot).await;
    let (_, equipment) = alice.recv_until("minecraft:set_equipment").await;
    let mut r = Reader::new(&equipment);
    assert_eq!(r.varint().unwrap(), bob_entity);
    assert_eq!(r.u8().unwrap(), 0, "main hand, last entry");
    assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (1, stone_item));

    let use_item_on = |x: i32, y: i32, z: i32, sequence: i32| {
        let mut body = Vec::new();
        body.put_varint(0);
        body.put_position(mc_protocol::Position::new(x, y, z));
        body.put_varint(1); // top face
        for c in [0.5f32, 1.0, 0.5] {
            body.put_f32(c);
        }
        body.put_bool(false);
        body.put_bool(false);
        body.put_varint(sequence);
        body
    };
    let block_update = |body: &[u8]| {
        let mut r = Reader::new(body);
        let p = r.position().unwrap();
        ((p.x, p.y, p.z), r.varint().unwrap())
    };

    // Placing on top of the grass two blocks east.
    bob.send("minecraft:use_item_on", use_item_on(2, -61, 0, 1))
        .await;
    let (_, update) = alice.recv_until("minecraft:block_update").await;
    assert_eq!(block_update(&update), ((2, -60, 0), stone));
    let (_, swing) = alice.recv_until("minecraft:swing_animation").await;
    assert_eq!(Reader::new(&swing).varint().unwrap(), bob_entity);
    let mut placer = Vec::new();
    let ack = loop {
        let (name, body) = bob.recv().await;
        match name.as_str() {
            "minecraft:block_update" => placer.push(block_update(&body)),
            "minecraft:block_changed_ack" => break Reader::new(&body).varint().unwrap(),
            _ => {}
        }
    };
    assert_eq!(ack, 1);
    assert!(placer.contains(&((2, -60, 0), stone)), "{placer:?}");

    // Where Bob stands: refused, and he is told the block is still air.
    bob.send("minecraft:use_item_on", use_item_on(0, -61, 0, 2))
        .await;
    let mut refused = Vec::new();
    let ack = loop {
        let (name, body) = bob.recv().await;
        match name.as_str() {
            "minecraft:block_update" => refused.push(block_update(&body)),
            "minecraft:block_changed_ack" => break Reader::new(&body).varint().unwrap(),
            _ => {}
        }
    };
    assert_eq!(ack, 2);
    assert!(refused.contains(&((0, -60, 0), air)), "{refused:?}");

    // Breaking it: Alice sees particles, then air.
    let mut dig = Vec::new();
    dig.put_varint(0); // start digging (instant in creative)
    dig.put_position(mc_protocol::Position::new(2, -60, 0));
    dig.put_i8(1);
    dig.put_varint(3);
    bob.send("minecraft:player_action", dig).await;
    let (_, event) = alice.recv_until("minecraft:level_event").await;
    let mut r = Reader::new(&event);
    assert_eq!(r.i32().unwrap(), 2001);
    let p = r.position().unwrap();
    assert_eq!(((p.x, p.y, p.z), r.i32().unwrap()), ((2, -60, 0), stone));
    let (_, update) = alice.recv_until("minecraft:block_update").await;
    assert_eq!(block_update(&update), ((2, -60, 0), air));

    // One more block; a player who joins later gets it with the chunk.
    bob.send("minecraft:use_item_on", use_item_on(3, -61, 0, 4))
        .await;
    let (_, update) = alice.recv_until("minecraft:block_update").await;
    assert_eq!(block_update(&update), ((3, -60, 0), stone));
    let mut carol = Bot::connect(addr, ids).await;
    carol.login("Carol").await;
    let (_, body) = carol.recv_until("minecraft:login_compression").await;
    carol.compression = Some(Reader::new(&body).varint().unwrap() as usize);
    carol.recv_until("minecraft:login_finished").await;
    carol.send("minecraft:login_acknowledged", Vec::new()).await;
    carol.state = State::Configuration;
    let (_, packs) = carol.recv_until("minecraft:select_known_packs").await;
    carol.send("minecraft:select_known_packs", packs).await;
    carol.recv_until("minecraft:finish_configuration").await;
    carol
        .send("minecraft:finish_configuration", Vec::new())
        .await;
    carol.state = State::Play;
    let chunk = loop {
        let (_, body) = carol.recv_until("minecraft:level_chunk_with_light").await;
        let mut r = Reader::new(&body);
        if (r.i32().unwrap(), r.i32().unwrap()) == (0, 0) {
            break body;
        }
    };
    assert_eq!(block_in_flat_chunk(&chunk, 3, -60, 0), stone);
    assert_eq!(block_in_flat_chunk(&chunk, 2, -60, 0), air);
    assert_eq!(block_in_flat_chunk(&chunk, 2, -61, 0), {
        blocks.default_state("minecraft:grass_block").unwrap() as i32
    });
    let _ = stop.send(());
}

/// Block state at (x, y, z) in the bottom section of a chunk packet, decoded
/// by the documented layout (y must be in -64..-48).
fn block_in_flat_chunk(body: &[u8], x: usize, y: i32, z: usize) -> i32 {
    let mut r = Reader::new(body);
    r.i32().unwrap();
    r.i32().unwrap();
    for _ in 0..r.length_prefix().unwrap() {
        r.varint().unwrap();
        let longs = r.length_prefix().unwrap();
        r.bytes(longs * 8).unwrap();
    }
    let data = r.byte_array(1 << 21).unwrap().to_vec();
    let mut s = Reader::new(&data);
    s.i16().unwrap();
    s.i16().unwrap();
    let bits = s.u8().unwrap();
    if bits == 0 {
        return s.varint().unwrap();
    }
    let palette: Vec<i32> = (0..s.length_prefix().unwrap())
        .map(|_| s.varint().unwrap())
        .collect();
    let index = (((y + 64) as usize) << 8) | (z << 4) | x;
    let per_long = 64 / bits as usize;
    let longs: Vec<u64> = (0..4096usize.div_ceil(per_long))
        .map(|_| s.i64().unwrap() as u64)
        .collect();
    let word = longs[index / per_long];
    let value = (word >> ((index % per_long) * bits as usize)) & ((1 << bits) - 1);
    palette[value as usize]
}

#[tokio::test]
async fn chunks_nobody_has_are_unloaded() {
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
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let (alice, _) = join_offline(addr, ctx.ids.clone(), "Alice").await;
    assert_eq!(ctx.world.loaded_chunks(), 25, "a 5×5 view");
    assert_eq!(ctx.world.unload_unused(), 0, "Alice still has them");
    drop(alice);
    // The server notices the closed connection and forgets her chunks.
    let mut dropped = 0;
    for _ in 0..50 {
        dropped += ctx.world.unload_unused();
        if ctx.world.loaded_chunks() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!((dropped, ctx.world.loaded_chunks()), (25, 0));
    let _ = stop.send(());
}

#[tokio::test]
async fn operators_run_commands_from_chat() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Admin".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut admin, _) = join_offline(addr, ctx.ids.clone(), "Admin").await;
    let (mut guest, _) = join_offline(addr, ctx.ids.clone(), "Guest").await;
    let command = |text: &str| {
        let mut body = Vec::new();
        body.put_string(text);
        body
    };
    let chat_text = |body: &[u8]| format!("{:?}", Reader::new(body).nbt().unwrap());

    guest
        .send("minecraft:chat_command", command("setblock 1 -60 1 stone"))
        .await;
    loop {
        let (_, message) = guest.recv_until("minecraft:system_chat").await;
        // Commands an operator may use do not exist for a guest.
        if chat_text(&message).contains("command.unknown.command") {
            break;
        }
    }

    // Several blocks in one section arrive as one Update Section Blocks.
    admin
        .send(
            "minecraft:chat_command",
            command("fill 0 -60 0 2 -60 0 oak_planks"),
        )
        .await;
    let (_, update) = guest.recv_until("minecraft:section_blocks_update").await;
    let mut r = Reader::new(&update);
    assert_eq!(r.i64().unwrap(), (-4i64) & 0xFFFFF, "section (0, -4, 0)");
    assert_eq!(r.length_prefix().unwrap(), 3);
    let planks = ctx
        .data
        .blocks
        .default_state("minecraft:oak_planks")
        .unwrap() as i64;
    let entry = r.varlong().unwrap();
    assert_eq!(
        (entry >> 12, entry & 0xFFF),
        (planks, 0x004),
        "x 0, z 0, y 4"
    );
    loop {
        let (_, feedback) = admin.recv_until("minecraft:system_chat").await;
        let text = chat_text(&feedback);
        if text.contains("commands.fill.success") && text.contains("Int(3)") {
            break;
        }
    }

    admin
        .send("minecraft:chat_command", command("tick freeze"))
        .await;
    let (_, state) = guest.recv_until("minecraft:ticking_state").await;
    let mut r = Reader::new(&state);
    assert_eq!((r.f32().unwrap(), r.bool().unwrap()), (20.0, true));
    let _ = stop.send(());
}

#[tokio::test]
async fn survival_breaking_drops_items_that_get_picked_up() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Miner".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let cobblestone = ctx
        .data
        .builtin
        .id("minecraft:item", "minecraft:cobblestone")
        .unwrap();
    let item_type = ctx
        .data
        .builtin
        .id("minecraft:entity_type", "minecraft:item")
        .unwrap();
    let (mut miner, _) = join_offline(addr, ctx.ids.clone(), "Miner").await;
    let command = |text: &str| {
        let mut body = Vec::new();
        body.put_string(text);
        body
    };
    miner
        .send("minecraft:chat_command", command("gamemode survival"))
        .await;
    let (_, event) = miner.recv_until("minecraft:game_event").await;
    let mut r = Reader::new(&event);
    assert_eq!(
        (r.u8().unwrap(), r.f32().unwrap()),
        (3, 0.0),
        "now survival"
    );

    miner
        .send("minecraft:chat_command", command("setblock 1 -60 0 stone"))
        .await;
    miner.recv_until("minecraft:block_update").await;

    let mut dig = Vec::new();
    dig.put_varint(2); // finished digging
    dig.put_position(mc_protocol::Position::new(1, -60, 0));
    dig.put_i8(1);
    dig.put_varint(1);
    miner.send("minecraft:player_action", dig).await;

    let (_, spawned) = miner.recv_until("minecraft:add_entity").await;

    let mut r = Reader::new(&spawned);
    let item_entity = r.varint().unwrap();
    r.uuid().unwrap();
    assert_eq!(r.varint().unwrap(), item_type);
    let (_, metadata) = miner.recv_until("minecraft:set_entity_data").await;
    let mut r = Reader::new(&metadata);
    assert_eq!(r.varint().unwrap(), item_entity);
    assert_eq!(
        (r.u8().unwrap(), r.varint().unwrap()),
        (8, 7),
        "index 8, Slot"
    );
    assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (1, cobblestone));

    // The miner walks onto it and picks it up once its 10-tick delay is over.
    let mut slot_seen = None;
    let mut taken_seen = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !(taken_seen && slot_seen.is_some()) {
        assert!(
            std::time::Instant::now() < deadline,
            "no pickup within 10 s"
        );
        let (name, body) = miner.recv().await;
        match name.as_str() {
            "minecraft:keep_alive" => miner.send("minecraft:keep_alive", body).await,
            "minecraft:entity_position_sync" => {
                let mut r = Reader::new(&body);
                if r.varint().unwrap() == item_entity {
                    r.varint().unwrap();
                    let (x, _, z) = (r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap());
                    let mut moved = Vec::new();
                    moved.put_f64(x);
                    moved.put_f64(-60.0);
                    moved.put_f64(z);
                    moved.put_u8(0x01);
                    miner.send("minecraft:move_player_pos", moved).await;
                }
            }
            "minecraft:take_item_entity" => {
                assert_eq!(Reader::new(&body).varint().unwrap(), item_entity);
                taken_seen = true;
            }
            "minecraft:container_set_slot" => {
                let mut r = Reader::new(&body);
                assert_eq!(r.varint().unwrap(), 0, "inventory window");
                r.varint().unwrap(); // state id
                slot_seen = Some((
                    i32::from(r.i16().unwrap()),
                    r.varint().unwrap(),
                    r.varint().unwrap(),
                ));
            }
            _ => {}
        }
    }
    assert_eq!(
        slot_seen,
        Some((36, 1, cobblestone)),
        "one cobblestone in the first hotbar slot"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn players_come_back_where_they_left_with_their_items() {
    let original =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/vanilla-server/26.3/world-normal");
    let (Some(data_dir), true) = (data_dir(), original.join("level.dat").is_file()) else {
        eprintln!("skipped: no extracted data or vanilla test world");
        return;
    };
    let world = copy_world(&original, "players");
    let start = |data_dir: std::path::PathBuf, world: std::path::PathBuf| async move {
        let mut config = Config::new(data_dir);
        config.port = 0;
        config.view_distance = 2;
        config.online_mode = false;
        config.world = Some(world);
        config.ops = vec!["Walker".into()];
        let server = Server::bind(config).await.unwrap();
        let addr = server.local_addr();
        let ctx = server.context().clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let running = tokio::spawn(server.run(async {
            let _ = stopped.await;
        }));
        (addr, ctx, stop, running)
    };

    let (addr, ctx, stop, running) = start(data_dir.clone(), world.clone()).await;
    let stone = ctx
        .data
        .builtin
        .id("minecraft:item", "minecraft:stone")
        .unwrap();
    let (mut walker, spawn) = join_offline(addr, ctx.ids.clone(), "Walker").await;
    // The world's default game mode is survival: no creative slots. Put the
    // stone in through the server, then walk off.
    ctx.inventories
        .lock()
        .unwrap()
        .get_mut(&mc_server_uuid(&ctx, "Walker"))
        .unwrap()
        .slots[38] = Some(mc_protocol::packets::play::ItemStack {
        count: 7,
        item: stone,
        has_component_changes: false,
    });
    let target = (spawn.0 + 3.0, spawn.1, spawn.2 - 1.5);
    let mut moved = Vec::new();
    for v in [target.0, target.1, target.2] {
        moved.put_f64(v);
    }
    moved.put_f32(45.0);
    moved.put_f32(10.0);
    moved.put_u8(0x01);
    walker.send("minecraft:move_player_pos_rot", moved).await;
    // Creative, then take off: the flight is kept too.
    let mut command = Vec::new();
    command.put_string("gamemode creative");
    walker.send("minecraft:chat_command", command).await;
    walker.recv_until("minecraft:player_abilities").await;
    walker.send("minecraft:player_abilities", vec![0x02]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(walker);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let _ = stop.send(());
    running.await.unwrap().unwrap();

    // Next session: same place, same items.
    let (addr, ctx, stop, running) = start(data_dir, world.clone()).await;
    let mut bot = Bot::connect(addr, ctx.ids.clone()).await;
    bot.login("Walker").await;
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
    let (_, abilities) = bot.recv_until("minecraft:player_abilities").await;
    assert_eq!(abilities[0], 0x0F, "creative and still flying");
    let (_, position) = bot.recv_until("minecraft:player_position").await;
    let mut r = Reader::new(&position);
    r.varint().unwrap();
    assert_eq!(
        (r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap()),
        target
    );
    let (_, content) = bot.recv_until("minecraft:container_set_content").await;
    let mut r = Reader::new(&content);
    assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (0, 1));
    assert_eq!(r.length_prefix().unwrap(), 46);
    for slot in 0..46 {
        let count = r.varint().unwrap();
        if slot == 38 {
            assert_eq!(
                (count, r.varint().unwrap()),
                (7, stone),
                "stone in hotbar slot 3"
            );
            r.varint().unwrap();
            r.varint().unwrap();
        } else {
            assert_eq!(count, 0, "slot {slot} empty");
        }
    }
    drop(bot);
    let _ = stop.send(());
    running.await.unwrap().unwrap();
    std::fs::remove_dir_all(&world).unwrap();
}

/// The offline-mode UUID the server gives a name.
fn mc_server_uuid(ctx: &mc_server::ServerContext, name: &str) -> u128 {
    ctx.players
        .lock()
        .unwrap()
        .values()
        .find(|p| p.profile.name == name)
        .map(|p| p.profile.uuid)
        .unwrap()
}

#[tokio::test]
async fn chests_keep_what_players_put_in() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Keeper".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let torch = ctx
        .data
        .builtin
        .id("minecraft:item", "minecraft:torch")
        .unwrap();
    let (mut keeper, _) = join_offline(addr, ctx.ids.clone(), "Keeper").await;
    let mut command = Vec::new();
    command.put_string("setblock 2 -60 0 chest");
    keeper.send("minecraft:chat_command", command).await;
    keeper.recv_until("minecraft:block_update").await;

    // Creative: 12 torches into the first hotbar slot, then open the chest.
    let mut slot = Vec::new();
    slot.put_i16(36);
    for v in [12, torch, 0, 0] {
        slot.put_varint(v);
    }
    keeper.send("minecraft:set_creative_mode_slot", slot).await;
    let mut use_on = Vec::new();
    use_on.put_varint(0);
    use_on.put_position(mc_protocol::Position::new(2, -60, 0));
    use_on.put_varint(1);
    for c in [0.5f32, 1.0, 0.5] {
        use_on.put_f32(c);
    }
    use_on.put_bool(false);
    use_on.put_bool(false);
    use_on.put_varint(1);
    keeper.send("minecraft:use_item_on", use_on).await;
    let (_, screen) = keeper.recv_until("minecraft:open_screen").await;
    let mut r = Reader::new(&screen);
    let window = r.varint().unwrap();
    let menu = ctx
        .data
        .builtin
        .id("minecraft:menu", "minecraft:generic_9x3")
        .unwrap();
    assert_eq!(r.varint().unwrap(), menu);
    let (_, content) = keeper.recv_until("minecraft:container_set_content").await;
    let mut r = Reader::new(&content);
    assert_eq!(r.varint().unwrap(), window);
    r.varint().unwrap();
    assert_eq!(
        r.length_prefix().unwrap(),
        27 + 36,
        "chest, then the inventory"
    );

    // Shift-click the torches over: hotbar slot 0 is window slot 27 + 27.
    let hashed = |item: i32, count: i32| {
        let mut b = Vec::new();
        b.put_bool(true);
        b.put_varint(item);
        b.put_varint(count);
        b.put_varint(0);
        b.put_varint(0);
        b
    };
    let mut click = Vec::new();
    click.put_varint(window);
    click.put_varint(2);
    click.put_i16(54);
    click.put_i8(0);
    click.put_varint(1); // shift-click
    click.put_varint(2);
    click.put_i16(54);
    click.put_bool(false);
    click.put_i16(0);
    click.extend(hashed(torch, 12));
    click.put_bool(false); // nothing carried
    keeper.send("minecraft:container_click", click).await;
    let mut close = Vec::new();
    close.put_varint(window);
    keeper.send("minecraft:container_close", close).await;
    // Wait until the server has it.
    for _ in 0..50 {
        if ctx.containers.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let be = ctx
        .world
        .block_entity(mc_protocol::Position::new(2, -60, 0), &ctx.data.blocks)
        .expect("chest block entity");
    assert_eq!(be.id, "minecraft:chest");
    let Some(Tag::List(items)) = be.data.get("Items") else {
        panic!("no Items: {:?}", be.data)
    };
    let Tag::Compound(first) = &items[0] else {
        panic!()
    };
    assert_eq!(first.get("Slot"), Some(&Tag::Byte(0)));
    assert_eq!(
        first.get("id"),
        Some(&Tag::String("minecraft:torch".into()))
    );
    assert_eq!(first.get("count"), Some(&Tag::Int(12)));
    assert!(
        ctx.inventories
            .lock()
            .unwrap()
            .values()
            .all(|h| h.slots[36].is_none())
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn the_inventory_grid_crafts() {
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
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let log = ctx
        .data
        .builtin
        .id("minecraft:item", "minecraft:oak_log")
        .unwrap();
    let planks = ctx
        .data
        .builtin
        .id("minecraft:item", "minecraft:oak_planks")
        .unwrap();
    let (mut crafter, _) = join_offline(addr, ctx.ids.clone(), "Crafter").await;
    let hashed = |b: &mut Vec<u8>, item: Option<(i32, i32)>| match item {
        None => b.put_bool(false),
        Some((item, count)) => {
            b.put_bool(true);
            b.put_varint(item);
            b.put_varint(count);
            b.put_varint(0);
            b.put_varint(0);
        }
    };
    let click = |slot: i16, changed: &[(i16, Option<(i32, i32)>)], carried: Option<(i32, i32)>| {
        let mut b = Vec::new();
        b.put_varint(0);
        b.put_varint(1);
        b.put_i16(slot);
        b.put_i8(0);
        b.put_varint(0);
        b.put_varint(changed.len() as i32);
        for (s, item) in changed {
            b.put_i16(*s);
            hashed(&mut b, *item);
        }
        hashed(&mut b, carried);
        b
    };
    // A log goes into the first grid slot: the result shows four planks.
    crafter
        .send(
            "minecraft:container_click",
            click(1, &[(1, Some((log, 1)))], None),
        )
        .await;
    let (_, result) = crafter.recv_until("minecraft:container_set_slot").await;
    let mut r = Reader::new(&result);
    assert_eq!(r.varint().unwrap(), 0);
    r.varint().unwrap();
    assert_eq!(r.i16().unwrap(), 0, "result slot");
    assert_eq!((r.varint().unwrap(), r.varint().unwrap()), (4, planks));
    // Taking the result empties the grid and the result.
    crafter
        .send(
            "minecraft:container_click",
            click(0, &[(0, None), (1, None)], Some((planks, 4))),
        )
        .await;
    for _ in 0..50 {
        let carried = ctx
            .inventories
            .lock()
            .unwrap()
            .values()
            .next()
            .and_then(|h| h.carried.clone());
        if carried.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let hands = ctx
        .inventories
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    assert_eq!(hands.carried.map(|c| (c.item, c.count)), Some((planks, 4)));
    assert!(hands.slots[0].is_none() && hands.slots[1].is_none());
    let _ = stop.send(());
}

#[tokio::test]
async fn falls_hurt_and_kill_and_players_respawn() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Jumper".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut jumper, spawn) = join_offline(addr, ctx.ids.clone(), "Jumper").await;
    let mut command = Vec::new();
    command.put_string("gamemode survival");
    jumper.send("minecraft:chat_command", command).await;
    jumper.recv_until("minecraft:game_event").await;
    let fall = |from: f64, to: f64| {
        let mut packets = Vec::new();
        let mut y = from;
        while y > to {
            let mut b = Vec::new();
            b.put_f64(spawn.0);
            b.put_f64(y);
            b.put_f64(spawn.2);
            b.put_u8(0);
            packets.push(b);
            y -= 1.0;
        }
        let mut landed = Vec::new();
        landed.put_f64(spawn.0);
        landed.put_f64(to);
        landed.put_f64(spawn.2);
        landed.put_u8(1);
        packets.push(landed);
        packets
    };
    let health_after = |body: &[u8]| Reader::new(body).f32().unwrap();
    // 20 blocks: 17 damage.
    for b in fall(spawn.1 + 20.0, spawn.1) {
        jumper.send("minecraft:move_player_pos", b).await;
    }
    let (_, health) = jumper.recv_until("minecraft:set_health").await;
    assert_eq!(health_after(&health), 3.0);
    let (_, hurt) = jumper.recv_until("minecraft:damage_event").await;
    let fall_type = ctx.damage_type("minecraft:fall");
    let mut r = Reader::new(&hurt);
    r.varint().unwrap();
    assert_eq!(r.varint().unwrap(), fall_type);
    // Wait out the 10 immune ticks, then fall again and die.
    tokio::time::sleep(Duration::from_millis(600)).await;
    for b in fall(spawn.1 + 10.0, spawn.1) {
        jumper.send("minecraft:move_player_pos", b).await;
    }
    let (_, death) = jumper.recv_until("minecraft:player_combat_kill").await;
    let mut r = Reader::new(&death);
    r.varint().unwrap();
    let message = format!("{:?}", r.nbt().unwrap());
    assert!(message.contains("death.attack.fall"), "{message}");
    // Respawn.
    let mut respawn = Vec::new();
    respawn.put_varint(0);
    jumper.send("minecraft:client_command", respawn).await;
    jumper.recv_until("minecraft:respawn").await;
    let (_, health) = jumper.recv_until("minecraft:set_health").await;
    assert_eq!(health_after(&health), 20.0);
    let _ = stop.send(());
}

#[tokio::test]
async fn players_hurt_each_other_and_give_works() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Hitter".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut hitter, _) = join_offline(addr, ctx.ids.clone(), "Hitter").await;
    let (mut target, _) = join_offline(addr, ctx.ids.clone(), "Target").await;
    let command = |text: &str| {
        let mut b = Vec::new();
        b.put_string(text);
        b
    };
    hitter
        .send(
            "minecraft:chat_command",
            command("gamemode survival Target"),
        )
        .await;
    target.recv_until("minecraft:game_event").await;
    hitter
        .send("minecraft:chat_command", command("give Hitter stone_sword"))
        .await;
    let (_, slot) = hitter.recv_until("minecraft:container_set_slot").await;
    let mut r = Reader::new(&slot);
    r.varint().unwrap();
    r.varint().unwrap();
    assert_eq!(r.i16().unwrap(), 36, "the sword lands in the hotbar");
    let target_entity = ctx
        .players
        .lock()
        .unwrap()
        .values()
        .find(|p| p.profile.name == "Target")
        .unwrap()
        .entity_id;
    let mut attack = Vec::new();
    attack.put_varint(target_entity);
    hitter.send("minecraft:attack", attack).await;
    let (_, health) = target.recv_until("minecraft:set_health").await;
    assert_eq!(
        Reader::new(&health).f32().unwrap(),
        15.0,
        "a stone sword does 5"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn serves_a_world_saved_by_vanilla() {
    let world =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/vanilla-server/26.3/world-normal");
    let (Some(data_dir), true) = (data_dir(), world.join("level.dat").is_file()) else {
        eprintln!("skipped: no extracted data or vanilla test world");
        return;
    };
    // The server saves when it stops: serve a copy, never the reference world.
    let world = copy_world(&world, "serve");
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.world = Some(world.clone());
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ids = server.context().ids.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let mut bot = Bot::connect(addr, ids).await;
    bot.login("Explorer").await;
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

    let (_, login) = bot.recv_until("minecraft:login").await;
    let (_, position) = bot.recv_until("minecraft:player_position").await;
    let mut r = Reader::new(&position);
    r.varint().unwrap();
    let spawn_y = {
        r.f64().unwrap();
        r.f64().unwrap()
    };
    assert!(
        spawn_y > 50.0,
        "spawn on the terrain from level.dat, got y={spawn_y}"
    );
    assert!(!login.is_empty());

    // Real terrain: the bottom sections are mostly stone, not a superflat.
    let mut solid_bottoms = 0;
    loop {
        let (name, body) = bot.recv().await;
        if name == "minecraft:chunk_batch_finished" {
            break;
        }
        if name != "minecraft:level_chunk_with_light" {
            continue;
        }
        let mut r = Reader::new(&body);
        r.i32().unwrap();
        r.i32().unwrap();
        for _ in 0..r.length_prefix().unwrap() {
            r.varint().unwrap();
            let longs = r.length_prefix().unwrap();
            r.bytes(longs * 8).unwrap();
        }
        let data = r.byte_array(1 << 21).unwrap().to_vec();
        let non_air = i16::from_be_bytes([data[0], data[1]]);
        solid_bottoms += usize::from(non_air > 3000);
    }
    assert!(
        solid_bottoms >= 20,
        "{solid_bottoms} chunks with a solid bottom section"
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn changes_to_a_vanilla_world_are_saved_on_shutdown() {
    let original =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/vanilla-server/26.3/world-normal");
    let (Some(data_dir), true) = (data_dir(), original.join("level.dat").is_file()) else {
        eprintln!("skipped: no extracted data or vanilla test world");
        return;
    };
    let world = copy_world(&original, "save");
    let regions = world.join("dimensions/minecraft/overworld/region");

    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.world = Some(world.clone());
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let (mut bot, (x, y, z)) = join_offline(addr, ctx.ids.clone(), "Digger").await;
    let below =
        mc_protocol::Position::new(x.floor() as i32, y.floor() as i32 - 1, z.floor() as i32);
    let mut dig = Vec::new();
    dig.put_varint(2); // finished digging (the world's game mode is survival)
    dig.put_position(below);
    dig.put_i8(1);
    dig.put_varint(1);
    bot.send("minecraft:player_action", dig).await;
    let (_, ack) = bot.recv_until("minecraft:block_changed_ack").await;
    assert_eq!(Reader::new(&ack).varint().unwrap(), 1);
    drop(bot);
    let _ = stop.send(());
    running.await.unwrap().unwrap();

    // Read the region file back, independently of the running server.
    let mut storage = mc_storage::RegionStorage::new(&regions);
    let tag = storage
        .read_chunk(below.x >> 4, below.z >> 4)
        .unwrap()
        .unwrap();
    let Tag::Compound(root) = &tag else {
        panic!("chunk root is not a compound")
    };
    assert_eq!(
        root.get("isLightOn"),
        Some(&Tag::Byte(0)),
        "relight on load"
    );
    assert_eq!(
        root.get("Status"),
        Some(&Tag::String("minecraft:full".into()))
    );
    let biomes: Vec<String> = ctx
        .data
        .data_pack()
        .registry_entries("minecraft:worldgen/biome")
        .unwrap();
    let biome = |name: &str| biomes.iter().position(|b| b == name).map(|i| i as u32);
    let air = ctx.data.blocks.default_state("minecraft:air").unwrap();
    let palettes = mc_storage::Palettes {
        blocks: &ctx.data.blocks,
        biome: &biome,
        air,
        fallback_biome: 0,
        unresolved: Default::default(),
    };
    let mc_storage::Decoded::Chunk(chunk) =
        mc_storage::decode_chunk(&tag, -64, 384, &palettes).unwrap()
    else {
        panic!("saved chunk is not full")
    };
    assert_eq!(
        chunk.block((below.x & 15) as usize, below.y, (below.z & 15) as usize),
        air,
        "the dug block is air on disk"
    );
    assert_eq!(palettes.unresolved.get(), 0);

    // The time went on and was written back, the old level.dat kept.
    let level_time = |path: &Path| {
        let raw = std::fs::read(path).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[..]), &mut bytes)
            .unwrap();
        let (_, Tag::Compound(root), _) = mc_nbt::read_named(&bytes).unwrap() else {
            panic!("level.dat root")
        };
        let Some(Tag::Compound(data)) = root.get("Data") else {
            panic!("level.dat Data")
        };
        match data.get("Time") {
            Some(Tag::Long(t)) => *t,
            other => panic!("Time is {other:?}"),
        }
    };
    let before = level_time(&original.join("level.dat"));
    let after = level_time(&world.join("level.dat"));
    assert!(after > before, "time advanced: {before} -> {after}");
    assert_eq!(level_time(&world.join("level.dat_old")), before);
    std::fs::remove_dir_all(&world).unwrap();
}

/// Copies what the server reads and writes of a world folder (level.dat, data/
/// and the overworld regions) into a fresh temporary folder.
fn copy_world(original: &Path, tag: &str) -> std::path::PathBuf {
    let world = std::env::temp_dir().join(format!("mc-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&world);
    for sub in ["dimensions/minecraft/overworld/region", "data/minecraft"] {
        let (from, to) = (original.join(sub), world.join(sub));
        std::fs::create_dir_all(&to).unwrap();
        for entry in std::fs::read_dir(&from).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                std::fs::copy(&path, to.join(path.file_name().unwrap())).unwrap();
            }
        }
    }
    std::fs::copy(original.join("level.dat"), world.join("level.dat")).unwrap();
    world
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
    // Only the bottom section has sky light other than open sky (clients
    // read sections without data above the last one as open sky).
    assert_eq!(masks[0], [0x02], "sky data for the bottom section");
    assert!(masks[1].is_empty() && masks[2].is_empty());
    assert_eq!(r.length_prefix().unwrap(), 1);
    let sky = r.byte_array(2048).unwrap();
    let level = |y: usize| sky[(y << 8) / 2] & 15;
    assert_eq!(
        (0..16).map(level).collect::<Vec<_>>(),
        [0, 0, 0, 0, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15],
        "dark in the ground, open sky above it"
    );
    assert_eq!(r.length_prefix().unwrap(), 0, "no block light arrays");
    assert_eq!(r.remaining(), 0);
}

/// Reads Update Light: chunk x, z and the block light arrays by section bit.
fn read_block_light(body: &[u8]) -> (i32, i32, Vec<(usize, Vec<u8>)>) {
    let mut r = Reader::new(body);
    let (x, z) = (r.varint().unwrap(), r.varint().unwrap());
    let mut masks = Vec::new();
    for _ in 0..4 {
        let n = r.length_prefix().unwrap();
        masks.push(r.bytes(n).unwrap().to_vec());
    }
    let bits = |mask: &[u8]| -> Vec<usize> {
        (0..mask.len() * 8)
            .filter(|i| mask[i / 8] & (1 << (i % 8)) != 0)
            .collect()
    };
    let sky = r.length_prefix().unwrap();
    for _ in 0..sky {
        r.byte_array(2048).unwrap();
    }
    let count = r.length_prefix().unwrap();
    let block = (0..count)
        .map(|_| r.byte_array(2048).unwrap().to_vec())
        .collect::<Vec<_>>();
    (x, z, bits(&masks[1]).into_iter().zip(block).collect())
}

#[tokio::test]
async fn light_sources_light_up_around_them() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Admin".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut admin, _) = join_offline(addr, ctx.ids.clone(), "Admin").await;
    let mut body = Vec::new();
    body.put_string("setblock 3 -60 3 glowstone");
    admin.send("minecraft:chat_command", body).await;
    let (_, update) = admin.recv_until("minecraft:light_update").await;
    let (x, z, arrays) = read_block_light(&update);
    assert_eq!((x, z), (0, 0));
    // y -60 is section -4: bit 1 (bit 0 is below the world), local y 4.
    let (_, array) = arrays
        .iter()
        .find(|(bit, _)| *bit == 1)
        .expect("section -4");
    let level = |x: usize, y: usize, z: usize| {
        let i = (y << 8) | (z << 4) | x;
        (array[i / 2] >> (4 * (i % 2))) & 15
    };
    assert_eq!(level(3, 4, 3), 15, "glowstone gives 15");
    assert_eq!(level(4, 4, 3), 14);
    assert_eq!(level(3, 5, 3), 14);
    assert_eq!(level(6, 4, 3), 12);
    assert_eq!(level(3, 3, 3), 0, "the grass under it stays dark");

    let mut body = Vec::new();
    body.put_string("setblock 3 -60 3 air");
    admin.send("minecraft:chat_command", body).await;
    // The first glowstone also lit the chunks next to it: skip those.
    let mut dark = false;
    for _ in 0..8 {
        let (_, update) = admin.recv_until("minecraft:light_update").await;
        let (x, z, arrays) = read_block_light(&update);
        if (x, z) == (0, 0) {
            dark = arrays
                .iter()
                .find(|(bit, _)| *bit == 1)
                .is_none_or(|(_, a)| a.iter().all(|&b| b == 0));
            if dark {
                break;
            }
        }
    }
    assert!(dark, "the light goes when the glowstone does");
    let _ = stop.send(());
}

#[tokio::test]
async fn hungry_players_eat_food() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Eater".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut eater, _) = join_offline(addr, ctx.ids.clone(), "Eater").await;
    for line in ["gamemode survival", "give Eater bread 2"] {
        let mut command = Vec::new();
        command.put_string(line);
        eater.send("minecraft:chat_command", command).await;
    }
    eater.recv_until("minecraft:container_set_slot").await;
    let uuid = mc_server_uuid(&ctx, "Eater");
    ctx.vitals.lock().unwrap().get_mut(&uuid).unwrap().food = 10;

    let mut use_item = Vec::new();
    use_item.put_varint(0); // main hand
    use_item.put_varint(1);
    use_item.put_f32(0.0);
    use_item.put_f32(0.0);
    let started = std::time::Instant::now();
    eater.send("minecraft:use_item", use_item).await;
    // The stack shrinks, then food and saturation go up.
    let (_, slot) = eater.recv_until("minecraft:container_set_slot").await;
    assert!(
        started.elapsed() >= Duration::from_millis(1400),
        "eating takes 1.6 s"
    );
    let mut r = Reader::new(&slot);
    r.varint().unwrap();
    r.varint().unwrap();
    assert_eq!(r.i16().unwrap(), 36, "first hotbar slot");
    assert_eq!(r.varint().unwrap(), 1, "one bread left");
    let (_, health) = eater.recv_until("minecraft:set_health").await;
    let mut r = Reader::new(&health);
    let (_, food, saturation) = (r.f32().unwrap(), r.varint().unwrap(), r.f32().unwrap());
    assert_eq!(food, 15, "bread gives 5");
    assert!((saturation - 11.0).abs() < 1e-4, "5 + 6 = {saturation}");
    eater.recv_until("minecraft:entity_event").await;

    // Full players cannot eat bread: nothing happens.
    ctx.vitals.lock().unwrap().get_mut(&uuid).unwrap().food = 20;
    let mut use_item = Vec::new();
    use_item.put_varint(0);
    use_item.put_varint(2);
    use_item.put_f32(0.0);
    use_item.put_f32(0.0);
    eater.send("minecraft:use_item", use_item).await;
    tokio::time::sleep(Duration::from_millis(1800)).await;
    let left = ctx.inventories.lock().unwrap().get(&uuid).unwrap().slots[36]
        .as_ref()
        .map(|s| s.count);
    assert_eq!(left, Some(1));
    let _ = stop.send(());
}

#[tokio::test]
async fn furnaces_smelt_with_fuel() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Cook".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut cook, _) = join_offline(addr, ctx.ids.clone(), "Cook").await;
    for line in [
        "tick rate 400",
        r#"setblock 2 -60 2 furnace{Items:[{Slot:0b,id:"minecraft:raw_iron",count:2},{Slot:1b,id:"minecraft:coal",count:1}]}"#,
    ] {
        let mut command = Vec::new();
        command.put_string(line);
        cook.send("minecraft:chat_command", command).await;
    }
    // The furnace lights up.
    let lit = loop {
        let (_, update) = cook.recv_until("minecraft:block_update").await;
        let mut r = Reader::new(&update);
        let p = r.position().unwrap();
        let state = r.varint().unwrap() as u32;
        if p == mc_protocol::Position::new(2, -60, 2)
            && ctx.data.blocks.property(state, "lit") == Some("true")
        {
            break state;
        }
    };
    assert_eq!(ctx.data.blocks.block_name(lit), Some("minecraft:furnace"));
    // 400 ticks at 400 a second: both ores smelt, a coal lasts 1600 ticks.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let be = ctx
        .world
        .block_entity(mc_protocol::Position::new(2, -60, 2), &ctx.data.blocks)
        .unwrap();
    let text = format!("{:?}", be.data.get("Items"));
    assert!(
        text.contains("iron_ingot") && text.contains("Int(2)"),
        "{text}"
    );
    assert!(!text.contains("raw_iron"), "{text}");
    let _ = stop.send(());
}

#[tokio::test]
async fn two_chests_open_as_a_large_chest() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Hoarder".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut hoarder, _) = join_offline(addr, ctx.ids.clone(), "Hoarder").await;
    // A left half west of a single chest: they join, the single one as right.
    for line in [
        "setblock 3 -60 0 chest[facing=north]",
        "setblock 2 -60 0 chest[facing=north,type=left]",
        "item replace block 3 -60 0 container.0 with diamond 5",
        "item replace block 2 -60 0 container.0 with emerald 7",
    ] {
        let mut command = Vec::new();
        command.put_string(line);
        hoarder.send("minecraft:chat_command", command).await;
    }
    loop {
        let (_, chat) = hoarder.recv_until("minecraft:system_chat").await;
        if format!("{:?}", Reader::new(&chat).nbt().unwrap()).contains("emerald") {
            break;
        }
    }
    let right = ctx
        .world
        .block(mc_protocol::Position::new(3, -60, 0), &ctx.data.blocks)
        .unwrap();
    assert_eq!(ctx.data.blocks.property(right, "type"), Some("right"));

    let mut use_on = Vec::new();
    use_on.put_varint(0);
    use_on.put_position(mc_protocol::Position::new(2, -60, 0));
    use_on.put_varint(1);
    for c in [0.5f32, 1.0, 0.5] {
        use_on.put_f32(c);
    }
    use_on.put_bool(false);
    use_on.put_bool(false);
    use_on.put_varint(1);
    hoarder.send("minecraft:use_item_on", use_on).await;
    let (_, screen) = hoarder.recv_until("minecraft:open_screen").await;
    let mut r = Reader::new(&screen);
    r.varint().unwrap();
    let large = ctx
        .data
        .builtin
        .id("minecraft:menu", "minecraft:generic_9x6")
        .unwrap();
    assert_eq!(r.varint().unwrap(), large);
    let (_, content) = hoarder.recv_until("minecraft:container_set_content").await;
    let mut r = Reader::new(&content);
    r.varint().unwrap();
    r.varint().unwrap();
    assert_eq!(r.length_prefix().unwrap(), 54 + 36);
    // The right half comes first: its diamonds in slot 0, the emeralds in 27.
    let diamond = ctx.items.id("minecraft:diamond").unwrap();
    let emerald = ctx.items.id("minecraft:emerald").unwrap();
    let mut seen = Vec::new();
    for _ in 0..54 {
        let count = r.varint().unwrap();
        if count > 0 {
            let item = r.varint().unwrap();
            let (add, remove) = (r.varint().unwrap(), r.varint().unwrap());
            assert_eq!((add, remove), (0, 0));
            seen.push((count, item));
        } else {
            seen.push((0, 0));
        }
    }
    assert_eq!(seen[0], (5, diamond));
    assert_eq!(seen[27], (7, emerald));
    let _ = stop.send(());
}

#[tokio::test]
async fn a_new_world_is_generated_served_and_saved() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("mc-rewrite-new-world-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = Config::new(data_dir.clone());
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.world = Some(dir.clone());
    config.seed = Some(12345);
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    assert!(dir.join("level.dat").is_file(), "the new world is written");
    assert!(dir.join("data/minecraft/world_gen_settings.dat").is_file());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut player, spawn) = join_offline(addr, ctx.ids.clone(), "Explorer").await;
    // Standing on land above the sea.
    let below = mc_protocol::Position::new(
        spawn.0.floor() as i32,
        spawn.1.floor() as i32 - 1,
        spawn.2.floor() as i32,
    );
    let ground = ctx.world.block(below, &ctx.data.blocks).unwrap();
    assert!(!ctx.data.blocks.is_air(ground) && !ctx.data.blocks.is_fluid(ground));
    assert!(spawn.1 >= 64.0, "spawn {spawn:?}");
    let _ = &mut player;
    // Bedrock at the bottom, deepslate deep down.
    let bottom = ctx
        .world
        .block(
            mc_protocol::Position::new(below.x, -64, below.z),
            &ctx.data.blocks,
        )
        .unwrap();
    assert_eq!(
        ctx.data.blocks.block_name(bottom),
        Some("minecraft:bedrock")
    );
    // Features are there: something grows or lies on the ground around.
    let blocks = &ctx.data.blocks;
    let (cx, cz) = (below.x >> 4, below.z >> 4);
    let mut decorated = 0;
    for x in (cx - 1) * 16..(cx + 2) * 16 {
        for z in (cz - 1) * 16..(cz + 2) * 16 {
            for y in 40..140 {
                let s = ctx
                    .world
                    .block(mc_protocol::Position::new(x, y, z), blocks)
                    .unwrap();
                let name = blocks.block_name(s).unwrap_or("");
                if name.ends_with("_log")
                    || name.ends_with("_leaves")
                    || name.ends_with("_ore")
                    || name.contains("grass")
                    || name.contains("seagrass")
                    || name.contains("kelp")
                {
                    decorated += 1;
                }
            }
        }
    }
    assert!(decorated > 50, "only {decorated} decorated blocks");
    // Light at rest across chunk edges: no air darker than a neighbour by
    // more than one.
    for x in cx * 16 - 1..=(cx + 1) * 16 {
        for z in cz * 16 - 1..=(cz + 1) * 16 {
            for y in 40..140 {
                let p = mc_protocol::Position::new(x, y, z);
                if !blocks.is_air(ctx.world.block(p, blocks).unwrap()) {
                    continue;
                }
                let here = ctx.world.brightness(p);
                for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let there = ctx
                        .world
                        .brightness(mc_protocol::Position::new(x + dx, y, z + dz));
                    assert!(
                        i32::from(here) >= i32::from(there) - 1,
                        "dark spot at {x} {y} {z}: {here} next to {there}"
                    );
                }
            }
        }
    }
    let _ = stop.send(());
    let _ = running.await;
    // The generated chunks are on disk now.
    let region = dir.join("dimensions/minecraft/overworld/region");
    let files = std::fs::read_dir(&region).unwrap().count();
    assert!(files > 0, "region files in {}", region.display());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn whitelist_and_bans_keep_players_out_as_vanilla() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("mc-rewrite-lists-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("banned-players.json"),
        r#"[{"name": "Villain", "created": "2026-01-01 00:00:00 +0000", "source": "Server", "expires": "forever", "reason": "Griefing"}]"#,
    )
    .unwrap();
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.server_dir = Some(dir.clone());
    config.white_list = true;
    config.ops = vec!["Boss".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ids = server.context().ids.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let refused = |name: &'static str| {
        let ids = ids.clone();
        async move {
            let mut bot = Bot::connect(addr, ids).await;
            bot.login(name).await;
            loop {
                let (packet, body) = bot.recv().await;
                match packet.as_str() {
                    "minecraft:login_disconnect" => {
                        return Some(Reader::new(&body).string(32767).unwrap());
                    }
                    "minecraft:login_compression" => {
                        bot.compression = Some(Reader::new(&body).varint().unwrap() as usize);
                    }
                    "minecraft:login_finished" => return None,
                    _ => {}
                }
            }
        }
    };
    let stranger = refused("Stranger").await.expect("strangers are refused");
    assert!(
        stranger.contains("multiplayer.disconnect.not_whitelisted"),
        "{stranger}"
    );
    let villain = refused("Villain")
        .await
        .expect("banned players are refused");
    assert!(
        villain.contains("multiplayer.disconnect.banned.reason") && villain.contains("Griefing"),
        "{villain}"
    );
    assert!(
        refused("Boss").await.is_none(),
        "operators pass the whitelist"
    );
    // The files are there, in vanilla's format.
    for f in [
        "ops.json",
        "whitelist.json",
        "banned-players.json",
        "banned-ips.json",
        "usercache.json",
    ] {
        assert!(dir.join(f).is_file(), "{f}");
    }
    let ops: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("ops.json")).unwrap()).unwrap();
    assert_eq!(ops[0]["name"], "Boss");
    assert_eq!(ops[0]["level"], 4);
    let _ = stop.send(());
    let _ = running.await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn game_mode_switcher_pick_block_and_signs() {
    let Some(data_dir) = data_dir() else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Admin".into()];
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    let (mut admin, at) = join_offline(addr, ctx.ids.clone(), "Admin").await;
    let chat_text = |body: &[u8]| format!("{:?}", Reader::new(body).nbt().unwrap());

    // F3+F4: as `/gamemode`.
    for mode in [0, 1] {
        let mut body = Vec::new();
        body.put_varint(mode);
        admin.send("minecraft:change_game_mode", body).await;
        loop {
            let (_, message) = admin.recv_until("minecraft:system_chat").await;
            if chat_text(&message).contains("commands.gamemode.success.self") {
                break;
            }
        }
    }

    // Middle click on a wall torch next to the player gives a torch.
    let (x, y, z) = (
        at.0.floor() as i32 + 1,
        at.1.floor() as i32,
        at.2.floor() as i32,
    );
    let mut command = Vec::new();
    command.put_string(&format!("setblock {x} {y} {z} wall_torch[facing=east]"));
    admin.send("minecraft:chat_command", command).await;
    admin.recv_until("minecraft:block_update").await;
    let mut pick = Vec::new();
    pick.put_position(mc_protocol::Position::new(x, y, z));
    pick.put_bool(false);
    admin.send("minecraft:pick_item_from_block", pick).await;
    let (_, slot) = admin.recv_until("minecraft:container_set_slot").await;
    let mut r = Reader::new(&slot);
    assert_eq!(r.varint().unwrap(), 0, "the inventory");
    r.varint().unwrap();
    assert_eq!(r.i16().unwrap(), 36, "the selected hotbar slot");
    assert_eq!(r.varint().unwrap(), 1);
    assert_eq!(
        r.varint().unwrap(),
        ctx.items.id("minecraft:torch").unwrap()
    );

    // What is written on a sign is kept and shown.
    let mut command = Vec::new();
    command.put_string(&format!("setblock {x} {y} {z} oak_sign"));
    admin.send("minecraft:chat_command", command).await;
    admin.recv_until("minecraft:block_update").await;
    let mut sign = Vec::new();
    sign.put_position(mc_protocol::Position::new(x, y, z));
    sign.put_bool(true);
    for line in ["Hello", "", "world", ""] {
        sign.put_string(line);
    }
    admin.send("minecraft:sign_update", sign).await;
    let (_, data) = admin.recv_until("minecraft:block_entity_data").await;
    let text = format!("{data:?}");
    assert!(!text.is_empty());
    let be = ctx
        .world
        .block_entity(mc_protocol::Position::new(x, y, z), &ctx.data.blocks)
        .unwrap();
    assert!(format!("{:?}", be.data).contains("Hello"));
    let _ = stop.send(());
}
