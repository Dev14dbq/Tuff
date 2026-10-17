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

/// Overworld behaviour of each fluid.
struct Props {
    /// Ticks between spreading steps.
    delay: i64,
    /// Amount lost per block of horizontal flow.
    drop: i32,
    /// How far to look for a way down.
    slope: i32,
    /// Two sources make a new one between them.
    makes_sources: bool,
}

const WATER: Props = Props {
    delay: 5,
    drop: 1,
    slope: 4,
    makes_sources: true,
};
const LAVA: Props = Props {
    delay: 30,
    drop: 2,
    slope: 2,
    makes_sources: false,
};

fn props(fluid: Fluid) -> &'static Props {
    match fluid {
        Fluid::Water => &WATER,
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
        None => ctx.shapes.is_air(target) || !ctx.facts.blocks_motion(target),
    }
}

/// Called whenever a fluid block is set: lava may harden, otherwise the fluid
/// ticks after its delay.
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let Some((fluid, _)) = fluid_of(ctx, state) else {
        return;
    };
    if fluid == Fluid::Lava && harden_lava(level, ctx, p, state) {
        return;
    }
    level.schedule_fluid_tick(p, fluid as u32, props(fluid).delay);
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
        level.schedule_fluid_tick(p, fluid as u32, props(fluid).delay);
    }
}

/// Lava touching water anywhere but below turns into obsidian (a source) or
/// cobblestone (flowing). Returns whether it did.
fn harden_lava(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let touches_water = [Dir::Up, Dir::North, Dir::South, Dir::West, Dir::East]
        .into_iter()
        .any(|d| {
            matches!(
                fluid_of(ctx, level.block(p.offset(d))),
                Some((Fluid::Water, _))
            )
        });
    if !touches_water {
        return false;
    }
    let source = matches!(fluid_of(ctx, state), Some((_, 0)));
    let into = if source {
        "minecraft:obsidian"
    } else {
        "minecraft:cobblestone"
    };
    let new = ctx.shapes.state(into, &[]).unwrap_or(ctx.facts.air);
    set_block(level, ctx, p, new, Flags::ALL);
    true
}

/// A due fluid tick at `p`.
pub fn tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, fluid_id: u32) {
    let mut state = level.block(p);
    let Some((fluid, lvl)) = fluid_of(ctx, state) else {
        return;
    };
    if fluid as u32 != fluid_id {
        return;
    }
    if lvl != 0 {
        let new = new_state(level, ctx, p, fluid);
        if new != state {
            set_block(level, ctx, p, new, Flags::ALL);
            if ctx.shapes.is_air(new) {
                return;
            }
            state = new;
        }
    }
    spread(level, ctx, p, state, fluid);
}

/// What a flowing block should be, from its neighbours.
fn new_state(level: &dyn Level, ctx: Ctx, p: BlockPos, fluid: Fluid) -> u32 {
    let props = props(fluid);
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
    if props.makes_sources && sources >= 2 {
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

/// Whether fluid at `p` could fall into the block below it. Lava can fall
/// into water too (turning it to stone).
fn hole_below(level: &dyn Level, ctx: Ctx, fluid: Fluid, p: BlockPos) -> bool {
    let below = level.block(p.offset(Dir::Down));
    can_replace(ctx, fluid, below)
        || (fluid == Fluid::Lava && matches!(fluid_of(ctx, below), Some((Fluid::Water, _))))
}

fn spread(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, fluid: Fluid) {
    let Some((_, lvl)) = fluid_of(ctx, state) else {
        return;
    };
    let props = props(fluid);
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
    for d in spread_directions(level, ctx, p, fluid) {
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
        let distance = if hole_below(level, ctx, fluid, np) {
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
        if hole_below(level, ctx, fluid, np) {
            return depth;
        }
        if depth < props(fluid).slope {
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
