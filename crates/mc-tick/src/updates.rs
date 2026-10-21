//! Setting a block and the updates that follow (minecraft.wiki, "Block
//! update"): the new block reacts to being placed, then neighbour-changed
//! updates go to the six neighbours, then post-placement (shape) updates,
//! depth first.
//!
//! A block that only changes its shape in answer to a shape update passes
//! shape updates on, but no neighbour-changed updates; one that breaks
//! (becomes air) sends both.

use crate::dir::{BlockPos, Dir};
use crate::facts::BlockRules;
use crate::fluids;
use crate::gravity;
use crate::level::Level;
use crate::redstone;
use crate::shapes::Shapes;

/// How deep shape updates may chain from one change.
pub const SHAPE_UPDATE_LIMIT: u32 = 512;

/// The block facts the update logic reads.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub shapes: &'a Shapes,
    pub facts: &'a BlockRules,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    /// Send neighbour-changed updates.
    pub neighbors: bool,
    /// Send shape updates.
    pub shapes: bool,
}

impl Flags {
    pub const ALL: Flags = Flags {
        neighbors: true,
        shapes: true,
    };
    pub const SHAPES_ONLY: Flags = Flags {
        neighbors: false,
        shapes: true,
    };
    pub const NONE: Flags = Flags {
        neighbors: false,
        shapes: false,
    };
}

/// Sets the block at `p` and runs the updates. Returns whether it changed.
pub fn set_block(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, flags: Flags) -> bool {
    set_with_limit(level, ctx, p, state, flags, SHAPE_UPDATE_LIMIT)
}

fn set_with_limit(
    level: &mut dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    flags: Flags,
    limit: u32,
) -> bool {
    let old = level.block(p);
    if old == state || !level.set(p, state) {
        return false;
    }
    // The new block reacts to being placed before its neighbours hear of it.
    fluids::on_place(level, ctx, p, state);
    crate::fire::on_place(level, ctx, p, old, state);
    redstone::on_place(level, ctx, p, old, state);
    crate::sponge::on_place(level, ctx, p, old, state);
    if ctx.shapes.block_index(old) != ctx.shapes.block_index(state) {
        crate::rails::on_place(level, ctx, p, state);
        crate::hopper::on_place(level, ctx, p, state);
    }
    gravity::wake(level, ctx, p, state);
    if ctx.shapes.block_index(old) != ctx.shapes.block_index(state) {
        powered_tnt(level, ctx, p);
    }
    if flags.neighbors {
        for dir in Dir::NC_ORDER {
            neighbor_changed(level, ctx, p.offset(dir), p);
        }
    }
    if flags.shapes && limit > 0 {
        update_neighbor_shapes(level, ctx, p, state, limit - 1);
        if redstone::part(ctx, state) == redstone::Part::Wire {
            update_diagonal_shapes(level, ctx, p, state, limit - 1);
        }
    }
    if ctx.shapes.block_index(old) != ctx.shapes.block_index(state) {
        crate::support::placed(level, ctx, p, state);
        crate::pistons::check(level, ctx, p);
    }
    true
}

/// Dust pointing sideways at a block that is not dust also updates the shapes
/// of the blocks above and below that block (minecraft.wiki, "Block
/// update"), so dust one level up or down notices it.
fn update_diagonal_shapes(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, limit: u32) {
    for dir in Dir::HORIZONTAL {
        if matches!(ctx.shapes.property(state, dir.name()), Some("none") | None) {
            continue;
        }
        let side = p.offset(dir);
        let side_state = level.block(side);
        if redstone::part(ctx, side_state) == redstone::Part::Wire {
            continue;
        }
        for target in [side.offset(Dir::Up), side.offset(Dir::Down)] {
            let current = level.block(target);
            if redstone::part(ctx, current) != redstone::Part::Wire {
                continue;
            }
            let new = redstone::wire_shape(level, ctx, target, current);
            if new != current {
                set_with_limit(level, ctx, target, new, Flags::SHAPES_ONLY, limit);
            }
        }
    }
}

