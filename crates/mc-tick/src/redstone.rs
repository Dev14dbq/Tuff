//! Redstone, from the public descriptions (minecraft.wiki: "Redstone
//! mechanics", "Redstone Dust", "Redstone Torch", "Redstone Repeater",
//! "Lever", "Redstone Lamp") and checked on the parity bench.
//!
//! Power: a component *emits* power towards its neighbours, and some send
//! *strong* power into one block (a lever into the block it is on, a torch
//! into the block above, a repeater into the block in front). A conductor
//! (observed per state) holding strong power passes it on to anything next
//! to it. Power from dust into a conductor only counts for things that are
//! not dust: while dust works out its own level it ignores dust everywhere
//! except its direct neighbours, which is what makes such blocks "weakly
//! powered".

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    None,
    Wire,
    Torch,
    WallTorch,
    Lever,
    Block,
    Lamp,
    Repeater,
}

pub fn part(ctx: Ctx, state: u32) -> Part {
    match ctx.shapes.block_name(state) {
        Some("minecraft:redstone_wire") => Part::Wire,
        Some("minecraft:redstone_torch") => Part::Torch,
        Some("minecraft:redstone_wall_torch") => Part::WallTorch,
        Some("minecraft:lever") => Part::Lever,
        Some("minecraft:redstone_block") => Part::Block,
        Some("minecraft:redstone_lamp") => Part::Lamp,
        Some("minecraft:repeater") => Part::Repeater,
        _ => Part::None,
    }
}

const TORCH_DELAY: i64 = 2;
const LAMP_OFF_DELAY: i64 = 4;

fn is(ctx: Ctx, state: u32, property: &str) -> bool {
    ctx.shapes.property(state, property) == Some("true")
}

fn with(ctx: Ctx, state: u32, property: &str, value: &str) -> u32 {
    ctx.shapes.with(state, property, value)
}

fn with_bool(ctx: Ctx, state: u32, property: &str, value: bool) -> u32 {
    with(ctx, state, property, if value { "true" } else { "false" })
}

fn facing(ctx: Ctx, state: u32) -> Option<Dir> {
    ctx.shapes
        .property(state, "facing")
        .and_then(Dir::from_name)
}

fn wire_power(ctx: Ctx, state: u32) -> u8 {
    if part(ctx, state) == Part::Wire {
        ctx.shapes
            .property(state, "power")
            .and_then(|p| p.parse().ok())
            .unwrap_or(0)
    } else {
        0
    }
}

/// The side of a torch or lever that holds it up.
fn attached(ctx: Ctx, state: u32) -> Option<Dir> {
    match part(ctx, state) {
        Part::Torch => Some(Dir::Down),
        Part::WallTorch => facing(ctx, state).map(Dir::opposite),
        Part::Lever => match ctx.shapes.property(state, "face") {
            Some("floor") => Some(Dir::Down),
            Some("ceiling") => Some(Dir::Up),
            _ => facing(ctx, state).map(Dir::opposite),
        },
        _ => None,
    }
}

/// A repeater takes power from the `facing` side and gives it to the other.
fn repeater_output(ctx: Ctx, state: u32) -> Option<Dir> {
    facing(ctx, state).map(Dir::opposite)
}

fn wire_points(ctx: Ctx, state: u32, toward: Dir) -> bool {
    match toward {
        Dir::Down => true,
        Dir::Up => false,
        _ => !matches!(
            ctx.shapes.property(state, toward.name()),
            Some("none") | None
        ),
    }
}

/// Power the block `state` at a position sends to its neighbour `toward`.
fn emitted(ctx: Ctx, state: u32, toward: Dir, wires: bool) -> u8 {
    match part(ctx, state) {
        Part::Block => 15,
        Part::Lever if is(ctx, state, "powered") => 15,
        Part::Torch | Part::WallTorch
            if is(ctx, state, "lit") && Some(toward) != attached(ctx, state) =>
        {
            15
        }
        Part::Repeater
            if is(ctx, state, "powered") && Some(toward) == repeater_output(ctx, state) =>
        {
            15
        }
        Part::Wire if wires && wire_points(ctx, state, toward) => wire_power(ctx, state),
        _ => 0,
    }
}

