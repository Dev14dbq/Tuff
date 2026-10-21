//! Redstone, from the public descriptions (minecraft.wiki: "Redstone
//! mechanics", "Redstone Dust", "Redstone Torch", "Redstone Repeater",
//! "Lever", "Redstone Lamp", "Button", "Pressure Plate", "Observer",
//! "Redstone Comparator", "Door") and checked on the parity bench.
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
    Button,
    Plate,
    WeightedPlate,
    Observer,
    Comparator,
    /// Doors, trapdoors and fence gates: open while powered.
    Opens,
    NoteBlock,
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
        Some("minecraft:observer") => Part::Observer,
        Some("minecraft:comparator") => Part::Comparator,
        Some("minecraft:note_block") => Part::NoteBlock,
        Some(
            "minecraft:light_weighted_pressure_plate" | "minecraft:heavy_weighted_pressure_plate",
        ) => Part::WeightedPlate,
        Some(name) if name.ends_with("_button") => Part::Button,
        Some(name) if name.ends_with("_pressure_plate") => Part::Plate,
        Some(name)
            if name.ends_with("_door")
                || name.ends_with("_trapdoor")
                || name.ends_with("_fence_gate") =>
        {
            Part::Opens
        }
        _ => Part::None,
    }
}

/// Note block instruments, in the order of the `instrument` property.
pub const INSTRUMENTS: [&str; 27] = [
    "harp",
    "basedrum",
    "snare",
    "hat",
    "bass",
    "flute",
    "bell",
    "guitar",
    "chime",
    "xylophone",
    "iron_xylophone",
    "cow_bell",
    "didgeridoo",
    "bit",
    "banjo",
    "pling",
    "trumpet",
    "trumpet_exposed",
    "trumpet_oxidized",
    "trumpet_weathered",
    "zombie",
    "skeleton",
    "creeper",
    "dragon",
    "wither_skeleton",
    "piglin",
    "custom_head",
];

/// The instrument of a note block at `p`: a mob head on top sets it, else
/// the block below (minecraft.wiki, "Note Block").
pub fn note_instrument(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u32 {
    let above = ctx.shapes.block_name(level.block(p.offset(Dir::Up)));
    let head = match above {
        Some("minecraft:zombie_head") => Some("zombie"),
        Some("minecraft:skeleton_skull") => Some("skeleton"),
        Some("minecraft:creeper_head") => Some("creeper"),
        Some("minecraft:dragon_head") => Some("dragon"),
        Some("minecraft:wither_skeleton_skull") => Some("wither_skeleton"),
        Some("minecraft:piglin_head") => Some("piglin"),
        Some("minecraft:player_head") => Some("custom_head"),
        _ => None,
    };
    let name = head.unwrap_or_else(|| {
        let below = level.block(p.offset(Dir::Down));
        INSTRUMENTS[usize::from(ctx.shapes.instrument_below(below)).min(INSTRUMENTS.len() - 1)]
    });
    with(ctx, state, "instrument", name)
}

const TORCH_DELAY: i64 = 2;
const LAMP_OFF_DELAY: i64 = 4;
const OBSERVER_DELAY: i64 = 2;
const COMPARATOR_DELAY: i64 = 2;
/// Stone buttons stay pressed for a second, wooden ones for 1.5 s.
const STONE_BUTTON_TICKS: i64 = 20;
const WOODEN_BUTTON_TICKS: i64 = 30;
/// Pressure plates look again for entities this often while pressed.
const PLATE_TICKS: i64 = 20;
const TORCH_BURNOUT_DELAY: i64 = 160;
const WEIGHTED_PLATE_TICKS: i64 = 10;

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

fn level_of(ctx: Ctx, state: u32, property: &str) -> u8 {
    ctx.shapes
        .property(state, property)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
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
        Part::Plate | Part::WeightedPlate => Some(Dir::Down),
        Part::Lever | Part::Button => match ctx.shapes.property(state, "face") {
            Some("floor") => Some(Dir::Down),
            Some("ceiling") => Some(Dir::Up),
            _ => facing(ctx, state).map(Dir::opposite),
        },
        _ => None,
    }
}

/// Repeaters and comparators take power from the `facing` side and give it
/// to the other; an observer watches its `facing` side and gives power out
/// of its back.
fn repeater_output(ctx: Ctx, state: u32) -> Option<Dir> {
    facing(ctx, state).map(Dir::opposite)
}

