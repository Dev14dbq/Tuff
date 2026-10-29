//! How the server shows itself before anyone is in (minecraft.wiki,
//! "Server.properties", "Java Edition protocol/Server List Ping"): the
//! server list answer with `server-icon.png` and, unless
//! `hide-online-players`, a sample of who is on; and the resource pack
//! offered as players join.

use std::path::Path;

use mc_nbt::Tag;

use crate::context::ServerContext;
use crate::properties::Properties;

/// The icon file must be a 64×64 PNG.
const ICON_SIZE: u32 = 64;

/// How many players the server list sample names.
const SAMPLE: usize = 12;

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The width and height a PNG's header gives.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if bytes.get(..8)? != SIGNATURE || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let int = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    Some((int(16)?, int(20)?))
}

/// `server-icon.png` as a data URL, if there is one and it is a 64×64 PNG.
pub fn icon(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    match png_size(&bytes) {
        Some((ICON_SIZE, ICON_SIZE)) => Some(format!("data:image/png;base64,{}", base64(&bytes))),
        Some((w, h)) => {
            tracing::warn!(
                "Couldn't load server icon: it must be {ICON_SIZE} pixels wide and high, not {w}x{h}"
            );
            None
        }
        None => {
            tracing::warn!("Couldn't load server icon: it is not a PNG");
            None
        }
    }
}

/// The server list answer.
pub fn status(ctx: &ServerContext) -> serde_json::Value {
    let mut players = serde_json::json!({
        "max": ctx.player_limit.load(std::sync::atomic::Ordering::Relaxed),
        "online": ctx.online_count(),
    });
    if !ctx.config.hide_online_players {
        let sample: Vec<_> = ctx
            .players
            .lock()
            .expect("players lock")
            .values()
            .take(SAMPLE)
            .map(|p| {
                serde_json::json!({
                    "name": p.profile.name,
                    "id": crate::util::uuid_string(p.profile.uuid),
                })
            })
            .collect();
        players["sample"] = serde_json::Value::Array(sample);
    }
    let mut json = serde_json::json!({
        "version": {"name": ctx.data.version.name, "protocol": mc_protocol::PROTOCOL_VERSION},
        "players": players,
        "description": {"text": ctx.shown_motd.lock().expect("motd lock").clone()},
        "enforcesSecureChat": ctx.config.enforce_secure_profile,
    });
    if let Some(icon) = &ctx.config.icon {
        json["favicon"] = serde_json::Value::String(icon.clone());
    }
    json
}

/// The pack offered to joining players.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourcePack {
    pub id: u128,
    pub url: String,
    /// SHA-1 of the pack, lowercase hex; may be empty.
    pub hash: String,
    /// Declining it (or failing to load it) is a kick.
    pub required: bool,
    pub prompt: Option<Tag>,
}

/// Built-in server link ids, in protocol order.
const BUILTIN_LINKS: &[&str] = &[
    "bug_report",
    "community_guidelines",
    "support",
    "status",
    "feedback",
    "community",
    "website",
    "forums",
    "news",
    "announcements",
];

fn builtin_id(name: &str) -> Option<i32> {
    BUILTIN_LINKS
        .iter()
        .position(|n| *n == name)
        .map(|i| i as i32)
}