/// Strong power the block `state` sends into its neighbour `toward`.
fn emitted_strong(ctx: Ctx, state: u32, toward: Dir, wires: bool) -> u8 {
    match part(ctx, state) {
        Part::Lever if is(ctx, state, "powered") && Some(toward) == attached(ctx, state) => 15,
        Part::Torch | Part::WallTorch if is(ctx, state, "lit") && toward == Dir::Up => 15,
        Part::Repeater
            if is(ctx, state, "powered") && Some(toward) == repeater_output(ctx, state) =>
        {
            15
        }
        Part::Wire if wires && wire_points(ctx, state, toward) => wire_power(ctx, state),
        _ => 0,
    }
}

/// Strongest power sent into the conductor at `p`.
fn conductor_power(level: &dyn Level, ctx: Ctx, p: BlockPos, wires: bool) -> u8 {
    Dir::ALL
        .into_iter()
        .map(|d| emitted_strong(ctx, level.block(p.offset(d)), d.opposite(), wires))
        .max()
        .unwrap_or(0)
}

/// Power arriving at `p` from its neighbour in direction `from`.
pub fn power_from(level: &dyn Level, ctx: Ctx, p: BlockPos, from: Dir, wires: bool) -> u8 {
    let n = p.offset(from);
    let state = level.block(n);
    if ctx.shapes.conducts(state) {
        conductor_power(level, ctx, n, wires)
    } else {
        emitted(ctx, state, from.opposite(), wires)
    }
}

/// Strongest power arriving at `p` from any side.
pub fn received(level: &dyn Level, ctx: Ctx, p: BlockPos, wires: bool) -> u8 {
    Dir::ALL
        .into_iter()
        .map(|d| power_from(level, ctx, p, d, wires))
        .max()
        .unwrap_or(0)
}

/// The level dust at `p` should have.
fn wire_target(level: &dyn Level, ctx: Ctx, p: BlockPos) -> u8 {
    let mut best = received(level, ctx, p, false);
    if best >= 15 {
        return 15;
    }
    let cut_above = ctx.shapes.conducts(level.block(p.offset(Dir::Up)));
    for d in Dir::HORIZONTAL {
        let n = p.offset(d);
        let side = level.block(n);
        let from = |s: u32| wire_power(ctx, s).saturating_sub(1);
        best = best.max(from(side));
        if ctx.shapes.conducts(side) {
            if !cut_above {
                best = best.max(from(level.block(n.offset(Dir::Up))));
            }
        } else {
            best = best.max(from(level.block(n.offset(Dir::Down))));
        }
    }
    best
}

/// Whether dust next to `state` (in direction `d` from the dust) points at it.
fn dust_connects_to(ctx: Ctx, state: u32, d: Dir) -> bool {
    match part(ctx, state) {
        Part::Wire | Part::Torch | Part::WallTorch | Part::Lever | Part::Block => true,
        Part::Repeater => facing(ctx, state).is_some_and(|f| f.axis() == d.axis()),
        Part::Lamp | Part::None => {
            let name = ctx.shapes.block_name(state).unwrap_or("");
            name.ends_with("_button")
                || name.ends_with("_pressure_plate")
                || matches!(
                    name,
                    "minecraft:target"
                        | "minecraft:daylight_detector"
                        | "minecraft:tripwire_hook"
                        | "minecraft:trapped_chest"
                        | "minecraft:detector_rail"
                        | "minecraft:comparator"
                        | "minecraft:lightning_rod"
                        | "minecraft:sculk_sensor"
                        | "minecraft:calibrated_sculk_sensor"
                )
        }
    }
}

