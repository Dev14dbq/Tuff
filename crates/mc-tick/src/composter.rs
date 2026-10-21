//! Composters (minecraft.wiki, "Composter"): each item put in fills a
//! layer by its chance; the seventh layer turns into bone meal ready to
//! take a second later.

use crate::dir::BlockPos;
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

/// Ticks between the last layer and the bone meal being ready.
pub const READY_DELAY: i64 = 20;

fn layers(ctx: Ctx, state: u32) -> u32 {
    ctx.shapes
        .property(state, "level")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// An item went into the composter `state` at `p`; `success` is its roll.
/// The fill shows (world event 1500) either way.
pub fn add(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, success: bool) {
    let l = layers(ctx, state);
    if success && l < 7 {
        let new = ctx.shapes.with(state, "level", &(l + 1).to_string());
        set_block(level, ctx, p, new, Flags::ALL);
        if l + 1 == 7 {
            level.schedule_block_tick(p, ctx.shapes.block_index(state), READY_DELAY, 0);
        }
        level.sound("block.composter.fill_success", p);
    } else {
        level.sound("block.composter.fill", p);
    }
    level.level_event(1500, p, i32::from(success));
}

/// The composter at `p` is emptied of its bone meal.
pub fn empty(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    set_block(
        level,
        ctx,
        p,
        ctx.shapes.with(state, "level", "0"),
        Flags::ALL,
    );
    level.sound("block.composter.empty", p);
}

/// The composter's block tick: a full one becomes ready.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind
        || ctx.shapes.block_name(state) != Some("minecraft:composter")
        || layers(ctx, state) != 7
    {
        return;
    }
    set_block(
        level,
        ctx,
        p,
        ctx.shapes.with(state, "level", "8"),
        Flags::ALL,
    );
    level.sound("block.composter.ready", p);
}
