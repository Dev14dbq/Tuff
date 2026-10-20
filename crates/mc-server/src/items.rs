//! Item facts from the data generator's component reports
//! (`reports/minecraft/components/item/<name>.json`, the default components
//! of every item): stack size, food, how long using takes, what is left after
//! use, attack damage.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use mc_nbt::{Compound, Tag};
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
}

/// A stack and its other components, with any `minecraft:damage` among them
/// moved onto the stack (where clients see it).
pub fn split_held(
    mut stack: mc_protocol::packets::play::ItemStack,
    mut comps: Compound,
) -> (mc_protocol::packets::play::ItemStack, Compound) {
    if let Some(Tag::Int(d)) = comps.remove("minecraft:damage") {
        stack.damage = d;
    }
    stack.has_component_changes = !comps.is_empty();
    (stack, comps)
}

/// Item components for a stack worn by `damage`.
pub fn damage_components(damage: i32) -> Compound {
    let mut c = Compound::new();
    if damage != 0 {
        c.insert("minecraft:damage", Tag::Int(damage));
    }
    c
}

/// Saved `components` of a stack split into its damage (kept on the stack)
/// and the rest (kept as NBT beside it), `None` if nothing is left.
pub fn split_damage(components: Option<&Tag>) -> (i32, Option<Tag>) {
    let Some(Tag::Compound(c)) = components else {
        return (0, components.cloned());
    };
    let damage = match c.get("minecraft:damage") {
        Some(Tag::Int(d)) => *d,
        _ => 0,
    };
    let mut rest = c.clone();
    rest.remove("minecraft:damage");
    (damage, (!rest.is_empty()).then_some(Tag::Compound(rest)))
}

/// The `components` to save for a stack: the damage (when not 0) first,
/// then the rest kept beside it.
pub fn join_damage(damage: i32, rest: Option<&Tag>) -> Option<Tag> {
    if damage == 0 {
        return rest.cloned();
    }
    let mut c = Compound::new();
    c.insert("minecraft:damage", Tag::Int(damage));
    if let Some(Tag::Compound(rest)) = rest {
        for (k, v) in rest.iter() {
            if k != "minecraft:damage" {
                c.insert(k, v.clone());
            }
        }
    }
    Some(Tag::Compound(c))
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