/// Dust's connections from its neighbours: towards components, dust beside
/// it, and dust one block up or down. With none it is a cross (or stays a dot
/// if it was one); with one, a line through it.
pub fn wire_shape(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u32 {
    let cut_above = ctx.shapes.conducts(level.block(p.offset(Dir::Up)));
    let mut connections = [("none", Dir::North); 4];
    for (i, d) in Dir::HORIZONTAL.into_iter().enumerate() {
        let n = p.offset(d);
        let side = level.block(n);
        let value = if !cut_above
            && part(ctx, level.block(n.offset(Dir::Up))) == Part::Wire
            && ctx.shapes.face_attaches(side, d)
        {
            "up"
        } else if dust_connects_to(ctx, side, d)
            || (!ctx.shapes.conducts(side)
                && part(ctx, level.block(n.offset(Dir::Down))) == Part::Wire)
        {
            "side"
        } else {
            "none"
        };
        connections[i] = (value, d);
    }
    let count = connections.iter().filter(|(v, _)| *v != "none").count();
    let was_dot = Dir::HORIZONTAL
        .iter()
        .all(|d| ctx.shapes.property(state, d.name()) == Some("none"));
    match count {
        0 if was_dot => {}
        0 => connections = connections.map(|(_, d)| ("side", d)),
        1 => {
            let (_, d) = *connections.iter().find(|(v, _)| *v != "none").expect("one");
            let i = Dir::HORIZONTAL
                .iter()
                .position(|&h| h == d.opposite())
                .expect("horizontal");
            connections[i].0 = "side";
        }
        _ => {}
    }
    connections
        .into_iter()
        .fold(state, |s, (v, d)| with(ctx, s, d.name(), v))
}

/// Neighbour-changed updates to the six neighbours of `p`.
pub fn update_neighbors_at(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    for d in Dir::NC_ORDER {
        crate::updates::neighbor_changed(level, ctx, p.offset(d), p);
    }
}

/// Updates up to two blocks away (dust, torches).
fn update_around(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    update_neighbors_at(level, ctx, p);
    for d in Dir::NC_ORDER {
        update_neighbors_at(level, ctx, p.offset(d));
    }
}

fn set_quietly(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    set_block(level, ctx, p, state, Flags::SHAPES_ONLY)
}

fn update_wire(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let target = wire_target(level, ctx, p);
    if target == wire_power(ctx, state) {
        return;
    }
    let new = with(ctx, state, "power", &target.to_string());
    if set_quietly(level, ctx, p, new) {
        update_around(level, ctx, p);
    }
}

fn torch_should_light(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    attached(ctx, state).is_none_or(|a| power_from(level, ctx, p, a, true) == 0)
}

fn repeater_input(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u8 {
    facing(ctx, state).map_or(0, |input| {
        let n = p.offset(input);
        let s = level.block(n);
        power_from(level, ctx, p, input, true).max(wire_power(ctx, s))
    })
}

/// A side neighbour that is a powered repeater pointing into this one.
fn repeater_locked(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let Some(input) = facing(ctx, state) else {
        return false;
    };
    [input.cw(), input.ccw()].into_iter().any(|side| {
        let s = level.block(p.offset(side));
        part(ctx, s) == Part::Repeater
            && is(ctx, s, "powered")
            && repeater_output(ctx, s) == Some(side.opposite())
    })
}

fn repeater_delay(ctx: Ctx, state: u32) -> i64 {
    ctx.shapes
        .property(state, "delay")
        .and_then(|d| d.parse::<i64>().ok())
        .unwrap_or(1)
        * 2
}

/// After a repeater changes: the block in front, then that block's other
/// neighbours.
fn update_in_front(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let Some(out) = repeater_output(ctx, state) else {
        return;
    };
    let front = p.offset(out);
    crate::updates::neighbor_changed(level, ctx, front, p);
    for d in Dir::NC_ORDER {
        if d != out.opposite() {
            crate::updates::neighbor_changed(level, ctx, front.offset(d), front);
        }
    }
}

/// A neighbour of the component at `p` changed.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    match part(ctx, state) {
        Part::Wire => update_wire(level, ctx, p, state),
        Part::Torch | Part::WallTorch => {
            if is(ctx, state, "lit") != torch_should_light(level, ctx, p, state) {
                let kind = ctx.shapes.block_index(state);
                level.schedule_block_tick(p, kind, TORCH_DELAY, 0);
            }
        }
        Part::Lamp => {
            let powered = received(level, ctx, p, true) > 0;
            let lit = is(ctx, state, "lit");
            if lit && !powered {
                let kind = ctx.shapes.block_index(state);
                level.schedule_block_tick(p, kind, LAMP_OFF_DELAY, 0);
            } else if !lit && powered {
                set_quietly(level, ctx, p, with_bool(ctx, state, "lit", true));
            }
        }
        Part::Repeater => {
            let locked = repeater_locked(level, ctx, p, state);
            if locked {
                return;
            }
            let should = repeater_input(level, ctx, p, state) > 0;
            let kind = ctx.shapes.block_index(state);
            if is(ctx, state, "powered") != should && !level.is_block_tick_scheduled(p, kind) {
                let priority = if is(ctx, state, "powered") { -2 } else { -1 };
                level.schedule_block_tick(p, kind, repeater_delay(ctx, state), priority);
            }
        }
        _ => {}
    }
}

