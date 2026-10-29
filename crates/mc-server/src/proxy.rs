//! Login through a proxy.
//!
//! `proxy=velocity` asks for Velocity modern forwarding
//! (`velocity:player_info`, version 1) and checks the HMAC-SHA256 with
//! `velocity-secret`. `proxy=bungee` reads the NUL-separated address
//! BungeeCord appends to the handshake. When `bungee-tokens` is set, one
//! of those tokens must arrive as the profile property
//! `bungeeguard-token` (BungeeGuard); that property is removed before the
//! profile is shown to anyone.

use std::net::{IpAddr, SocketAddr};

use hmac::Mac;
use mc_protocol::packets::{GameProfile, ProfileProperty};
use mc_protocol::{Reader, Write};
use sha2::Sha256;

const VELOCITY_VERSION: i32 = 1;
const GUARD_TOKEN: &str = "bungeeguard-token";

/// How players reach this server.
#[derive(Clone, Debug)]
pub enum Mode {
    Off,
    Velocity {
        secret: String,
    },
    /// `tokens` empty is BungeeCord alone. Any entry turns BungeeGuard on.
    Bungee {
        tokens: Vec<String>,
    },
}

impl Mode {
    pub fn from_settings(mode: &str, secret: &str, tokens: &str) -> Self {
        match mode {
            "velocity" => Self::Velocity {
                secret: secret.to_owned(),
            },
            "bungee" => Self::Bungee {
                tokens: tokens
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            },
            _ => Self::Off,
        }
    }

    pub fn log(&self) {
        match self {
            Self::Off => {}
            Self::Velocity { secret } if secret.is_empty() => {
                tracing::warn!("proxy=velocity but velocity-secret is empty");
            }
            Self::Velocity { .. } => tracing::info!("Using Velocity modern forwarding"),
            Self::Bungee { tokens } if tokens.is_empty() => {
                tracing::warn!(
                    "BungeeCord forwarding is on without BungeeGuard tokens; anyone who can reach this port can pretend to be the proxy"
                );
            }
            Self::Bungee { tokens } => {
                tracing::info!(
                    "Using BungeeCord forwarding with {} BungeeGuard token(s)",
                    tokens.len()
                );
            }
        }
    }
}

/// Who the proxy says this connection is.
#[derive(Clone, Debug)]
pub struct Forwarded {
    pub address: String,
    pub profile: GameProfile,
}

/// The address a ban list and the join line should use.
pub fn player_socket(address: &str, fallback: SocketAddr) -> SocketAddr {
    let address = address.trim();
    if let Ok(ip) = address.parse::<IpAddr>() {
        return SocketAddr::new(ip, fallback.port());
    }
    address.parse::<SocketAddr>().unwrap_or(fallback)
}

/// Checks a Velocity modern-forwarding plugin response. The first 32 bytes
/// are the HMAC-SHA256 of the rest, keyed by the shared secret.
pub fn verify_velocity(secret: &str, payload: &[u8]) -> Result<Forwarded, &'static str> {
    if secret.is_empty() {
        return Err("Velocity forwarding secret is empty");
    }
    if payload.len() < 32 {
        return Err("This server requires Velocity modern forwarding.");
    }
    let (sig, body) = payload.split_at(32);
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| "Velocity forwarding secret is empty")?;
    mac.update(body);
    let got = mac.finalize().into_bytes();
    if !same_bytes(&got, sig) {
        return Err("This server requires Velocity modern forwarding.");
    }
    let mut r = Reader::new(body);
    let version = r.varint().map_err(|_| "bad velocity profile")?;
    if !(1..=4).contains(&version) {
        return Err("This server requires Velocity modern forwarding.");
    }
    let address = r.string(1024).map_err(|_| "bad velocity profile")?;
    let uuid = r.uuid().map_err(|_| "bad velocity profile")?;
    let name = r.string(16).map_err(|_| "bad velocity profile")?;
    let count = r.length_prefix().map_err(|_| "bad velocity profile")?;
    if count > 16 {
        return Err("bad velocity profile");
    }
    let mut properties = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.string(64).map_err(|_| "bad velocity profile")?;
        let value = r.string(32767).map_err(|_| "bad velocity profile")?;
        let signature = if r.bool().map_err(|_| "bad velocity profile")? {
            Some(r.string(32767).map_err(|_| "bad velocity profile")?)
        } else {
            None
        };
        properties.push(ProfileProperty {
            name,
            value,
            signature,
        });
    }
    Ok(Forwarded {
        address,
        profile: GameProfile {
            uuid,
            name,
            properties,
        },
    })
}

