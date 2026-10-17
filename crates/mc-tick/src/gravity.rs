//! Blocks that fall (minecraft.wiki, "Falling Block"): sand, gravel,
//! concrete powder, anvils and the like. Placed or updated, they check two
//! ticks later whether the block below lets them fall; if so they turn into
//! a falling block entity.

use crate::dir::{BlockPos, Dir};
use crate::fluids::fluid_of;
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

pub const DELAY: i64 = 2;

pub fn falls(ctx: Ctx, state: u32) -> bool {
    let name = ctx.shapes.block_name(state).unwrap_or("");
    name.ends_with("_concrete_powder")
        || matches!(
            name,
            "minecraft:sand"
                | "minecraft:red_sand"
                | "minecraft:gravel"
                | "minecraft:suspicious_sand"
                | "minecraft:suspicious_gravel"
                | "minecraft:anvil"
                | "minecraft:chipped_anvil"
                | "minecraft:damaged_anvil"
        )
}

/// What a falling block can fall into (and land in): air, fire, fluids and
/// replaceable blocks.
pub fn free(ctx: Ctx, state: u32) -> bool {
    let name = ctx.shapes.block_name(state).unwrap_or("");
    ctx.shapes.is_air(state)
        || fluid_of(ctx, state).is_some()
        || name == "minecraft:fire"
        || name == "minecraft:soul_fire"
        || ctx.facts.is_replaceable(state)
}

/// Placed or a neighbour changed: look again in two ticks.
pub fn wake(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    if falls(ctx, state) {
        level.schedule_block_tick(p, ctx.shapes.block_index(state), DELAY, 0);
    }
}

pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if !falls(ctx, state) || ctx.shapes.block_index(state) != kind {
        return;
    }
    if free(ctx, level.block(p.offset(Dir::Down))) && level.in_world(p.offset(Dir::Down)) {
        set_block(level, ctx, p, ctx.facts.air, Flags::ALL);
        level.spawn_falling_block(p, state);
    }
}
