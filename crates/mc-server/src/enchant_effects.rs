//! What enchantments do, read from their `effects` in the data pack: value
//! effects (`damage_protection`, `damage`, `item_damage`, `knockback`...)
//! under their `requirements`, for the stacks a player wears and holds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use mc_protocol::packets::play::{AttributeModifier, ItemStack};
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
    /// The enchantment's level (`enchantment_level` chances).
    pub level: i32,
    /// Whether a location effect was on before (`enchantment_active_check`).
    pub active: bool,
    /// How the entity the predicates look at moves and stands.
    pub mover: Option<&'a Mover>,
    pub raining: bool,
    pub thundering: bool,
}

/// What `entity_properties` predicates read of a moving entity.
#[derive(Clone, Debug, Default)]
pub struct Mover {
    pub on_ground: bool,
    /// Flying with the creative abilities.
    pub flying: bool,
    pub fall_flying: bool,
    pub in_water: bool,
    pub in_vehicle: bool,
    /// The block under the feet that sets how it moves (`soul_sand`).
    pub below: Option<String>,
    pub fall_distance: f64,
    pub horizontal_speed: f64,
    pub can_see_sky: bool,
    /// Ticks it has lived, for `periodic_tick`.
    pub age: i64,
    pub creative: bool,
    pub food: i32,
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

pub fn full(id: &str) -> String {
    mc_protocol::components::full_name(id).into_owned()
}

/// Whether `id` is `want`: an id, `#tag` or a list of ids.
pub(crate) fn is(ctx: &ServerContext, registry: &str, want: &Value, id: &str) -> bool {
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
                && moves_as(ctx, p, s.mover)
        }
        "minecraft:enchantment_active_check" => c["active"].as_bool().unwrap_or(true) == s.active,
        "minecraft:weather_check" => {
            c.get("raining")
                .and_then(Value::as_bool)
                .is_none_or(|r| r == s.raining)
                && c.get("thundering")
                    .and_then(Value::as_bool)
                    .is_none_or(|t| t == s.thundering)
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
        "minecraft:random_chance" => roll() < chance(&c["chance"], s.level),
        _ => false,
    }
}

/// A chance: a number, or `enchantment_level` of a level-based value.
fn chance(v: &Value, level: i32) -> f32 {
    match v.get("type").and_then(Value::as_str) {
        Some("minecraft:enchantment_level") => value(&v["amount"], level),
        _ => value(v, level),
    }
}

/// A float range `{min, max}` (or one number) holds `x`.
fn in_range(r: &Value, x: f64) -> bool {
    match r {
        Value::Number(n) => n.as_f64().is_some_and(|n| (n - x).abs() < 1e-9),
        Value::Object(o) => {
            o.get("min").and_then(Value::as_f64).is_none_or(|m| x >= m)
                && o.get("max").and_then(Value::as_f64).is_none_or(|m| x <= m)
        }
        _ => true,
    }
}

