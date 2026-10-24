//! What a lightning bolt does to blocks (minecraft.wiki, "Lightning",
//! "Lightning Rod", "Oxidation"): a lightning rod it strikes gives a signal
//! for a moment, and unwaxed copper at the spot is cleaned back to new,
//! with a few random walks of cleaning among the copper around it.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::random::weathering_age;
use crate::rng::Rng;
use crate::updates::{Ctx, Flags, set_block};

/// The block at `p` (the one under the bolt) was struck.
pub fn strike(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    crate::redstone::rod_struck(level, ctx, p);
    clean_copper(level, ctx, p, rng);
}

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes
        .block_name(state)
        .map_or("", |n| n.strip_prefix("minecraft:").unwrap_or(n))
}

/// The copper block `name` (without `minecraft:`) as it was new.
fn first_stage(name: &str, age: usize) -> String {
    let stages = ["exposed_", "weathered_", "oxidized_"];
    let base = if age == 0 {
        name
    } else {
        &name[stages[age - 1].len()..]
    };
    if base == "copper" {
        "copper_block".to_owned()
    } else {
        base.to_owned()
    }
}

/// The copper block `name` one stage less weathered.
fn previous_stage(name: &str, age: usize) -> Option<String> {
    match age {
        0 => None,
        1 => Some(first_stage(name, age)),
        _ => {
            let stages = ["exposed_", "weathered_", "oxidized_"];
            Some(format!(
                "{}{}",
                stages[age - 2],
                &name[stages[age - 1].len()..]
            ))
        }
    }
}

fn weathering(ctx: Ctx, state: u32) -> Option<usize> {
    weathering_age(ctx, name(ctx, state))
}

fn rename(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, to: &str) {
    if let Some(new) = ctx.shapes.renamed(state, &format!("minecraft:{to}")) {
        set_block(level, ctx, p, new, Flags::ALL);
    }
}

/// Copper at the strike (or, for a rod, what the rod is on) becomes new;
/// then 3 to 5 walks of up to 8 steps clean copper around it by a stage.
fn clean_copper(level: &mut dyn Level, ctx: Ctx, strike: BlockPos, rng: &mut Rng) {
    let state = level.block(strike);
    let at = if name(ctx, state).ends_with("lightning_rod") {
        match ctx
            .shapes
            .property(state, "facing")
            .and_then(Dir::from_name)
        {
            Some(f) => strike.offset(f.opposite()),
            None => return,
        }
    } else {
        strike
    };
    let block = level.block(at);
    let Some(age) = weathering(ctx, block) else {
        return;
    };
    let first = first_stage(name(ctx, block), age);
    rename(level, ctx, at, block, &first);
    for _ in 0..rng.below(3) + 3 {
        let steps = rng.below(8) + 1;
        let mut from = at;
        for _ in 0..steps {
            let Some(next) = step(level, ctx, from, rng) else {
                break;
            };
            from = next;
        }
    }
}

/// One step of a cleaning walk: the first of 10 random spots within a block
/// of `from` that holds weathering copper, cleaned by a stage.
fn step(level: &mut dyn Level, ctx: Ctx, from: BlockPos, rng: &mut Rng) -> Option<BlockPos> {
    for _ in 0..10 {
        let jump = |rng: &mut Rng| rng.below(3) as i32 - 1;
        let p = BlockPos::new(from.x + jump(rng), from.y + jump(rng), from.z + jump(rng));
        let state = level.block(p);
        let Some(age) = weathering(ctx, state) else {
            continue;
        };
        if let Some(previous) = previous_stage(name(ctx, state), age) {
            rename(level, ctx, p, state, &previous);
        }
        level.level_event(3002, p, -1);
        return Some(p);
    }
    None
}
