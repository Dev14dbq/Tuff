//! What enchantments do, read from their `effects` in the data pack: value
//! effects (`damage_protection`, `damage`, `item_damage`, `knockback`...)
//! under their `requirements`, for the stacks a player wears and holds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use mc_protocol::packets::play::ItemStack;
use serde_json::Value;

use crate::context::ServerContext;
use crate::tick::lock;

/// What the requirements of an effect may ask about.
#[derive(Clone, Copy, Debug, Default)]
pub struct Situation<'a> {
    /// The damage type of the hit (`minecraft:fall`).
    pub damage_type: Option<&'a str>,
    /// Whether the attacker hit directly (not by a projectile).
    pub direct: bool,
    /// Entity types: the one the effect is about (`this`), the attacker, the
    /// direct attacker (the arrow) and the victim.
    pub this: Option<&'a str>,
    pub attacker: Option<&'a str>,
    pub direct_attacker: Option<&'a str>,
    pub victim: Option<&'a str>,
}

/// A uniform roll in [0, 1).
fn roll() -> f32 {
    (crate::util::random_u64() >> 40) as f32 / (1u64 << 24) as f32
}

type TagCache = HashMap<(String, String), Arc<Vec<String>>>;

/// Members of a tag, cached (tags do not change while running).
fn tag(ctx: &ServerContext, registry: &str, name: &str) -> Arc<Vec<String>> {
    static CACHE: OnceLock<Mutex<TagCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let key = (registry.to_owned(), name.to_owned());
    if let Some(v) = lock(cache).get(&key) {
        return v.clone();
    }
    let v = Arc::new(ctx.commands.env.tag(registry, name));
    lock(cache).insert(key, v.clone());
    v
}

fn full(id: &str) -> String {
    mc_protocol::components::full_name(id).into_owned()
}

/// Whether `id` is `want`: an id, `#tag` or a list of ids.
fn is(ctx: &ServerContext, registry: &str, want: &Value, id: &str) -> bool {
    match want {
        Value::String(s) => match s.strip_prefix('#') {
            Some(t) => tag(ctx, registry, t).iter().any(|m| *m == id),
            None => full(s) == id,
        },
        Value::Array(l) => l.iter().any(|w| is(ctx, registry, w, id)),
        _ => false,
    }
}

/// Whether a damage type is in a damage type tag.
pub fn damage_is(ctx: &ServerContext, damage_type: &str, tag_name: &str) -> bool {
    tag(ctx, "minecraft:damage_type", tag_name)
        .iter()
        .any(|m| m == damage_type)
}

/// A level-based value (`linear`, `clamped`, `fraction`, `levels_squared`,
/// `lookup`, or a number).
pub fn value(v: &Value, level: i32) -> f32 {
    let l = level as f32;
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.0) as f32,
        Value::Object(o) => match o.get("type").and_then(Value::as_str).unwrap_or("") {
            "minecraft:linear" => {
                let base = o.get("base").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                let per = o
                    .get("per_level_above_first")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0) as f32;
                base + per * (l - 1.0)
            }
            "minecraft:clamped" => {
                let x = value(&o["value"], level);
                let min = o.get("min").and_then(Value::as_f64).unwrap_or(f64::MIN) as f32;
                let max = o.get("max").and_then(Value::as_f64).unwrap_or(f64::MAX) as f32;
                x.clamp(min, max)
            }
            "minecraft:fraction" => {
                let d = value(&o["denominator"], level);
                if d == 0.0 {
                    0.0
                } else {
                    value(&o["numerator"], level) / d
                }
            }
            "minecraft:levels_squared" => {
                l * l + o.get("added").and_then(Value::as_f64).unwrap_or(0.0) as f32
            }
            "minecraft:lookup" => {
                let values = o.get("values").and_then(Value::as_array);
                match values.and_then(|v| v.get(level as usize - 1)) {
                    Some(x) => x.as_f64().unwrap_or(0.0) as f32,
                    None => value(&o["fallback"], level),
                }
            }
            _ => 0.0,
        },
        _ => 0.0,
    }
}

/// The entity an `entity_properties` condition looks at.
fn entity<'a>(s: &Situation<'a>, which: &str) -> Option<&'a str> {
    match which {
        "this" => s.this,
        "attacker" | "attacking_player" => s.attacker,
        "direct_attacker" => s.direct_attacker,
        "damaged" | "victim" => s.victim,
        _ => None,
    }
}

