//! The server's player lists, in vanilla's files and formats: `ops.json`,
//! `whitelist.json`, `banned-players.json`, `banned-ips.json` and
//! `usercache.json` next to `server.properties`. Each is read at start and
//! written back whenever it changes.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracing::warn;

use crate::util::{date_text, now_secs, parse_date, parse_uuid, uuid_string};

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

fn ban_of(v: &Value, key: &str) -> Ban {
    Ban {
        uuid: uuid_of(v),
        name: s(v, key),
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
        l.ops = read(&dir.join("ops.json"))
            .iter()
            .map(|v| Op {
                uuid: uuid_of(v),
                name: s(v, "name"),
                level: v["level"].as_i64().unwrap_or(4) as i32,
                bypasses_player_limit: v["bypassesPlayerLimit"].as_bool().unwrap_or(false),
            })
            .collect();
        l.whitelist = read(&dir.join("whitelist.json"))
            .iter()
            .map(|v| Named {
                uuid: uuid_of(v),
                name: s(v, "name"),
            })
            .collect();
        l.banned_players = read(&dir.join("banned-players.json"))
            .iter()
            .map(|v| ban_of(v, "name"))
            .collect();
        l.banned_ips = read(&dir.join("banned-ips.json"))
            .iter()
            .map(|v| ban_of(v, "ip"))
            .collect();
        l.usercache = read(&dir.join("usercache.json"))
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

    pub fn save_usercache(&self) {
        self.write(
            "usercache.json",
            self.usercache
                .iter()
                .map(|(n, e)| {
                    let mut v = json!({ "name": n.name });
                    if let Some(u) = n.uuid {
                        v["uuid"] = Value::String(uuid_string(u));
                    }
                    v["expiresOn"] = Value::String(e.clone());
                    v
                })
                .collect(),
        );
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
        self.usercache.truncate(1000);
        self.save_usercache();
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
