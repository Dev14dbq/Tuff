//! Leaves (minecraft.wiki, "Leaves"): each knows how far it is from a log
//! (1-7, through other leaves). A change nearby asks for a tick a game tick
//! later, which works the distance out again; leaves 7 away that no player
//! placed decay on a random tick.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

pub const FAR: u32 = 7;

fn is_leaves(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.in_tag(state, "minecraft:leaves") && ctx.shapes.property(state, "distance").is_some()
}

/// How far `state` is from a log, as its leaf neighbour sees it.
fn distance_of(ctx: Ctx, state: u32) -> u32 {
    if ctx.shapes.in_tag(state, "minecraft:logs") {
        return 0;
    }
    if is_leaves(ctx, state) {
        return ctx
            .shapes
            .property(state, "distance")
            .and_then(|d| d.parse().ok())
            .unwrap_or(FAR);
    }
    FAR
}

/// The distance leaves at `p` should have now.
pub fn distance_at(level: &dyn Level, ctx: Ctx, p: BlockPos) -> u32 {
    Dir::ALL
        .iter()
        .map(|&d| distance_of(ctx, level.block(p.offset(d))) + 1)
        .min()
        .unwrap_or(FAR)
        .min(FAR)
}

/// A neighbour of the leaves at `p` changed: look again next tick if the
/// distance is not what it was.
pub fn shape_updated(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    if !is_leaves(ctx, state) {
        return;
    }
    let now = distance_at(level, ctx, p);
    if now != 1 || distance_of(ctx, state) != now {
        let kind = ctx.shapes.block_index(state);
        if !level.is_block_tick_scheduled(p, kind) {
            level.schedule_block_tick(p, kind, 1, 0);
        }
    }
}

/// The due tick: store the new distance.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind || !is_leaves(ctx, state) {
        return;
    }
    let now = distance_at(level, ctx, p);
    let new = ctx.shapes.with(state, "distance", &now.to_string());
    if new != state {
        set_block(level, ctx, p, new, Flags::ALL);
    }
}

/// Leaves cut off from logs that no player placed.
pub fn decays(ctx: Ctx, state: u32) -> bool {
    is_leaves(ctx, state)
        && ctx.shapes.property(state, "persistent") == Some("false")
        && distance_of(ctx, state) == FAR
}