/// Whether a condition holds (those we do not know fail).
pub fn holds(ctx: &ServerContext, c: &Value, item: &str, s: &Situation) -> bool {
    match c["condition"].as_str().or(c["type"].as_str()).unwrap_or("") {
        "minecraft:inverted" => !holds(ctx, &c["term"], item, s),
        "minecraft:all_of" => c["terms"]
            .as_array()
            .is_some_and(|t| t.iter().all(|t| holds(ctx, t, item, s))),
        "minecraft:any_of" => c["terms"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| holds(ctx, t, item, s))),
        "minecraft:match_tool" => {
            let p = &c["predicate"];
            p.get("items")
                .is_none_or(|w| is(ctx, "minecraft:item", w, item))
        }
        "minecraft:entity_properties" => {
            let Some(e) = entity(s, c["entity"].as_str().unwrap_or("this")) else {
                return false;
            };
            let p = &c["predicate"];
            p.get("type")
                .or(p.get("minecraft:entity_type"))
                .is_none_or(|w| is(ctx, "minecraft:entity_type", w, e))
        }
        "minecraft:damage_source_properties" => {
            let Some(dt) = s.damage_type else {
                return false;
            };
            let p = &c["predicate"];
            let tags_ok = p["tags"].as_array().into_iter().flatten().all(|t| {
                let id = t["id"].as_str().unwrap_or("");
                let id = id.strip_prefix('#').unwrap_or(id);
                damage_is(ctx, dt, id) == t["expected"].as_bool().unwrap_or(true)
            });
            let direct_ok = p
                .get("is_direct")
                .and_then(Value::as_bool)
                .is_none_or(|d| d == s.direct);
            tags_ok && direct_ok
        }
        "minecraft:random_chance" => roll() < value(&c["chance"], 1),
        _ => false,
    }
}

/// Applies one value effect to `v`.
fn apply(effect: &Value, level: i32, v: f32) -> f32 {
    match effect["type"].as_str().unwrap_or("") {
        "minecraft:add" => v + value(&effect["value"], level),
        "minecraft:multiply" => v * value(&effect["factor"], level),
        "minecraft:set" => value(&effect["value"], level),
        "minecraft:remove_binomial" => {
            let chance = value(&effect["chance"], level);
            let n = v.max(0.0) as i32;
            let removed = (0..n).filter(|_| roll() < chance).count();
            v - removed as f32
        }
        "minecraft:all_of" => effect["effects"]
            .as_array()
            .into_iter()
            .flatten()
            .fold(v, |v, e| apply(e, level, v)),
        _ => v,
    }
}

/// Whether an enchantment's slot groups cover the inventory slot.
fn in_slot(groups: &[String], slot: usize, main: usize) -> bool {
    groups.iter().any(|g| match g.as_str() {
        "any" => true,
        "armor" => (5..9).contains(&slot),
        "head" => slot == 5,
        "chest" => slot == 6,
        "legs" => slot == 7,
        "feet" => slot == 8,
        "hand" => slot == 45 || slot == main,
        "offhand" => slot == 45,
        "mainhand" => slot == main,
        _ => false,
    })
}

/// Runs one value effect component of every enchantment of `stack` on `v`.
pub fn modify(
    ctx: &ServerContext,
    component: &str,
    stack: &ItemStack,
    s: &Situation,
    v: f32,
) -> f32 {
    let item = ctx.items.name(stack.item).unwrap_or("").to_owned();
    let mut v = v;
    for (name, level) in crate::enchants::levels(stack) {
        let Some(e) = ctx.enchantments.get(&name) else {
            continue;
        };
        for entry in e.effects[component].as_array().into_iter().flatten() {
            if entry
                .get("requirements")
                .is_none_or(|r| holds(ctx, r, &item, s))
            {
                v = apply(&entry["effect"], level, v);
            }
        }
    }
    v
}

/// [`modify`] over a player's equipment: the stacks in slots whose
/// enchantments act from there.
pub fn modify_worn(ctx: &ServerContext, uuid: u128, component: &str, s: &Situation, v: f32) -> f32 {
    let stacks: Vec<(usize, ItemStack)> = {
        let inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get(&uuid) else {
            return v;
        };
        let main = hands.hand_slot(0);
        [5, 6, 7, 8, 45, main]
            .into_iter()
            .filter_map(|i| Some((i, hands.slots[i].clone()?)))
            .map(|(i, st)| (i, st, main))
            .filter(|(i, st, main)| {
                crate::enchants::levels(st).iter().any(|(n, _)| {
                    ctx.enchantments
                        .get(n)
                        .is_some_and(|e| in_slot(&e.slots, *i, *main))
                })
            })
            .map(|(i, st, _)| (i, st))
            .collect()
    };
    stacks
        .iter()
        .fold(v, |v, (_, st)| modify(ctx, component, st, s, v))
}

