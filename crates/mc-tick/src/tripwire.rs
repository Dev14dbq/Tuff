//! Tripwire (minecraft.wiki, "Tripwire Hook", "String" → "Tripwire").
//!
//! Two hooks facing each other with 1 to 40 blocks of string between them
//! are attached. Anything standing in the string powers the string, and the
//! hooks give power while any string of the line is powered and not
//! disarmed (cut with shears). Breaking a string trips the line as well.
//! The hooks look again 10 ticks after the string is powered; powered
//! string looks again every 10 ticks while something is in it.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

/// Blocks a hook looks along for the other hook.
const REACH: i32 = 42;
const RECHECK: i64 = 10;

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes.block_name(state).unwrap_or("")
}

fn is_hook(ctx: Ctx, state: u32) -> bool {
    name(ctx, state) == "minecraft:tripwire_hook"
}

fn is_wire(ctx: Ctx, state: u32) -> bool {
    name(ctx, state) == "minecraft:tripwire"
}

fn flag(ctx: Ctx, state: u32, key: &str) -> bool {
    ctx.shapes.property(state, key) == Some("true")
}

fn with(ctx: Ctx, state: u32, key: &str, value: bool) -> u32 {
    ctx.shapes
        .with(state, key, if value { "true" } else { "false" })
}

fn facing(ctx: Ctx, state: u32) -> Option<Dir> {
    ctx.shapes
        .property(state, "facing")
        .and_then(Dir::from_name)
}

fn walk(p: BlockPos, d: Dir, n: i32) -> BlockPos {
    let (dx, dy, dz) = d.offset();
    BlockPos::new(p.x + dx * n, p.y + dy * n, p.z + dz * n)
}

/// The string's sides that join string or a hook facing it.
pub fn connections(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u32 {
    let mut s = state;
    for d in Dir::HORIZONTAL {
        let n = level.block(p.offset(d));
        let joins = is_wire(ctx, n) || (is_hook(ctx, n) && facing(ctx, n) == Some(d.opposite()));
        s = with(ctx, s, d.name(), joins);
    }
    s
}

/// Power and attachment of the hook at `p` (and the hook facing it) worked
/// out again from the line between them. `removed`: the hook is going.
/// `at`/`wire`: the string at that distance is taken to be `wire` (one that
/// just changed or is being removed).
fn update_hook(
    level: &mut dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    removed: bool,
    notify: bool,
    at: Option<(i32, u32)>,
) {
    let Some(dir) = facing(ctx, state) else {
        return;
    };
    let was_attached = flag(ctx, state, "attached");
    let was_powered = flag(ctx, state, "powered");
    let mut attached = !removed;
    let mut powered = false;
    let mut other = 0;
    let mut line: Vec<Option<u32>> = vec![None; REACH as usize];
    for i in 1..REACH {
        let q = walk(p, dir, i);
        let mut s = level.block(q);
        if is_hook(ctx, s) {
            if facing(ctx, s) == Some(dir.opposite()) {
                other = i;
            }
            break;
        }
        let here = at.filter(|&(j, _)| j == i);
        if !is_wire(ctx, s) && here.is_none() {
            attached = false;
            continue;
        }
        if let Some((_, w)) = here {
            s = w;
        }
        let armed = !flag(ctx, s, "disarmed");
        powered |= armed && flag(ctx, s, "powered");
        line[i as usize] = Some(s);
        if here.is_some() {
            level.schedule_block_tick(p, ctx.shapes.block_index(state), RECHECK, 0);
            attached &= armed;
        }
    }
    attached &= other > 1;
    powered &= attached;
    let base = with(
        ctx,
        with(ctx, state, "attached", attached),
        "powered",
        powered,
    );
    if other > 0 {
        let q = walk(p, dir, other);
        let opposite = ctx.shapes.with(base, "facing", dir.opposite().name());
        set_block(level, ctx, q, opposite, Flags::ALL);
        notify_around(level, ctx, q, dir.opposite());
        sound(level, q, attached, powered, was_attached, was_powered);
    }
    sound(level, p, attached, powered, was_attached, was_powered);
    if !removed {
        set_block(level, ctx, p, base, Flags::ALL);
        if notify {
            notify_around(level, ctx, p, dir);
        }
    }
    if was_attached != attached {
        for i in 1..other {
            let q = walk(p, dir, i);
            let Some(s) = line[i as usize] else { continue };
            let now = level.block(q);
            if is_wire(ctx, now) || is_hook(ctx, now) {
                set_block(
                    level,
                    ctx,
                    q,
                    with(ctx, s, "attached", attached),
                    Flags::ALL,
                );
            }
        }
    }
}

fn sound(
    level: &mut dyn Level,
    p: BlockPos,
    attached: bool,
    powered: bool,
    was_a: bool,
    was_p: bool,
) {
    let name = if powered && !was_p {
        "block.tripwire.click_on"
    } else if !powered && was_p {
        "block.tripwire.click_off"
    } else if attached && !was_a {
        "block.tripwire.attach"
    } else if !attached && was_a {
        "block.tripwire.detach"
    } else {
        return;
    };
    level.sound(name, p);
}

/// The hook's neighbours and the block it hangs on hear of a change.
fn notify_around(level: &mut dyn Level, ctx: Ctx, p: BlockPos, facing: Dir) {
    crate::redstone::update_neighbors_at(level, ctx, p);
    crate::redstone::update_neighbors_at(level, ctx, p.offset(facing.opposite()));
}

/// The string at `p` (now `wire`) changed: the hooks at the ends of its
/// line look again.
fn update_source(level: &mut dyn Level, ctx: Ctx, p: BlockPos, wire: u32) {
    for d in [Dir::South, Dir::West] {
        for i in 1..REACH {
            let q = walk(p, d, i);
            let s = level.block(q);
            if is_hook(ctx, s) {
                if facing(ctx, s) == Some(d.opposite()) {
                    update_hook(level, ctx, q, s, false, true, Some((i, wire)));
                }
                break;
            }
            if !is_wire(ctx, s) {
                break;
            }
        }
    }
}

/// A block was set at `p` (it was `old`).
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, old: u32, state: u32) {
    let same = ctx.shapes.block_index(old) == ctx.shapes.block_index(state);
    if is_wire(ctx, state) && !same {
        update_source(level, ctx, p, state);
        refresh_neighbours(level, ctx, p);
    } else if is_wire(ctx, old) && !same {
        // Breaking string trips the line (unless it was cut).
        update_source(level, ctx, p, with(ctx, old, "powered", true));
        refresh_neighbours(level, ctx, p);
    } else if is_hook(ctx, state) && !same {
        update_hook(level, ctx, p, state, false, false, None);
        refresh_neighbours(level, ctx, p);
    } else if is_hook(ctx, old) && !same {
        if flag(ctx, old, "attached") || flag(ctx, old, "powered") {
            update_hook(level, ctx, p, old, true, false, None);
        }
        if flag(ctx, old, "powered")
            && let Some(f) = facing(ctx, old)
        {
            notify_around(level, ctx, p, f);
        }
        refresh_neighbours(level, ctx, p);
    }
}

