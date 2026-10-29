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
        // `max_minecart_speed` is behind the `minecart_improvements`
        // feature, which we do not run: like vanilla without it, the rule
        // does not exist (no default, no command, not saved).
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
                // Vanilla drops what is not a rule of this world.
                let value = match out.get(k) {
                    Some(Value::Bool(_)) => Value::Bool(n != 0),
                    Some(Value::Int(_)) => Value::Int(n),
                    None => continue,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Vanilla 26.3's rules without experimental features, with their
    /// defaults (`GameRules` in the decompile).
    const VANILLA: &[(&str, Value)] = &[
        ("advance_time", Value::Bool(true)),
        ("advance_weather", Value::Bool(true)),
        ("allow_entering_nether_using_portals", Value::Bool(true)),
        ("block_drops", Value::Bool(true)),
        ("block_explosion_drop_decay", Value::Bool(true)),
        ("command_blocks_work", Value::Bool(true)),
        ("command_block_output", Value::Bool(true)),
        ("drowning_damage", Value::Bool(true)),
        ("elytra_movement_check", Value::Bool(true)),
        ("ender_pearls_vanish_on_death", Value::Bool(true)),
        ("entity_drops", Value::Bool(true)),
        ("fall_damage", Value::Bool(true)),
        ("fire_damage", Value::Bool(true)),
        ("fire_spread_radius_around_player", Value::Int(128)),
        ("forgive_dead_players", Value::Bool(true)),
        ("freeze_damage", Value::Bool(true)),
        ("global_sound_events", Value::Bool(true)),
        ("immediate_respawn", Value::Bool(false)),
        ("keep_inventory", Value::Bool(false)),
        ("lava_source_conversion", Value::Bool(false)),
        ("limited_crafting", Value::Bool(false)),
        ("locator_bar", Value::Bool(true)),
        ("log_admin_commands", Value::Bool(true)),
        ("max_block_modifications", Value::Int(32768)),
        ("max_command_forks", Value::Int(65536)),
        ("max_command_sequence_length", Value::Int(65536)),
        ("max_entity_cramming", Value::Int(24)),
        ("max_snow_accumulation_height", Value::Int(1)),
        ("mob_drops", Value::Bool(true)),
        ("mob_explosion_drop_decay", Value::Bool(true)),
        ("mob_griefing", Value::Bool(true)),
        ("natural_health_regeneration", Value::Bool(true)),
        ("player_movement_check", Value::Bool(true)),
        ("players_nether_portal_creative_delay", Value::Int(0)),
        ("players_nether_portal_default_delay", Value::Int(80)),
        ("players_sleeping_percentage", Value::Int(100)),
        ("projectiles_can_break_blocks", Value::Bool(true)),
        ("pvp", Value::Bool(true)),
        ("raids", Value::Bool(true)),
        ("random_tick_speed", Value::Int(3)),
        ("reduced_debug_info", Value::Bool(false)),
        ("respawn_radius", Value::Int(10)),
        ("send_command_feedback", Value::Bool(true)),
        ("show_advancement_messages", Value::Bool(true)),
        ("show_death_messages", Value::Bool(true)),
        ("spawner_blocks_work", Value::Bool(true)),
        ("spawn_mobs", Value::Bool(true)),
        ("spawn_monsters", Value::Bool(true)),
        ("spawn_patrols", Value::Bool(true)),
        ("spawn_phantoms", Value::Bool(true)),
        ("spawn_wandering_traders", Value::Bool(true)),
        ("spawn_wardens", Value::Bool(true)),
        ("spectators_generate_chunks", Value::Bool(true)),
        ("spread_vines", Value::Bool(true)),
        ("tnt_explodes", Value::Bool(true)),
        ("tnt_explosion_drop_decay", Value::Bool(false)),
        ("universal_anger", Value::Bool(false)),
        ("water_source_conversion", Value::Bool(true)),
    ];

    fn data_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3")
    }

    #[test]
    fn the_rules_their_types_and_defaults_are_vanillas() {
        let dir = data_dir();
        if !dir.join("observed/commands.json").is_file() {
            eprintln!("skipped: no game data");
            return;
        }
        let rules = GameRules::load(&dir, None, 0);
        let ours: Vec<(String, Value)> = rules.entries();
        let want: Vec<(String, Value)> = {
            let mut v: Vec<_> = VANILLA
                .iter()
                .map(|(k, v)| (format!("minecraft:{k}"), *v))
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        assert_eq!(ours, want);
    }

    #[test]
    fn a_saved_file_keeps_known_rules_and_drops_the_rest() {
        let dir = data_dir();
        if !dir.join("observed/commands.json").is_file() {
            eprintln!("skipped: no game data");
            return;
        }
        let world = std::env::temp_dir().join(format!("tuff-rules-{}", std::process::id()));
        let mut data = Compound::new();
        data.insert("minecraft:raids", Tag::Byte(0));
        data.insert("minecraft:max_entity_cramming", Tag::Int(5));
        data.insert("minecraft:max_minecart_speed", Tag::Int(20));
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        let file = world.join(FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        crate::world::write_nbt_gz(&file, &root).unwrap();
        let rules = GameRules::load(&dir, Some(&world), 0);
        assert_eq!(rules.get("raids"), Some(Value::Bool(false)));
        assert_eq!(rules.get("max_entity_cramming"), Some(Value::Int(5)));
        assert_eq!(rules.get("max_minecart_speed"), None);
        let _ = std::fs::remove_dir_all(&world);
    }
}