/// Whether one of a stack's enchantments has an effect component (curses:
/// `prevent_equipment_drop`, `prevent_armor_change`).
pub fn has_effect(ctx: &ServerContext, stack: &ItemStack, component: &str) -> bool {
    crate::enchants::levels(stack).iter().any(|(name, _)| {
        ctx.enchantments
            .get(name)
            .is_some_and(|e| e.effects.get(component).is_some())
    })
}

/// The `add_value` attribute modifiers (`minecraft:oxygen_bonus`) that the
/// enchantments a player wears and holds give.
pub fn attribute(ctx: &ServerContext, uuid: u128, attribute: &str) -> f32 {
    let stacks: Vec<(usize, ItemStack, usize)> = {
        let inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get(&uuid) else {
            return 0.0;
        };
        let main = hands.hand_slot(0);
        [5, 6, 7, 8, 45, main]
            .into_iter()
            .filter_map(|i| Some((i, hands.slots[i].clone()?, main)))
            .collect()
    };
    let mut sum = 0.0;
    for (slot, stack, main) in &stacks {
        for (name, level) in crate::enchants::levels(stack) {
            let Some(e) = ctx.enchantments.get(&name) else {
                continue;
            };
            if !in_slot(&e.slots, *slot, *main) {
                continue;
            }
            for m in e.effects["minecraft:attributes"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if m["attribute"] == attribute && m["operation"] == "add_value" {
                    sum += value(&m["amount"], level);
                }
            }
        }
    }
    sum
}

/// Damage after the protection enchantments a player wears (vanilla caps
/// their sum at 20, each point taking 4%).
pub fn protect(ctx: &ServerContext, uuid: u128, amount: f32, damage_type: &str) -> f32 {
    if damage_is(ctx, damage_type, "minecraft:bypasses_enchantments") {
        return amount;
    }
    let s = Situation {
        damage_type: Some(damage_type),
        ..Situation::default()
    };
    let p = modify_worn(ctx, uuid, "minecraft:damage_protection", &s, 0.0);
    amount * (1.0 - p.clamp(0.0, 20.0) / 25.0)
}

/// How much of `amount` wear unbreaking (`item_damage`) lets through;
/// none on `unbreakable` stacks.
pub fn wear(ctx: &ServerContext, stack: &ItemStack, amount: i32) -> i32 {
    if stack.components.get("unbreakable").is_some() {
        return 0;
    }
    let s = Situation::default();
    modify(ctx, "minecraft:item_damage", stack, &s, amount as f32).max(0.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_by_level() {
        let lin = json!({"type": "minecraft:linear", "base": 2.5, "per_level_above_first": 2.5});
        assert_eq!(value(&lin, 1), 2.5);
        assert_eq!(value(&lin, 5), 12.5);
        let frac = json!({"type": "minecraft:fraction",
            "numerator": {"type": "minecraft:linear", "base": 1.0, "per_level_above_first": 1.0},
            "denominator": {"type": "minecraft:linear", "base": 2.0, "per_level_above_first": 1.0}});
        assert_eq!(value(&frac, 3), 0.75);
        assert_eq!(value(&json!(100.0), 3), 100.0);
        let sq = json!({"type": "minecraft:levels_squared", "added": 1.0});
        assert_eq!(value(&sq, 3), 10.0);
    }

    #[test]
    fn effects_apply_in_order() {
        let add = json!({"type": "minecraft:add", "value": 2.0});
        assert_eq!(apply(&add, 1, 1.0), 3.0);
        let set = json!({"type": "minecraft:set", "value": 0.0});
        assert_eq!(apply(&set, 1, 7.0), 0.0);
        let never = json!({"type": "minecraft:remove_binomial", "chance": 0.0});
        assert_eq!(apply(&never, 1, 5.0), 5.0);
        let always = json!({"type": "minecraft:remove_binomial", "chance": 1.0});
        assert_eq!(apply(&always, 1, 5.0), 0.0);
    }

    #[test]
    fn slot_groups() {
        let g = |s: &str| vec![s.to_owned()];
        assert!(in_slot(&g("armor"), 6, 36));
        assert!(!in_slot(&g("armor"), 36, 36));
        assert!(in_slot(&g("mainhand"), 38, 38));
        assert!(in_slot(&g("hand"), 45, 36));
        assert!(!in_slot(&g("feet"), 5, 36));
    }
}
