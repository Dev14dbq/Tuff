//! What eating or drinking something does besides feeding, all from the
//! stack's components (minecraft.wiki, "Data component format" →
//! `consumable`, `potion_contents`, `suspicious_stew_effects`,
//! `ominous_bottle_amplifier`; "Chorus Fruit", "Milk Bucket"):
//! - a potion gives its effects (instant ones act at once), scaled by
//!   `potion_duration_scale`; suspicious stew its effects; an ominous
//!   bottle Bad Omen for 100 minutes;
//! - `on_consume_effects`: `apply_effects` (with a `probability`),
//!   `remove_effects`, `clear_all_effects` (milk), `teleport_randomly`
//!   (chorus fruit: 16 tries within `diameter` blocks for a spot over
//!   solid ground with room and no liquid) and `play_sound`.
//!
//! Food burps when it is done.

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use serde_json::Value;

use crate::context::{Event, Pose, ServerContext};
use crate::effects::{self, Instance, Kind};
use crate::fx;
use crate::tick::lock;

/// Bad Omen from an ominous bottle lasts 100 minutes.
const BAD_OMEN_TICKS: i32 = 120_000;

/// A player finished eating or drinking `stack` (as it was before one was
/// taken from it).
pub fn eaten(ctx: &ServerContext, uuid: u128, stack: &ItemStack) {
    crate::stats::item(ctx, uuid, crate::stats::USED, stack.item, 1);
    crate::criteria::with_item(ctx, uuid, "minecraft:consume_item", stack);
    let pos = lock(&ctx.players)
        .get(&uuid)
        .and_then(|p| p.pose)
        .map_or([0.0; 3], |p| [p.x, p.y, p.z]);
    if ctx.items.component(stack, "food").is_some() {
        let pitch = 0.9 + crate::hits::rnd() as f32 * 0.1;
        fx::sound(
            ctx,
            "entity.player.burp",
            fx::category::PLAYERS,
            pos,
            0.5,
            pitch,
        );
    }
    let scale = ctx
        .items
        .component(stack, "potion_duration_scale")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    for (kind, mut i) in potion_effects(ctx, stack) {
        if kind.instant {
            effects::instant(ctx, uuid, kind, i.amplifier, 1.0);
        } else {
            if i.duration != -1 {
                i.duration = (f64::from(i.duration) * scale) as i32;
            }
            effects::give(ctx, uuid, kind, i);
        }
    }
    if let Some(Value::Array(stew)) = ctx.items.component(stack, "suspicious_stew_effects") {
        for e in &stew {
            let Some(kind) = e["id"].as_str().and_then(effects::kind_of) else {
                continue;
            };
            let ticks = e["duration"].as_i64().unwrap_or(160) as i32;
            effects::give(ctx, uuid, kind, Instance::new(0, ticks, true));
        }
    }
    if let Some(amp) = ctx
        .items
        .component(stack, "ominous_bottle_amplifier")
        .and_then(|v| v.as_i64())
        && let Some(kind) = effects::kind_of("bad_omen")
    {
        let mut omen = Instance::new(amp as i32, BAD_OMEN_TICKS, false);
        omen.icon = true;
        effects::give(ctx, uuid, kind, omen);
    }
    let Some(consumable) = ctx.items.component(stack, "consumable") else {
        return;
    };
    for effect in consumable["on_consume_effects"]
        .as_array()
        .into_iter()
        .flatten()
    {
        consume_effect(ctx, uuid, effect, pos);
    }
}

/// The effects of a stack's `potion_contents`: its potion's, then the
/// custom ones (saved as a compound, or as just the potion's name).
pub fn potion_effects(ctx: &ServerContext, stack: &ItemStack) -> Vec<(&'static Kind, Instance)> {
    let Some(contents) = ctx.items.component(stack, "potion_contents") else {
        return Vec::new();
    };
    let potion = match &contents {
        Value::String(name) => Some(name.as_str()),
        other => other["potion"].as_str(),
    };
    let mut out: Vec<(&'static Kind, Instance)> = potion
        .map(crate::projectiles::potions::effects_of)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, amp, ticks)| {
            Some((effects::kind_of(&name)?, Instance::new(amp, ticks, true)))
        })
        .collect();
    out.extend(
        contents["custom_effects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(effects::instance_of),
    );
    out
}

/// The effects a holder set names: one id, a `#tag` or a list of them.
fn effect_set(ctx: &ServerContext, v: &Value) -> Vec<&'static Kind> {
    match v {
        Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => ctx
                .commands
                .env
                .tag("minecraft:mob_effect", tag)
                .iter()
                .filter_map(|n| effects::kind_of(n))
                .collect(),
            None => effects::kind_of(s).into_iter().collect(),
        },
        Value::Array(list) => list.iter().flat_map(|e| effect_set(ctx, e)).collect(),
        _ => Vec::new(),
    }
}