/// String beside `p` joins (or leaves) it.
fn refresh_neighbours(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    for d in Dir::HORIZONTAL {
        let q = p.offset(d);
        let s = level.block(q);
        if is_wire(ctx, s) {
            let new = connections(level, ctx, q, s);
            if new != s {
                set_block(level, ctx, q, new, Flags::NONE);
            }
        }
    }
}

/// The box things must touch to press the string.
fn press_box(ctx: Ctx, p: BlockPos, state: u32) -> ([f64; 3], [f64; 3]) {
    let (lo, hi) = if flag(ctx, state, "attached") {
        (1.0 / 16.0, 2.5 / 16.0)
    } else {
        (0.0, 0.5)
    };
    (
        [f64::from(p.x), f64::from(p.y) + lo, f64::from(p.z)],
        [
            f64::from(p.x) + 1.0,
            f64::from(p.y) + hi,
            f64::from(p.z) + 1.0,
        ],
    )
}

/// Whether anything is in the string at `p` now; updates it and the hooks.
fn check_pressed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if !is_wire(ctx, state) {
        return;
    }
    let (min, max) = press_box(ctx, p, state);
    let pressed = level.entities_in(min, max).0 > 0;
    if pressed != flag(ctx, state, "powered") {
        let new = with(ctx, state, "powered", pressed);
        set_block(level, ctx, p, new, Flags::NONE);
        update_source(level, ctx, p, new);
    }
    if pressed {
        level.schedule_block_tick(p, ctx.shapes.block_index(state), RECHECK, 0);
    }
}

/// Something is in the block at `p`.
pub fn entity_inside(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if is_wire(ctx, state) && !flag(ctx, state, "powered") {
        check_pressed(level, ctx, p);
    }
}

/// A due block tick: string looks for things in it, a hook at its line.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind {
        return;
    }
    if is_wire(ctx, state) {
        if flag(ctx, state, "powered") {
            check_pressed(level, ctx, p);
        }
    } else if is_hook(ctx, state) {
        update_hook(level, ctx, p, state, false, true, None);
    }
}

/// Shears cut the string at `p` before it breaks: the line does not trip.
pub fn disarm(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if is_wire(ctx, state) {
        level.set(p, with(ctx, state, "disarmed", true));
    }
}

/// Power a hook gives toward `toward` (strong only into the block it hangs
/// on).
pub fn emits(ctx: Ctx, state: u32, toward: Dir, strong: bool) -> u8 {
    if !is_hook(ctx, state) || !flag(ctx, state, "powered") {
        return 0;
    }
    if strong && facing(ctx, state).map(Dir::opposite) != Some(toward) {
        return 0;
    }
    15
}