/// Shape updates from the block at `p` (now `state`) to its neighbours.
pub fn update_neighbor_shapes(
    level: &mut dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    limit: u32,
) {
    for dir in Dir::PP_ORDER {
        let np = p.offset(dir);
        let current = level.block(np);
        fluids::shape_updated(level, ctx, np, current, state);
        let mut new = ctx
            .shapes
            .update_shape(level, np, current, dir.opposite(), state);
        match redstone::part(ctx, new) {
            redstone::Part::Wire => new = redstone::wire_shape(level, ctx, np, new),
            redstone::Part::NoteBlock if !dir.is_horizontal() => {
                new = redstone::note_instrument(level, ctx, np, new);
            }
            _ => {}
        }
        new = crate::fire::update_shape(level, ctx, np, new);
        new = crate::portal::update_shape(level, ctx, np, new, dir.opposite(), state);
        crate::leaves::shape_updated(level, ctx, np, new);
        // Something that lost what held it up breaks (or looks again next
        // tick).
        let from = dir.opposite();
        match crate::support::check(ctx, new) {
            crate::support::Check::From(d) if d != from => {}
            crate::support::Check::Never => {}
            crate::support::Check::Tick => {
                let kind = ctx.shapes.block_index(new);
                if !level.is_block_tick_scheduled(np, kind) {
                    level.schedule_block_tick(np, kind, 1, 0);
                }
            }
            _ => {
                if !crate::support::can_survive(level, ctx, np, new) {
                    level.destroyed(np, current);
                    new = ctx.shapes.air();
                }
            }
        }
        // An observer looking at `p` sees it change.
        if redstone::part(ctx, current) == redstone::Part::Observer
            && ctx.shapes.property(current, "facing") == Some(dir.opposite().name())
        {
            redstone::observed_change(level, ctx, np);
        }
        if new != current {
            let flags = if ctx.shapes.is_air(new) {
                Flags::ALL
            } else {
                Flags::SHAPES_ONLY
            };
            set_with_limit(level, ctx, np, new, flags, limit);
        }
    }
}

/// TNT next to power lights itself.
fn powered_tnt(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    if ctx.shapes.block_name(level.block(p)) == Some("minecraft:tnt")
        && redstone::received(level, ctx, p, true) > 0
        && level.prime_tnt(p)
    {
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
    }
}

/// A neighbour-changed update to the block at `p` from `from`: fluids and
/// redstone components react.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos, _from: BlockPos) {
    if crate::support::neighbor_changed(level, ctx, p) {
        return;
    }
    fluids::neighbor_changed(level, ctx, p);
    crate::sponge::neighbor_changed(level, ctx, p);
    redstone::neighbor_changed(level, ctx, p);
    powered_tnt(level, ctx, p);
    crate::rails::neighbor_changed(level, ctx, p);
    crate::hopper::neighbor_changed(level, ctx, p);
    crate::pistons::check(level, ctx, p);
    let state = level.block(p);
    gravity::wake(level, ctx, p, state);
}

/// A due block tick.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    redstone::block_tick(level, ctx, p, kind);
    crate::rails::block_tick(level, ctx, p, kind);
    crate::hopper::block_tick(level, ctx, p, kind);
    gravity::block_tick(level, ctx, p, kind);
    crate::fire::block_tick(level, ctx, p, kind);
    crate::leaves::block_tick(level, ctx, p, kind);
    let state = level.block(p);
    if ctx.shapes.block_index(state) == kind
        && crate::support::check(ctx, state) == crate::support::Check::Tick
        && !crate::support::can_survive(level, ctx, p, state)
    {
        level.destroyed(p, state);
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
    }
}

/// The state a block takes when placed at `p` by a player or a command:
/// connections and shapes from its neighbours.
pub fn placement_state(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u32 {
    let state = ctx.shapes.placement_shape(level, p, state);
    match redstone::part(ctx, state) {
        redstone::Part::Wire => redstone::wire_shape(level, ctx, p, state),
        redstone::Part::NoteBlock => redstone::note_instrument(level, ctx, p, state),
        _ => state,
    }
}

/// A player right-clicked the block at `p`: levers flip, doors, trapdoors and
/// fence gates (not iron ones) open or close, repeaters change delay.
/// Returns whether the block reacted.
pub fn use_block(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    if redstone::use_block(level, ctx, p) {
        return true;
    }
    // Iron doors and trapdoors open only by redstone.
    let state = level.block(p);
    let name = ctx.shapes.block_name(state).unwrap_or("");
    let opens =
        (name.ends_with("_door") || name.ends_with("_trapdoor") || name.ends_with("_fence_gate"))
            && !name.starts_with("minecraft:iron_");
    if !opens {
        return false;
    }
    let open = ctx.shapes.property(state, "open") == Some("true");
    let new = ctx
        .shapes
        .with(state, "open", if open { "false" } else { "true" });
    set_block(level, ctx, p, new, Flags::ALL)
}
