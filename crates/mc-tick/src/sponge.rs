//! Sponges (minecraft.wiki, "Sponge"): a dry sponge that is placed, or
//! touched by a neighbour change, soaks up water around it - up to 65
//! water blocks, at most 6 steps away, counting through the water it takes
//! (Manhattan distance) - and becomes a wet sponge. A wet sponge placed in
//! the Nether dries at once.

use std::collections::VecDeque;

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

/// Water blocks one sponge soaks up.
pub const MAX_BLOCKS: u32 = 65;
/// How many steps away from the sponge water is taken.
pub const MAX_DISTANCE: u32 = 6;

const ALL: [Dir; 6] = [
    Dir::Down,
    Dir::Up,
    Dir::North,
    Dir::South,
    Dir::West,
    Dir::East,
];

fn name(ctx: Ctx, state: u32) -> &'static str {
    // Block names live as long as the shapes table.
    match ctx.shapes.block_name(state) {
        Some("minecraft:sponge") => "sponge",
        Some("minecraft:wet_sponge") => "wet_sponge",
        _ => "",
    }
}

/// Whether the block holds water (a water block, waterlogged, kelp,
/// seagrass or a bubble column).
fn has_water(ctx: Ctx, state: u32) -> bool {
    if matches!(
        crate::fluids::fluid_of(ctx, state),
        Some((crate::fluids::Fluid::Water, _))
    ) {
        return true;
    }
    if ctx.shapes.property(state, "waterlogged") == Some("true") {
        return true;
    }
    matches!(
        ctx.shapes.block_name(state),
        Some(
            "minecraft:seagrass"
                | "minecraft:tall_seagrass"
                | "minecraft:kelp"
                | "minecraft:kelp_plant"
                | "minecraft:bubble_column"
        )
    )
}

/// Takes the water out of the block at `p`.
fn drain(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    if ctx.shapes.property(state, "waterlogged") == Some("true") {
        let dry = ctx.shapes.with(state, "waterlogged", "false");
        set_block(level, ctx, p, dry, Flags::ALL);
        return;
    }
    let plant = matches!(
        ctx.shapes.block_name(state),
        Some(
            "minecraft:seagrass"
                | "minecraft:tall_seagrass"
                | "minecraft:kelp"
                | "minecraft:kelp_plant"
        )
    );
    if plant {
        level.destroyed(p, state);
    }
    set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
}

/// Soaks up the water around the dry sponge at `p`; `true` if it did.
pub fn absorb(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let mut seen = vec![p];
    let mut queue = VecDeque::from([(p, 0u32)]);
    let mut count = 0;
    while let Some((at, distance)) = queue.pop_front() {
        for dir in ALL {
            let next = at.offset(dir);
            if seen.contains(&next) {
                continue;
            }
            seen.push(next);
            let state = level.block(next);
            if !has_water(ctx, state) {
                continue;
            }
            drain(level, ctx, next, state);
            count += 1;
            if count >= MAX_BLOCKS {
                return true;
            }
            if distance < MAX_DISTANCE {
                queue.push_back((next, distance + 1));
            }
        }
    }
    count > 0
}

fn try_absorb(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let dry = level.block(p);
    if name(ctx, dry) != "sponge" {
        return;
    }
    let Some(wet) = ctx.shapes.default_state("minecraft:wet_sponge") else {
        return;
    };
    // Wet while it soaks, so the changes around it do not start it again.
    level.set(p, wet);
    if absorb(level, ctx, p) {
        level.set(p, dry);
        set_block(level, ctx, p, wet, Flags::ALL);
    } else {
        level.set(p, dry);
    }
}

/// A block was set at `p`.
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, old: u32, state: u32) {
    if ctx.shapes.block_index(old) == ctx.shapes.block_index(state) {
        return;
    }
    match name(ctx, state) {
        "sponge" => try_absorb(level, ctx, p),
        "wet_sponge" if level.ultrawarm() => {
            if let Some(dry) = ctx.shapes.default_state("minecraft:sponge") {
                set_block(level, ctx, p, dry, Flags::ALL);
                level.sponge_dried(p);
            }
        }
        _ => {}
    }
}

/// A neighbour of the block at `p` changed.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    try_absorb(level, ctx, p);
}
