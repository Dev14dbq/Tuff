//! Water and lava (minecraft.wiki, "Fluid"): levels, spreading by scheduled
//! ticks, falling, new sources, and lava meeting water.
//!
//! A fluid block's `level` is 0 for a source, 1–7 for flowing fluid (the
//! further from the source, the higher), 8 and up when it falls from above.
//! In "amount" terms used here a source and falling fluid hold 8, level n
//! holds 8 − n.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fluid {
    Water = 0,
    Lava = 1,
}

/// How each fluid behaves.
struct Props {
    /// Ticks between spreading steps.
    delay: i64,
    /// Amount lost per block of horizontal flow.
    drop: i32,
    /// How far to look for a way down.
    slope: i32,
}

const WATER: Props = Props {
    delay: 5,
    drop: 1,
    slope: 4,
};
const LAVA: Props = Props {
    delay: 30,
    drop: 2,
    slope: 2,
};

/// Lava where it is fast (the Nether).
const FAST_LAVA: Props = Props {
    delay: 10,
    drop: 1,
    slope: 4,
};

fn props(level: &dyn Level, fluid: Fluid) -> &'static Props {
    match fluid {
        Fluid::Water => &WATER,
        Fluid::Lava if level.fast_lava() => &FAST_LAVA,
        Fluid::Lava => &LAVA,
    }
}

/// The fluid a block is, with its level.
pub fn fluid_of(ctx: Ctx, state: u32) -> Option<(Fluid, u32)> {
    let fluid = match ctx.shapes.block_name(state)? {
        "minecraft:water" => Fluid::Water,
        "minecraft:lava" => Fluid::Lava,
        _ => return None,
    };
    let level = ctx.shapes.property(state, "level")?.parse().ok()?;
    Some((fluid, level))
}

fn amount(level: u32) -> i32 {
    match level {
        0 => 8,
        1..=7 => 8 - level as i32,
        _ => 8,
    }
}

fn state_of(ctx: Ctx, fluid: Fluid, level: u32) -> u32 {
    let name = match fluid {
        Fluid::Water => "minecraft:water",
        Fluid::Lava => "minecraft:lava",
    };
    ctx.shapes
        .state(name, &[("level", &level.to_string())])
        .unwrap_or(ctx.facts.air)
}

/// Flowing fluid holding `amount` (1–7), or falling fluid for 8.
fn flowing(ctx: Ctx, fluid: Fluid, amount: i32) -> u32 {
    let level = if amount >= 8 { 8 } else { (8 - amount) as u32 };
    state_of(ctx, fluid, level)
}

/// Whether `fluid` may flow into a block that is `target`.
fn can_replace(ctx: Ctx, fluid: Fluid, target: u32) -> bool {
    match fluid_of(ctx, target) {
        Some((other, level)) => other == fluid && level != 0,
        None => {
            ctx.shapes.is_air(target) || (washable(ctx, target) && !ctx.facts.blocks_motion(target))
        }
    }
}

/// Whether flowing fluid may wash a block away: blocks that hold water
/// themselves (waterlogged, seagrass, kelp), doors, signs, ladders, sugar
/// cane, bubble columns and portals stay (minecraft.wiki, "Water": "Water
/// cannot destroy...").
fn washable(ctx: Ctx, target: u32) -> bool {
    if ctx.shapes.property(target, "waterlogged") == Some("true") {
        return false;
    }
    let name = ctx.shapes.block_name(target).unwrap_or("");
    !(name.ends_with("_door")
        || name.ends_with("_sign")
        || matches!(
            name,
            "minecraft:seagrass"
                | "minecraft:tall_seagrass"
                | "minecraft:kelp"
                | "minecraft:kelp_plant"
                | "minecraft:bubble_column"
                | "minecraft:ladder"
                | "minecraft:sugar_cane"
                | "minecraft:nether_portal"
                | "minecraft:end_portal"
                | "minecraft:end_gateway"
                | "minecraft:structure_void"
        ))
}

