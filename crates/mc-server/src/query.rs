//! The query service (minecraft.wiki, "Query"; the GameSpy 4 protocol over
//! UDP). Every request starts `FE FD`, a type and a session id. A handshake
//! (type 9) gets a challenge token, written as decimal text; a stat request
//! (type 0) carrying a live token gets the basic stat, or, with four more
//! bytes of padding, the full one with the player names.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::net::UdpSocket;

use crate::context::ServerContext;

const HANDSHAKE: u8 = 9;
const STAT: u8 = 0;

/// How long a challenge token holds.
const TOKEN_LIFE: Duration = Duration::from_secs(30);

/// What a stat answer tells.
#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub motd: String,
    pub map: String,
    pub version: String,
    pub players: Vec<String>,
    pub max_players: usize,
    pub port: u16,
    pub ip: String,
}

/// A request: its type, session id and what follows.
pub fn decode(bytes: &[u8]) -> Option<(u8, i32, &[u8])> {
    if bytes.len() < 7 || bytes[0] != 0xFE || bytes[1] != 0xFD {
        return None;
    }
    let session = i32::from_be_bytes(bytes[3..7].try_into().ok()?) & 0x0F0F_0F0F;
    Some((bytes[2], session, &bytes[7..]))
}

fn head(kind: u8, session: i32) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&session.to_be_bytes());
    out
}

