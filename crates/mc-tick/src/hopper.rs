//! Hoppers, dispensers and droppers (minecraft.wiki: "Hopper", "Dispenser",
//! "Dropper").
//!
//! A hopper is locked (`enabled` false) while powered. Every game tick its
//! cooldown runs down; with none left it first pushes one item into the
//! container it faces, then pulls one from the container above (or picks up
//! item entities there), and after any move waits 8 ticks. A hopper that
//! was empty and is handed an item rests too: 8 ticks, or 7 when it already
//! had its turn this tick (so that it always waits 7 turns; the wiki's
//! "pushed into an empty hopper, the cooldown lasts 7 game ticks").
//!
//! Dispensers and droppers read power at their own block or the one above
//! (like pistons, "quasi-connectivity"); a rising edge sets `triggered` and
//! schedules a tick 4 later, which fires them once. Crafters do the same,
//! reading power at their own block only, and craft.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::redstone;
use crate::updates::{Ctx, Flags, set_block};

/// Ticks a hopper rests after moving something.
pub const HOPPER_COOLDOWN: i32 = 8;
const DISPENSER_DELAY: i64 = 4;

/// The hoppers that tick, in the order they came (placed or loaded), which
/// is the order they take their turns in, as vanilla's block entity list.
#[derive(Debug, Default)]
pub struct Hoppers {
    order: Vec<BlockPos>,
    known: std::collections::HashSet<BlockPos>,
}

impl Hoppers {
    pub fn insert(&mut self, p: BlockPos) {
        if self.known.insert(p) {
            self.order.push(p);
        }
    }

    pub fn remove(&mut self, p: BlockPos) {
        if self.known.remove(&p) {
            self.order.retain(|k| *k != p);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &BlockPos> {
        self.order.iter()
    }
}

pub fn is_hopper(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.block_name(state) == Some("minecraft:hopper")
}

fn is_dispenser(ctx: Ctx, state: u32) -> Option<bool> {
    match ctx.shapes.block_name(state) {
        Some("minecraft:dispenser") => Some(false),
        Some("minecraft:dropper") => Some(true),
        _ => None,
    }
}

fn is_crafter(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.block_name(state) == Some("minecraft:crafter")
}

fn flag(ctx: Ctx, state: u32, name: &str) -> bool {
    ctx.shapes.property(state, name) == Some("true")
}

/// Locks or unlocks a hopper by the power around it.
fn check_enabled(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let enabled = redstone::received(level, ctx, p, true) == 0;
    if enabled != flag(ctx, state, "enabled") {
        let new = ctx
            .shapes
            .with(state, "enabled", if enabled { "true" } else { "false" });
        set_block(level, ctx, p, new, Flags::SHAPES_ONLY);
    }
}

fn powered_for_dispense(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    redstone::received(level, ctx, p, true) > 0
        || redstone::received(level, ctx, p.offset(Dir::Up), true) > 0
}

pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    if is_hopper(ctx, state) {
        check_enabled(level, ctx, p, state);
    }
}

pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if is_hopper(ctx, state) {
        check_enabled(level, ctx, p, state);
    } else if is_dispenser(ctx, state).is_some() || is_crafter(ctx, state) {
        let powered = if is_crafter(ctx, state) {
            redstone::received(level, ctx, p, true) > 0
        } else {
            powered_for_dispense(level, ctx, p)
        };
        let triggered = flag(ctx, state, "triggered");
        if powered && !triggered {
            let kind = ctx.shapes.block_index(state);
            level.schedule_block_tick(p, kind, DISPENSER_DELAY, 0);
            let new = ctx.shapes.with(state, "triggered", "true");
            set_block(level, ctx, p, new, Flags::SHAPES_ONLY);
        } else if !powered && triggered {
            let new = ctx.shapes.with(state, "triggered", "false");
            set_block(level, ctx, p, new, Flags::SHAPES_ONLY);
        }
    }
}

pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind {
        return;
    }
    if let Some(dropper) = is_dispenser(ctx, state)
        && let Some(facing) = ctx
            .shapes
            .property(state, "facing")
            .and_then(Dir::from_name)
    {
        level.dispense(p, facing, dropper);
    } else if is_crafter(ctx, state) {
        level.craft(p);
    }
}

/// One game tick of the hopper at `p` (the host calls it for every hopper
/// that exists in loaded chunks). `false` if there is no hopper there.
pub fn tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    tick_with(level, ctx, p, true)
}

/// [`tick`]; with `skip`, an empty hopper does not try to push and a full
/// one does not try to pull, as vanilla does (`tryMoveItems`). Either try
/// would move nothing and change nothing, so the result is the same; the
/// tests compare both.
pub fn tick_with(level: &mut dyn Level, ctx: Ctx, p: BlockPos, skip: bool) -> bool {
    let state = level.block(p);
    if !is_hopper(ctx, state) {
        return false;
    }
    level.mark_hopper_ticked(p);
    if level.count_down_hopper(p) > 1 {
        return true;
    }
    if !flag(ctx, state, "enabled") {
        return true;
    }
    let facing = ctx
        .shapes
        .property(state, "facing")
        .and_then(Dir::from_name)
        .unwrap_or(Dir::Down);
    let mut moved = false;
    // Push into what it faces.
    let empty = skip && level.container_empty(p) == Some(true);
    if !empty && push(level, ctx, p, facing) {
        moved = true;
    }
    // Pull from above (its own rest is set below).
    let full = skip && level.container_full(p) == Some(true);
    let above = p.offset(Dir::Up);
    if !full {
        if level.container_transfer(above, Dir::Down, p, Dir::Up) {
            moved = true;
            touched(level, ctx, above, p);
        } else if level.hopper_pickup(p) {
            moved = true;
            redstone::container_changed(level, ctx, p);
        }
    }
    if moved {
        level.set_hopper_cooldown(p, HOPPER_COOLDOWN);
    }
    true
}

/// The hopper at `p` pushes one item into what it faces.
fn push(level: &mut dyn Level, ctx: Ctx, p: BlockPos, facing: Dir) -> bool {
    let out = p.offset(facing);
    let was_empty = empty_hopper(level, ctx, out);
    if level.container_transfer(p, facing, out, facing.opposite()) {
        touched(level, ctx, p, out);
        if was_empty {
            let turn = i32::from(level.hopper_ticked(out));
            level.set_hopper_cooldown(out, HOPPER_COOLDOWN - turn);
        }
        true
    } else {
        false
    }
}

/// Whether `p` is a hopper with nothing in it.
pub fn empty_hopper(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    is_hopper(ctx, level.block(p)) && level.container_empty(p) == Some(true)
}

/// Two containers changed: comparators look again.
fn touched(level: &mut dyn Level, ctx: Ctx, from: BlockPos, to: BlockPos) {
    redstone::container_changed(level, ctx, from);
    redstone::container_changed(level, ctx, to);
}
