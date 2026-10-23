//! Frosted ice (minecraft.wiki, "Frosted Ice"), the ice Frost Walker
//! leaves: on its ticks it ages a stage when lit enough (always with few
//! frosted neighbours, else one time in three) and melts into water after
//! the fourth stage, taking frosted neighbours a stage with it.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::rng::Rng;
use crate::updates::{Ctx, Flags, set_block};

const NAME: &str = "minecraft:frosted_ice";

fn is_frosted(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.block_name(state) == Some(NAME)
}

fn age(ctx: Ctx, state: u32) -> u32 {
    ctx.shapes
        .property(state, "age")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Whether fewer than `n` of the six neighbours are frosted ice.
fn fewer_neighbours_than(level: &dyn Level, ctx: Ctx, p: BlockPos, n: usize) -> bool {
    let mut count = 0;
    for d in Dir::ALL {
        if is_frosted(ctx, level.block(p.offset(d))) {
            count += 1;
            if count >= n {
                return false;
            }
        }
    }
    true
}

/// Turns the ice at `p` to water (or air where water cannot stay).
fn melt(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let water = if level.ultrawarm() {
        ctx.shapes.air()
    } else {
        ctx.shapes
            .default_state("minecraft:water")
            .unwrap_or(ctx.shapes.air())
    };
    set_block(level, ctx, p, water, Flags::ALL);
}

/// One stage older, or melted after the last; whether it melted.
fn slightly_melt(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let a = age(ctx, state);
    if a < 3 {
        let older = ctx.shapes.with(state, "age", &(a + 1).to_string());
        set_block(level, ctx, p, older, Flags::SHAPES_ONLY);
        false
    } else {
        melt(level, ctx, p);
        true
    }
}

/// A due tick or a random tick of the frosted ice at `p`.
pub fn tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    let state = level.block(p);
    if !is_frosted(ctx, state) {
        return;
    }
    let lit = i64::from(level.brightness(p))
        > 11 - i64::from(age(ctx, state)) - i64::from(level.light_block(state));
    if (rng.below(3) == 0 || fewer_neighbours_than(level, ctx, p, 4))
        && lit
        && slightly_melt(level, ctx, p, state)
    {
        for d in Dir::ALL {
            let q = p.offset(d);
            let s = level.block(q);
            if is_frosted(ctx, s) {
                slightly_melt(level, ctx, q, s);
            }
        }
    } else {
        let kind = ctx.shapes.block_index(state);
        level.schedule_block_tick(p, kind, 20 + i64::from(rng.below(21)), 0);
    }
}

/// A due block tick.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind || !is_frosted(ctx, state) {
        return;
    }
    let mut rng = Rng::at(p, level.game_time());
    tick(level, ctx, p, &mut rng);
}

/// A neighbour of the ice at `p` changed: frosted ice with fewer than two
/// frosted neighbours melts when one of them goes (melted to water or
/// broken; what was there is not known here, what is there now tells).
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos, from: BlockPos) {
    let state = level.block(p);
    if !is_frosted(ctx, state) {
        return;
    }
    let other = level.block(from);
    let gone = ctx.shapes.is_air(other) || ctx.shapes.block_name(other) == Some("minecraft:water");
    if gone && fewer_neighbours_than(level, ctx, p, 2) {
        melt(level, ctx, p);
    }
}

/// Frosted ice just placed at `p` (Frost Walker) asks for its first tick
/// 60–119 ticks later.
pub fn placed(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    let state = level.block(p);
    if is_frosted(ctx, state) {
        let kind = ctx.shapes.block_index(state);
        level.schedule_block_tick(p, kind, 60 + i64::from(rng.below(60)), 0);
    }
}