fn consume_effect(ctx: &ServerContext, uuid: u128, effect: &Value, pos: [f64; 3]) {
    match effect["type"].as_str().unwrap_or("") {
        "minecraft:apply_effects" => {
            let chance = effect["probability"].as_f64().unwrap_or(1.0);
            if crate::hits::rnd() >= chance {
                return;
            }
            for (kind, i) in effect["effects"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(effects::instance_of)
            {
                effects::give(ctx, uuid, kind, i);
            }
        }
        "minecraft:remove_effects" => {
            for kind in effect_set(ctx, &effect["effects"]) {
                effects::remove(ctx, uuid, Some(kind));
            }
        }
        "minecraft:clear_all_effects" => {
            effects::remove(ctx, uuid, None);
        }
        "minecraft:teleport_randomly" => {
            let diameter = effect["diameter"].as_f64().unwrap_or(16.0);
            teleport_randomly(ctx, uuid, diameter);
        }
        "minecraft:play_sound" => {
            let name = match &effect["sound"] {
                Value::String(s) => s.as_str(),
                other => other["sound_id"].as_str().unwrap_or(""),
            };
            let name = name.strip_prefix("minecraft:").unwrap_or(name);
            let at = pos.map(|c| c.floor() + 0.5);
            fx::sound(ctx, name, fx::category::PLAYERS, at, 1.0, 1.0);
        }
        _ => {}
    }
}

/// Up to 16 tries to move the player to a random spot within `diameter`
/// blocks (up and down too); whether one worked.
pub fn teleport_randomly(ctx: &ServerContext, uuid: u128, diameter: f64) -> bool {
    let Some((pose, entity_id)) = lock(&ctx.players)
        .get(&uuid)
        .and_then(|p| Some((p.pose?, p.entity_id)))
    else {
        return false;
    };
    let low = f64::from(ctx.world.min_y);
    let high = low + f64::from(ctx.world.height) - 1.0;
    let span = diameter.max(1.0) as u64;
    for _ in 0..16 {
        let x = pose.x + (crate::hits::rnd() - 0.5) * diameter;
        let dy = (crate::util::random_u64() % span) as f64 - (diameter / 2.0).floor();
        let y = (pose.y + dy).clamp(low, high);
        let z = pose.z + (crate::hits::rnd() - 0.5) * diameter;
        let Some(y) = landing(ctx, [x, y, z]) else {
            continue;
        };
        let to = Pose {
            x,
            y,
            z,
            on_ground: false,
            ..pose
        };
        let _ = ctx.events.send(Event::Teleport { uuid, pose: to });
        if let Some(p) = lock(&ctx.players).get_mut(&uuid) {
            p.pose = Some(to);
        }
        if let Some(v) = lock(&ctx.vitals).get_mut(&uuid) {
            v.fall_distance = 0.0;
        }
        fx::status(ctx, entity_id, 46);
        fx::sound(
            ctx,
            "item.chorus_fruit.teleport",
            fx::category::PLAYERS,
            [x, y, z],
            1.0,
            1.0,
        );
        return true;
    }
    false
}

/// Where a player put at `at` comes to stand: straight down to the first
/// block that stops motion, if there is room there and no liquid.
fn landing(ctx: &ServerContext, at: [f64; 3]) -> Option<f64> {
    let blocks = &ctx.data.blocks;
    let state = |p: [i32; 3]| ctx.world.block(Position::new(p[0], p[1], p[2]), blocks);
    let (x, z) = (at[0].floor() as i32, at[2].floor() as i32);
    let mut y = at[1];
    loop {
        let cell = y.floor() as i32;
        if cell <= ctx.world.min_y {
            return None;
        }
        if ctx.rules.blocks_motion(state([x, cell - 1, z])?) {
            break;
        }
        y -= 1.0;
    }
    let (min, max) = (
        [at[0] - 0.3, y, at[2] - 0.3],
        [at[0] + 0.3, y + 1.8, at[2] + 0.3],
    );
    let table = crate::collision::table(ctx);
    for cx in min[0].floor() as i32..=max[0].floor() as i32 {
        for cy in min[1].floor() as i32..=max[1].floor() as i32 {
            for cz in min[2].floor() as i32..=max[2].floor() as i32 {
                let s = state([cx, cy, cz])?;
                if blocks.is_fluid(s) {
                    return None;
                }
                let base = [f64::from(cx), f64::from(cy), f64::from(cz)];
                let full = [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
                let boxes: &[crate::collision::Box6] = match table.get(s as usize) {
                    Some(Some(b)) => b,
                    _ if ctx.rules.blocks_motion(s) => &full,
                    _ => &[],
                };
                let hits = boxes.iter().any(|b| {
                    (0..3).all(|a| {
                        base[a] + b[a] < max[a] - 1e-7 && base[a] + b[a + 3] > min[a] + 1e-7
                    })
                });
                if hits {
                    return None;
                }
            }
        }
    }
    Some(y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{flat, join};

    const U: u128 = 0xc0de;

    fn eat(ctx: &ServerContext, item: &str, f: impl FnOnce(&mut ItemStack)) {
        let mut stack = ItemStack::new(ctx.items.id(&format!("minecraft:{item}")).unwrap(), 1);
        f(&mut stack);
        eaten(ctx, U, &stack);
    }

    fn has(ctx: &ServerContext, name: &str) -> Option<Instance> {
        let kind = effects::kind_of(name)?;
        lock(&ctx.vitals).get(&U)?.effects.get(kind).cloned()
    }

    #[test]
    fn foods_and_drinks_give_and_take_effects_from_their_components() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 901, [0.5, -60.0, 0.5], 0);
        eat(&ctx, "enchanted_golden_apple", |_| {});
        let regen = has(&ctx, "regeneration").expect("regeneration");
        assert_eq!((regen.amplifier, regen.duration), (1, 400));
        assert_eq!(has(&ctx, "absorption").map(|i| i.amplifier), Some(3));
        assert_eq!(lock(&ctx.vitals)[&U].absorption, 16.0);
        assert!(has(&ctx, "fire_resistance").is_some());
        // Milk clears them all.
        eat(&ctx, "milk_bucket", |_| {});
        assert!(lock(&ctx.vitals)[&U].effects.is_empty());
        // Pufferfish: three effects; honey takes the poison away.
        eat(&ctx, "pufferfish", |_| {});
        assert_eq!(has(&ctx, "poison").map(|i| i.amplifier), Some(1));
        assert_eq!(has(&ctx, "hunger").map(|i| i.duration), Some(300));
        eat(&ctx, "honey_bottle", |_| {});
        assert!(has(&ctx, "poison").is_none() && has(&ctx, "nausea").is_some());
        // A potion, long: 8 minutes of swiftness, and its custom effects.
        eat(&ctx, "potion", |s| {
            let mut c = mc_nbt::Compound::new();
            c.insert("potion", mc_nbt::Tag::from("minecraft:long_swiftness"));
            let mut custom = mc_nbt::Compound::new();
            custom.insert("id", mc_nbt::Tag::from("minecraft:glowing"));
            custom.insert("duration", mc_nbt::Tag::Int(100));
            custom.insert("show_particles", mc_nbt::Tag::Byte(0));
            c.insert(
                "custom_effects",
                mc_nbt::Tag::List(vec![mc_nbt::Tag::Compound(custom)]),
            );
            s.components
                .set("potion_contents", mc_nbt::Tag::Compound(c));
        });
        assert_eq!(has(&ctx, "speed").map(|i| i.duration), Some(9600));
        let glow = has(&ctx, "glowing").expect("glowing");
        assert!(!glow.particles && !glow.icon);
        // Harming hurts at once and is no lasting effect.
        eat(&ctx, "potion", |s| {
            s.components
                .set("potion_contents", mc_nbt::Tag::from("minecraft:harming"));
        });
        assert_eq!(lock(&ctx.vitals)[&U].health, 14.0);
        assert!(has(&ctx, "instant_damage").is_none());
        // Suspicious stew and the ominous bottle.
        eat(&ctx, "suspicious_stew", |s| {
            let mut e = mc_nbt::Compound::new();
            e.insert("id", mc_nbt::Tag::from("minecraft:night_vision"));
            e.insert("duration", mc_nbt::Tag::Int(100));
            s.components.set(
                "suspicious_stew_effects",
                mc_nbt::Tag::List(vec![mc_nbt::Tag::Compound(e)]),
            );
        });
        assert_eq!(has(&ctx, "night_vision").map(|i| i.duration), Some(100));
        eat(&ctx, "ominous_bottle", |s| {
            s.components
                .set("ominous_bottle_amplifier", mc_nbt::Tag::Int(2));
        });
        let omen = has(&ctx, "bad_omen").expect("bad omen");
        assert_eq!((omen.amplifier, omen.duration), (2, BAD_OMEN_TICKS));
        assert!(!omen.particles && omen.icon);
    }

    #[test]
    fn chorus_fruit_lands_the_player_on_the_ground_nearby() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 902, [0.5, -60.0, 0.5], 0);
        let mut events = ctx.events.subscribe();
        eat(&ctx, "chorus_fruit", |_| {});
        let mut moved = None;
        while let Some(e) = events.try_changed() {
            if let Event::Teleport { uuid: U, pose } = e {
                moved = Some(pose);
            }
        }
        let pose = moved.expect("teleported");
        assert_eq!(pose.y, -60.0);
        assert!((pose.x - 0.5).abs() <= 8.0 && (pose.z - 0.5).abs() <= 8.0);
        // Inside solid ground there is no room.
        assert_eq!(landing(&ctx, [0.5, -62.0, 0.5]), None);
        assert_eq!(landing(&ctx, [0.5, -57.5, 0.5]), Some(-59.5));
    }
}