/// BungeeCord's handshake host is `host\0ip\0uuid\0[properties json]`.
/// `login_name` is the name from Login Start; the handshake has no name.
pub fn verify_bungee(
    handshake_host: &str,
    login_name: &str,
    tokens: &[String],
) -> Result<Forwarded, &'static str> {
    let mut parts = handshake_host.split('\0');
    let _host = parts.next().ok_or("not forwarded")?;
    let Some(address) = parts.next() else {
        return Err(
            "If you wish to use IP forwarding, please enable it in your BungeeCord config as well!",
        );
    };
    let Some(uuid) = parts.next().and_then(parse_uuid) else {
        return Err(
            "If you wish to use IP forwarding, please enable it in your BungeeCord config as well!",
        );
    };
    let mut properties = match parts.next() {
        Some(json) if !json.is_empty() => read_properties(json)?,
        _ => Vec::new(),
    };
    let mut token = None;
    properties.retain(|p| {
        if p.name == GUARD_TOKEN {
            token = Some(p.value.clone());
            false
        } else {
            true
        }
    });
    if !tokens.is_empty() {
        let presented = token.as_deref().unwrap_or("");
        if !tokens.iter().any(|t| same_str(t, presented)) {
            return Err("Invalid BungeeGuard token");
        }
    }
    Ok(Forwarded {
        address: address.to_owned(),
        profile: GameProfile {
            uuid,
            name: login_name.to_owned(),
            properties,
        },
    })
}

fn read_properties(json: &str) -> Result<Vec<ProfileProperty>, &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "Incomplete proxy profile")?;
    let Some(items) = value.as_array() else {
        return Err("Incomplete proxy profile");
    };
    if items.len() > 16 {
        return Err("Incomplete proxy profile");
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let name = item["name"].as_str().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let signature = item["signature"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        out.push(ProfileProperty {
            name: name.to_owned(),
            value: item["value"].as_str().unwrap_or("").to_owned(),
            signature,
        });
    }
    Ok(out)
}

fn parse_uuid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn same_str(a: &str, b: &str) -> bool {
    same_bytes(a.as_bytes(), b.as_bytes())
}

/// The byte a server sends in the Velocity query: forwarding version 1.
pub fn velocity_query() -> Vec<u8> {
    let mut out = Vec::new();
    out.put_varint(VELOCITY_VERSION);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> Vec<u8> {
        let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = mac.finalize().into_bytes();
        let mut out = sig.to_vec();
        out.extend_from_slice(body);
        out
    }

    fn body(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.put_varint(1);
        out.put_string("203.0.113.8");
        out.put_uuid(0xabc);
        out.put_string(name);
        out.put_len(1);
        out.put_string("textures");
        out.put_string("skin-data");
        out.put_bool(true);
        out.put_string("skin-sig");
        out
    }

    #[test]
    fn velocity_signature_keeps_the_real_player_and_rejects_a_forgery() {
        let payload = sign("topsecret", &body("Alex"));
        let fwd = verify_velocity("topsecret", &payload).unwrap();
        assert_eq!(fwd.address, "203.0.113.8");
        assert_eq!(fwd.profile.name, "Alex");
        assert_eq!(fwd.profile.uuid, 0xabc);
        assert_eq!(fwd.profile.properties[0].value, "skin-data");
        assert_eq!(
            player_socket(&fwd.address, "127.0.0.1:25565".parse().unwrap()),
            "203.0.113.8:25565".parse().unwrap()
        );
        let mut forged = payload.clone();
        let last = forged.len() - 1;
        forged[last] ^= 0xff;
        assert!(verify_velocity("topsecret", &forged).is_err());
        assert!(verify_velocity("other", &payload).is_err());
        assert_eq!(velocity_query(), vec![1]);
    }

    #[test]
    fn bungee_guard_requires_the_token_and_does_not_keep_it_on_the_profile() {
        let uuid = "11111111111111111111111111111111";
        let host = format!(
            "play.example\0198.51.100.4\0{uuid}\0[{{\"name\":\"textures\",\"value\":\"abc\",\"signature\":\"def\"}},{{\"name\":\"bungeeguard-token\",\"value\":\"s3cret\"}}]"
        );
        let denied = verify_bungee(&host, "Alex", &["nope".into()]);
        assert_eq!(denied.unwrap_err(), "Invalid BungeeGuard token");
        let fwd = verify_bungee(&host, "Alex", &["s3cret".into()]).unwrap();
        assert_eq!(fwd.address, "198.51.100.4");
        assert_eq!(fwd.profile.name, "Alex");
        assert_eq!(fwd.profile.uuid, u128::from_str_radix(uuid, 16).unwrap());
        assert_eq!(fwd.profile.properties.len(), 1);
        assert_eq!(fwd.profile.properties[0].name, "textures");
        assert!(verify_bungee("localhost", "Alex", &[]).is_err());
        let open = verify_bungee(&host, "Alex", &[]).unwrap();
        assert!(
            open.profile
                .properties
                .iter()
                .all(|p| p.name != "bungeeguard-token")
        );
    }
}
