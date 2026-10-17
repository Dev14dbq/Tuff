//! Setting a block and the updates that follow (minecraft.wiki, "Block
//! update"): neighbour-changed updates to the six neighbours, then
//! post-placement (shape) updates to them, depth first.
//!
//! A block that only changes its shape in answer to a shape update passes
//! shape updates on, but no neighbour-changed updates; one that breaks
//! (becomes air) sends both.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::shapes::Shapes;

/// How deep shape updates may chain from one change.
pub const SHAPE_UPDATE_LIMIT: u32 = 512;

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
pub fn set_block(
    level: &mut dyn Level,
    shapes: &Shapes,
    p: BlockPos,
    state: u32,
    flags: Flags,
) -> bool {
    set_with_limit(level, shapes, p, state, flags, SHAPE_UPDATE_LIMIT)
}

fn set_with_limit(
    level: &mut dyn Level,
    shapes: &Shapes,
    p: BlockPos,
    state: u32,
    flags: Flags,
    limit: u32,
) -> bool {
    if level.block(p) == state || !level.set(p, state) {
        return false;
    }
    if flags.neighbors {
        for dir in Dir::NC_ORDER {
            neighbor_changed(level, shapes, p.offset(dir), p);
        }
    }
    if flags.shapes && limit > 0 {
        update_neighbor_shapes(level, shapes, p, state, limit - 1);
    }
    true
}

/// Shape updates from the block at `p` (now `state`) to its neighbours.
pub fn update_neighbor_shapes(
    level: &mut dyn Level,
    shapes: &Shapes,
    p: BlockPos,
    state: u32,
    limit: u32,
) {
    for dir in Dir::PP_ORDER {
        let np = p.offset(dir);
        let current = level.block(np);
        let new = shapes.update_shape(level, np, current, dir.opposite(), state);
        if new != current {
            let flags = if shapes.is_air(new) {
                Flags::ALL
            } else {
                Flags::SHAPES_ONLY
            };
            set_with_limit(level, shapes, np, new, flags, limit);
        }
    }
}

/// A neighbour-changed update to the block at `p` from `from`. No block
/// reacts yet; redstone components and fluids will (ROADMAP Ф3).
fn neighbor_changed(_level: &mut dyn Level, _shapes: &Shapes, _p: BlockPos, _from: BlockPos) {}
