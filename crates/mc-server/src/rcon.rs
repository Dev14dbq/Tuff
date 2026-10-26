//! The remote console (minecraft.wiki, "RCON"): over TCP, packets of a
//! little-endian length, request id and type, then a null-terminated body
//! and one more zero byte. A client logs in (type 3) with `rcon.password`
//! and then runs commands (type 2) as the console does; the answer (type 0)
//! is the command's feedback, in pieces of at most 4096 bytes.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Result, bail};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use crate::context::ServerContext;

const LOGIN: i32 = 3;
const COMMAND: i32 = 2;
const AUTH_RESPONSE: i32 = 2;
const RESPONSE: i32 = 0;

/// The most a client's packet may hold (vanilla reads into 1460 bytes).
const MAX_IN: usize = 1460;
/// Answers are cut into pieces this long.
const MAX_OUT: usize = 4096;

/// One packet as it goes over the wire.
pub fn encode(id: i32, kind: i32, body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + body.len());
    let len = (4 + 4 + body.len() + 2) as i32;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// A packet's id, type and body from what follows its length.
pub fn decode(bytes: &[u8]) -> Option<(i32, i32, String)> {
    if bytes.len() < 10 {
        return None;
    }
    let int = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
    let body = &bytes[8..];
    let end = body.iter().position(|&b| b == 0)?;
    Some((
        int(0),
        int(4),
        String::from_utf8_lossy(&body[..end]).into_owned(),
    ))
}

/// The answer to a command, cut into packets.
pub fn answer(id: i32, text: &str) -> Vec<Vec<u8>> {
    if text.is_empty() {
        return vec![encode(id, RESPONSE, "")];
    }
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut cut = rest.len().min(MAX_OUT);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(encode(id, RESPONSE, &rest[..cut]));
        rest = &rest[cut..];
    }
    out
}

/// Runs a command for the remote console on the tick thread: its
/// feedback, a line each.
async fn run(ctx: &ServerContext, line: String) -> String {
    let (reply, mut replies) = tokio::sync::mpsc::unbounded_channel();
    let request = crate::tick::Request {
        dim: 0,
        actor: crate::interact::Actor::console(),
        action: crate::tick::Action::Command {
            line,
            source: crate::commands::Source::Rcon,
        },
        reply,
    };
    if ctx.actions.send(request).is_err() {
        return String::new();
    }
    let Some(outcome) = replies.recv().await else {
        return String::new();
    };
    outcome
        .messages
        .iter()
        .map(|l| ctx.commands.lang.render(l))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn read_packet(stream: &mut TcpStream) -> Result<(i32, i32, String)> {
    let len = stream.read_i32_le().await?;
    if !(10..=MAX_IN as i32).contains(&len) {
        bail!("rcon packet of {len} bytes");
    }
    let mut body = vec![0; len as usize];
    stream.read_exact(&mut body).await?;
    match decode(&body) {
        Some(p) => Ok(p),
        None => bail!("malformed rcon packet"),
    }
}

/// One remote console, its commands run by `run`.
async fn client<F, Fut>(mut stream: TcpStream, password: String, run: F) -> Result<()>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let mut authed = false;
    loop {
        let (id, kind, text) = read_packet(&mut stream).await?;
        match kind {
            LOGIN if !password.is_empty() && text == password => {
                authed = true;
                stream.write_all(&encode(id, AUTH_RESPONSE, "")).await?;
            }
            LOGIN => {
                authed = false;
                stream.write_all(&encode(-1, AUTH_RESPONSE, "")).await?;
            }
            COMMAND if authed => {
                let out = run(text).await;
                for packet in answer(id, &out) {
                    stream.write_all(&packet).await?;
                }
            }
            COMMAND => stream.write_all(&encode(-1, AUTH_RESPONSE, "")).await?,
            other => {
                let text = format!("Unknown request {other:x}");
                stream.write_all(&encode(id, RESPONSE, &text)).await?;
            }
        }
    }
}

/// Listens for remote consoles until the server stops.
pub async fn serve(ctx: Arc<ServerContext>, at: SocketAddr, password: String) -> Result<()> {
    let listener = TcpListener::bind(at).await?;
    tracing::info!("RCON running on {at}");
    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::info!("Rcon connection from: {}", peer.ip());
        let ctx = ctx.clone();
        let password = password.clone();
        tokio::spawn(async move {
            let run = |line| {
                let ctx = ctx.clone();
                async move { run(&ctx, line).await }
            };
            if let Err(e) = client(stream, password, run).await {
                tracing::debug!("rcon client {peer}: {e:#}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip() {
        let p = encode(7, COMMAND, "time set day");
        assert_eq!(
            i32::from_le_bytes(p[0..4].try_into().unwrap()) as usize,
            p.len() - 4
        );
        assert_eq!(decode(&p[4..]), Some((7, COMMAND, "time set day".into())));
        assert!(decode(&[1, 2, 3]).is_none());
    }

    #[test]
    fn long_answers_come_in_pieces() {
        let text = "x".repeat(MAX_OUT * 2 + 10);
        let parts = answer(3, &text);
        assert_eq!(parts.len(), 3);
        let back: String = parts.iter().map(|p| decode(&p[4..]).unwrap().2).collect();
        assert_eq!(back, text);
        assert_eq!(answer(3, "").len(), 1);
    }

    #[test]
    fn a_login_then_a_command_answers_with_its_output() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let at = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let run = |line: String| async move { format!("ran {line}") };
                let _ = client(stream, "secret".into(), run).await;
            });
            let mut s = TcpStream::connect(at).await.unwrap();
            s.write_all(&encode(1, COMMAND, "list")).await.unwrap();
            assert_eq!(read_packet(&mut s).await.unwrap().0, -1);
            s.write_all(&encode(1, LOGIN, "wrong")).await.unwrap();
            assert_eq!(read_packet(&mut s).await.unwrap().0, -1);
            s.write_all(&encode(2, LOGIN, "secret")).await.unwrap();
            assert_eq!(
                read_packet(&mut s).await.unwrap(),
                (2, AUTH_RESPONSE, String::new())
            );
            s.write_all(&encode(3, COMMAND, "time set 1000"))
                .await
                .unwrap();
            assert_eq!(
                read_packet(&mut s).await.unwrap(),
                (3, RESPONSE, "ran time set 1000".into())
            );
        });
    }

    #[test]
    fn a_remote_console_command_gives_back_its_feedback() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let mut control = crate::tick::TickControl::default();
        let out = crate::commands::execute(
            &ctx,
            &mut control,
            &crate::commands::Source::Rcon,
            "time set 1000",
        );
        let text: Vec<String> = out
            .lines
            .iter()
            .map(|l| ctx.commands.lang.render(l))
            .collect();
        assert_eq!(text, ["Set minecraft:overworld to 1000 tick(s)"]);
    }
}
