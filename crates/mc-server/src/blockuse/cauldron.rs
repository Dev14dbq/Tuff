//! Cauldrons and the items used on them (minecraft.wiki, "Cauldron" →
//! "Usage"): buckets fill and empty them, bottles take and give water a
//! third at a time, and water washes dye off leather and wolf armor, a
//! pattern off a banner and the colour off a shulker box.

use mc_nbt::{Compound, Tag};

use super::{Click, exchange, give, set, state_of, use_up};
use crate::interact::Outcome;

const WATER: &str = "minecraft:water_cauldron";
const LAVA: &str = "minecraft:lava_cauldron";
const SNOW: &str = "minecraft:powder_snow_cauldron";
const EMPTY: &str = "minecraft:cauldron";

/// Whether the held potion is plain water.
fn water_potion(c: &Click) -> bool {
    if c.held_name() != "minecraft:potion" {
        return false;
    }
    match c
        .comps()
        .and_then(|comps| comps.get("minecraft:potion_contents"))
    {
        Some(Tag::String(p)) => p == "minecraft:water",
        Some(Tag::Compound(p)) => {
            matches!(p.get("potion"), Some(Tag::String(p)) if p == "minecraft:water")
        }
        _ => false,
    }
}

fn water_bottle() -> Compound {
    let mut contents = Compound::new();
    contents.insert("potion", Tag::String("minecraft:water".into()));
    let mut comps = Compound::new();
    comps.insert("minecraft:potion_contents", Tag::Compound(contents));
    comps
}

/// Empties the held bucket into the cauldron: it becomes `kind` (full).
fn pour(c: &Click, outcome: &mut Outcome, kind: &str, sound: &str) -> bool {
    let level = (kind != LAVA).then_some(("level", "3"));
    let Some(new) = state_of(c.ctx, kind, level.as_slice()) else {
        return false;
    };
    exchange(c, outcome, "minecraft:bucket", None);
    set(c.ctx, outcome, c.at, new);
    c.sound(sound);
    true
}

/// Takes the cauldron's contents into an empty bucket.
fn scoop(c: &Click, outcome: &mut Outcome, result: &str, sound: &str) -> bool {
    let Some(empty) = state_of(c.ctx, EMPTY, &[]) else {
        return false;
    };
    exchange(c, outcome, result, None);
    set(c.ctx, outcome, c.at, empty);
    c.sound(sound);
    true
}

/// One layer of water goes: the cauldron is empty after the last.
fn lower(c: &Click, outcome: &mut Outcome) {
    let level = c.int("level");
    let new = if level <= 1 {
        state_of(c.ctx, EMPTY, &[])
    } else {
        Some(c.with("level", &(level - 1).to_string()))
    };
    if let Some(new) = new {
        set(c.ctx, outcome, c.at, new);
    }
}

/// The buckets every cauldron takes.
fn fill(c: &Click, outcome: &mut Outcome) -> Option<bool> {
    Some(match c.held_name() {
        "minecraft:water_bucket" => pour(c, outcome, WATER, "item.bucket.empty"),
        "minecraft:lava_bucket" => pour(c, outcome, LAVA, "item.bucket.empty_lava"),
        "minecraft:powder_snow_bucket" => pour(c, outcome, SNOW, "item.bucket.empty_powder_snow"),
        _ => return None,
    })
}

/// A dyed item loses its colour; `true` if it had one.
fn wash(c: &Click, outcome: &mut Outcome, key: &str) -> bool {
    let Some(comps) = c.comps() else {
        return false;
    };
    if comps.get(key).is_none() {
        return false;
    }
    let mut kept = Compound::new();
    for (k, v) in comps.iter() {
        if k != key {
            kept.insert(k, v.clone());
        }
    }
    replace_held_comps(c, outcome, kept);
    lower(c, outcome);
    true
}

/// Puts new components on the held stack.
fn replace_held_comps(c: &Click, outcome: &mut Outcome, comps: Compound) {
    let Some(h) = &c.held else { return };
    let mut inventories = crate::tick::lock(&c.ctx.inventories);
    let Some(hands) = inventories.get_mut(&c.actor.uuid) else {
        return;
    };
    let Some(stack) = hands.slots[h.slot].as_mut() else {
        return;
    };
    let (damage, rest) = crate::items::split_damage(Some(&Tag::Compound(comps)));
    stack.damage = damage;
    stack.has_component_changes = rest.is_some();
    match rest {
        Some(rest) => hands.components.insert(h.slot, rest),
        None => hands.components.remove(&h.slot),
    };
    outcome.events.push(crate::context::Event::InventorySlot {
        uuid: c.actor.uuid,
        slot: h.slot as i16,
        item: hands.slots[h.slot].clone(),
    });
}

