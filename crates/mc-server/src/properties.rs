//! `server.properties`, as vanilla 26.3 writes it (every key with its
//! default, observed with `server.jar --initSettings`): read at start, keys
//! missing from the file added with their defaults and the file written
//! back, as vanilla does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Keys and defaults of vanilla 26.3's `server.properties`. `server-ip` is
/// the one difference: this server listens on 127.0.0.1 unless told
/// otherwise (empty means every address, as vanilla).
pub const DEFAULTS: &[(&str, &str)] = &[
    ("accepts-transfers", "false"),
    ("allow-flight", "false"),
    ("broadcast-console-to-ops", "true"),
    ("broadcast-rcon-to-ops", "true"),
    ("bug-report-link", ""),
    ("chat-spam-threshold-seconds", "10"),
    ("command-spam-threshold-seconds", "10"),
    ("difficulty", "easy"),
    ("enable-code-of-conduct", "false"),
    ("enable-jmx-monitoring", "false"),
    ("enable-query", "false"),
    ("enable-rcon", "false"),
    ("enable-status", "true"),
    ("enforce-secure-profile", "true"),
    ("enforce-whitelist", "false"),
    ("entity-broadcast-range-percentage", "100"),
    ("force-gamemode", "false"),
    ("function-permission-level", "2"),
    ("gamemode", "survival"),
    ("generate-structures", "true"),
    ("generator-settings", "{}"),
    ("hardcore", "false"),
    ("hide-online-players", "false"),
    ("initial-disabled-packs", ""),
    ("initial-enabled-packs", "vanilla"),
    ("level-name", "world"),
    ("level-seed", ""),
    ("level-type", "minecraft\\:normal"),
    ("log-ips", "true"),
    ("management-server-allowed-origins", ""),
    ("management-server-enabled", "false"),
    ("management-server-host", "localhost"),
    ("management-server-port", "0"),
    ("management-server-secret", ""),
    ("management-server-tls-enabled", "true"),
    ("management-server-tls-keystore", ""),
    ("management-server-tls-keystore-password", ""),
    ("max-chained-neighbor-updates", "1000000"),
    ("max-players", "20"),
    ("max-tick-time", "60000"),
    ("max-world-size", "29999984"),
    ("motd", "A Minecraft Server"),
    ("network-compression-threshold", "256"),
    ("online-mode", "true"),
    ("op-permission-level", "4"),
    ("pause-when-empty-seconds", "60"),
    ("player-idle-timeout", "0"),
    ("prevent-proxy-connections", "false"),
    ("query.port", "25565"),
    ("rate-limit", "0"),
    ("rcon.password", ""),
    ("rcon.port", "25575"),
    ("region-file-compression", "deflate"),
    ("require-resource-pack", "false"),
    ("resource-pack", ""),
    ("resource-pack-id", ""),
    ("resource-pack-prompt", ""),
    ("resource-pack-sha1", ""),
    ("server-ip", "127.0.0.1"),
    ("server-port", "25565"),
    ("simulation-distance", "10"),
    ("spawn-protection", "16"),
    ("status-heartbeat-interval", "0"),
    ("sync-chunk-writes", "true"),
    ("text-filtering-config", ""),
    ("text-filtering-version", "0"),
    ("use-native-transport", "true"),
    ("view-distance", "10"),
    ("white-list", "true"),
];

#[derive(Debug, Clone)]
pub struct Properties {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

/// `a\:b` → `a:b` (the properties format escapes `:` and `=`).
fn unescape(v: &str) -> String {
    let mut out = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape(v: &str) -> String {
    let mut out = String::new();
    for c in v.chars() {
        match c {
            ':' | '=' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

impl Properties {
    /// Reads `path` (or starts from the defaults), fills in missing keys and
    /// writes the file back.
    pub fn load(path: &Path) -> Result<Self> {
        let mut values: BTreeMap<String, String> = DEFAULTS
            .iter()
            .map(|(k, v)| ((*k).to_owned(), unescape(v)))
            .collect();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let line = line.trim_start();
                if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    values.insert(unescape(k.trim()), unescape(v));
                }
            }
        }
        let p = Self {
            path: path.to_owned(),
            values,
        };
        p.save()?;
        Ok(p)
    }

    pub fn save(&self) -> Result<()> {
        let mut text = String::from("#Minecraft server properties\n");
        text.push_str(&format!("#{}\n", crate::util::now_text()));
        for (k, v) in &self.values {
            text.push_str(&format!("{}={}\n", escape(k), escape(v)));
        }
        std::fs::write(&self.path, text).with_context(|| format!("writing {}", self.path.display()))
    }

    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map_or("", String::as_str)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key).eq_ignore_ascii_case("true")
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).trim().parse().ok()
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_owned(), value.to_owned());
    }
}

/// Changes one key of the folder's `server.properties` (`/whitelist on`).
pub fn set(dir: &Path, key: &str, value: &str) {
    let path = dir.join("server.properties");
    match Properties::load(&path) {
        Ok(mut p) => {
            p.set(key, value);
            if let Err(e) = p.save() {
                tracing::warn!("saving {}: {e:#}", path.display());
            }
        }
        Err(e) => tracing::warn!("reading {}: {e:#}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_in_and_keeps_values() {
        let dir = std::env::temp_dir().join(format!("mc-props-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.properties");
        std::fs::write(
            &path,
            "#x\nmotd=Hello\\: world\nserver-port=25570\nlevel-type=minecraft\\:flat\n",
        )
        .unwrap();
        let p = Properties::load(&path).unwrap();
        assert_eq!(p.get("motd"), "Hello: world");
        assert_eq!(p.int("server-port"), Some(25570));
        assert_eq!(p.get("level-type"), "minecraft:flat");
        assert!(p.bool("white-list"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("level-type=minecraft\\:flat\n"));
        assert!(text.contains("view-distance=10\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
