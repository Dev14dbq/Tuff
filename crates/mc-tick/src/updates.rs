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
use crate::level::Level;
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
    if level.block(p) == state || !level.set(p, state) {
        return false;
    }
    // The new block reacts to being placed before its neighbours hear of it.
    fluids::on_place(level, ctx, p, state);
    if flags.neighbors {
        for dir in Dir::NC_ORDER {
            neighbor_changed(level, ctx, p.offset(dir), p);
        }
    }
    if flags.shapes && limit > 0 {
        update_neighbor_shapes(level, ctx, p, state, limit - 1);
    }
    true
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
        let new = ctx
            .shapes
            .update_shape(level, np, current, dir.opposite(), state);
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

/// A neighbour-changed update to the block at `p` from `from`. Fluids react;
/// redstone components will (ROADMAP Ф3).
fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos, _from: BlockPos) {
    fluids::neighbor_changed(level, ctx, p);
}
