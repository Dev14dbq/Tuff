//! End-to-end: configuration has no time limit of its own. A player who
//! takes longer than login may (vanilla's 30 s) over a resource pack still
//! joins. Needs extracted data (`cargo xtask extract-data`); skipped
//! otherwise.

use std::path::Path;
use std::time::Duration;

use mc_protocol::frame::{FrameBuffer, write_frame};
use mc_protocol::{Direction, PacketIds, Reader, State, Write as _};
use mc_server::{Config, Server, find_data_dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Bot {
    stream: TcpStream,
    frames: FrameBuffer,
    ids: PacketIds,
    state: State,
}

impl Bot {
    async fn send(&mut self, name: &str, body: &[u8]) {
        let id = self
            .ids
            .id(self.state, Direction::Serverbound, name)
            .unwrap();
        let mut out = Vec::new();
        write_frame(&mut out, id, body, None);
        self.stream.write_all(&out).await.unwrap();
    }

    /// Packets until `wanted`, whose body is returned.
    async fn until(&mut self, wanted: &str) -> Vec<u8> {
        loop {
            while let Some(frame) = self.frames.next_frame().unwrap() {
                let mut r = Reader::new(frame);
                let id = r.varint().unwrap();
                let name = self.ids.name(self.state, Direction::Clientbound, id);
                if name == Some(wanted) {
                    return r.rest().to_vec();
                }
            }
            let read = self.stream.read_buf(self.frames.tail());
            let n = tokio::time::timeout(Duration::from_secs(30), read)
                .await
                .expect("server answered in time")
                .unwrap();
            assert!(n > 0, "server closed the connection waiting for {wanted}");
        }
    }
}

#[tokio::test]
async fn a_slow_resource_pack_does_not_time_the_login_out() {
    let Ok(data_dir) = find_data_dir(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!("skipped: no extracted data");
        return;
    };
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.online_mode = false;
    config.compression_threshold = None;
    config.login_timeout = Duration::from_millis(300);
    let pack_id = 0x1234_u128;
    config.resource_pack = Some(mc_server::serverlist::ResourcePack {
        id: pack_id,
        url: "http://localhost/pack.zip".into(),
        hash: String::new(),
        required: true,
        prompt: None,
    });
    let server = Server::bind(config).await.unwrap();
    let (addr, ids) = (server.local_addr(), server.context().ids.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));

    let mut bot = Bot {
        stream: TcpStream::connect(addr).await.unwrap(),
        frames: FrameBuffer::new(),
        ids,
        state: State::Handshake,
    };
    let mut intention = Vec::new();
    intention.put_varint(mc_protocol::PROTOCOL_VERSION);
    intention.put_string("localhost");
    intention.put_u16(25565);
    intention.put_varint(2);
    bot.send("minecraft:intention", &intention).await;
    bot.state = State::Login;
    let mut hello = Vec::new();
    hello.put_string("Slowpoke");
    hello.put_uuid(0);
    bot.send("minecraft:hello", &hello).await;
    bot.until("minecraft:login_finished").await;
    bot.send("minecraft:login_acknowledged", &[]).await;
    bot.state = State::Configuration;
    bot.until("minecraft:select_known_packs").await;
    let mut packs = Vec::new();
    packs.put_len(0);
    bot.send("minecraft:select_known_packs", &packs).await;
    bot.until("minecraft:resource_pack_push").await;

    // Ten login timeouts' worth of "downloading".
    tokio::time::sleep(Duration::from_secs(3)).await;
    let mut loaded = Vec::new();
    loaded.put_uuid(pack_id);
    loaded.put_varint(0);
    bot.send("minecraft:resource_pack", &loaded).await;
    bot.until("minecraft:finish_configuration").await;
    bot.send("minecraft:finish_configuration", &[]).await;
    bot.state = State::Play;
    bot.until("minecraft:login").await;
    let _ = stop.send(());
}