/// Water (and a waterlogged block) cannot stay where the dimension
/// evaporates it. The block becomes air, or dry, and the world hisses.
/// `true` when the block that was just set is already gone.
fn boil_away(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    if !level.ultrawarm() {
        return false;
    }
    let water = matches!(fluid_of(ctx, state), Some((Fluid::Water, _)));
    let logged = ctx.shapes.property(state, "waterlogged") == Some("true");
    if !water && !logged {
        return false;
    }
    let next = if logged {
        let dry = ctx.shapes.with(state, "waterlogged", "false");
        if dry == state {
            return false;
        }
        dry
    } else {
        ctx.facts.air
    };
    set_block(level, ctx, p, next, Flags::ALL);
    level.sponge_dried(p);
    true
}

/// Called whenever a fluid block is set: lava may harden, water may vanish,
/// otherwise the fluid ticks after its delay. `true` when the block could
/// not stay and has already been replaced.
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    if boil_away(level, ctx, p, state) {
        return true;
    }
    let Some((fluid, _)) = fluid_of(ctx, state) else {
        return false;
    };
    if fluid == Fluid::Lava && harden_lava(level, ctx, p, state) {
        return false;
    }
    level.schedule_fluid_tick(p, fluid as u32, props(level, fluid).delay);
    false
}

/// A neighbour changed: same as being placed.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    on_place(level, ctx, p, state);
}

/// A shape update reached a fluid block: only one from the same fluid wakes it.
pub fn shape_updated(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, neighbor: u32) {
    if let (Some((fluid, _)), Some((other, _))) = (fluid_of(ctx, state), fluid_of(ctx, neighbor))
        && fluid == other
    {
        level.schedule_fluid_tick(p, fluid as u32, props(level, fluid).delay);
    }
}

/// Lava touching water anywhere but below turns into obsidian (a source) or
/// cobblestone (flowing); over soul soil, touching blue ice, into basalt.
/// It fizzes. Returns whether it did.
fn harden_lava(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let soul_soil =
        ctx.shapes.block_name(level.block(p.offset(Dir::Down))) == Some("minecraft:soul_soil");
    let mut into = None;
    for d in [Dir::Up, Dir::North, Dir::South, Dir::West, Dir::East] {
        let next = level.block(p.offset(d));
        if matches!(fluid_of(ctx, next), Some((Fluid::Water, _))) {
            let source = matches!(fluid_of(ctx, state), Some((_, 0)));
            into = Some(if source {
                "minecraft:obsidian"
            } else {
                "minecraft:cobblestone"
            });
            break;
        }
        if soul_soil && ctx.shapes.block_name(next) == Some("minecraft:blue_ice") {
            into = Some("minecraft:basalt");
            break;
        }
    }
    let Some(into) = into else {
        return false;
    };
    let new = ctx.shapes.state(into, &[]).unwrap_or(ctx.facts.air);
    set_block(level, ctx, p, new, Flags::ALL);
    // World event 1501: lava hisses.
    level.level_event(1501, p, 0);
    true
}

/// A due fluid tick at `p`.
pub fn tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, fluid_id: u32) {
    let mut state = level.block(p);
    if boil_away(level, ctx, p, state) {
        return;
    }
    let Some((fluid, lvl)) = fluid_of(ctx, state) else {
        return;
    };
    if fluid as u32 != fluid_id {
        return;
    }
    let mut became_fall = false;
    if lvl != 0 {
        let new = new_state(level, ctx, p, fluid);
        if new != state {
            let new_lvl = fluid_of(ctx, new).map(|(_, l)| l).unwrap_or(0);
            became_fall = new_lvl >= 8 && lvl < 8;
            set_block(level, ctx, p, new, Flags::ALL);
            if ctx.shapes.is_air(new) {
                return;
            }
            state = new;
        }
    }
    spread(level, ctx, p, state, fluid, became_fall);
}

/// What a flowing block should be, from its neighbours.
fn new_state(level: &dyn Level, ctx: Ctx, p: BlockPos, fluid: Fluid) -> u32 {
    let props = props(level, fluid);
    let (mut most, mut sources) = (0, 0);
    for d in Dir::HORIZONTAL {
        if let Some((other, l)) = fluid_of(ctx, level.block(p.offset(d)))
            && other == fluid
        {
            if l == 0 {
                sources += 1;
            }
            most = most.max(amount(l));
        }
    }
    if level.source_conversion(fluid) && sources >= 2 {
        let below = level.block(p.offset(Dir::Down));
        let below_source = matches!(fluid_of(ctx, below), Some((f, 0)) if f == fluid);
        if below_source || (fluid_of(ctx, below).is_none() && ctx.facts.blocks_motion(below)) {
            return state_of(ctx, fluid, 0);
        }
    }
    if matches!(fluid_of(ctx, level.block(p.offset(Dir::Up))), Some((f, _)) if f == fluid) {
        return flowing(ctx, fluid, 8);
    }
    let left = most - props.drop;
    if left <= 0 {
        ctx.facts.air
    } else {
        flowing(ctx, fluid, left)
    }
}

