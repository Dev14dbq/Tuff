//! The server's player lists, in vanilla's files and formats: `ops.json`,
//! `whitelist.json`, `banned-players.json`, `banned-ips.json` and
//! `usercache.json` next to `server.properties`. Each is read at start and
//! written back whenever it changes.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracing::warn;

use crate::util::{date_text, now_secs, parse_date, parse_uuid, uuid_string};

/// Vanilla keeps the 1000 most recently used profiles in `usercache.json`.
pub const USERCACHE_LIMIT: usize = 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub uuid: Option<u128>,
    pub name: String,
    pub level: i32,
    pub bypasses_player_limit: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Named {
    pub uuid: Option<u128>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ban {
    /// A player (uuid and name) or an address.
    pub uuid: Option<u128>,
    pub name: String,
    pub created: String,
    pub source: String,
    pub expires: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Lists {
    dir: Option<PathBuf>,
    pub ops: Vec<Op>,
    pub whitelist: Vec<Named>,
    pub banned_players: Vec<Ban>,
    pub banned_ips: Vec<Ban>,
    pub usercache: Vec<(Named, String)>,
}

fn read(path: &Path) -> Vec<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Array(a)) => a,
            _ => {
                warn!("{}: not a JSON list, starting empty", path.display());
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_owned()
}

fn uuid_of(v: &Value) -> Option<u128> {
    v["uuid"].as_str().and_then(parse_uuid)
}

/// A list entry's name: a JSON string (a legacy name list) or an object's
/// `key` field.
fn entry_name(v: &Value, key: &str) -> String {
    match v {
        Value::String(name) => name.clone(),
        other => s(other, key),
    }
}

/// Uuid from the entry, or from `usercache.json` when the entry is a bare
/// name or an object with no uuid. Unknown names stay name-only.
fn resolve_uuid(v: &Value, name: &str, cache: &[(Named, String)]) -> Option<u128> {
    if let Some(uuid) = uuid_of(v) {
        return Some(uuid);
    }
    if name.is_empty() {
        return None;
    }
    cache.iter().find_map(|(n, _)| {
        if n.name.eq_ignore_ascii_case(name) {
            n.uuid
        } else {
            None
        }
    })
}

fn ban_of(v: &Value, key: &str, cache: &[(Named, String)]) -> Ban {
    let name = entry_name(v, key);
    let uuid = if key == "name" {
        resolve_uuid(v, &name, cache)
    } else {
        uuid_of(v)
    };
    Ban {
        uuid,
        name,
        created: s(v, "created"),
        source: s(v, "source"),
        expires: s(v, "expires"),
        reason: s(v, "reason"),
    }
}

impl Lists {
    /// The lists in `dir`; missing files are created empty, as vanilla does.
    pub fn load(dir: &Path) -> Self {
        let mut l = Lists {
            dir: Some(dir.to_owned()),
            ..Lists::default()
        };
        let usercache: Vec<(Named, String)> = read(&dir.join("usercache.json"))
            .iter()
            .map(|v| {
                (
                    Named {
                        uuid: uuid_of(v),
                        name: s(v, "name"),
                    },
                    s(v, "expiresOn"),
                )
            })
            .collect();
        l.ops = read(&dir.join("ops.json"))
            .iter()
            .map(|v| {
                let name = entry_name(v, "name");
                Op {
                    uuid: resolve_uuid(v, &name, &usercache),
                    name,
                    level: v["level"].as_i64().unwrap_or(4) as i32,
                    bypasses_player_limit: v["bypassesPlayerLimit"].as_bool().unwrap_or(false),
                }
            })
            .collect();
        l.whitelist = read(&dir.join("whitelist.json"))
            .iter()
            .map(|v| {
                let name = entry_name(v, "name");
                Named {
                    uuid: resolve_uuid(v, &name, &usercache),
                    name,
                }
            })
            .collect();
        l.banned_players = read(&dir.join("banned-players.json"))
            .iter()
            .map(|v| ban_of(v, "name", &usercache))
            .collect();
        l.banned_ips = read(&dir.join("banned-ips.json"))
            .iter()
            .map(|v| ban_of(v, "ip", &usercache))
            .collect();
        l.usercache = usercache;
        l.save_all();
        l
    }

    fn write(&self, file: &str, list: Vec<Value>) {
        let Some(dir) = &self.dir else { return };
        let path = dir.join(file);
        let text =
            serde_json::to_string_pretty(&Value::Array(list)).unwrap_or_else(|_| "[]".into());
        if let Err(err) = std::fs::write(&path, text) {
            warn!("writing {}: {err}", path.display());
        }
    }

    /// Puts `uuid` first when known; entries still waiting for one (an
    /// `--op` name) leave it out, which vanilla skips rather than fails on.
    fn with_uuid(u: Option<u128>, mut v: Value) -> Value {
        if let (Some(u), Value::Object(map)) = (u, &mut v) {
            let mut out = serde_json::Map::new();
            out.insert("uuid".into(), Value::String(uuid_string(u)));
            out.extend(std::mem::take(map));
            return Value::Object(out);
        }
        v
    }

    pub fn save_ops(&self) {
        self.write(
            "ops.json",
            self.ops
                .iter()
                .map(|o| {
                    Self::with_uuid(
                        o.uuid,
                        json!({
                            "name": o.name,
                            "level": o.level,
                            "bypassesPlayerLimit": o.bypasses_player_limit,
                        }),
                    )
                })
                .collect(),
        );
    }

    pub fn save_whitelist(&self) {
        self.write(
            "whitelist.json",
            self.whitelist
                .iter()
                .map(|n| Self::with_uuid(n.uuid, json!({ "name": n.name })))
                .collect(),
        );
    }

    pub fn save_bans(&self) {
        let ban = |b: &Ban, key: &str| {
            let mut v = json!({
                "created": b.created,
                "source": b.source,
                "expires": b.expires,
                "reason": b.reason,
            });
            v[key] = Value::String(b.name.clone());
            if key == "name" {
                Self::with_uuid(b.uuid, v)
            } else {
                v
            }
        };
        self.write(
            "banned-players.json",
            self.banned_players.iter().map(|b| ban(b, "name")).collect(),
        );
        self.write(
            "banned-ips.json",
            self.banned_ips.iter().map(|b| ban(b, "ip")).collect(),
        );
    }

    /// `usercache.json` as vanilla writes it: one compact line, newest
    /// access first, at most [`USERCACHE_LIMIT`] entries, keys in the order
    /// `uuid`, `name`, `expiresOn`. Entries without a uuid are not kept.
    pub fn save_usercache(&self) {
        let Some(dir) = &self.dir else { return };
        let entries: Vec<String> = self
            .usercache
            .iter()
            .filter_map(|(n, e)| {
                Some(format!(
                    "{{\"uuid\":{},\"name\":{},\"expiresOn\":{}}}",
                    Value::String(uuid_string(n.uuid?)),
                    Value::String(n.name.clone()),
                    Value::String(e.clone()),
                ))
            })
            .take(USERCACHE_LIMIT)
            .collect();
        let path = dir.join("usercache.json");
        if let Err(err) = std::fs::write(&path, format!("[{}]", entries.join(","))) {
            warn!("writing {}: {err}", path.display());
        }
    }

    /// Remembers a profile for a month and makes it the newest entry.
    pub fn cache_profile(&mut self, uuid: u128, name: &str) {
        self.usercache
            .retain(|(n, _)| n.uuid != Some(uuid) && !n.name.eq_ignore_ascii_case(name));
        self.usercache.insert(
            0,
            (
                Named {
                    uuid: Some(uuid),
                    name: name.to_owned(),
                },
                date_text(now_secs() + 30 * 86_400),
            ),
        );
        self.usercache.truncate(USERCACHE_LIMIT);
        self.save_usercache();
    }

    /// The cached profile of a name (any letter case). An entry past its
    /// expiry is dropped and the file written again; a hit becomes the
    /// newest entry, as in vanilla.
    pub fn cached_by_name(&mut self, name: &str) -> Option<Named> {
        let at = self
            .usercache
            .iter()
            .position(|(n, _)| n.name.eq_ignore_ascii_case(name))?;
        if expired(&self.usercache[at].1) {
            self.usercache.remove(at);
            self.save_usercache();
            return None;
        }
        let entry = self.usercache.remove(at);
        let found = entry.0.clone();
        self.usercache.insert(0, entry);
        Some(found)
    }

    pub fn save_all(&self) {
        self.save_ops();
        self.save_whitelist();
        self.save_bans();
        self.save_usercache();
    }

    fn matches(uuid: Option<u128>, name: &str, who: u128, who_name: &str) -> bool {
        match uuid {
            Some(u) => u == who,
            None => name.eq_ignore_ascii_case(who_name),
        }
    }

    /// The op entry of a player (by uuid, or by name for entries without
    /// one).
    pub fn op(&self, uuid: u128, name: &str) -> Option<&Op> {
        self.ops
            .iter()
            .find(|o| Self::matches(o.uuid, &o.name, uuid, name))
    }

    pub fn whitelisted(&self, uuid: u128, name: &str) -> bool {
        self.whitelist
            .iter()
            .any(|n| Self::matches(n.uuid, &n.name, uuid, name))
    }

    /// The ban on a player, unless it ran out.
    pub fn player_ban(&self, uuid: u128, name: &str) -> Option<&Ban> {
        self.banned_players
            .iter()
            .find(|b| Self::matches(b.uuid, &b.name, uuid, name) && !expired(&b.expires))
    }

    pub fn ip_ban(&self, ip: &str) -> Option<&Ban> {
        self.banned_ips
            .iter()
            .find(|b| b.name == ip && !expired(&b.expires))
    }

    /// Fills in uuids for name-only entries now that the player is known,
    /// and remembers the player in the user cache for a month.
    pub fn seen(&mut self, uuid: u128, name: &str) {
        let mut ops_changed = false;
        for o in &mut self.ops {
            if o.uuid.is_none() && o.name.eq_ignore_ascii_case(name) {
                o.uuid = Some(uuid);
                o.name = name.to_owned();
                ops_changed = true;
            }
        }
        if ops_changed {
            self.save_ops();
        }
        let mut wl_changed = false;
        for n in &mut self.whitelist {
            if n.uuid.is_none() && n.name.eq_ignore_ascii_case(name) {
                n.uuid = Some(uuid);
                wl_changed = true;
            }
        }
        if wl_changed {
            self.save_whitelist();
        }
        self.cache_profile(uuid, name);
    }

    /// An op without a uuid yet (the `--op` option); it is filled in on join.
    pub fn add_op_name(&mut self, name: &str) {
        if self.ops.iter().any(|o| o.name.eq_ignore_ascii_case(name)) {
            return;
        }
        self.ops.push(Op {
            uuid: None,
            name: name.to_owned(),
            level: 4,
            bypasses_player_limit: false,
        });
        self.save_ops();
    }
}

/// `forever`, or a date in the past.
fn expired(expires: &str) -> bool {
    if expires.is_empty() || expires == "forever" {
        return false;
    }
    parse_date(expires).is_some_and(|t| t < now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-lists-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn usercache_is_compact_newest_first_and_capped() {
        let dir = temp("cap");
        let mut lists = Lists::load(&dir);
        for i in 0..USERCACHE_LIMIT as u128 + 5 {
            lists.cache_profile(i + 1, &format!("p{i}"));
        }
        assert_eq!(lists.usercache.len(), USERCACHE_LIMIT);
        assert_eq!(
            lists.usercache[0].0.name,
            format!("p{}", USERCACHE_LIMIT + 4)
        );
        let text = std::fs::read_to_string(dir.join("usercache.json")).unwrap();
        assert!(!text.contains('\n'));
        assert!(text.starts_with("[{\"uuid\":\""));
        let first: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(first[0]["name"], format!("p{}", USERCACHE_LIMIT + 4));
        assert!(first[0]["expiresOn"].as_str().unwrap().len() > 20);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn usercache_drops_expired_names_and_refreshes_hits() {
        let dir = temp("expiry");
        std::fs::write(
            dir.join("usercache.json"),
            r#"[{"uuid":"00000000-0000-0000-0000-000000000001","name":"Old","expiresOn":"2001-01-01 00:00:00 +0000"},{"uuid":"00000000-0000-0000-0000-000000000002","name":"Fresh","expiresOn":"2999-01-01 00:00:00 +0000"},{"uuid":"00000000-0000-0000-0000-000000000003","name":"Other","expiresOn":"2999-01-01 00:00:00 +0000"}]"#,
        )
        .unwrap();
        let mut lists = Lists::load(&dir);
        assert!(lists.cached_by_name("old").is_none());
        assert_eq!(lists.usercache.len(), 2);
        assert_eq!(lists.cached_by_name("OTHER").unwrap().uuid, Some(3));
        assert_eq!(lists.usercache[0].0.name, "Other");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_whitelist_names_resolve_through_usercache() {
        let dir = std::env::temp_dir().join(format!("mc-lists-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let uuid = parse_uuid("853c80ef-3c37-49fd-aa49-938b674adae6").unwrap();
        std::fs::write(
            dir.join("usercache.json"),
            serde_json::json!([{
                "name": "Steve",
                "uuid": uuid_string(uuid),
                "expiresOn": "2027-01-01 00:00:00 +0000"
            }])
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("whitelist.json"), r#"["Steve", "Alex"]"#).unwrap();
        let lists = Lists::load(&dir);
        let steve = lists
            .whitelist
            .iter()
            .find(|n| n.name == "Steve")
            .expect("cached name is kept");
        assert_eq!(steve.uuid, Some(uuid));
        let alex = lists
            .whitelist
            .iter()
            .find(|n| n.name == "Alex")
            .expect("unknown name is kept");
        assert_eq!(alex.uuid, None);
        assert!(lists.whitelisted(0, "Alex"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