/// Power of a pressed plate.
fn plate_power(ctx: Ctx, state: u32) -> u8 {
    match part(ctx, state) {
        Part::Plate if is(ctx, state, "powered") => 15,
        Part::WeightedPlate => level_of(ctx, state, "power"),
        _ => 0,
    }
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

/// Power the block `state` at `at` sends to its neighbour `toward`.
fn emitted(level: &dyn Level, ctx: Ctx, at: BlockPos, state: u32, toward: Dir, wires: bool) -> u8 {
    match part(ctx, state) {
        Part::Block => 15,
        Part::Lever | Part::Button if is(ctx, state, "powered") => 15,
        Part::Plate | Part::WeightedPlate => plate_power(ctx, state),
        Part::Torch | Part::WallTorch
            if is(ctx, state, "lit") && Some(toward) != attached(ctx, state) =>
        {
            15
        }
        Part::Repeater | Part::Observer
            if is(ctx, state, "powered") && Some(toward) == repeater_output(ctx, state) =>
        {
            15
        }
        Part::Comparator if Some(toward) == repeater_output(ctx, state) => {
            level.comparator_output(at)
        }
        Part::Wire if wires && wire_points(ctx, state, toward) => wire_power(ctx, state),
        _ => crate::rails::emits(ctx, state, toward, false)
            .max(crate::tripwire::emits(ctx, state, toward, false))
            .max(other_power(ctx, state))
            .max(trapped_chest(level, ctx, at, state)),
    }
}

/// A trapped chest gives as much power as players have it open (at most
/// 15), weakly around and strongly into the block below.
fn trapped_chest(level: &dyn Level, ctx: Ctx, at: BlockPos, state: u32) -> u8 {
    if ctx.shapes.block_name(state) == Some("minecraft:trapped_chest") {
        level.viewers(at).min(15)
    } else {
        0
    }
}

/// Weak power of the other sources that hold it in a `power` property:
/// daylight detectors and targets.
fn other_power(ctx: Ctx, state: u32) -> u8 {
    match ctx.shapes.block_name(state) {
        Some("minecraft:daylight_detector" | "minecraft:target") => level_of(ctx, state, "power"),
        _ => 0,
    }
}

/// Strong power the block `state` at `at` sends into its neighbour `toward`.
fn emitted_strong(
    level: &dyn Level,
    ctx: Ctx,
    at: BlockPos,
    state: u32,
    toward: Dir,
    wires: bool,
) -> u8 {
    match part(ctx, state) {
        Part::Lever | Part::Button
            if is(ctx, state, "powered") && Some(toward) == attached(ctx, state) =>
        {
            15
        }
        Part::Plate | Part::WeightedPlate if toward == Dir::Down => plate_power(ctx, state),
        Part::Torch | Part::WallTorch if is(ctx, state, "lit") && toward == Dir::Up => 15,
        Part::Repeater | Part::Observer
            if is(ctx, state, "powered") && Some(toward) == repeater_output(ctx, state) =>
        {
            15
        }
        Part::Comparator if Some(toward) == repeater_output(ctx, state) => {
            level.comparator_output(at)
        }
        Part::Wire if wires && wire_points(ctx, state, toward) => wire_power(ctx, state),
        _ if toward == Dir::Down
            && ctx.shapes.block_name(state) == Some("minecraft:trapped_chest") =>
        {
            trapped_chest(level, ctx, at, state)
        }
        _ => crate::rails::emits(ctx, state, toward, true)
            .max(crate::tripwire::emits(ctx, state, toward, true)),
    }
}

/// Strongest power sent into the conductor at `p`.
fn conductor_power(level: &dyn Level, ctx: Ctx, p: BlockPos, wires: bool) -> u8 {
    Dir::ALL
        .into_iter()
        .map(|d| {
            let n = p.offset(d);
            emitted_strong(level, ctx, n, level.block(n), d.opposite(), wires)
        })
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
        emitted(level, ctx, n, state, from.opposite(), wires)
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
        Part::Wire
        | Part::Torch
        | Part::WallTorch
        | Part::Lever
        | Part::Block
        | Part::Button
        | Part::Plate
        | Part::WeightedPlate
        | Part::Comparator => true,
        Part::Repeater => facing(ctx, state).is_some_and(|f| f.axis() == d.axis()),
        Part::Observer => facing(ctx, state) == Some(d),
        Part::Lamp | Part::Opens | Part::NoteBlock | Part::None => {
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

/// Signal a comparator reads from a block by its state (minecraft.wiki,
/// "Redstone Comparator": cake, cauldrons, composter, portal frame...), or
/// from a container.
fn analog_signal(level: &dyn Level, ctx: Ctx, p: BlockPos) -> Option<u8> {
    let state = level.block(p);
    let name = ctx.shapes.block_name(state)?;
    let signal = match name {
        "minecraft:cake" => 14 - 2 * level_of(ctx, state, "bites"),
        "minecraft:water_cauldron" | "minecraft:powder_snow_cauldron" => {
            level_of(ctx, state, "level")
        }
        "minecraft:lava_cauldron" => 3,
        "minecraft:cauldron" => 0,
        "minecraft:composter" => level_of(ctx, state, "level"),
        "minecraft:end_portal_frame" => {
            if is(ctx, state, "eye") {
                15
            } else {
                0
            }
        }
        "minecraft:respawn_anchor" => level_of(ctx, state, "charges") * 15 / 4,
        "minecraft:beehive" | "minecraft:bee_nest" => level_of(ctx, state, "honey_level"),
        n if n.ends_with("candle_cake") => 14,
        n if n.ends_with("copper_bulb") => {
            if is(ctx, state, "lit") {
                15
            } else {
                0
            }
        }
        _ => {
            if let Some(signal) = level.block_entity_signal(p) {
                return Some(signal);
            }
            return crate::rails::comparator_reading(level, ctx, p)
                .or_else(|| level.container_signal(p));
        }
    };
    Some(signal)
}

/// What a comparator gets from behind: power, or what the block behind
/// (or the block behind a conductor behind it) holds.
fn comparator_rear(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u8 {
    let Some(back) = facing(ctx, state) else {
        return 0;
    };
    let n = p.offset(back);
    let power = repeater_input(level, ctx, p, state);
    if let Some(signal) = analog_signal(level, ctx, n) {
        return signal;
    }
    if power < 15
        && ctx.shapes.conducts(level.block(n))
        && let Some(signal) = analog_signal(level, ctx, n.offset(back))
    {
        return signal.max(power);
    }
    power
}

/// Side input of a comparator: only dust, redstone blocks, repeaters and
/// comparators count.
fn comparator_side(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u8 {
    let Some(back) = facing(ctx, state) else {
        return 0;
    };
    [back.cw(), back.ccw()]
        .into_iter()
        .map(|side| {
            let n = p.offset(side);
            let s = level.block(n);
            match part(ctx, s) {
                Part::Wire => wire_power(ctx, s),
                Part::Block => 15,
                Part::Repeater | Part::Comparator => {
                    emitted(level, ctx, n, s, side.opposite(), true)
                }
                _ => 0,
            }
        })
        .max()
        .unwrap_or(0)
}

fn comparator_signal(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u8 {
    let rear = comparator_rear(level, ctx, p, state);
    let side = comparator_side(level, ctx, p, state);
    if ctx.shapes.property(state, "mode") == Some("subtract") {
        rear.saturating_sub(side)
    } else if side > rear {
        0
    } else {
        rear
    }
}

/// Works out a comparator's signal now; updates what is in front if it
/// changed.
fn refresh_comparator(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let signal = comparator_signal(level, ctx, p, state);
    let old = level.comparator_output(p);
    let powered = is(ctx, state, "powered");
    if signal == old && (signal > 0) == powered {
        return;
    }
    level.set_comparator_output(p, signal);
    let mut new = state;
    if (signal > 0) != powered {
        new = with_bool(ctx, state, "powered", signal > 0);
        set_quietly(level, ctx, p, new);
    }
    update_in_front(level, ctx, p, new);
}

/// How hard entities press the plate at `p`.
fn plate_target(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u8 {
    let (x, y, z) = (f64::from(p.x), f64::from(p.y), f64::from(p.z));
    let (all, living) = level.entities_in(
        [x + 0.0625, y, z + 0.0625],
        [x + 0.9375, y + 0.25, z + 0.9375],
    );
    let name = ctx.shapes.block_name(state).unwrap_or("");
    match name {
        "minecraft:light_weighted_pressure_plate" => all.min(15) as u8,
        "minecraft:heavy_weighted_pressure_plate" => all.div_ceil(10).min(15) as u8,
        "minecraft:stone_pressure_plate" | "minecraft:polished_blackstone_pressure_plate" => {
            if living > 0 {
                15
            } else {
                0
            }
        }
        _ => {
            if all > 0 {
                15
            } else {
                0
            }
        }
    }
}

/// Sets a plate's power and tells the blocks around and below.
fn press_plate(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let target = plate_target(level, ctx, p, state);
    let weighted = part(ctx, state) == Part::WeightedPlate;
    if target != plate_power(ctx, state) {
        let new = if weighted {
            with(ctx, state, "power", &target.to_string())
        } else {
            with_bool(ctx, state, "powered", target > 0)
        };
        set_quietly(level, ctx, p, new);
        update_neighbors_at(level, ctx, p);
        update_neighbors_at(level, ctx, p.offset(Dir::Down));
    }
    if target > 0 {
        let delay = if weighted {
            WEIGHTED_PLATE_TICKS
        } else {
            PLATE_TICKS
        };
        let kind = ctx.shapes.block_index(state);
        level.schedule_block_tick(p, kind, delay, 0);
    }
}

/// An entity is in the block at `p`: an unpressed plate there checks
/// whether it is pressed now.
pub fn entity_inside(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    crate::rails::entity_inside(level, ctx, p);
    let state = level.block(p);
    if matches!(part(ctx, state), Part::Plate | Part::WeightedPlate) && plate_power(ctx, state) == 0
    {
        press_plate(level, ctx, p, state);
    }
}

/// The block in front of the observer at `p` changed.
pub fn observed_change(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if part(ctx, state) != Part::Observer || is(ctx, state, "powered") {
        return;
    }
    let kind = ctx.shapes.block_index(state);
    if !level.is_block_tick_scheduled(p, kind) {
        level.schedule_block_tick(p, kind, OBSERVER_DELAY, 0);
    }
}

/// What a container at `p` holds changed: comparators reading it look again.
pub fn container_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    for d in Dir::HORIZONTAL {
        let n = p.offset(d);
        let s = level.block(n);
        if part(ctx, s) == Part::Comparator {
            neighbor_changed(level, ctx, n);
        } else if ctx.shapes.conducts(s) {
            let beyond = n.offset(d);
            if part(ctx, level.block(beyond)) == Part::Comparator {
                neighbor_changed(level, ctx, beyond);
            }
        }
    }
}

fn button_ticks(ctx: Ctx, state: u32) -> i64 {
    match ctx.shapes.block_name(state) {
        Some("minecraft:stone_button" | "minecraft:polished_blackstone_button") => {
            STONE_BUTTON_TICKS
        }
        _ => WOODEN_BUTTON_TICKS,
    }
}

/// Power a door, trapdoor or gate gets: a door from either half.
fn opens_power(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    if received(level, ctx, p, true) > 0 {
        return true;
    }
    let name = ctx.shapes.block_name(state).unwrap_or("");
    if !name.ends_with("_door") {
        return false;
    }
    let other = match ctx.shapes.property(state, "half") {
        Some("lower") => p.offset(Dir::Up),
        _ => p.offset(Dir::Down),
    };
    received(level, ctx, other, true) > 0
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
        Part::Comparator => {
            let kind = ctx.shapes.block_index(state);
            if level.is_block_tick_scheduled(p, kind) {
                return;
            }
            let signal = comparator_signal(level, ctx, p, state);
            if signal != level.comparator_output(p) || (signal > 0) != is(ctx, state, "powered") {
                // Sooner when it feeds a repeater or comparator not facing it.
                let front = level.block(p.offset(repeater_output(ctx, state).unwrap_or(Dir::Up)));
                let feeds_diode = matches!(part(ctx, front), Part::Repeater | Part::Comparator)
                    && facing(ctx, front) != facing(ctx, state).map(Dir::opposite);
                let priority = if feeds_diode { -1 } else { 0 };
                level.schedule_block_tick(p, kind, COMPARATOR_DELAY, priority);
            }
        }
        Part::Opens => {
            let powered = opens_power(level, ctx, p, state);
            if powered != is(ctx, state, "powered") {
                let new = with_bool(ctx, state, "powered", powered);
                let new = with_bool(ctx, new, "open", powered);
                set_quietly(level, ctx, p, new);
            }
        }
        Part::NoteBlock => {
            let powered = received(level, ctx, p, true) > 0;
            if powered != is(ctx, state, "powered") {
                set_quietly(level, ctx, p, with_bool(ctx, state, "powered", powered));
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
        _ if is_bulb(ctx, state) => flip_bulb(level, ctx, p, state),
        _ => {}
    }
}

fn is_bulb(ctx: Ctx, state: u32) -> bool {
    ctx.shapes
        .block_name(state)
        .is_some_and(|n| n.ends_with("copper_bulb"))
}

/// A copper bulb toggles its light each time power arrives (minecraft.wiki,
/// "Copper Bulb"); it remembers being powered so it waits for the power to
/// go and come again.
fn flip_bulb(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let powered = received(level, ctx, p, true) > 0;
    if powered == is(ctx, state, "powered") {
        return;
    }
    let mut new = with_bool(ctx, state, "powered", powered);
    if powered {
        let lit = !is(ctx, state, "lit");
        new = with_bool(ctx, new, "lit", lit);
        let sound = if lit {
            "block.copper_bulb.turn_on"
        } else {
            "block.copper_bulb.turn_off"
        };
        level.sound(sound, p);
    }
    set_block(level, ctx, p, new, Flags::ALL);
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
            let lit = is(ctx, state, "lit");
            if lit && !should {
                set_quietly(level, ctx, p, with_bool(ctx, state, "lit", false));
                update_around(level, ctx, p);
                // Toggling 8 times in 60 ticks burns it out: it stays dark
                // and looks again after 160 ticks.
                if level.torch_burnout(p, true) {
                    level.torch_fizz(p);
                    level.schedule_block_tick(p, kind, TORCH_BURNOUT_DELAY, 0);
                }
            } else if !lit && should && !level.torch_burnout(p, false) {
                set_quietly(level, ctx, p, with_bool(ctx, state, "lit", true));
                update_around(level, ctx, p);
            }
        }
        Part::Lamp => {
            if is(ctx, state, "lit") && received(level, ctx, p, true) == 0 {
                set_quietly(level, ctx, p, with_bool(ctx, state, "lit", false));
            }
        }
        Part::Comparator => refresh_comparator(level, ctx, p, state),
        Part::Observer => {
            let powered = !is(ctx, state, "powered");
            let new = with_bool(ctx, state, "powered", powered);
            set_quietly(level, ctx, p, new);
            if powered {
                level.schedule_block_tick(p, kind, OBSERVER_DELAY, 0);
            }
            update_in_front(level, ctx, p, new);
        }
        Part::Button if is(ctx, state, "powered") => {
            let new = with_bool(ctx, state, "powered", false);
            set_block(level, ctx, p, new, Flags::ALL);
            if let Some(a) = attached(ctx, new) {
                update_neighbors_at(level, ctx, p.offset(a));
            }
        }
        Part::Plate | Part::WeightedPlate => press_plate(level, ctx, p, state),
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
    if is_bulb(ctx, state) && ctx.shapes.block_index(old) != ctx.shapes.block_index(state) {
        flip_bulb(level, ctx, p, state);
    }
    for (s, placed) in [(old, false), (state, true)] {
        match part(ctx, s) {
            Part::Wire => {
                if placed {
                    update_wire(level, ctx, p, state);
                }
                update_around(level, ctx, p);
            }
            Part::Torch | Part::WallTorch => update_around(level, ctx, p),
            Part::Lever | Part::Button if is(ctx, s, "powered") && !placed => {
                if let Some(a) = attached(ctx, s) {
                    update_neighbors_at(level, ctx, p.offset(a));
                }
            }
            Part::Plate | Part::WeightedPlate if plate_power(ctx, s) > 0 && !placed => {
                update_neighbors_at(level, ctx, p.offset(Dir::Down));
            }
            Part::Observer if is(ctx, s, "powered") && !placed => {
                update_in_front(level, ctx, p, s);
            }
            // Only when a comparator comes or goes, not when it changes state.
            Part::Comparator if part(ctx, old) != part(ctx, state) => {
                if placed {
                    neighbor_changed(level, ctx, p);
                } else {
                    level.set_comparator_output(p, 0);
                }
                update_in_front(level, ctx, p, s);
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
        Part::Button => {
            if !is(ctx, state, "powered") {
                let new = with_bool(ctx, state, "powered", true);
                set_block(level, ctx, p, new, Flags::ALL);
                if let Some(a) = attached(ctx, new) {
                    update_neighbors_at(level, ctx, p.offset(a));
                }
                let kind = ctx.shapes.block_index(new);
                level.schedule_block_tick(p, kind, button_ticks(ctx, new), 0);
            }
            true
        }
        Part::NoteBlock => {
            let note = (level_of(ctx, state, "note") + 1) % 25;
            set_block(
                level,
                ctx,
                p,
                with(ctx, state, "note", &note.to_string()),
                Flags::ALL,
            );
            true
        }
        Part::Comparator => {
            let mode = if ctx.shapes.property(state, "mode") == Some("subtract") {
                "compare"
            } else {
                "subtract"
            };
            let new = with(ctx, state, "mode", mode);
            set_block(level, ctx, p, new, Flags::SHAPES_ONLY);
            refresh_comparator(level, ctx, p, new);
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
