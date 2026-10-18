use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};

/// UUID vanilla assigns to a player in offline mode:
/// name-based (version 3) UUID of `OfflinePlayer:<name>`.
pub fn offline_uuid(name: &str) -> u128 {
    let hash: [u8; 16] = Md5::digest(format!("OfflinePlayer:{name}")).into();
    let mut bytes = hash;
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    u128::from_be_bytes(bytes)
}

pub fn uuid_string(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Non-cryptographic randomness for ids (session ids, keep-alives).
/// Online mode will need a real CSPRNG for encryption.
pub fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.finish()
}

pub fn random_uuid() -> u128 {
    let mut bytes = ((u128::from(random_u64()) << 64) | u128::from(random_u64())).to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    u128::from_be_bytes(bytes)
}

/// Vanilla rules for player names: 1–16 of `[A-Za-z0-9_]`.
pub fn valid_player_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_matches_known_value() {
        // Well-known offline UUID of "Notch".
        assert_eq!(
            uuid_string(offline_uuid("Notch")),
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
    }

    #[test]
    fn random_uuids_differ_and_are_v4() {
        let (a, b) = (random_uuid(), random_uuid());
        assert_ne!(a, b);
        assert_eq!(uuid_string(a).as_bytes()[14], b'4');
    }

    #[test]
    fn player_names() {
        assert!(valid_player_name("Steve_01"));
        assert!(!valid_player_name(""));
        assert!(!valid_player_name("seventeen_chars__"));
        assert!(!valid_player_name("bad name"));
    }
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// `2026-10-19 02:00:58 +0300`, as the JSON lists write dates (local time).
pub fn date_text(secs: i64) -> String {
    match chrono::DateTime::from_timestamp(secs, 0) {
        Some(t) => t
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S %z")
            .to_string(),
        None => String::new(),
    }
}

/// A list date back to seconds.
pub fn parse_date(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S %z")
        .ok()
        .map(|t| t.timestamp())
}

/// `Mon Oct 19 02:00:58 +03:00 2026`, as `server.properties` is stamped.
pub fn now_text() -> String {
    chrono::Local::now()
        .format("%a %b %d %H:%M:%S %Z %Y")
        .to_string()
}

/// `xxxxxxxx-xxxx-...` back to a number.
pub fn parse_uuid(s: &str) -> Option<u128> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}
