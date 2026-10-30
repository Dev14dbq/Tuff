//! End-to-end: connections a hostile client opens and what the server does
//! with them. Needs extracted data (`cargo xtask extract-data`); skipped
//! otherwise.

use std::path::Path;
use std::time::Duration;

use mc_protocol::Write as _;
use mc_protocol::frame::write_frame;
use mc_server::{Config, Server, find_data_dir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn start(change: impl FnOnce(&mut Config)) -> Option<(std::net::SocketAddr, impl Drop)> {
    let data_dir = find_data_dir(Path::new(env!("CARGO_MANIFEST_DIR"))).ok()?;
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.online_mode = false;
    change(&mut config);
    let server = Server::bind(config).await.unwrap();
    let addr = server.local_addr();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.run(async {
        let _ = stopped.await;
    }));
    struct Stop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Stop {
        fn drop(&mut self) {
            if let Some(stop) = self.0.take() {
                let _ = stop.send(());
            }
        }
    }
    Some((addr, Stop(Some(stop))))
}

/// Reads until the server closes the connection; what it sent before.
async fn until_closed(stream: &mut TcpStream, within: Duration) -> Option<Vec<u8>> {
    let mut got = Vec::new();
    let read = async {
        let mut buf = [0; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => got.extend_from_slice(&buf[..n]),
            }
        }
    };
    tokio::time::timeout(within, read).await.ok()?;
    Some(got)
}

/// Handshake with intent 2 (login), then Login Start.
async fn begin_login(stream: &mut TcpStream) {
    let mut out = Vec::new();
    let mut body = Vec::new();
    body.put_varint(mc_protocol::PROTOCOL_VERSION);
    body.put_string("localhost");
    body.put_u16(25565);
    body.put_varint(2);
    write_frame(&mut out, 0, &body, None);
    let mut hello = Vec::new();
    hello.put_string("Hostile");
    hello.put_uuid(0);
    write_frame(&mut out, 0, &hello, None);
    stream.write_all(&out).await.unwrap();
}

#[tokio::test]
async fn a_silent_connection_is_dropped() {
    let Some((addr, _server)) = start(|c| c.read_timeout = Duration::from_millis(300)).await else {
        eprintln!("skipped: no extracted data");
        return;
    };
    // Nothing at all, then half a handshake: both time out.
    let mut silent = TcpStream::connect(addr).await.unwrap();
    assert!(
        until_closed(&mut silent, Duration::from_secs(5))
            .await
            .is_some()
    );
    let mut partial = TcpStream::connect(addr).await.unwrap();
    partial.write_all(&[0x10, 0x00]).await.unwrap();
    assert!(
        until_closed(&mut partial, Duration::from_secs(5))
            .await
            .is_some()
    );
}

#[tokio::test]
async fn half_open_connections_per_address_are_capped_when_asked() {
    let Some((addr, _server)) = start(|c| c.max_pending_per_ip = 2).await else {
        eprintln!("skipped: no extracted data");
        return;
    };
    let _first = TcpStream::connect(addr).await.unwrap();
    let _second = TcpStream::connect(addr).await.unwrap();
    let mut third = TcpStream::connect(addr).await.unwrap();
    let closed = until_closed(&mut third, Duration::from_secs(5)).await;
    assert_eq!(closed, Some(Vec::new()), "the third is closed at once");
}

#[tokio::test]
async fn quick_relogins_are_throttled_when_asked() {
    let Some((addr, _server)) = start(|c| c.connection_throttle = Duration::from_secs(60)).await
    else {
        eprintln!("skipped: no extracted data");
        return;
    };
    let mut first = TcpStream::connect(addr).await.unwrap();
    begin_login(&mut first).await;
    let mut buf = [0; 16];
    // The first login goes on (the server answers it).
    let n = tokio::time::timeout(Duration::from_secs(5), first.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0);
    let mut second = TcpStream::connect(addr).await.unwrap();
    begin_login(&mut second).await;
    let sent = until_closed(&mut second, Duration::from_secs(5))
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&sent);
    assert!(text.contains("Connection throttled"), "{text}");
}