fn text(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// The handshake's answer: the token as decimal text.
pub fn handshake(session: i32, token: i32) -> Vec<u8> {
    let mut out = head(HANDSHAKE, session);
    text(&mut out, &token.to_string());
    out
}

/// The basic stat.
pub fn basic(session: i32, f: &Facts) -> Vec<u8> {
    let mut out = head(STAT, session);
    text(&mut out, &f.motd);
    text(&mut out, "SMP");
    text(&mut out, &f.map);
    text(&mut out, &f.players.len().to_string());
    text(&mut out, &f.max_players.to_string());
    out.extend_from_slice(&f.port.to_le_bytes());
    text(&mut out, &f.ip);
    out
}

/// The full stat: the keys and values, then the players.
pub fn full(session: i32, f: &Facts) -> Vec<u8> {
    let mut out = head(STAT, session);
    out.extend_from_slice(b"splitnum\0\x80\0");
    let pairs = [
        ("hostname", f.motd.clone()),
        ("gametype", "SMP".into()),
        ("game_id", "MINECRAFT".into()),
        ("version", f.version.clone()),
        ("plugins", String::new()),
        ("map", f.map.clone()),
        ("numplayers", f.players.len().to_string()),
        ("maxplayers", f.max_players.to_string()),
        ("hostport", f.port.to_string()),
        ("hostip", f.ip.clone()),
    ];
    for (k, v) in pairs {
        text(&mut out, k);
        text(&mut out, &v);
    }
    out.push(0);
    out.extend_from_slice(b"\x01player_\0\0");
    for p in &f.players {
        text(&mut out, p);
    }
    out.push(0);
    out
}

/// What the server tells now.
fn facts(ctx: &ServerContext) -> Facts {
    let ip = match ctx.config.bind {
        a if a.is_unspecified() => "127.0.0.1".to_owned(),
        a => a.to_string(),
    };
    Facts {
        motd: ctx.config.motd.clone(),
        map: ctx
            .config
            .level_dir
            .as_ref()
            .and_then(|d| d.file_name())
            .map_or_else(|| "world".to_owned(), |n| n.to_string_lossy().into_owned()),
        version: ctx.data.version.name.clone(),
        players: ctx
            .players
            .lock()
            .expect("players lock")
            .values()
            .map(|p| p.profile.name.clone())
            .collect(),
        max_players: ctx.config.max_players,
        port: ctx.config.port,
        ip,
    }
}

/// Live challenge tokens by where they were given.
#[derive(Default)]
struct Tokens(HashMap<SocketAddr, (i32, Instant)>);

impl Tokens {
    fn give(&mut self, to: SocketAddr, now: Instant) -> i32 {
        self.0
            .retain(|_, (_, at)| now.duration_since(*at) < TOKEN_LIFE);
        let token = (crate::util::random_u64() & 0x7FFF_FFFF) as i32;
        self.0.insert(to, (token, now));
        token
    }

    fn holds(&self, from: SocketAddr, token: i32, now: Instant) -> bool {
        self.0
            .get(&from)
            .is_some_and(|&(t, at)| t == token && now.duration_since(at) < TOKEN_LIFE)
    }
}

/// The answer to one request, if it gets one.
fn answer(
    tokens: &mut Tokens,
    from: SocketAddr,
    bytes: &[u8],
    now: Instant,
    facts: impl FnOnce() -> Facts,
) -> Option<Vec<u8>> {
    let (kind, session, rest) = decode(bytes)?;
    match kind {
        HANDSHAKE => Some(handshake(session, tokens.give(from, now))),
        STAT => {
            let token = i32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
            if !tokens.holds(from, token, now) {
                return None;
            }
            // Four bytes of padding ask for the full stat.
            Some(if rest.len() >= 8 {
                full(session, &facts())
            } else {
                basic(session, &facts())
            })
        }
        _ => None,
    }
}

/// Answers queries until the server stops.
pub async fn serve(ctx: Arc<ServerContext>, at: SocketAddr) -> Result<()> {
    let socket = UdpSocket::bind(at).await?;
    tracing::info!("Query running on {at}");
    let mut tokens = Tokens::default();
    let mut buf = [0u8; 1460];
    loop {
        let (n, from) = socket.recv_from(&mut buf).await?;
        if let Some(out) = answer(&mut tokens, from, &buf[..n], Instant::now(), || facts(&ctx)) {
            let _ = socket.send_to(&out, from).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            motd: "A Minecraft Server".into(),
            map: "world".into(),
            version: "26.3".into(),
            players: vec!["alex".into(), "steve".into()],
            max_players: 20,
            port: 25565,
            ip: "127.0.0.1".into(),
        }
    }

    fn request(kind: u8, session: i32, rest: &[u8]) -> Vec<u8> {
        let mut b = vec![0xFE, 0xFD, kind];
        b.extend_from_slice(&session.to_be_bytes());
        b.extend_from_slice(rest);
        b
    }

    #[test]
    fn a_stat_needs_the_token_the_handshake_gave() {
        let mut tokens = Tokens::default();
        let from: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let t0 = Instant::now();
        let hello = answer(&mut tokens, from, &request(HANDSHAKE, 1, &[]), t0, facts).unwrap();
        assert_eq!(&hello[..5], &[9, 0, 0, 0, 1]);
        let token: i32 = std::str::from_utf8(&hello[5..hello.len() - 1])
            .unwrap()
            .parse()
            .unwrap();
        // A wrong token gets nothing; an old one neither.
        assert!(
            answer(
                &mut tokens,
                from,
                &request(STAT, 1, &(token ^ 1).to_be_bytes()),
                t0,
                facts
            )
            .is_none()
        );
        let late = t0 + TOKEN_LIFE + Duration::from_secs(1);
        assert!(
            answer(
                &mut tokens,
                from,
                &request(STAT, 1, &token.to_be_bytes()),
                late,
                facts
            )
            .is_none()
        );
        let basic_stat = answer(
            &mut tokens,
            from,
            &request(STAT, 1, &token.to_be_bytes()),
            t0,
            facts,
        )
        .unwrap();
        assert_eq!(basic_stat, basic(1, &facts()));
        let mut padded = token.to_be_bytes().to_vec();
        padded.extend_from_slice(&[0; 4]);
        let full_stat = answer(&mut tokens, from, &request(STAT, 1, &padded), t0, facts).unwrap();
        assert_eq!(full_stat, full(1, &facts()));
    }

    #[test]
    fn the_basic_stat_is_laid_out_as_the_protocol_says() {
        let b = basic(0x0102_0304, &facts());
        let mut want = vec![0, 1, 2, 3, 4];
        want.extend_from_slice(b"A Minecraft Server\0SMP\0world\x002\x0020\0");
        want.extend_from_slice(&[0xDD, 0x63]);
        want.extend_from_slice(b"127.0.0.1\0");
        assert_eq!(b, want);
    }

    #[test]
    fn the_full_stat_lists_keys_then_players() {
        let f = full(0, &facts());
        let s = String::from_utf8_lossy(&f);
        assert!(s.contains(
            "splitnum\0\u{FFFD}\0hostname\0A Minecraft Server\0gametype\0SMP\0game_id\0MINECRAFT\0"
        ));
        assert!(s.ends_with("\0\u{1}player_\0\0alex\0steve\0\0"));
    }

    #[test]
    fn session_ids_keep_only_their_low_nibbles() {
        let (_, s, _) = decode(&request(HANDSHAKE, -1, &[])).unwrap();
        assert_eq!(s, 0x0F0F_0F0F);
        assert!(decode(&[0xFE, 0xFC, 9, 0, 0, 0, 0]).is_none());
    }
}
