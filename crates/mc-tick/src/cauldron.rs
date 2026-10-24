//! Cauldrons filling on their own (minecraft.wiki, "Cauldron" → "Filling"):
//! rain and snow falling into them, and water or lava dripping from a
//! pointed dripstone hanging over them with the fluid above its base.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::rng::Rng;
use crate::updates::{Ctx, Flags, set_block};

/// Chance per random tick that a stalactite under water or lava passes a
/// drop on.
const WATER_DRIP: f64 = 0.175_781_25;
const LAVA_DRIP: f64 = 0.058_593_75;
/// How far a stalactite reaches down, and how far below its tip a cauldron
/// may be.
const REACH: i32 = 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drip {
    Water,
    Lava,
}

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes.block_name(state).unwrap_or("")
}

fn layers(ctx: Ctx, state: u32) -> u32 {
    ctx.shapes
        .property(state, "level")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn state(ctx: Ctx, name: &str, level: Option<u32>) -> Option<u32> {
    let base = ctx.shapes.default_state(name)?;
    Some(match level {
        Some(l) => ctx.shapes.with(base, "level", &l.to_string()),
        None => base,
    })
}

/// Rain or snow (`rain` false) reached the cauldron at `p`: an empty one
/// starts filling 1 time in 20 (snow 1 in 10), a filling one of the same
/// kind gets a layer.
pub fn precipitation(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rain: bool, rng: &mut Rng) {
    let here = level.block(p);
    let chance = if rain { 0.05 } else { 0.1 };
    let new = match name(ctx, here) {
        "minecraft:cauldron" => {
            if rng.unit() >= chance {
                return;
            }
            let kind = if rain {
                "minecraft:water_cauldron"
            } else {
                "minecraft:powder_snow_cauldron"
            };
            state(ctx, kind, Some(1))
        }
        n @ ("minecraft:water_cauldron" | "minecraft:powder_snow_cauldron") => {
            let same = (n == "minecraft:water_cauldron") == rain;
            let l = layers(ctx, here);
            if !same || l >= 3 || rng.unit() >= chance {
                return;
            }
            Some(ctx.shapes.with(here, "level", &(l + 1).to_string()))
        }
        _ => return,
    };
    if let Some(new) = new {
        set_block(level, ctx, p, new, Flags::ALL);
    }
}

/// A burning entity whose feet are at `y` and whose box reaches up to `top`
/// is in the block `p`: in a water or powder snow cauldron whose contents it
/// touches (below the surface, above the floor) the fire goes out and the
/// cauldron loses a layer (powder snow turns to water on the way). Says
/// whether it did.
pub fn put_out(level: &mut dyn Level, ctx: Ctx, p: BlockPos, y: f64, top: f64) -> bool {
    let here = level.block(p);
    let kind = name(ctx, here);
    if kind != "minecraft:water_cauldron" && kind != "minecraft:powder_snow_cauldron" {
        return false;
    }
    let l = layers(ctx, here);
    let floor = f64::from(p.y);
    if y > floor + (6.0 + 3.0 * f64::from(l)) / 16.0 || top <= floor + 0.25 {
        return false;
    }
    let new = if l <= 1 {
        ctx.shapes.default_state("minecraft:cauldron")
    } else {
        state(ctx, "minecraft:water_cauldron", Some(l - 1))
    };
    if let Some(new) = new {
        set_block(level, ctx, p, new, Flags::ALL);
    }
    true
}

fn is_down_dripstone(ctx: Ctx, state: u32) -> bool {
    name(ctx, state) == "minecraft:pointed_dripstone"
        && ctx.shapes.property(state, "vertical_direction") == Some("down")
}

/// The fluid a stalactite at `p` (its base) draws from the block above it.
fn fluid_above(level: &dyn Level, ctx: Ctx, p: BlockPos) -> Option<(Drip, BlockPos)> {
    // The fluid sits on the block the stalactite hangs from.
    let source = p.offset(Dir::Up).offset(Dir::Up);
    let s = level.block(source);
    let n = name(ctx, s);
    let still = ctx.shapes.property(s, "level") == Some("0");
    if n == "minecraft:mud" {
        return Some((Drip::Water, source));
    }
    if (n == "minecraft:water" && still) || ctx.shapes.property(s, "waterlogged") == Some("true") {
        return (!level.ultrawarm()).then_some((Drip::Water, source));
    }
    (n == "minecraft:lava" && still).then_some((Drip::Lava, source))
}

/// The tip of the stalactite whose top is at `p`, at most [`REACH`] down.
fn tip_below(level: &dyn Level, ctx: Ctx, p: BlockPos) -> Option<BlockPos> {
    let mut at = p;
    for _ in 0..REACH {
        let s = level.block(at);
        if !is_down_dripstone(ctx, s) {
            return None;
        }
        if ctx.shapes.property(s, "thickness") == Some("tip") {
            return Some(at);
        }
        at = at.offset(Dir::Down);
    }
    None
}

/// Whether the cauldron `state` can take a drop of `fluid`.
fn takes(ctx: Ctx, state: u32, fluid: Drip) -> bool {
    match name(ctx, state) {
        "minecraft:cauldron" => true,
        "minecraft:water_cauldron" => fluid == Drip::Water && layers(ctx, state) < 3,
        _ => false,
    }
}

/// The first block under the tip at `tip` that is not air, if it is a
/// cauldron that takes `fluid`.
fn cauldron_below(level: &dyn Level, ctx: Ctx, tip: BlockPos, fluid: Drip) -> Option<BlockPos> {
    let mut at = tip;
    for _ in 0..REACH {
        at = at.offset(Dir::Down);
        let s = level.block(at);
        if ctx.shapes.is_air(s) {
            continue;
        }
        return takes(ctx, s, fluid).then_some(at);
    }
    None
}

/// A random tick of a pointed dripstone at `p`: the top of a stalactite
/// under water (or mud) or lava now and then sends a drop down; a cauldron
/// below gets it a moment later. Mud above dries to clay instead.
pub fn dripstone_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    let roll = rng.unit();
    if roll >= WATER_DRIP {
        return;
    }
    let here = level.block(p);
    if !is_down_dripstone(ctx, here) || is_down_dripstone(ctx, level.block(p.offset(Dir::Up))) {
        return;
    }
    let Some((fluid, source)) = fluid_above(level, ctx, p) else {
        return;
    };
    let chance = match fluid {
        Drip::Water => WATER_DRIP,
        Drip::Lava => LAVA_DRIP,
    };
    if roll >= chance {
        return;
    }
    let Some(tip) = tip_below(level, ctx, p) else {
        return;
    };
    if name(ctx, level.block(source)) == "minecraft:mud" {
        if let Some(clay) = ctx.shapes.default_state("minecraft:clay") {
            set_block(level, ctx, source, clay, Flags::ALL);
            // World event 1504: a drop falls from the tip.
            level.level_event(1504, tip, 0);
        }
        return;
    }
    let Some(cauldron) = cauldron_below(level, ctx, tip, fluid) else {
        return;
    };
    level.level_event(1504, tip, 0);
    let kind = ctx.shapes.block_index(level.block(cauldron));
    level.schedule_block_tick(cauldron, kind, i64::from(50 + tip.y - cauldron.y), 0);
}

