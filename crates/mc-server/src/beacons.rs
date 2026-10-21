//! Beacons (minecraft.wiki, "Beacon"): the pyramid below sets the level
//! (1-4), the chosen effects go to players in range every 80 ticks, and the
//! menu sells the choice for one ingot, gem or netherite ingot.
//!
//! The block entity keeps `Levels`, `primary_effect` and `secondary_effect`;
//! clients work the beam out themselves from the blocks above.

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use crate::context::ServerContext;
use crate::effects;

/// Ticks between two rounds of effects.
pub const PERIOD: i64 = 80;

/// The blocks a pyramid is made of.
const BASE: [&str; 5] = [
    "minecraft:iron_block",
    "minecraft:gold_block",
    "minecraft:emerald_block",
    "minecraft:diamond_block",
    "minecraft:netherite_block",
];

/// What pays for a choice of effects.
const PAYMENT: [&str; 5] = [
    "minecraft:iron_ingot",
    "minecraft:gold_ingot",
    "minecraft:emerald",
    "minecraft:diamond",
    "minecraft:netherite_ingot",
];

/// Effects by the pyramid level that unlocks them (the fourth tier is the
/// secondary choice).
const TIERS: [&[&str]; 4] = [
    &["minecraft:speed", "minecraft:haste"],
    &["minecraft:resistance", "minecraft:jump_boost"],
    &["minecraft:strength"],
    &["minecraft:regeneration"],
];

pub fn is_payment(ctx: &ServerContext, item: i32) -> bool {
    ctx.items.name(item).is_some_and(|n| PAYMENT.contains(&n))
}

/// Whether `effect` may be the primary one at pyramid level `levels`.
pub fn primary_allowed(levels: i32, effect: &str) -> bool {
    TIERS
        .iter()
        .take(levels.clamp(0, 3) as usize)
        .any(|tier| tier.contains(&effect))
}

/// Whether `secondary` may go with `primary` at pyramid level `levels`: from
/// level 4, regeneration or the primary again (then stronger).
pub fn secondary_allowed(levels: i32, primary: &str, secondary: &str) -> bool {
    levels >= 4 && (secondary == "minecraft:regeneration" || secondary == primary)
}

/// The pyramid level under the beacon at `p` (0 when the sky is shut off
/// by solid blocks above, or the pyramid is incomplete).
pub fn pyramid_levels(ctx: &ServerContext, p: Position) -> i32 {
    let blocks = &ctx.data.blocks;
    let name_at = |q: Position| {
        ctx.world
            .block(q, blocks)
            .and_then(|s| blocks.block_name(s).map(str::to_owned))
    };
    if !open_above(ctx, p) {
        return 0;
    }
    let mut levels = 0;
    for layer in 1..=4 {
        let y = p.y - layer;
        if y < ctx.world.min_y {
            break;
        }
        let complete = (p.x - layer..=p.x + layer).all(|x| {
            (p.z - layer..=p.z + layer).all(|z| {
                name_at(Position::new(x, y, z)).is_some_and(|n| BASE.contains(&n.as_str()))
            })
        });
        if !complete {
            break;
        }
        levels = layer;
    }
    levels
}

/// Nothing opaque above (bedrock does not block the beam; glass and other
/// see-through blocks do not either).
fn open_above(ctx: &ServerContext, p: Position) -> bool {
    let blocks = &ctx.data.blocks;
    let top = ctx.world.min_y + ctx.world.height as i32;
    (p.y + 1..top).all(|y| {
        let Some(state) = ctx.world.block(Position::new(p.x, y, p.z), blocks) else {
            return true;
        };
        blocks.block_name(state) == Some("minecraft:bedrock") || ctx.world.light_block(state) < 15
    })
}

