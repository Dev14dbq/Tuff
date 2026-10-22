//! Nether portal frames (minecraft.wiki, "Nether portal"): an upright
//! rectangle of obsidian with an inside 2–21 wide and 3–21 tall (corners
//! do not matter). Fire inside an empty one fills it with portal blocks; a
//! portal block whose frame is damaged disappears.

use crate::dir::{Axis, BlockPos, Dir};
use crate::level::Level;
use crate::updates::Ctx;

const MIN_WIDTH: i32 = 2;
const MAX_SIZE: i32 = 21;
const MIN_HEIGHT: i32 = 3;

/// A rectangle of obsidian with room inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// `X`: the portal lies in the x–y plane; `Z`: in the z–y plane.
    pub axis: Axis,
    /// The lowest inside block on the side the width counts from.
    pub bottom_left: BlockPos,
    pub width: i32,
    pub height: i32,
    /// Portal blocks already inside.
    pub portals: i32,
}

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes.block_name(state).unwrap_or("")
}

fn empty(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.is_air(state)
        || matches!(
            name(ctx, state),
            "minecraft:fire" | "minecraft:nether_portal"
        )
}

fn frame(ctx: Ctx, state: u32) -> bool {
    name(ctx, state) == "minecraft:obsidian"
}

fn right_of(axis: Axis) -> Dir {
    if axis == Axis::X {
        Dir::West
    } else {
        Dir::South
    }
}

fn step(p: BlockPos, dir: Dir, n: i32) -> BlockPos {
    let (dx, dy, dz) = dir.offset();
    BlockPos::new(p.x + dx * n, p.y + dy * n, p.z + dz * n)
}

/// Steps from `p` along `dir` while the blocks are empty with obsidian
/// under them; the count if they end at a frame block, else 0.
fn distance_until_edge(level: &dyn Level, ctx: Ctx, p: BlockPos, dir: Dir) -> i32 {
    for i in 0..=MAX_SIZE {
        let at = step(p, dir, i);
        let state = level.block(at);
        if !empty(ctx, state) {
            return if frame(ctx, state) { i } else { 0 };
        }
        if !frame(ctx, level.block(at.offset(Dir::Down))) {
            break;
        }
    }
    0
}

/// The portal frame around `p` (empty or with portal blocks) lying along
/// `axis`.
pub fn find(level: &dyn Level, ctx: Ctx, p: BlockPos, axis: Axis) -> Option<Shape> {
    let right = right_of(axis);
    let left = right.opposite();
    let mut pos = p;
    for _ in 0..MAX_SIZE {
        let below = pos.offset(Dir::Down);
        if !level.in_world(below) || !empty(ctx, level.block(below)) {
            break;
        }
        pos = below;
    }
    let to_left = distance_until_edge(level, ctx, pos, left) - 1;
    if to_left < 0 {
        return None;
    }
    let bottom_left = step(pos, left, to_left);
    let width = distance_until_edge(level, ctx, bottom_left, right);
    if !(MIN_WIDTH..=MAX_SIZE).contains(&width) {
        return None;
    }
    let mut portals = 0;
    let mut height = 0;
    'rows: while height < MAX_SIZE {
        for w in 0..width {
            let at = step(bottom_left.offset_up(height), right, w);
            let state = level.block(at);
            if !empty(ctx, state) {
                break 'rows;
            }
            if name(ctx, state) == "minecraft:nether_portal" {
                portals += 1;
            }
            let side = if w == 0 {
                Some(step(at, left, 1))
            } else if w == width - 1 {
                Some(step(at, right, 1))
            } else {
                None
            };
            if let Some(side) = side
                && !frame(ctx, level.block(side))
            {
                break 'rows;
            }
        }
        height += 1;
    }
    // The top row must be obsidian.
    for w in 0..width {
        let above = step(bottom_left.offset_up(height), right, w);
        if !frame(ctx, level.block(above)) {
            return None;
        }
    }
    (MIN_HEIGHT..=MAX_SIZE).contains(&height).then_some(Shape {
        axis,
        bottom_left,
        width,
        height,
        portals,
    })
}

impl BlockPos {
    fn offset_up(self, n: i32) -> BlockPos {
        BlockPos::new(self.x, self.y + n, self.z)
    }
}

impl Shape {
    /// Whether the whole inside is portal blocks.
    pub fn complete(&self) -> bool {
        self.portals == self.width * self.height
    }

    /// The inside's blocks.
    pub fn inside(&self) -> impl Iterator<Item = BlockPos> + '_ {
        let right = right_of(self.axis);
        (0..self.width).flat_map(move |w| {
            (0..self.height).map(move |h| step(self.bottom_left.offset_up(h), right, w))
        })
    }
}

/// Fire was just placed at `p`: if it is inside an empty frame, fills the
/// frame with portal blocks. Returns whether it did.
pub fn try_light(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    if !level.allows_nether_portal() {
        return false;
    }
    for axis in [Axis::X, Axis::Z] {
        let Some(shape) = find(level, ctx, p, axis) else {
            continue;
        };
        if shape.portals != 0 {
            continue;
        }
        let value = if axis == Axis::X { "x" } else { "z" };
        let Some(portal) = ctx
            .shapes
            .state("minecraft:nether_portal", &[("axis", value)])
        else {
            return false;
        };
        let cells: Vec<BlockPos> = shape.inside().collect();
        for cell in cells {
            level.set(cell, portal);
        }
        return true;
    }
    false
}

/// A neighbour of the portal block `state` at `p` (in direction `dir`)
/// became `neighbor`: the block goes if its frame is no longer whole.
pub fn update_shape(
    level: &dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    dir: Dir,
    neighbor: u32,
) -> u32 {
    if name(ctx, state) != "minecraft:nether_portal" {
        return state;
    }
    let axis = match ctx.shapes.property(state, "axis") {
        Some("z") => Axis::Z,
        _ => Axis::X,
    };
    // A change across the portal's plane does not matter.
    let across = dir.axis() != axis && dir.is_horizontal();
    if across || name(ctx, neighbor) == "minecraft:nether_portal" {
        return state;
    }
    match find(level, ctx, p, axis) {
        Some(shape) if shape.complete() => state,
        _ => ctx.shapes.air(),
    }
}