/// The parts of an entity predicate about movement and flags; without a
/// [`Mover`] those that ask fail.
fn moves_as(ctx: &ServerContext, p: &Value, m: Option<&Mover>) -> bool {
    let asks = [
        "minecraft:vehicle",
        "minecraft:flags",
        "minecraft:movement_affected_by",
        "minecraft:movement",
        "minecraft:location",
        "minecraft:periodic_tick",
        "minecraft:type_specific/player",
    ];
    if !asks.iter().any(|k| p.get(*k).is_some()) {
        return true;
    }
    let Some(m) = m else {
        return false;
    };
    if p.get("minecraft:vehicle").is_some() && !m.in_vehicle {
        return false;
    }
    if let Some(f) = p.get("minecraft:flags") {
        let flag =
            |k: &str, have: bool| f.get(k).and_then(Value::as_bool).is_none_or(|w| w == have);
        if !(flag("is_on_ground", m.on_ground)
            && flag("is_flying", m.flying)
            && flag("is_fall_flying", m.fall_flying)
            && flag("is_in_water", m.in_water))
        {
            return false;
        }
    }
    if let Some(b) = p.get("minecraft:movement_affected_by") {
        let want = &b["block"]["blocks"];
        if !m
            .below
            .as_deref()
            .is_some_and(|n| want.is_null() || is(ctx, "minecraft:block", want, n))
        {
            return false;
        }
    }
    if let Some(mv) = p.get("minecraft:movement")
        && (mv
            .get("fall_distance")
            .is_some_and(|r| !in_range(r, m.fall_distance))
            || mv
                .get("horizontal_speed")
                .is_some_and(|r| !in_range(r, m.horizontal_speed)))
    {
        return false;
    }
    if let Some(l) = p.get("minecraft:location")
        && l.get("can_see_sky")
            .and_then(Value::as_bool)
            .is_some_and(|w| w != m.can_see_sky)
    {
        return false;
    }
    if let Some(n) = p.get("minecraft:periodic_tick").and_then(Value::as_i64)
        && n > 0
        && m.age % n != 0
    {
        return false;
    }
    if let Some(t) = p.get("minecraft:type_specific/player") {
        if let Some(modes) = t.get("gamemode").and_then(Value::as_array)
            && !modes
                .iter()
                .any(|g| g == if m.creative { "creative" } else { "survival" })
        {
            return false;
        }
        if t["food"]["level"]
            .get("min")
            .and_then(Value::as_i64)
            .is_some_and(|min| i64::from(m.food) < min)
        {
            return false;
        }
    }
    true
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

/// The entries of an effect component: a list of conditional effects, or
/// one effect on its own (`trident_spin_attack_strength`).
pub fn effects_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(l) => l.clone(),
        Value::Object(o) if o.contains_key("type") => vec![serde_json::json!({ "effect": v })],
        _ => Vec::new(),
    }
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
    let enchants = ctx.enchantments.get();
    for (name, level) in crate::enchants::levels(stack) {
        let Some(e) = enchants.get(&name) else {
            continue;
        };
        let s = Situation { level, ..*s };
        for entry in effects_of(&e.effects[component]) {
            if entry
                .get("requirements")
                .is_none_or(|r| holds(ctx, r, &item, &s))
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
                        .get()
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
            .get()
            .get(name)
            .is_some_and(|e| e.effects.get(component).is_some())
    })
}

/// What a player wears and holds: (inventory slot, equipment slot name,
/// stack), the slots enchantments act from.
pub fn gear(ctx: &ServerContext, uuid: u128) -> Vec<(usize, &'static str, ItemStack)> {
    let inventories = lock(&ctx.inventories);
    let Some(hands) = inventories.get(&uuid) else {
        return Vec::new();
    };
    let main = hands.hand_slot(0);
    [
        (5, "head"),
        (6, "chest"),
        (7, "legs"),
        (8, "feet"),
        (45, "offhand"),
        (main, "mainhand"),
    ]
    .into_iter()
    .filter_map(|(i, name)| Some((i, name, hands.slots[i].clone()?)))
    .collect()
}

/// The group name of an equipment slot, as `in_slot` reads it.
fn slot_index(name: &str, main: usize) -> usize {
    match name {
        "head" => 5,
        "chest" => 6,
        "legs" => 7,
        "feet" => 8,
        "offhand" => 45,
        _ => main,
    }
}

/// Whether the enchantment acts from this equipment slot.
pub fn acts_from(ctx: &ServerContext, enchantment: &str, slot: &str) -> bool {
    ctx.enchantments
        .get()
        .get(enchantment)
        .is_some_and(|e| in_slot(&e.slots, slot_index(slot, 36), 36))
}

/// Operation number of an attribute modifier operation's name.
pub fn operation(name: &str) -> i32 {
    match name {
        "add_multiplied_base" => 1,
        "add_multiplied_total" => 2,
        _ => 0,
    }
}

/// One `attribute` entry as the modifier it puts on in `slot`: vanilla
/// names it after the enchantment's id and the slot.
pub fn modifier(m: &Value, level: i32, slot: &str) -> (String, AttributeModifier) {
    (
        full(m["attribute"].as_str().unwrap_or("")),
        AttributeModifier {
            id: format!("{}/{slot}", full(m["id"].as_str().unwrap_or(""))),
            amount: f64::from(value(&m["amount"], level)),
            operation: operation(m["operation"].as_str().unwrap_or("")),
        },
    )
}