/// `server-links.json` next to `server.properties`, plus `bug-report-link`
/// when that property is set and the file does not already name `bug_report`.
/// A missing or empty file sends nothing unless the property has a URL.
/// Each object is either `{"builtin":"website","url":"https://..."}` or
/// `{"label":"Custom","url":"https://..."}`.
pub fn server_links(server_dir: Option<&Path>) -> Option<Vec<mc_protocol::packets::ServerLink>> {
    use mc_protocol::packets::{ServerLink, ServerLinkLabel};

    let dir = server_dir?;
    let mut links = Vec::new();
    let path = dir.join("server-links.json");
    match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => {}
        Ok(text) => links.extend(parse_link_file(&path, &text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("Couldn't read {}: {e}", path.display()),
    }
    let bug = property_line(&dir.join("server.properties"), "bug-report-link");
    let has_bug = links
        .iter()
        .any(|link| matches!(link.label, ServerLinkLabel::BuiltIn(0)));
    if !bug.is_empty() && !has_bug {
        links.insert(
            0,
            ServerLink {
                label: ServerLinkLabel::BuiltIn(0),
                url: bug,
            },
        );
    }
    if links.is_empty() { None } else { Some(links) }
}

fn parse_link_file(path: &Path, text: &str) -> Vec<mc_protocol::packets::ServerLink> {
    use mc_protocol::packets::{ServerLink, ServerLinkLabel};

    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!("Couldn't parse {}: {e}", path.display());
            return Vec::new();
        }
    };
    let Some(entries) = value.as_array() else {
        tracing::warn!("{} must be a JSON array", path.display());
        return Vec::new();
    };
    let mut links = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        let Some(url) = entry.get("url").and_then(|u| u.as_str()) else {
            tracing::warn!("{} entry {i} has no url", path.display());
            continue;
        };
        let url = url.to_owned();
        if let Some(name) = entry.get("builtin").and_then(|v| v.as_str()) {
            match builtin_id(name) {
                Some(id) => links.push(ServerLink {
                    label: ServerLinkLabel::BuiltIn(id),
                    url,
                }),
                None => tracing::warn!("{} entry {i} has unknown builtin {name:?}", path.display()),
            }
        } else if let Some(label) = entry.get("label").and_then(|v| v.as_str()) {
            links.push(ServerLink {
                label: ServerLinkLabel::Custom(mc_protocol::text::plain(label)),
                url,
            });
        } else {
            tracing::warn!("{} entry {i} needs a builtin id or a label", path.display());
        }
    }
    links
}

fn property_line(path: &Path, key: &str) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if let Some((name, value)) = line.split_once('=')
            && name.trim() == key
        {
            return value.trim().to_owned();
        }
    }
    String::new()
}

/// The code of conduct to show, or nothing. The flag off never sends it,
/// even when `codeofconduct.txt` is there. The flag on with a missing or
/// empty file logs a warning and continues without the packet.
pub fn code_of_conduct(enabled: bool, server_dir: Option<&Path>) -> Option<String> {
    if !enabled {
        return None;
    }
    let Some(dir) = server_dir else {
        tracing::warn!(
            "enable-code-of-conduct is true, but there is no server folder for codeofconduct.txt"
        );
        return None;
    };
    let path = dir.join("codeofconduct.txt");
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.is_empty() => Some(cap_text(text, 32767)),
        Ok(_) => {
            tracing::warn!(
                "enable-code-of-conduct is true, but {} is empty",
                path.display()
            );
            None
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                "enable-code-of-conduct is true, but {} is missing",
                path.display()
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "enable-code-of-conduct is true, but {} could not be read: {e}",
                path.display()
            );
            None
        }
    }
}

/// Protocol strings count UTF-16 code units.
fn cap_text(text: String, max_utf16: usize) -> String {
    if text.encode_utf16().count() <= max_utf16 {
        return text;
    }
    tracing::warn!("codeofconduct.txt is longer than {max_utf16} characters; the rest is left out");
    let mut units = 0;
    let mut end = text.len();
    for (i, ch) in text.char_indices() {
        let n = ch.len_utf16();
        if units + n > max_utf16 {
            end = i;
            break;
        }
        units += n;
    }
    text[..end].to_owned()
}

/// A name-based (version 3) UUID of some bytes, as vanilla makes a pack's
/// id from its URL when `resource-pack-id` is empty.
fn name_uuid(bytes: &[u8]) -> u128 {
    use md5::Digest;
    let mut hash: [u8; 16] = md5::Md5::digest(bytes).into();
    hash[6] = hash[6] & 0x0f | 0x30;
    hash[8] = hash[8] & 0x3f | 0x80;
    u128::from_be_bytes(hash)
}

fn parse_uuid(s: &str) -> Option<u128> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    (hex.len() == 32).then(|| u128::from_str_radix(&hex, 16).ok())?
}