fn effect_name(data: &Compound, key: &str) -> Option<String> {
    match data.get(key) {
        Some(Tag::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The block entity at `p`, made when there is none.
fn entity(ctx: &ServerContext, p: Position) -> mc_world::BlockEntity {
    ctx.world
        .block_entity(p, &ctx.data.blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: "minecraft:beacon".to_owned(),
            data: Compound::new(),
        })
}

/// The beacon at `p` (a block entity of its own from now on).
pub fn placed(ctx: &ServerContext, p: Position) {
    let blocks = &ctx.data.blocks;
    if ctx.world.block_entity(p, blocks).is_none() {
        ctx.world.set_block_entity(entity(ctx, p), blocks);
    }
}

/// The chosen effects of the beacon at `p` and its level now.
pub fn state(ctx: &ServerContext, p: Position) -> (i32, Option<String>, Option<String>) {
    let be = entity(ctx, p);
    (
        pyramid_levels(ctx, p),
        effect_name(&be.data, "primary_effect"),
        effect_name(&be.data, "secondary_effect"),
    )
}

/// Stores the choice (the caller has checked and been paid).
pub fn choose(ctx: &ServerContext, p: Position, primary: Option<&str>, secondary: Option<&str>) {
    let mut be = entity(ctx, p);
    for (key, value) in [("primary_effect", primary), ("secondary_effect", secondary)] {
        match value {
            Some(v) => {
                be.data.insert(key, Tag::String(v.to_owned()));
            }
            None => {
                be.data.remove(key);
            }
        }
    }
    ctx.world.set_block_entity(be, &ctx.data.blocks);
}

/// Numbers the menu shows: level, primary and secondary as registry id + 1
/// (0 for none).
pub fn menu_values(ctx: &ServerContext, p: Position) -> [i16; 3] {
    let (levels, primary, secondary) = state(ctx, p);
    let code = |name: Option<String>| {
        name.and_then(|n| ctx.data.builtin.id("minecraft:mob_effect", &n))
            .map_or(0, |id| id as i16 + 1)
    };
    [levels as i16, code(primary), code(secondary)]
}

/// Gives the effects of the beacon at `p` (level `levels`) to the players in
/// range.
fn apply(ctx: &ServerContext, p: Position, levels: i32, primary: &str, secondary: Option<&str>) {
    let range = f64::from(levels * 10 + 10);
    let duration = (9 + levels * 2) * 20;
    let upgraded = levels >= 4 && secondary == Some(primary);
    let near: Vec<u128> = ctx
        .players
        .lock()
        .expect("players lock")
        .values()
        .filter(|pl| pl.game_mode != 3)
        .filter(|pl| {
            pl.pose.is_some_and(|pose| {
                (pose.x - (f64::from(p.x) + 0.5)).abs() <= range
                    && (pose.z - (f64::from(p.z) + 0.5)).abs() <= range
                    && pose.y >= f64::from(p.y) - range
            })
        })
        .map(|pl| pl.profile.uuid)
        .collect();
    let give = |uuid: u128, name: &str, amplifier: i32| {
        if let Some(kind) = effects::kind_of(name) {
            let mut instance = effects::Instance::new(amplifier, duration, true);
            instance.ambient = true;
            effects::give(ctx, uuid, kind, instance);
        }
    };
    for uuid in near {
        give(uuid, primary, i32::from(upgraded));
        if levels >= 4
            && let Some(secondary) = secondary
            && secondary != primary
        {
            give(uuid, secondary, 0);
        }
    }
}

/// One game tick: every 80 ticks each beacon looks at its pyramid and
/// gives out its effects.
pub fn tick(ctx: &ServerContext, count: i64) {
    if count % PERIOD != 0 {
        return;
    }
    for be in ctx.world.block_entities_of("minecraft:beacon") {
        let p = Position::new(be.x, be.y, be.z);
        let still_beacon = ctx
            .world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            == Some("minecraft:beacon");
        if !still_beacon {
            continue;
        }
        let levels = pyramid_levels(ctx, p);
        let mut be = be;
        if be.data.get("Levels") != Some(&Tag::Int(levels)) {
            be.data.insert("Levels", Tag::Int(levels));
            ctx.world.set_block_entity(be.clone(), &ctx.data.blocks);
        }
        if levels == 0 {
            continue;
        }
        let Some(primary) = effect_name(&be.data, "primary_effect") else {
            continue;
        };
        let secondary = effect_name(&be.data, "secondary_effect");
        apply(ctx, p, levels, &primary, secondary.as_deref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_unlock_by_level() {
        assert!(primary_allowed(1, "minecraft:speed"));
        assert!(!primary_allowed(1, "minecraft:resistance"));
        assert!(primary_allowed(2, "minecraft:jump_boost"));
        assert!(!primary_allowed(2, "minecraft:strength"));
        assert!(primary_allowed(3, "minecraft:strength"));
        assert!(primary_allowed(4, "minecraft:strength"));
        assert!(!primary_allowed(4, "minecraft:regeneration"));
        assert!(!primary_allowed(0, "minecraft:speed"));
        assert!(!secondary_allowed(
            3,
            "minecraft:speed",
            "minecraft:regeneration"
        ));
        assert!(secondary_allowed(
            4,
            "minecraft:speed",
            "minecraft:regeneration"
        ));
        assert!(secondary_allowed(4, "minecraft:speed", "minecraft:speed"));
        assert!(!secondary_allowed(4, "minecraft:speed", "minecraft:haste"));
    }
}