/// The attribute modifiers (`minecraft:attributes`) that the enchantments
/// of the gear put on, by attribute.
pub fn gear_attributes(
    ctx: &ServerContext,
    gear: &[(usize, &'static str, ItemStack)],
) -> Vec<(String, AttributeModifier)> {
    let mut out = Vec::new();
    let enchants = ctx.enchantments.get();
    for (_, slot, stack) in gear {
        for (name, level) in crate::enchants::levels(stack) {
            let Some(e) = enchants.get(&name) else {
                continue;
            };
            if !acts_from(ctx, &name, slot) {
                continue;
            }
            for m in e.effects["minecraft:attributes"]
                .as_array()
                .into_iter()
                .flatten()
            {
                out.push(modifier(m, level, slot));
            }
        }
    }
    out
}

/// The `add_value` attribute modifiers (`minecraft:oxygen_bonus`) that the
/// enchantments a player wears and holds give.
pub fn attribute(ctx: &ServerContext, uuid: u128, attribute: &str) -> f32 {
    gear_attributes(ctx, &gear(ctx, uuid))
        .iter()
        .filter(|(a, m)| a == attribute && m.operation == 0)
        .map(|(_, m)| m.amount as f32)
        .sum()
}

/// Whether a worn enchantment makes the player immune to this damage
/// (`damage_immunity`: Frost Walker against magma blocks).
pub fn immune(ctx: &ServerContext, uuid: u128, damage_type: &str) -> bool {
    let s = Situation {
        damage_type: Some(damage_type),
        ..Situation::default()
    };
    gear(ctx, uuid).iter().any(|(_, slot, stack)| {
        let item = ctx.items.name(stack.item).unwrap_or("").to_owned();
        crate::enchants::levels(stack).iter().any(|(name, level)| {
            acts_from(ctx, name, slot)
                && ctx.enchantments.get().get(name).is_some_and(|e| {
                    let s = Situation { level: *level, ..s };
                    effects_of(&e.effects["minecraft:damage_immunity"])
                        .iter()
                        .any(|en| {
                            en.get("requirements")
                                .is_none_or(|r| holds(ctx, r, &item, &s))
                        })
                })
        })
    })
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

/// What a `post_attack` effect does to the entity it is aimed at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PostAttack {
    /// Damage of this type (`minecraft:thorns`), rolled between its bounds.
    pub damage: Option<(String, f32)>,
    /// Wear on the enchanted item, and its inventory slot.
    pub wear: i32,
    pub slot: usize,
    /// A lightning bolt (Channeling) and the sound with it.
    pub lightning: bool,
    pub sound: Option<(String, f32)>,
    /// A wind burst (Wind Burst): its `explode` effect.
    pub explode: Option<Value>,
    pub level: i32,
}

fn post_effect(effect: &Value, level: i32, out: &mut PostAttack) {
    match effect["type"].as_str().unwrap_or("") {
        "minecraft:all_of" => {
            for e in effect["effects"].as_array().into_iter().flatten() {
                post_effect(e, level, out);
            }
        }
        "minecraft:damage_entity" => {
            let lo = value(&effect["min_damage"], level);
            let hi = value(&effect["max_damage"], level);
            out.damage = Some((
                full(
                    effect["damage_type"]
                        .as_str()
                        .unwrap_or("minecraft:generic"),
                ),
                lo + roll() * (hi - lo),
            ));
        }
        "minecraft:change_item_damage" => out.wear += value(&effect["amount"], level) as i32,
        "minecraft:summon_entity" => {
            out.lightning |= effect["entity"] == "minecraft:lightning_bolt";
        }
        "minecraft:play_sound" => {
            if let Some(n) = effect["sound"].as_str() {
                out.sound = Some((n.to_owned(), value(&effect["volume"], level)));
            }
        }
        "minecraft:explode" => out.explode = Some(effect.clone()),
        _ => {}
    }
}

/// The `post_attack` effects of one side's gear that fire on a hit:
/// `enchanted` is whose enchantments they are (`victim`, `attacker`) and
/// `affected` whom they act on. `gear` is that side's gear; `s` the hit.
pub fn post_attack(
    ctx: &ServerContext,
    gear: &[(usize, &'static str, ItemStack)],
    enchanted: &str,
    affected: &str,
    s: &Situation,
) -> Vec<PostAttack> {
    let mut out = Vec::new();
    let enchants = ctx.enchantments.get();
    for (slot_index, slot, stack) in gear {
        let item = ctx.items.name(stack.item).unwrap_or("").to_owned();
        for (name, level) in crate::enchants::levels(stack) {
            if !acts_from(ctx, &name, slot) {
                continue;
            }
            let Some(e) = enchants.get(&name) else {
                continue;
            };
            let s = Situation { level, ..*s };
            for entry in effects_of(&e.effects["minecraft:post_attack"]) {
                if entry["enchanted"] != enchanted || entry["affected"] != affected {
                    continue;
                }
                if entry
                    .get("requirements")
                    .is_none_or(|r| holds(ctx, r, &item, &s))
                {
                    let mut p = PostAttack {
                        slot: *slot_index,
                        level,
                        ..PostAttack::default()
                    };
                    post_effect(&entry["effect"], level, &mut p);
                    out.push(p);
                }
            }
        }
    }
    out
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
