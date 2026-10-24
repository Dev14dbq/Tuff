//! Pointed dripstone growing (minecraft.wiki, "Pointed Dripstone" →
//! "Growth"): a stalactite hanging under a dripstone block with a water
//! source above it lengthens on a random tick now and then (up to 8 long),
//! or a stalagmite grows up from a floor below it; tips that meet merge.
//! The thickness of every piece follows what is in front of and behind it.

use crate::dir::{BlockPos, Dir};
use crate::fluids::fluid_of;
use crate::level::Level;
use crate::rng::Rng;
use crate::support::TOP_FULL;
use crate::updates::{Ctx, Flags, set_block};

/// Chance per random tick that the stalactite a tick lands on grows.
const GROW: f64 = 0.011_377_778;
/// A tip is looked for this far down from the top of a stalactite, and a
/// stalagmite is looked for this far below its tip.
const TIP_REACH: i32 = 7;
const FLOOR_REACH: i32 = 10;

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes.block_name(state).unwrap_or("")
}

/// Pointed dripstone with its tip pointing along `dir`.
fn pointed(ctx: Ctx, state: u32, dir: Dir) -> bool {
    name(ctx, state) == "minecraft:pointed_dripstone"
        && ctx.shapes.property(state, "vertical_direction") == Some(dir.name())
}

fn thickness<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes.property(state, "thickness").unwrap_or("tip")
}

fn is_tip(ctx: Ctx, state: u32, dir: Dir) -> bool {
    pointed(ctx, state, dir) && thickness(ctx, state) == "tip"
}

fn has_fluid(ctx: Ctx, state: u32) -> bool {
    fluid_of(ctx, state).is_some() || ctx.shapes.property(state, "waterlogged") == Some("true")
}

/// How thick the pointed dripstone at `p` pointing along `tip` should be
/// from its neighbours.
fn thickness_at(level: &dyn Level, ctx: Ctx, p: BlockPos, tip: Dir, merge: bool) -> &'static str {
    let front = level.block(p.offset(tip));
    if pointed(ctx, front, tip.opposite()) {
        return if !merge && thickness(ctx, front) != "tip_merge" {
            "tip"
        } else {
            "tip_merge"
        };
    }
    if !pointed(ctx, front, tip) {
        return "tip";
    }
    let t = thickness(ctx, front);
    if t == "tip" || t == "tip_merge" {
        "frustum"
    } else if pointed(ctx, level.block(p.offset(tip.opposite())), tip) {
        "middle"
    } else {
        "base"
    }
}

/// Whether pointed dripstone pointing along `tip` at `p` has what it hangs
/// from or stands on.
fn supported(level: &dyn Level, ctx: Ctx, p: BlockPos, tip: Dir) -> bool {
    let behind = level.block(p.offset(tip.opposite()));
    if pointed(ctx, behind, tip) {
        return true;
    }
    let need = if tip == Dir::Down {
        crate::support::BOTTOM_FULL
    } else {
        TOP_FULL
    };
    ctx.shapes.support(behind) & need != 0
}

/// The shape update of a pointed dripstone at `p` (now `state`) told by the
/// neighbour on its `from` side changed: its thickness looks again.
pub fn update_shape(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32, from: Dir) -> u32 {
    if name(ctx, state) != "minecraft:pointed_dripstone" || !matches!(from, Dir::Up | Dir::Down) {
        return state;
    }
    let tip = if pointed(ctx, state, Dir::Up) {
        Dir::Up
    } else {
        Dir::Down
    };
    // What holds it gone: it breaks on its own time, not reshaped now.
    if from == tip.opposite() && !supported(level, ctx, p, tip) {
        return state;
    }
    let merge = thickness(ctx, state) == "tip_merge";
    ctx.shapes
        .with(state, "thickness", thickness_at(level, ctx, p, tip, merge))
}

fn dripstone(ctx: Ctx, tip: Dir, thickness: &str, waterlogged: bool) -> Option<u32> {
    let base = ctx.shapes.default_state("minecraft:pointed_dripstone")?;
    let s = ctx.shapes.with(base, "vertical_direction", tip.name());
    let s = ctx.shapes.with(s, "thickness", thickness);
    Some(
        ctx.shapes
            .with(s, "waterlogged", if waterlogged { "true" } else { "false" }),
    )
}

fn create(level: &mut dyn Level, ctx: Ctx, p: BlockPos, tip: Dir, thickness: &str) {
    let wet = fluid_of(ctx, level.block(p)).is_some_and(|(f, _)| f == crate::fluids::Fluid::Water);
    if let Some(s) = dripstone(ctx, tip, thickness, wet) {
        set_block(level, ctx, p, s, Flags::ALL);
    }
}