/// Whether fluid at `p` could fall into the block below it. A falling stream
/// (level 8 or more) is a drop, and so is air. A pool of the same fluid
/// sitting on the ground is not: aiming at it would send a full-strength
/// flow into water that is already there.
///
/// `near` is the cell directly beside the block that is spreading. Water
/// that is almost full (level 1) counts as a drop there, so a source still
/// aims at a side whose block below was filled by an earlier sideways flow.
/// Further along the search it does not: a level-1 pool four blocks away is
/// not a cliff, and treating it as one pulls the whole flow past the cells
/// beside a waterfall.
fn hole_below(level: &dyn Level, ctx: Ctx, fluid: Fluid, p: BlockPos, near: bool) -> bool {
    let below_pos = p.offset(Dir::Down);
    let below = level.block(below_pos);
    if let Some((other, lvl)) = fluid_of(ctx, below) {
        if other == fluid {
            return lvl >= 8 || (near && lvl == 1);
        }
        return fluid == Fluid::Lava && other == Fluid::Water;
    }
    can_replace(ctx, fluid, below)
}

fn spread(
    level: &mut dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    fluid: Fluid,
    became_fall: bool,
) {
    let Some((_, lvl)) = fluid_of(ctx, state) else {
        return;
    };
    let props = props(level, fluid);
    let below = p.offset(Dir::Down);
    let below_state = level.block(below);
    let below_same = matches!(fluid_of(ctx, below_state), Some((f, _)) if f == fluid);
    // Lava flowing down into water turns it to stone.
    if fluid == Fluid::Lava && matches!(fluid_of(ctx, below_state), Some((Fluid::Water, _))) {
        let stone = ctx
            .shapes
            .state("minecraft:stone", &[])
            .unwrap_or(ctx.facts.air);
        set_block(level, ctx, below, stone, Flags::ALL);
        return;
    }
    // Down first, into anything the fluid can replace that is not already
    // this fluid; sideways only on a later tick.
    if can_replace(ctx, fluid, below_state) && !below_same {
        set_block(level, ctx, below, flowing(ctx, fluid, 8), Flags::ALL);
        return;
    }
    // Flowing fluid spreads sideways only where it cannot go down: on solid
    // ground, not above its own stream. A source spreads either way.
    if lvl != 0 && (below_same || can_replace(ctx, fluid, below_state)) {
        return;
    }
    let left = if lvl >= 8 {
        8 - props.drop
    } else {
        amount(lvl) - props.drop
    };
    if left <= 0 {
        return;
    }
    let new = flowing(ctx, fluid, left);
    // Pond water that just started falling (a stream reached it from above)
    // runs out every open side. Slope search would send the whole amount
    // toward the nearest cliff and leave the cells beside the column behind.
    // A fall that is already boxed in by this fluid or solid blocks steps
    // into every weaker side. One that still has air beside it is still
    // finding a cliff; sending it everywhere reaches the pond a tick early.
    let boxed = Dir::HORIZONTAL.iter().all(|d| {
        let block = level.block(p.offset(*d));
        fluid_of(ctx, block).is_some() || !can_replace(ctx, fluid, block)
    });
    let dirs: Vec<Dir> = if became_fall && fluid == Fluid::Water && boxed {
        Dir::HORIZONTAL
            .into_iter()
            .filter(|d| can_replace(ctx, fluid, level.block(p.offset(*d))))
            .collect()
    } else {
        spread_directions(level, ctx, p, fluid)
    };
    for d in dirs {
        let np = p.offset(d);
        if takes_flow(ctx, fluid, level.block(np), left) {
            set_block(level, ctx, np, new, Flags::ALL);
        }
    }
    if became_fall && fluid == Fluid::Water && boxed {
        // The cell straight out from the solid block behind this fall (the
        // obsidian under the column) spreads to either side. Its own tick
        // already ran, while the cells beside it are still the old pond.
        let solids: Vec<Dir> = Dir::HORIZONTAL
            .into_iter()
            .filter(|d| {
                let block = level.block(p.offset(*d));
                fluid_of(ctx, block).is_none() && !can_replace(ctx, fluid, block)
            })
            .collect();
        if solids.len() == 1 {
            let away = solids[0].opposite();
            let front = p.offset(away);
            if matches!(fluid_of(ctx, level.block(front)), Some((f, 1)) if f == fluid)
                && across_matches(level, ctx, fluid, front, away)
            {
                spread_across(level, ctx, front, fluid, away);
            }
        }
    }
}