impl ResourcePack {
    /// From `resource-pack`, `resource-pack-sha1`, `resource-pack-id`,
    /// `require-resource-pack` and `resource-pack-prompt`; `None` without a
    /// URL.
    pub fn from_properties(p: &Properties) -> Option<Self> {
        let url = p.get("resource-pack").trim().to_owned();
        if url.is_empty() {
            return None;
        }
        let hash = p.get("resource-pack-sha1").trim().to_lowercase();
        if !hash.is_empty() && (hash.len() != 40 || !hash.chars().all(|c| c.is_ascii_hexdigit())) {
            tracing::warn!("Invalid sha1 for resource-pack-sha1");
        }
        if hash.is_empty() {
            tracing::warn!(
                "You specified a resource pack without providing a sha1 hash. Pack will be updated on the client only if you change the name of the pack."
            );
        }
        let id = p.get("resource-pack-id").trim();
        let id = if id.is_empty() {
            name_uuid(url.as_bytes())
        } else {
            parse_uuid(id).unwrap_or_else(|| {
                tracing::warn!(
                    "Failed to parse resource pack id {id}, using one made from the URL"
                );
                name_uuid(url.as_bytes())
            })
        };
        let prompt = p.get("resource-pack-prompt").trim();
        let prompt = (!prompt.is_empty()).then(|| {
            serde_json::from_str::<serde_json::Value>(prompt)
                .ok()
                .and_then(|v| mc_data::json_to_nbt(&v))
                .unwrap_or_else(|| mc_protocol::text::plain(prompt))
        });
        Some(Self {
            id,
            url,
            hash,
            required: p.bool("require-resource-pack"),
            prompt,
        })
    }
}

/// What the client said of the pack: whether it is done with it, and
/// whether that is a refusal.
pub fn pack_outcome(result: i32) -> (bool, bool) {
    match result {
        // Loaded.
        0 => (true, false),
        // Accepted, downloaded: still going.
        3 | 4 => (false, false),
        // Declined, failed to download, bad URL, failed to reload,
        // discarded.
        _ => (true, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_like_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn only_a_64_pixel_png_is_an_icon() {
        let dir = std::env::temp_dir().join(format!("mc-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = |w: u32, h: u32| {
            let mut b = vec![
                0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13,
            ];
            b.extend_from_slice(b"IHDR");
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&h.to_be_bytes());
            b
        };
        let path = dir.join("server-icon.png");
        std::fs::write(&path, png(64, 64)).unwrap();
        assert!(
            icon(&path)
                .unwrap()
                .starts_with("data:image/png;base64,iVBORw0KGgo")
        );
        std::fs::write(&path, png(128, 128)).unwrap();
        assert!(icon(&path).is_none());
        std::fs::write(&path, b"not a png").unwrap();
        assert!(icon(&path).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pack_without_an_id_takes_one_from_its_url() {
        let dir = std::env::temp_dir().join(format!("mc-pack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = Properties::load(&dir.join("server.properties")).unwrap();
        assert!(ResourcePack::from_properties(&p).is_none());
        p.set("resource-pack", "https://example.com/pack.zip");
        p.set("require-resource-pack", "true");
        p.set("resource-pack-prompt", "{\"text\":\"Please\"}");
        let pack = ResourcePack::from_properties(&p).unwrap();
        assert_eq!(pack.id, name_uuid(b"https://example.com/pack.zip"));
        assert_eq!(pack.id >> 76 & 0xf, 3);
        assert!(pack.required);
        assert!(pack.prompt.is_some());
        p.set("resource-pack-id", "0b4b5f7c-0000-4000-8000-000000000001");
        let pack = ResourcePack::from_properties(&p).unwrap();
        assert_eq!(pack.id, 0x0b4b5f7c_0000_4000_8000_000000000001);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn code_of_conduct_is_not_sent_when_the_flag_is_false() {
        let dir = std::env::temp_dir().join(format!("mc-conduct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("codeofconduct.txt"), "Be kind.").unwrap();
        assert!(code_of_conduct(false, Some(&dir)).is_none());
        assert_eq!(
            code_of_conduct(true, Some(&dir)).as_deref(),
            Some("Be kind.")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refused_or_failed_pack_is_done_and_a_refusal() {
        assert_eq!(pack_outcome(0), (true, false));
        assert_eq!(pack_outcome(3), (false, false));
        assert_eq!(pack_outcome(4), (false, false));
        for r in [1, 2, 5, 6, 7] {
            assert_eq!(pack_outcome(r), (true, true));
        }
    }

    #[test]
    fn bug_report_link_is_sent_when_the_json_file_is_absent() {
        let dir = std::env::temp_dir().join(format!("mc-bug-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("server.properties"),
            "bug-report-link=https://example.com/bugs\n",
        )
        .unwrap();
        let links = server_links(Some(&dir)).unwrap();
        assert_eq!(links.len(), 1);
        assert!(matches!(
            links[0].label,
            mc_protocol::packets::ServerLinkLabel::BuiltIn(0)
        ));
        assert_eq!(links[0].url, "https://example.com/bugs");
        let _ = std::fs::remove_dir_all(dir);
    }
}