/// The tip of the stalactite whose top is at `p`.
fn find_tip(level: &dyn Level, ctx: Ctx, top: BlockPos) -> Option<BlockPos> {
    if is_tip(ctx, level.block(top), Dir::Down) {
        return Some(top);
    }
    let mut at = top;
    for _ in 1..TIP_REACH {
        at = at.offset(Dir::Down);
        let s = level.block(at);
        if is_tip(ctx, s, Dir::Down) {
            return Some(at);
        }
        if !pointed(ctx, s, Dir::Down) {
            return None;
        }
    }
    None
}

/// Whether the tip at `p` pointing along `dir` has room to grow: nothing
/// but air, or the lone tip of the opposite kind, in front.
fn can_tip_grow(level: &dyn Level, ctx: Ctx, p: BlockPos, dir: Dir) -> bool {
    let next = level.block(p.offset(dir));
    if has_fluid(ctx, next) {
        return false;
    }
    ctx.shapes.is_air(next) || is_tip(ctx, next, dir.opposite())
}

/// Lengthens the tip at `tip` along `dir`: a new tip, or the two tips
/// that meet merge.
fn grow(level: &mut dyn Level, ctx: Ctx, tip: BlockPos, dir: Dir) {
    let at = tip.offset(dir);
    let existing = level.block(at);
    if is_tip(ctx, existing, dir.opposite()) {
        let (stalactite, stalagmite) = if pointed(ctx, existing, Dir::Up) {
            (at.offset(Dir::Up), at)
        } else {
            (at, at.offset(Dir::Down))
        };
        create(level, ctx, stalactite, Dir::Down, "tip_merge");
        create(level, ctx, stalagmite, Dir::Up, "tip_merge");
    } else if ctx.shapes.is_air(existing)
        || fluid_of(ctx, existing).is_some_and(|(f, _)| f == crate::fluids::Fluid::Water)
    {
        create(level, ctx, at, dir, "tip");
    }
}

/// Whether a drop can fall through the block (air, or something with no
/// collision and no fluid).
fn open(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let s = level.block(p);
    ctx.shapes.is_air(s) || (!ctx.facts.blocks_motion(s) && !has_fluid(ctx, s))
}

/// A stalagmite grows below the stalactite tip at `tip`: on the lone
/// upward tip under it, or from the floor within reach.
fn grow_stalagmite(level: &mut dyn Level, ctx: Ctx, tip: BlockPos) {
    let mut at = tip;
    for _ in 0..FLOOR_REACH {
        at = at.offset(Dir::Down);
        let s = level.block(at);
        if has_fluid(ctx, s) {
            return;
        }
        if is_tip(ctx, s, Dir::Up) && can_tip_grow(level, ctx, at, Dir::Up) {
            grow(level, ctx, at, Dir::Up);
            return;
        }
        let below = at.offset(Dir::Down);
        if supported(level, ctx, at, Dir::Up) && ctx.shapes.is_air(s) {
            let wet = level.block(below);
            if !fluid_of(ctx, wet).is_some_and(|(f, _)| f == crate::fluids::Fluid::Water) {
                grow(level, ctx, below, Dir::Up);
                return;
            }
        }
        if !open(level, ctx, at) {
            return;
        }
    }
}

fn water_source(ctx: Ctx, state: u32) -> bool {
    fluid_of(ctx, state).is_some_and(|(f, l)| f == crate::fluids::Fluid::Water && l == 0)
        || ctx.shapes.property(state, "waterlogged") == Some("true")
}

/// A random tick of the pointed dripstone at `p`: the top of a stalactite
/// under a dripstone block with water over it grows once in a while.
pub fn tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    if rng.unit() >= GROW {
        return;
    }
    let here = level.block(p);
    if !pointed(ctx, here, Dir::Down) || pointed(ctx, level.block(p.offset(Dir::Up)), Dir::Down) {
        return;
    }
    let above = p.offset(Dir::Up);
    if name(ctx, level.block(above)) != "minecraft:dripstone_block"
        || !water_source(ctx, level.block(above.offset(Dir::Up)))
    {
        return;
    }
    let Some(tip) = find_tip(level, ctx, p) else {
        return;
    };
    let ts = level.block(tip);
    if ctx.shapes.property(ts, "waterlogged") == Some("true")
        || !can_tip_grow(level, ctx, tip, Dir::Down)
    {
        return;
    }
    if rng.below(2) == 0 {
        grow(level, ctx, tip, Dir::Down);
    } else {
        grow_stalagmite(level, ctx, tip);
    }
}
