//! Fire: what burns and how fast (minecraft.wiki, "Fire"), fire climbing up
//! the sides of flammable blocks, burning out, and spreading every 30-39
//! ticks.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::rng::Rng;
use crate::support::TOP_FULL;
use crate::updates::{Ctx, Flags, set_block};

/// Woods that burn; crimson and warped stems do not.
const WOODS: &[&str] = &[
    "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry", "pale_oak",
    "bamboo",
];

/// Flowers and plants that catch at 60 and burn up at 100.
const PLANTS: &[&str] = &[
    "short_grass",
    "fern",
    "dead_bush",
    "tall_grass",
    "large_fern",
    "sunflower",
    "lilac",
    "rose_bush",
    "peony",
    "pitcher_plant",
    "dandelion",
    "poppy",
    "blue_orchid",
    "allium",
    "azure_bluet",
    "oxeye_daisy",
    "cornflower",
    "lily_of_the_valley",
    "wither_rose",
    "torchflower",
    "pink_petals",
    "open_eyeblossom",
    "closed_eyeblossom",
    "sweet_berry_bush",
    "spore_blossom",
    "leaf_litter",
    "wildflowers",
    "bush",
    "firefly_bush",
    "short_dry_grass",
    "tall_dry_grass",
    "cactus_flower",
];

/// How readily a block catches fire next to it, and how readily it burns
/// away: (ignite, burn). (0, 0) for blocks that do not burn.
pub fn flammability(ctx: Ctx, state: u32) -> (u32, u32) {
    if ctx.shapes.property(state, "waterlogged") == Some("true") {
        return (0, 0);
    }
    let Some(full) = ctx.shapes.block_name(state) else {
        return (0, 0);
    };
    let name = full.trim_start_matches("minecraft:");
    let wooden = || {
        let base = name.trim_start_matches("stripped_");
        WOODS.iter().any(|w| base.starts_with(w))
    };
    match name {
        "bookshelf" | "chiseled_bookshelf" | "lectern" | "bee_nest" => (30, 20),
        "tnt" => (15, 100),
        "vine" | "glow_lichen" | "big_dripleaf" | "big_dripleaf_stem" | "small_dripleaf" => {
            (15, 100)
        }
        "coal_block" | "bamboo_block" | "stripped_bamboo_block" => (5, 5),
        "hay_block" => (60, 20),
        "target" => (15, 20),
        "dried_kelp_block" | "hanging_roots" | "azalea" | "flowering_azalea" => (30, 60),
        "bamboo" | "scaffolding" => (60, 60),
        "composter" | "beehive" | "mangrove_roots" => (5, 20),
        "cave_vines" | "cave_vines_plant" => (15, 60),
        "moss_block" | "moss_carpet" | "pale_moss_block" | "pale_moss_carpet"
        | "pale_hanging_moss" => (5, 100),
        _ if PLANTS.contains(&name) => (60, 100),
        _ if name.ends_with("_leaves") => (30, 60),
        _ if name.ends_with("_wool") => (30, 60),
        _ if name.ends_with("_carpet") => (60, 20),
        _ if (name.ends_with("_log") || name.ends_with("_wood")) && wooden() => (5, 5),
        _ if (name.ends_with("_planks")
            || name.ends_with("_slab")
            || name.ends_with("_stairs")
            || name.ends_with("_fence")
            || name.ends_with("_fence_gate"))
            && wooden() =>
        {
            (5, 20)
        }
        _ => (0, 0),
    }
}

fn can_burn(ctx: Ctx, state: u32) -> bool {
    flammability(ctx, state).0 > 0
}

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes
        .block_name(state)
        .unwrap_or("")
        .trim_start_matches("minecraft:")
}

pub fn is_fire(ctx: Ctx, state: u32) -> bool {
    matches!(name(ctx, state), "fire" | "soul_fire")
}

fn soul_base(ctx: Ctx, state: u32) -> bool {
    matches!(name(ctx, state), "soul_sand" | "soul_soil")
}

fn sturdy_top(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.support(state) & TOP_FULL != 0
}

/// Whether a flammable block touches `p`.
fn next_to_fuel(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    Dir::ALL
        .iter()
        .any(|&d| can_burn(ctx, level.block(p.offset(d))))
}

/// The fire that would be lit at `p`: soul fire on soul sand and soil;
/// otherwise fire on the ground or, in the air, clinging to the flammable
/// blocks beside and above.
pub fn fire_state(level: &dyn Level, ctx: Ctx, p: BlockPos, age: u32) -> Option<u32> {
    let below = level.block(p.offset(Dir::Down));
    if soul_base(ctx, below) {
        return ctx.shapes.default_state("minecraft:soul_fire");
    }
    let mut state = ctx.shapes.default_state("minecraft:fire")?;
    state = ctx.shapes.with(state, "age", &age.to_string());
    if !(sturdy_top(ctx, below) || can_burn(ctx, below)) {
        for d in [Dir::North, Dir::East, Dir::South, Dir::West, Dir::Up] {
            let burns = can_burn(ctx, level.block(p.offset(d)));
            state = ctx
                .shapes
                .with(state, d.name(), if burns { "true" } else { "false" });
        }
    }
    Some(state)
}

/// Whether fire may stay at `p`.
pub fn can_survive(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let below = level.block(p.offset(Dir::Down));
    match name(ctx, state) {
        "soul_fire" => soul_base(ctx, below),
        _ => sturdy_top(ctx, below) || next_to_fuel(level, ctx, p),
    }
}