/// A cauldron's block tick: a drop from a stalactite above it lands.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let here = level.block(p);
    if ctx.shapes.block_index(here) != kind || !name(ctx, here).ends_with("cauldron") {
        return;
    }
    // The stalactite tip right above, through air.
    let mut at = p;
    let mut tip = None;
    for _ in 0..REACH {
        at = at.offset(Dir::Up);
        let s = level.block(at);
        if ctx.shapes.is_air(s) {
            continue;
        }
        if is_down_dripstone(ctx, s) && ctx.shapes.property(s, "thickness") == Some("tip") {
            tip = Some(at);
        }
        break;
    }
    let Some(tip) = tip else { return };
    // Up the stalactite to its top, then the fluid over it.
    let mut top = tip;
    while is_down_dripstone(ctx, level.block(top.offset(Dir::Up))) {
        top = top.offset(Dir::Up);
    }
    let Some((fluid, source)) = fluid_above(level, ctx, top) else {
        return;
    };
    if name(ctx, level.block(source)) == "minecraft:mud" || !takes(ctx, here, fluid) {
        return;
    }
    let (new, event) = match (name(ctx, here), fluid) {
        ("minecraft:cauldron", Drip::Water) => {
            (state(ctx, "minecraft:water_cauldron", Some(1)), 1047)
        }
        ("minecraft:cauldron", Drip::Lava) => (state(ctx, "minecraft:lava_cauldron", None), 1046),
        (_, Drip::Water) => (
            Some(
                ctx.shapes
                    .with(here, "level", &(layers(ctx, here) + 1).to_string()),
            ),
            1047,
        ),
        _ => return,
    };
    if let Some(new) = new {
        set_block(level, ctx, p, new, Flags::ALL);
        level.level_event(event, p, 0);
    }
}