/// The two sides of `p` are the same flowing level. An uneven edge (the
/// pond thinning toward air on one side) is not pulled in with the pair.
fn across_matches(level: &dyn Level, ctx: Ctx, fluid: Fluid, p: BlockPos, axis: Dir) -> bool {
    let mut seen = None;
    for d in Dir::HORIZONTAL {
        if d == axis || d == axis.opposite() {
            continue;
        }
        let Some((other, lvl)) = fluid_of(ctx, level.block(p.offset(d))) else {
            return false;
        };
        if other != fluid || lvl == 0 || lvl >= 8 {
            return false;
        }
        if let Some(prev) = seen
            && prev != lvl
        {
            return false;
        }
        seen = Some(lvl);
    }
    seen.is_some()
}

/// Level-1 water at `p` steps left and right of `axis`, not further along it.
fn spread_across(level: &mut dyn Level, ctx: Ctx, p: BlockPos, fluid: Fluid, axis: Dir) {
    let left = amount(1) - props(level, fluid).drop;
    if left <= 0 {
        return;
    }
    let new = flowing(ctx, fluid, left);
    for d in Dir::HORIZONTAL {
        if d == axis || d == axis.opposite() {
            continue;
        }
        let np = p.offset(d);
        if takes_flow(ctx, fluid, level.block(np), left) {
            set_block(level, ctx, np, new, Flags::ALL);
        }
    }
}

/// Whether flowing in with `left` would change the block `target`: it is
/// replaceable, or the same fluid holding less.
fn takes_flow(ctx: Ctx, fluid: Fluid, target: u32, left: i32) -> bool {
    match fluid_of(ctx, target) {
        Some((f, l)) if f == fluid => l != 0 && amount(l) < left,
        Some(_) => false,
        None => can_replace(ctx, fluid, target),
    }
}

/// Horizontal directions to spread to: towards the nearest way down within
/// the fluid's slope distance, or every open side if there is none.
fn spread_directions(level: &dyn Level, ctx: Ctx, p: BlockPos, fluid: Fluid) -> Vec<Dir> {
    let mut best = i32::MAX;
    let mut dirs = Vec::new();
    for d in Dir::HORIZONTAL {
        let np = p.offset(d);
        let target = level.block(np);
        if !can_replace(ctx, fluid, target) {
            continue;
        }
        let distance = if hole_below(level, ctx, fluid, np, true) {
            0
        } else {
            slope_distance(level, ctx, fluid, np, 1, d.opposite())
        };
        if distance < best {
            best = distance;
            dirs.clear();
        }
        if distance == best {
            dirs.push(d);
        }
    }
    dirs
}

fn slope_distance(
    level: &dyn Level,
    ctx: Ctx,
    fluid: Fluid,
    p: BlockPos,
    depth: i32,
    from: Dir,
) -> i32 {
    let mut best = 1000;
    for d in Dir::HORIZONTAL {
        if d == from {
            continue;
        }
        let np = p.offset(d);
        let target = level.block(np);
        if !can_replace(ctx, fluid, target)
            || matches!(fluid_of(ctx, target), Some((f, 0)) if f == fluid)
        {
            continue;
        }
        if hole_below(level, ctx, fluid, np, false) {
            return depth;
        }
        if depth < props(level, fluid).slope {
            best = best.min(slope_distance(
                level,
                ctx,
                fluid,
                np,
                depth + 1,
                d.opposite(),
            ));
        }
    }
    best
}