/// Whether a player may light fire at `p` (an empty spot where it holds).
pub fn can_light(level: &dyn Level, ctx: Ctx, p: BlockPos) -> Option<u32> {
    if !ctx.shapes.is_air(level.block(p)) {
        return None;
    }
    let state = fire_state(level, ctx, p, 0)?;
    can_survive(level, ctx, p, state).then_some(state)
}

/// A neighbour changed: fire re-clings or goes out.
pub fn update_shape(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u32 {
    if !is_fire(ctx, state) {
        return state;
    }
    if !can_survive(level, ctx, p, state) {
        return ctx.shapes.air();
    }
    if name(ctx, state) == "soul_fire" {
        return state;
    }
    fire_state(level, ctx, p, age(ctx, state)).unwrap_or(state)
}

fn age(ctx: Ctx, state: u32) -> u32 {
    ctx.shapes
        .property(state, "age")
        .and_then(|a| a.parse().ok())
        .unwrap_or(0)
}

/// Fire just placed starts its clock.
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, old: u32, state: u32) {
    if name(ctx, state) != "fire" || ctx.shapes.block_index(old) == ctx.shapes.block_index(state) {
        return;
    }
    let mut rng = Rng::at(p, level.game_time());
    schedule(level, ctx, p, state, &mut rng);
}

fn schedule(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    let kind = ctx.shapes.block_index(state);
    if !level.is_block_tick_scheduled(p, kind) {
        level.schedule_block_tick(p, kind, 30 + i64::from(rng.below(10)), 0);
    }
}

/// A due fire tick: age, burn out, burn neighbours away, spread.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind || name(ctx, state) != "fire" {
        return;
    }
    let mut rng = Rng::at(p, level.game_time());
    schedule_again(level, ctx, p, state, &mut rng);
    if !level.fire_spreads_at(p) {
        return;
    }
    if !can_survive(level, ctx, p, state) {
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
        return;
    }
    let below = level.block(p.offset(Dir::Down));
    let infinite = matches!(name(ctx, below), "netherrack" | "magma_block");
    let old_age = age(ctx, state);
    if !infinite && near_rain(level, p) && rng.unit() < 0.2 + f64::from(old_age) * 0.03 {
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
        return;
    }
    let age = (old_age + rng.below(3) / 2).min(15);
    if age != old_age {
        let aged = ctx.shapes.with(state, "age", &age.to_string());
        set_block(level, ctx, p, aged, Flags::NONE);
    }
    if !infinite {
        if !next_to_fuel(level, ctx, p) {
            if !sturdy_top(ctx, below) || age > 3 {
                set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
            }
            return;
        }
        if age == 15 && rng.below(4) == 0 && !can_burn(ctx, below) {
            set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
            return;
        }
    }
    for (d, chance) in [
        (Dir::East, 300),
        (Dir::West, 300),
        (Dir::Down, 250),
        (Dir::Up, 250),
        (Dir::North, 300),
        (Dir::South, 300),
    ] {
        burn_out(level, ctx, p.offset(d), chance, age, &mut rng);
    }
    // Normal difficulty.
    let difficulty = 2;
    for dx in -1..=1 {
        for dz in -1..=1 {
            for dy in -1..=4 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                let q = BlockPos::new(p.x + dx, p.y + dy, p.z + dz);
                let odds = if dy > 1 {
                    100 + (dy as u32 - 1) * 100
                } else {
                    100
                };
                let ignite = ignite_odds(level, ctx, q);
                if ignite == 0 {
                    continue;
                }
                let catch = (ignite + 40 + difficulty * 7) / (age + 30);
                if catch > 0 && rng.below(odds) <= catch && !near_rain(level, q) {
                    let new_age = (age + rng.below(5) / 4).min(15);
                    if let Some(fire) = fire_state(level, ctx, q, new_age) {
                        set_block(level, ctx, q, fire, Flags::ALL);
                    }
                }
            }
        }
    }
}

/// Rain on `p` or beside it.
fn near_rain(level: &dyn Level, p: BlockPos) -> bool {
    level.raining_at(p)
        || Dir::HORIZONTAL
            .iter()
            .any(|&d| level.raining_at(p.offset(d)))
}

fn schedule_again(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    let kind = ctx.shapes.block_index(state);
    level.schedule_block_tick(p, kind, 30 + i64::from(rng.below(10)), 0);
}

/// How readily the empty spot `q` catches: the best of its neighbours.
fn ignite_odds(level: &dyn Level, ctx: Ctx, q: BlockPos) -> u32 {
    if !level.in_world(q) || !ctx.shapes.is_air(level.block(q)) {
        return 0;
    }
    Dir::ALL
        .iter()
        .map(|&d| flammability(ctx, level.block(q.offset(d))).0)
        .max()
        .unwrap_or(0)
}

/// The block at `q` may burn away, sometimes leaving fire in its place.
fn burn_out(level: &mut dyn Level, ctx: Ctx, q: BlockPos, chance: u32, age: u32, rng: &mut Rng) {
    let state = level.block(q);
    let burn = flammability(ctx, state).1;
    if rng.below(chance) >= burn {
        return;
    }
    let tnt = name(ctx, state) == "tnt";
    // Burnt blocks drop nothing.
    if rng.below(age + 10) < 5 && !level.raining_at(q) {
        let new_age = (age + rng.below(5) / 4).min(15);
        if let Some(fire) = fire_state(level, ctx, q, new_age) {
            set_block(level, ctx, q, fire, Flags::ALL);
            if tnt {
                level.prime_tnt(q);
            }
            return;
        }
    }
    set_block(level, ctx, q, ctx.shapes.air(), Flags::ALL);
    if tnt {
        level.prime_tnt(q);
    }
}
