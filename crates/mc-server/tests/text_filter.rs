//! End-to-end: chat, signs and books through an external text filter (a mock
//! HTTP service on a local port), and what other players see. Needs extracted
//! data (`cargo xtask extract-data`); skipped otherwise.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mc_protocol::frame::{decode_frame_body, split_frame, write_frame};
use mc_protocol::{Direction, PacketIds, Reader, State, Write as _};
use mc_server::{Config, Server, find_data_dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Bot {
    stream: TcpStream,
    buf: Vec<u8>,
    compression: Option<usize>,
    ciphers: Option<(mc_protocol::crypto::Cfb8, mc_protocol::crypto::Cfb8)>,
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
            // Long enough for `/locate` while the rest of the suite is
            // generating chunks: the reply can sit quiet past a keep-alive.
            let n =
                tokio::time::timeout(Duration::from_secs(60), self.stream.read_buf(&mut self.buf))
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

fn data_dir() -> Option<std::path::PathBuf> {
    find_data_dir(Path::new(env!("CARGO_MANIFEST_DIR"))).ok()
}

type Requests = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

/// A filter service on a local port. A text starting with "bad" loses its
/// first three characters; "blocked" is refused; anything else passes.
/// Every request is recorded as (path, JSON body).
fn filter_service() -> (u16, Requests) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Requests = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            let (head_end, length) = loop {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break (0, 0);
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (i + 4, length);
                }
            };
            if head_end == 0 {
                continue;
            }
            while buf.len() < head_end + length {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let path = head
                .lines()
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .unwrap_or_default()
                .to_owned();
            let body: serde_json::Value =
                serde_json::from_slice(&buf[head_end..]).unwrap_or_default();
            let answer = match body["text"].as_str() {
                Some(t) if t.starts_with("bad") => {
                    r####"{"hashed":"###","hashes":[0,1,2]}"####.to_owned()
                }
                Some("blocked") => "{}".to_owned(),
                _ => r#"{"response":true}"#.to_owned(),
            };
            log.lock().unwrap().push((path, body));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (port, seen)
}

type Running = (
    std::net::SocketAddr,
    Arc<mc_server::ServerContext>,
    tokio::sync::oneshot::Sender<()>,
);

async fn start(port: Option<u16>) -> Running {
    let data_dir = data_dir().expect("data");
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.view_distance = 2;
    config.online_mode = false;
    config.ops = vec!["Admin".into()];
    if let Some(port) = port {
        config.text_filter = mc_server::textfilter::TextFilter::from_properties(
            0,
            &format!(r#"{{"apiServer":"http://127.0.0.1:{port}","apiKey":"k","serverId":"srv"}}"#),
        );
        assert!(config.text_filter.is_some());
    }
    let server = Server::bind(config).await.unwrap();
    server
        .context()
        .game_rules
        .lock()
        .unwrap()
        .set("respawn_radius", mc_server::rules::Value::Int(0));
    let addr = server.local_addr();
    let ctx = server.context().clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    (addr, ctx, stop)
}

async fn say(bot: &mut Bot, line: &str) {
    let mut chat = Vec::new();
    chat.put_string(line);
    chat.put_i64(0);
    chat.put_i64(0);
    chat.put_bool(false);
    chat.put_varint(0);
    chat.put_slice(&[0, 0, 0]);
    chat.put_u8(0);
    bot.send("minecraft:chat", chat).await;
}

/// The next chat line (or the notice that a message was hidden), as debug text.
async fn next_chat(bot: &mut Bot) -> String {
    loop {
        let (_, body) = bot.recv_until("minecraft:system_chat").await;
        let (tag, _) = mc_nbt::read_network(&body).unwrap();
        let text = format!("{tag:?}");
        if text.contains("chat.type.text") || text.contains("chat.filtered_full") {
            return text;
        }
    }
}

async fn set_text_filtering(bot: &mut Bot, ctx: &mc_server::ServerContext, name: &str, on: bool) {
    let mut info = Vec::new();
    info.put_string("en_us");
    info.put_i8(3);
    info.put_varint(0);
    info.put_bool(true);
    info.put_u8(0x7f);
    info.put_varint(1);
    info.put_bool(on);
    info.put_bool(true);
    info.put_varint(0);
    bot.send("minecraft:client_information", info).await;
    for _ in 0..200 {
        let now = ctx
            .players
            .lock()
            .unwrap()
            .values()
            .any(|p| p.profile.name == name && p.text_filtering == on);
        if now {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("{name} never changed text filtering");
}

#[tokio::test]
async fn chat_goes_through_the_filter_and_each_reader_gets_their_version() {
    if data_dir().is_none() {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    }
    let (port, seen) = filter_service();
    let (addr, ctx, stop) = start(Some(port)).await;
    let (mut alice, _) = join_offline(addr, ctx.ids.clone(), "Alice").await;
    let (mut bob, _) = join_offline(addr, ctx.ids.clone(), "Bob").await;
    let (mut carol, _) = join_offline(addr, ctx.ids.clone(), "Carol").await;
    set_text_filtering(&mut carol, &ctx, "Carol", true).await;

    // Some of it removed: Bob reads what was written, Carol (who asked for
    // filtered chat) reads it with the removed run hidden, Alice her own.
    say(&mut alice, "bad word").await;
    let for_bob = next_chat(&mut bob).await;
    assert!(
        for_bob.contains("bad word") && !for_bob.contains("dark_gray"),
        "{for_bob}"
    );
    let for_carol = next_chat(&mut carol).await;
    assert!(
        for_carol.contains("###") && for_carol.contains("dark_gray") && for_carol.contains(" word"),
        "{for_carol}"
    );
    assert!(!for_carol.contains("bad word"), "{for_carol}");
    assert!(next_chat(&mut alice).await.contains("bad word"));

    // All of it removed: Carol gets nothing, Alice is told, Bob reads it.
    say(&mut alice, "blocked").await;
    assert!(next_chat(&mut bob).await.contains("blocked"));
    assert!(next_chat(&mut alice).await.contains("blocked"));
    assert!(next_chat(&mut alice).await.contains("chat.filtered_full"));
    say(&mut alice, "after").await;
    let next = next_chat(&mut carol).await;
    assert!(
        next.contains("after") && !next.contains("blocked"),
        "{next}"
    );

    // The service was asked about each text, with who wrote it, and heard
    // that Alice came.
    let requests = seen.lock().unwrap().clone();
    let chat: Vec<_> = requests.iter().filter(|(p, _)| p == "/v1/chat").collect();
    let texts: Vec<_> = chat
        .iter()
        .map(|(_, b)| b["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["bad word", "blocked", "after"]);
    assert_eq!(chat[0].1["player_display_name"], "Alice");
    assert_eq!(chat[0].1["server"], "srv");
    assert!(
        requests
            .iter()
            .any(|(p, b)| p == "/v1/join" && b["user_display_name"] == "Alice")
    );

    drop(alice);
    for _ in 0..200 {
        if seen
            .lock()
            .unwrap()
            .iter()
            .any(|(p, b)| p == "/v1/leave" && b["user_display_name"] == "Alice")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|(p, b)| p == "/v1/leave" && b["user_display_name"] == "Alice")
    );
    let _ = stop.send(());
}

#[tokio::test]
async fn signs_and_books_keep_both_versions_of_what_was_written() {
    if data_dir().is_none() {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    }
    let (port, _seen) = filter_service();
    let (addr, ctx, stop) = start(Some(port)).await;
    let (mut admin, at) = join_offline(addr, ctx.ids.clone(), "Admin").await;
    let (x, y, z) = (
        at.0.floor() as i32 + 1,
        at.1.floor() as i32,
        at.2.floor() as i32,
    );
    let mut command = Vec::new();
    command.put_string(&format!("setblock {x} {y} {z} oak_sign"));
    admin.send("minecraft:chat_command", command).await;
    admin.recv_until("minecraft:block_update").await;
    let position = mc_protocol::Position::new(x, y, z);
    let mut sign = Vec::new();
    sign.put_position(position);
    for line in ["bad line", "fine", "", ""] {
        sign.put_string(line);
    }
    sign.put_bool(true);
    admin.send("minecraft:sign_update", sign).await;
    admin.recv_until("minecraft:block_entity_data").await;
    let be = ctx.world.block_entity(position, &ctx.data.blocks).unwrap();
    // What was written is what the sign says; what the filter left is kept
    // beside it for players who asked for filtered text.
    let Some(mc_nbt::Tag::Compound(front)) = be.data.get("front_text") else {
        panic!("{:?}", be.data);
    };
    let lines = |key: &str| match front.get(key) {
        Some(mc_nbt::Tag::List(l)) => l
            .iter()
            .map(|t| match t {
                mc_nbt::Tag::String(s) => s.clone(),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    assert_eq!(lines("messages"), ["bad line", "fine", "", ""]);
    assert_eq!(lines("filtered_messages"), ["### line", "fine", "", ""]);

    // A signed book: the title and pages get the same treatment.
    let mut give = Vec::new();
    give.put_string("give Admin writable_book");
    admin.send("minecraft:chat_command", give).await;
    admin.recv_until("minecraft:container_set_slot").await;
    let mut edit = Vec::new();
    edit.put_varint(0);
    edit.put_varint(2);
    edit.put_string("bad story");
    edit.put_string("blocked");
    edit.put_bool(true);
    edit.put_string("Diary");
    admin.send("minecraft:edit_book", edit).await;
    admin.recv_until("minecraft:container_set_slot").await;
    let book = {
        let hands = ctx.inventories.lock().unwrap();
        hands.values().next().unwrap().slots[36].clone().unwrap()
    };
    assert_eq!(ctx.items.name(book.item), Some("minecraft:written_book"));
    let Some(mc_nbt::Tag::Compound(content)) = book.components.get("written_book_content") else {
        panic!("not signed");
    };
    let Some(mc_nbt::Tag::List(pages)) = content.get("pages") else {
        panic!("no pages");
    };
    let page = |i: usize| match &pages[i] {
        mc_nbt::Tag::Compound(c) => c.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(page(0).get("raw"), Some(&mc_nbt::Tag::from("bad story")));
    assert_eq!(
        page(0).get("filtered"),
        Some(&mc_nbt::Tag::from("### story"))
    );
    // Refused whole: others read an empty page.
    assert_eq!(page(1).get("raw"), Some(&mc_nbt::Tag::from("blocked")));
    assert_eq!(page(1).get("filtered"), Some(&mc_nbt::Tag::from("")));
    let _ = stop.send(());
}

#[tokio::test]
async fn without_a_filter_chat_goes_out_as_written() {
    if data_dir().is_none() {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    }
    let (addr, ctx, stop) = start(None).await;
    let (mut alice, _) = join_offline(addr, ctx.ids.clone(), "Alice").await;
    let (mut carol, _) = join_offline(addr, ctx.ids.clone(), "Carol").await;
    set_text_filtering(&mut carol, &ctx, "Carol", true).await;
    say(&mut alice, "bad word").await;
    // Even a reader who wants filtered chat gets it whole: nobody filtered it.
    let line = next_chat(&mut carol).await;
    assert!(
        line.contains("bad word") && !line.contains("dark_gray"),
        "{line}"
    );
    let _ = stop.send(());
}
