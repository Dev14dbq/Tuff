//! Game rules (`/gamerule`), kept as vanilla keeps them in the world's
//! `data/minecraft/game_rules.dat` (`minecraft:name` → byte or int). New
//! worlds start with vanilla's values (recorded from a vanilla server by
//! `cargo xtask observe-commands`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};
use tracing::warn;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i32),
}

impl Value {
    pub fn text(self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
        }
    }
}

#[derive(Debug, Default)]
pub struct GameRules {
    /// By full id (`minecraft:keep_inventory`).
    values: BTreeMap<String, Value>,
    file: Option<PathBuf>,
    data_version: i32,
}

pub const FILE: &str = "data/minecraft/game_rules.dat";

impl GameRules {
    pub fn load(data_dir: &Path, world: Option<&Path>, data_version: i32) -> Self {
        let observed = std::fs::read_to_string(data_dir.join("observed/commands.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        // Types come from the command tree: bools are the rules whose value
        // is a brigadier:bool.
        let tree = std::fs::read_to_string(data_dir.join("reports/commands.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let is_bool = |name: &str| {
            let short = name.trim_start_matches("minecraft:");
            tree.as_ref().is_some_and(|t| {
                t["children"]["gamerule"]["children"][short]["children"]["value"]["parser"]
                    == "brigadier:bool"
            })
        };
        let mut out = BTreeMap::new();
        match observed.as_ref().and_then(|v| v["game_rules"].as_object()) {
            Some(rules) => {
                for (k, v) in rules {
                    let n = v.as_i64().unwrap_or(0) as i32;
                    let value = if is_bool(k) {
                        Value::Bool(n != 0)
                    } else {
                        Value::Int(n)
                    };
                    out.insert(k.clone(), value);
                }
            }
            None => warn!("no default game rules (cargo xtask observe-commands)"),
        }
        // Behind an experimental feature, so new worlds do not store it.
        out.entry("minecraft:max_minecart_speed".into())
            .or_insert(Value::Int(8));
        let file = world.map(|w| w.join(FILE));
        if let Some(f) = &file
            && let Ok(root) = crate::world::read_nbt_gz_compound(f)
            && let Some(Tag::Compound(data)) = root.get("data")
        {
            for (k, v) in data.iter() {
                let n = match v {
                    Tag::Byte(b) => i32::from(*b),
                    Tag::Int(i) => *i,
                    _ => continue,
                };
                let value = match out.get(k) {
                    Some(Value::Bool(_)) => Value::Bool(n != 0),
                    _ => Value::Int(n),
                };
                out.insert(k.to_owned(), value);
            }
        }
        Self {
            values: out,
            file,
            data_version,
        }
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.values.get(&full(name)).copied()
    }

    pub fn bool(&self, name: &str) -> bool {
        matches!(self.get(name), Some(Value::Bool(true)))
    }

    pub fn int(&self, name: &str) -> i32 {
        match self.get(name) {
            Some(Value::Int(n)) => n,
            _ => 0,
        }
    }

    pub fn entries(&self) -> Vec<(String, Value)> {
        self.values
            .iter()
            .map(|(key, value)| (key.clone(), *value))
            .collect()
    }

    pub fn set(&mut self, name: &str, value: Value) {
        self.values.insert(full(name), value);
        self.save();
    }

    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut data = Compound::new();
        for (k, v) in &self.values {
            match v {
                Value::Bool(b) => data.insert(k.as_str(), Tag::Byte(i8::from(*b))),
                Value::Int(n) => data.insert(k.as_str(), Tag::Int(*n)),
            };
        }
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            warn!("saving game rules: {e:#}");
        }
    }
}

fn full(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}