/// A banner loses its last pattern: one banner comes off the stack.
fn wash_banner(c: &Click, outcome: &mut Outcome) -> bool {
    let Some(comps) = c.comps() else {
        return false;
    };
    let Some(Tag::List(patterns)) = comps.get("minecraft:banner_patterns") else {
        return false;
    };
    if patterns.is_empty() {
        return false;
    }
    let mut washed = comps.clone();
    let mut fewer = patterns.clone();
    fewer.pop();
    if fewer.is_empty() {
        washed.remove("minecraft:banner_patterns");
    } else {
        washed.insert("minecraft:banner_patterns", Tag::List(fewer));
    }
    let item = c.held.as_ref().map(|h| h.stack.item).unwrap_or(0);
    if c.creative() {
        give(c, outcome, item, 1, Some(washed));
    } else if c.held.as_ref().is_some_and(|h| h.stack.count == 1) {
        replace_held_comps(c, outcome, washed);
    } else {
        use_up(c, 1);
        give(c, outcome, item, 1, Some(washed));
    }
    lower(c, outcome);
    true
}

/// A dyed shulker box turns plain, keeping what it holds.
fn wash_shulker(c: &Click, outcome: &mut Outcome) -> bool {
    let Some(h) = &c.held else { return false };
    let Some(plain) = c.ctx.items.id("minecraft:shulker_box") else {
        return false;
    };
    let mut inventories = crate::tick::lock(&c.ctx.inventories);
    let Some(hands) = inventories.get_mut(&c.actor.uuid) else {
        return false;
    };
    let Some(stack) = hands.slots[h.slot].as_mut() else {
        return false;
    };
    stack.item = plain;
    outcome.events.push(crate::context::Event::InventorySlot {
        uuid: c.actor.uuid,
        slot: h.slot as i16,
        item: hands.slots[h.slot].clone(),
    });
    drop(inventories);
    lower(c, outcome);
    true
}

/// The held item used on a cauldron; `true` if it did something.
pub fn use_on(c: &Click, outcome: &mut Outcome) -> bool {
    let held = c.held_name();
    match c.name {
        EMPTY => {
            if let Some(done) = fill(c, outcome) {
                return done;
            }
            if water_potion(c) {
                let Some(new) = state_of(c.ctx, WATER, &[("level", "1")]) else {
                    return false;
                };
                exchange(c, outcome, "minecraft:glass_bottle", None);
                set(c.ctx, outcome, c.at, new);
                c.sound("item.bottle.empty");
                return true;
            }
            false
        }
        WATER => {
            if let Some(done) = fill(c, outcome) {
                return done;
            }
            let level = c.int("level");
            match held {
                "minecraft:bucket" if level == 3 => {
                    scoop(c, outcome, "minecraft:water_bucket", "item.bucket.fill")
                }
                "minecraft:glass_bottle" => {
                    exchange(c, outcome, "minecraft:potion", Some(water_bottle()));
                    lower(c, outcome);
                    c.sound("item.bottle.fill");
                    true
                }
                "minecraft:potion" if water_potion(c) && level < 3 => {
                    exchange(c, outcome, "minecraft:glass_bottle", None);
                    set(
                        c.ctx,
                        outcome,
                        c.at,
                        c.with("level", &(level + 1).to_string()),
                    );
                    c.sound("item.bottle.empty");
                    true
                }
                "minecraft:leather_helmet"
                | "minecraft:leather_chestplate"
                | "minecraft:leather_leggings"
                | "minecraft:leather_boots"
                | "minecraft:leather_horse_armor"
                | "minecraft:wolf_armor" => wash(c, outcome, "minecraft:dyed_color"),
                n if n.ends_with("_banner") => wash_banner(c, outcome),
                n if n.ends_with("_shulker_box") => wash_shulker(c, outcome),
                _ => false,
            }
        }
        LAVA => {
            if held == "minecraft:bucket" {
                return scoop(c, outcome, "minecraft:lava_bucket", "item.bucket.fill_lava");
            }
            fill(c, outcome).unwrap_or(false)
        }
        SNOW => {
            if held == "minecraft:bucket" {
                if c.int("level") < 3 {
                    return false;
                }
                return scoop(
                    c,
                    outcome,
                    "minecraft:powder_snow_bucket",
                    "item.bucket.fill_powder_snow",
                );
            }
            fill(c, outcome).unwrap_or(false)
        }
        _ => false,
    }
}