/// A due block tick for a component.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind {
        return;
    }
    match part(ctx, state) {
        Part::Torch | Part::WallTorch => {
            let should = torch_should_light(level, ctx, p, state);
            if is(ctx, state, "lit") != should {
                set_quietly(level, ctx, p, with_bool(ctx, state, "lit", should));
                update_around(level, ctx, p);
            }
        }
        Part::Lamp => {
            if is(ctx, state, "lit") && received(level, ctx, p, true) == 0 {
                set_quietly(level, ctx, p, with_bool(ctx, state, "lit", false));
            }
        }
        Part::Repeater => {
            if repeater_locked(level, ctx, p, state) {
                return;
            }
            let should = repeater_input(level, ctx, p, state) > 0;
            let powered = is(ctx, state, "powered");
            if powered && !should {
                let new = with_bool(ctx, state, "powered", false);
                set_quietly(level, ctx, p, new);
                update_in_front(level, ctx, p, new);
            } else if !powered {
                let new = with_bool(ctx, state, "powered", true);
                set_quietly(level, ctx, p, new);
                update_in_front(level, ctx, p, new);
                if !should {
                    level.schedule_block_tick(p, kind, repeater_delay(ctx, new), -2);
                }
            }
        }
        _ => {}
    }
}

/// A component was placed or replaced: tell what it powers.
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, old: u32, state: u32) {
    for (s, placed) in [(old, false), (state, true)] {
        match part(ctx, s) {
            Part::Wire => {
                if placed {
                    update_wire(level, ctx, p, state);
                }
                update_around(level, ctx, p);
            }
            Part::Torch | Part::WallTorch => update_around(level, ctx, p),
            Part::Lever if is(ctx, s, "powered") && !placed => {
                if let Some(a) = attached(ctx, s) {
                    update_neighbors_at(level, ctx, p.offset(a));
                }
            }
            Part::Repeater if is(ctx, s, "powered") || placed => {
                update_in_front(level, ctx, p, s);
                if placed {
                    neighbor_changed(level, ctx, p);
                }
            }
            _ => {}
        }
    }
}

/// The state a player places (not a command): a lamp next to power comes
/// lit, dust takes its level.
pub fn player_placement(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u32 {
    match part(ctx, state) {
        Part::Lamp => with_bool(ctx, state, "lit", received(level, ctx, p, true) > 0),
        _ => state,
    }
}

/// A player used (right-clicked) the block at `p`. Returns whether it reacted.
pub fn use_block(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let state = level.block(p);
    match part(ctx, state) {
        Part::Lever => {
            let new = with_bool(ctx, state, "powered", !is(ctx, state, "powered"));
            set_block(level, ctx, p, new, Flags::ALL);
            if let Some(a) = attached(ctx, new) {
                update_neighbors_at(level, ctx, p.offset(a));
            }
            true
        }
        Part::Repeater => {
            let delay = ctx
                .shapes
                .property(state, "delay")
                .and_then(|d| d.parse::<u8>().ok())
                .unwrap_or(1);
            let next = delay % 4 + 1;
            set_block(
                level,
                ctx,
                p,
                with(ctx, state, "delay", &next.to_string()),
                Flags::SHAPES_ONLY,
            );
            true
        }
        _ => false,
    }
}
