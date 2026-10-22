//! Item facts from the data generator's component reports
//! (`reports/minecraft/components/item/<name>.json`, the default components
//! of every item): stack size, food, how long using takes, what is left after
//! use, attack damage.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Food {
    pub nutrition: i32,
    /// Saturation points it gives.
    pub saturation: f32,
    /// Can be eaten when full.
    pub always: bool,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: String,
    pub max_stack: i32,
    pub food: Option<Food>,
    /// Ticks to eat or drink it; `None` if it is not consumed by using it.
    pub use_ticks: Option<i32>,
    /// What using it leaves in the hand (a bottle, a bucket).
    pub remainder: Option<String>,
    /// Extra main-hand attack damage.
    pub attack_damage: f32,
    /// As fuel: the burn time and speed providers (`cooking_fuel`).
    pub fuel: Option<(String, String)>,
    /// Has `max_damage` (and so a `damage` component).
    pub damageable: bool,
    /// `max_damage`, 0 for items that do not wear.
    pub max_damage: i32,
    /// What repairs it on an anvil: item names or one `#tag`.
    pub repairable: Vec<String>,
    /// `enchantable` value (how well it takes enchantments), if it does.
    pub enchantable: Option<i32>,
    /// `jukebox_playable`: the song's id.
    pub jukebox_song: Option<String>,
    /// Every default component (`minecraft:dye`...), as in the report.
    pub defaults: Value,
}

#[derive(Debug, Default)]
pub struct Items {
    by_id: Vec<Option<Item>>,
    ids: HashMap<String, i32>,
}

impl Items {
    /// Reads the reports under `data_dir`; `ids` is the item registry.
    pub fn load(data_dir: &Path, ids: &HashMap<String, i32>) -> Result<Self> {
        let dir = data_dir.join("reports/minecraft/components/item");
        let mut by_id = vec![None; ids.values().map(|&i| i as usize + 1).max().unwrap_or(0)];
        for (name, &id) in ids {
            let file = dir.join(format!("{}.json", name.trim_start_matches("minecraft:")));
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("reading {}", file.display()))?;
            by_id[id as usize] = Some(parse(name, &v["components"]));
        }
        Ok(Self {
            by_id,
            ids: ids.clone(),
        })
    }

    pub fn get(&self, id: i32) -> Option<&Item> {
        self.by_id.get(usize::try_from(id).ok()?)?.as_ref()
    }

    pub fn id(&self, name: &str) -> Option<i32> {
        self.ids.get(name).copied()
    }

    pub fn name(&self, id: i32) -> Option<&str> {
        self.get(id).map(|i| i.name.as_str())
    }

    pub fn max_stack(&self, id: i32) -> i32 {
        self.get(id).map_or(64, |i| i.max_stack)
    }

    pub fn max_damage(&self, id: i32) -> i32 {
        self.get(id).map_or(0, |i| i.max_damage)
    }

    /// A default component of an item (`dye` or `minecraft:dye`).
    pub fn default_component(&self, id: i32, name: &str) -> Option<&Value> {
        let name = mc_protocol::components::full_name(name);
        self.get(id)?.defaults.get(name.as_ref())
    }

    /// How well it takes enchantments; 0 if not at all.
    pub fn enchantable(&self, id: i32) -> i32 {
        self.get(id).and_then(|i| i.enchantable).unwrap_or(0)
    }
}

/// A stack of `item` with saved components (damage among them).
pub fn stack_with(
    item: i32,
    count: i32,
    comps: Option<&mc_nbt::Compound>,
) -> mc_protocol::packets::play::ItemStack {
    let mut stack = mc_protocol::packets::play::ItemStack::new(item, count);
    if let Some(c) = comps {
        (stack.damage, stack.components) = mc_protocol::components::Components::from_nbt(c);
    }
    stack
}

fn parse(name: &str, c: &Value) -> Item {
    let food = c.get("minecraft:food").map(|f| Food {
        nutrition: f["nutrition"].as_i64().unwrap_or(0) as i32,
        saturation: f["saturation"].as_f64().unwrap_or(0.0) as f32,
        always: f["can_always_eat"].as_bool().unwrap_or(false),
    });
    // Consumables take 1.6 s unless they say otherwise.
    let use_ticks = c
        .get("minecraft:consumable")
        .map(|u| (u["consume_seconds"].as_f64().unwrap_or(1.6) * 20.0).round() as i32);
    let attack_damage = c["minecraft:attribute_modifiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["type"] == "minecraft:attack_damage" && m["operation"] == "add_value")
        .filter_map(|m| m["amount"].as_f64())
        .sum::<f64>() as f32;
    Item {
        defaults: c.clone(),
        name: name.to_owned(),
        max_stack: c["minecraft:max_stack_size"].as_i64().unwrap_or(64) as i32,
        food,
        use_ticks,
        remainder: c["minecraft:use_remainder"]["id"]
            .as_str()
            .map(str::to_owned),
        attack_damage,
        damageable: c.get("minecraft:max_damage").is_some(),
        max_damage: c["minecraft:max_damage"].as_i64().unwrap_or(0) as i32,
        repairable: match &c["minecraft:repairable"]["items"] {
            Value::String(s) => vec![s.clone()],
            Value::Array(list) => list
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        },
        jukebox_song: match &c["minecraft:jukebox_playable"] {
            Value::String(s) => Some(s.clone()),
            other => other["song"].as_str().map(str::to_owned),
        },
        enchantable: c["minecraft:enchantable"]["value"]
            .as_i64()
            .map(|v| v as i32),
        fuel: c.get("minecraft:cooking_fuel").and_then(|f| {
            Some((
                f["burn_time"].as_str()?.to_owned(),
                f["speed_multiplier"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ))
        }),
    }
}
