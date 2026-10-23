//! Random ticks (minecraft.wiki, "Tick" → "Random tick"): every game tick
//! each section near players has `random_tick_speed` blocks picked at
//! random; the ones that care grow, spread, decay or melt.
//!
//! What a state does is looked up once per state ([`Behaviour::of`]), so a
//! section holding none of them is skipped by its palette alone.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::rng::Rng;
use crate::updates::{Ctx, Flags, set_block};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    None,
    /// Wheat, carrots, potatoes, beetroots, torchflower crops.
    Crop {
        max: u8,
    },
    /// Pumpkin and melon stems.
    Stem,
    /// Sugar cane grows up to three high.
    Column,
    /// Cactus grows up to three high and may flower.
    Cactus,
    /// Bamboo grows up to 16 high; a shoot becomes bamboo.
    Bamboo,
    BambooSapling,
    /// Pointed dripstone drips into cauldrons.
    Dripstone,
    /// Glowing redstone ore goes dark.
    RedstoneOre,
    /// Copper ages a stage, slower beside younger copper.
    Weathering,
    /// Budding amethyst grows buds on its faces.
    BuddingAmethyst,
    /// Small mushrooms spread in the dark.
    Mushroom,
    NetherWart,
    SweetBerries,
    Cocoa,
    /// Kelp, weeping, twisting and cave vines growing at the tip.
    GrowingTip,
    /// Grass blocks and mycelium spread onto dirt and die in the dark.
    Spreading,
    Leaves,
    /// Vines creep along walls, up and down (game rule `spread_vines`).
    Vine,
    /// Saplings: stage 0 → 1, then a tree.
    Sapling,
    Ice,
    SnowLayer,
    Farmland,
    Lava,
}

impl Behaviour {
    pub fn of(full: &str) -> Self {
        let name = full.trim_start_matches("minecraft:");
        match name {
            "wheat" | "carrots" | "potatoes" => Self::Crop { max: 7 },
            "beetroots" => Self::Crop { max: 3 },
            "torchflower_crop" => Self::Crop { max: 2 },
            "pumpkin_stem" | "melon_stem" => Self::Stem,
            "sugar_cane" => Self::Column,
            "cactus" => Self::Cactus,
            "bamboo" => Self::Bamboo,
            "bamboo_sapling" => Self::BambooSapling,
            "pointed_dripstone" => Self::Dripstone,
            "redstone_ore" | "deepslate_redstone_ore" => Self::RedstoneOre,
            "budding_amethyst" => Self::BuddingAmethyst,
            "brown_mushroom" | "red_mushroom" => Self::Mushroom,
            _ if copper_age(name).is_some_and(|a| a < 3) => Self::Weathering,
            "nether_wart" => Self::NetherWart,
            "sweet_berry_bush" => Self::SweetBerries,
            "cocoa" => Self::Cocoa,
            "kelp" | "weeping_vines" | "twisting_vines" | "cave_vines" => Self::GrowingTip,
            "grass_block" | "mycelium" => Self::Spreading,
            "vine" => Self::Vine,
            "ice" => Self::Ice,
            "snow" => Self::SnowLayer,
            "farmland" => Self::Farmland,
            "lava" => Self::Lava,
            _ if name.ends_with("_leaves") => Self::Leaves,
            "oak_sapling" | "spruce_sapling" | "birch_sapling" | "jungle_sapling"
            | "acacia_sapling" | "dark_oak_sapling" | "pale_oak_sapling" | "cherry_sapling"
            | "mangrove_propagule" => Self::Sapling,
            _ => Self::None,
        }
    }
}

fn int(ctx: Ctx, state: u32, property: &str) -> u32 {
    ctx.shapes
        .property(state, property)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes
        .block_name(state)
        .unwrap_or("")
        .trim_start_matches("minecraft:")
}

/// A random tick of the block `state` at `p`.
pub fn tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    match ctx.shapes.random_behaviour(state) {
        Behaviour::None => {}
        Behaviour::Crop { max } => crop(level, ctx, p, state, u32::from(max), rng),
        Behaviour::Stem => stem(level, ctx, p, state, rng),
        Behaviour::Column => column(level, ctx, p, state),
        Behaviour::Cactus => cactus(level, ctx, p, state, rng),
        Behaviour::Bamboo => {
            let above = p.offset(Dir::Up);
            if int(ctx, state, "stage") == 0
                && rng.below(3) == 0
                && level.in_world(above)
                && ctx.shapes.is_air(level.block(above))
                && level.brightness(above) >= 9
            {
                let height = bamboo_below(level, ctx, p) + 1;
                if height < 16 {
                    grow_bamboo(level, ctx, p, state, height, rng);
                }
            }
        }
        Behaviour::BambooSapling => {
            let above = p.offset(Dir::Up);
            if rng.below(3) == 0
                && level.in_world(above)
                && ctx.shapes.is_air(level.block(above))
                && level.brightness(above) >= 9
            {
                grow_shoot(level, ctx, p);
            }
        }
        Behaviour::Dripstone => crate::cauldron::dripstone_tick(level, ctx, p, rng),
        Behaviour::Weathering => weather(level, ctx, p, state, rng),
        Behaviour::BuddingAmethyst => amethyst(level, ctx, p, rng),
        Behaviour::Mushroom => mushroom(level, ctx, p, state, rng),
        Behaviour::RedstoneOre => {
            if ctx.shapes.property(state, "lit") == Some("true") {
                let dark = ctx.shapes.with(state, "lit", "false");
                set_block(level, ctx, p, dark, Flags::ALL);
            }
        }
        Behaviour::NetherWart => {
            let age = int(ctx, state, "age");
            if age < 3 && rng.below(10) == 0 {
                grow(level, ctx, p, state, age + 1);
            }
        }
        Behaviour::SweetBerries => {
            let age = int(ctx, state, "age");
            if age < 3 && rng.below(5) == 0 && level.brightness(p.offset(Dir::Up)) >= 9 {
                grow(level, ctx, p, state, age + 1);
            }
        }
        Behaviour::Cocoa => {
            let age = int(ctx, state, "age");
            if age < 2 && rng.below(5) == 0 {
                grow(level, ctx, p, state, age + 1);
            }
        }
        Behaviour::GrowingTip => growing_tip(level, ctx, p, state, rng),
        Behaviour::Spreading => spreading(level, ctx, p, state, rng),
        Behaviour::Vine => vine(level, ctx, p, state, rng),
        Behaviour::Leaves => {
            if crate::leaves::decays(ctx, state) {
                level.destroyed(p, state);
                set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
            }
        }
        Behaviour::Sapling if name(ctx, state) == "mangrove_propagule" => {
            // Hanging ones ripen one stage a tick; planted ones need no
            // light and grow like saplings (wiki, "Mangrove Propagule").
            if ctx.shapes.property(state, "hanging") == Some("true") {
                let age = int(ctx, state, "age");
                if age < 4 {
                    grow(level, ctx, p, state, age + 1);
                }
            } else if rng.below(7) == 0 {
                if int(ctx, state, "stage") == 0 {
                    let next = ctx.shapes.with(state, "stage", "1");
                    set_block(level, ctx, p, next, Flags::NONE);
                } else {
                    level.grow_tree(p, state);
                }
            }
        }
        Behaviour::Sapling => {
            if level.brightness(p.offset(Dir::Up)) >= 9 && rng.below(7) == 0 {
                if int(ctx, state, "stage") == 0 {
                    let next = ctx.shapes.with(state, "stage", "1");
                    set_block(level, ctx, p, next, Flags::NONE);
                } else {
                    level.grow_tree(p, state);
                }
            }
        }
        Behaviour::Ice => {
            if level.block_light(p) > 11 - level.light_block(state).min(11) {
                let below = level.block(p.offset(Dir::Down));
                let holds =
                    ctx.facts.blocks_motion(below) || crate::fluids::fluid_of(ctx, below).is_some();
                let water = ctx
                    .shapes
                    .default_state("minecraft:water")
                    .unwrap_or(ctx.shapes.air());
                set_block(
                    level,
                    ctx,
                    p,
                    if holds { water } else { ctx.shapes.air() },
                    Flags::ALL,
                );
            }
        }
        Behaviour::SnowLayer => {
            if level.block_light(p) > 11 {
                level.destroyed(p, state);
                set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
            }
        }
        Behaviour::Farmland => farmland(level, ctx, p, state),
        Behaviour::Lava => lava(level, ctx, p, rng),
    }
}

fn grow(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, age: u32) {
    let new = ctx.shapes.with(state, "age", &age.to_string());
    set_block(level, ctx, p, new, Flags::ALL);
}

/// How well a crop grows (wiki, "Crops" → "Growth"): wet farmland under and
/// around helps, the same crop crowding in both directions (or diagonally)
/// halves it.
fn growth_speed(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> f64 {
    let block = ctx.shapes.block_index(state);
    let below = p.offset(Dir::Down);
    let mut speed = 1.0;
    for dx in -1..=1 {
        for dz in -1..=1 {
            let s = level.block(BlockPos::new(below.x + dx, below.y, below.z + dz));
            let mut g = 0.0;
            if name(ctx, s) == "farmland" {
                g = if int(ctx, s, "moisture") > 0 {
                    3.0
                } else {
                    1.0
                };
            }
            if dx != 0 || dz != 0 {
                g /= 4.0;
            }
            speed += g;
        }
    }
    let same = |dx: i32, dz: i32| {
        ctx.shapes
            .block_index(level.block(BlockPos::new(p.x + dx, p.y, p.z + dz)))
            == block
    };
    let rows_x = same(-1, 0) || same(1, 0);
    let rows_z = same(0, -1) || same(0, 1);
    let diagonal = same(-1, -1) || same(1, -1) || same(1, 1) || same(-1, 1);
    if (rows_x && rows_z) || diagonal {
        speed /= 2.0;
    }
    speed
}

fn crop(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, max: u32, rng: &mut Rng) {
    if level.brightness(p) < 9 {
        return;
    }
    let age = int(ctx, state, "age");
    if age >= max {
        return;
    }
    // Beetroots and torchflowers grow a third as often.
    if max < 7 && rng.below(3) != 0 {
        return;
    }
    let speed = growth_speed(level, ctx, p, state);
    if rng.below((25.0 / speed) as u32 + 1) != 0 {
        return;
    }
    // A torchflower crop's last stage is the flower itself.
    if name(ctx, state) == "torchflower_crop" && age + 1 == max {
        if let Some(flower) = ctx.shapes.default_state("minecraft:torchflower") {
            set_block(level, ctx, p, flower, Flags::ALL);
        }
        return;
    }
    grow(level, ctx, p, state, age + 1);
}

fn stem(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    if level.brightness(p) < 9 {
        return;
    }
    let speed = growth_speed(level, ctx, p, state);
    if rng.below((25.0 / speed) as u32 + 1) != 0 {
        return;
    }
    let age = int(ctx, state, "age");
    if age < 7 {
        grow(level, ctx, p, state, age + 1);
        return;
    }
    let pumpkin = name(ctx, state) == "pumpkin_stem";
    let dir = Dir::HORIZONTAL[rng.below(4) as usize];
    let at = p.offset(dir);
    let soil = level.block(at.offset(Dir::Down));
    let tag = if pumpkin {
        "minecraft:supports_pumpkin_stem_fruit"
    } else {
        "minecraft:supports_melon_stem_fruit"
    };
    if !ctx.shapes.is_air(level.block(at)) || !ctx.shapes.in_tag(soil, tag) {
        return;
    }
    let (fruit, attached) = if pumpkin {
        ("minecraft:pumpkin", "minecraft:attached_pumpkin_stem")
    } else {
        ("minecraft:melon", "minecraft:attached_melon_stem")
    };
    let (Some(fruit), Some(attached)) = (
        ctx.shapes.default_state(fruit),
        ctx.shapes.default_state(attached),
    ) else {
        return;
    };
    set_block(level, ctx, at, fruit, Flags::ALL);
    let attached = ctx.shapes.with(attached, "facing", dir.name());
    set_block(level, ctx, p, attached, Flags::ALL);
}

/// Sugar cane and cactus: the top block counts to 15, then grows one more
/// while the column is under three high.
fn column(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let above = p.offset(Dir::Up);
    if !level.in_world(above) || !ctx.shapes.is_air(level.block(above)) {
        return;
    }
    let block = ctx.shapes.block_index(state);
    let mut height = 1;
    while ctx
        .shapes
        .block_index(level.block(BlockPos::new(p.x, p.y - height, p.z)))
        == block
    {
        height += 1;
    }
    if height >= 3 {
        return;
    }
    let age = int(ctx, state, "age");
    if age == 15 {
        let young = ctx.shapes.with(state, "age", "0");
        set_block(level, ctx, above, young, Flags::ALL);
        set_block(level, ctx, p, young, Flags::NONE);
    } else {
        grow(level, ctx, p, state, age + 1);
    }
}

const COPPER_STAGES: [&str; 3] = ["exposed_", "weathered_", "oxidized_"];

/// How weathered unwaxed copper `name` (without `minecraft:`) is, 0..3;
/// `None` if it is not weathering copper.
pub fn copper_age(name: &str) -> Option<usize> {
    if name.starts_with("waxed_") || name.contains("ore") || name.starts_with("raw_") {
        return None;
    }
    let stage = COPPER_STAGES
        .iter()
        .position(|s| name.starts_with(s))
        .map_or(0, |i| i + 1);
    let rest = if stage == 0 {
        name
    } else {
        &name[COPPER_STAGES[stage - 1].len()..]
    };
    let copper = rest.contains("copper") || rest == "lightning_rod";
    (copper && !rest.starts_with("copper_ingot")).then_some(stage)
}

/// [`copper_age`] for copper that really weathers (it has a next stage, or
/// is fully oxidized): copper torches do not.
fn weathering_age(ctx: Ctx, name: &str) -> Option<usize> {
    let age = copper_age(name)?;
    (age == 3 || ctx.shapes.default_state(&next_copper(name, age)).is_some()).then_some(age)
}

/// The next stage of copper `name` (without `minecraft:`).
fn next_copper(name: &str, age: usize) -> String {
    let base = if age == 0 {
        if name == "copper_block" {
            "copper"
        } else {
            name
        }
    } else {
        &name[COPPER_STAGES[age - 1].len()..]
    };
    format!("minecraft:{}{base}", COPPER_STAGES[age])
}

/// Copper weathers (minecraft.wiki, "Oxidation"): about 1 random tick in
/// 18 it may age, never while younger copper is within 4 blocks (Manhattan
/// distance), and more likely the more older copper is around.
fn weather(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    if rng.unit() >= 0.056_888_89 {
        return;
    }
    let Some(age) = weathering_age(ctx, name(ctx, state)).filter(|&a| a < 3) else {
        return;
    };
    let (mut same, mut older) = (0, 0);
    for dx in -4i32..=4 {
        for dy in -4i32..=4 {
            for dz in -4i32..=4 {
                if dx.abs() + dy.abs() + dz.abs() > 4 || (dx, dy, dz) == (0, 0, 0) {
                    continue;
                }
                let q = BlockPos::new(p.x + dx, p.y + dy, p.z + dz);
                let Some(other) = weathering_age(ctx, name(ctx, level.block(q))) else {
                    continue;
                };
                if other < age {
                    return;
                }
                if other > age {
                    older += 1;
                } else {
                    same += 1;
                }
            }
        }
    }
    let share = f64::from(older + 1) / f64::from(older + same + 1);
    let modifier = if age == 0 { 0.75 } else { 1.0 };
    if rng.unit() >= share * share * modifier {
        return;
    }
    let next = next_copper(name(ctx, state), age);
    if let Some(new) = ctx.shapes.renamed(state, &next) {
        set_block(level, ctx, p, new, Flags::ALL);
    }
}

/// Budding amethyst: one tick in five a bud starts or grows on a random
/// face (in air or water).
fn amethyst(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    if rng.below(5) != 0 {
        return;
    }
    let d = Dir::ALL[rng.below(6) as usize];
    let q = p.offset(d);
    let s = level.block(q);
    let water = name(ctx, s) == "water" && ctx.shapes.property(s, "level") == Some("0");
    let facing = ctx.shapes.property(s, "facing") == Some(d.name());
    let next = match name(ctx, s) {
        _ if ctx.shapes.is_air(s) || water => "minecraft:small_amethyst_bud",
        "small_amethyst_bud" if facing => "minecraft:medium_amethyst_bud",
        "medium_amethyst_bud" if facing => "minecraft:large_amethyst_bud",
        "large_amethyst_bud" if facing => "minecraft:amethyst_cluster",
        _ => return,
    };
    let wet = water || ctx.shapes.property(s, "waterlogged") == Some("true");
    let Some(base) = ctx.shapes.default_state(next) else {
        return;
    };
    let new = ctx.shapes.with(
        ctx.shapes.with(base, "facing", d.name()),
        "waterlogged",
        if wet { "true" } else { "false" },
    );
    set_block(level, ctx, q, new, Flags::ALL);
}

/// A small mushroom spreads one time in 25 to a free spot nearby where it
/// can stand, unless five of its kind are already within 4 blocks.
fn mushroom(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    if rng.below(25) != 0 {
        return;
    }
    let kind = ctx.shapes.block_index(state);
    let mut left = 5;
    for dx in -4..=4 {
        for dy in -1..=1 {
            for dz in -4..=4 {
                let q = BlockPos::new(p.x + dx, p.y + dy, p.z + dz);
                if ctx.shapes.block_index(level.block(q)) == kind {
                    left -= 1;
                    if left <= 0 {
                        return;
                    }
                }
            }
        }
    }
    let step = |from: BlockPos, rng: &mut Rng| {
        let dy = rng.below(2) as i32 - rng.below(2) as i32;
        BlockPos::new(
            from.x + rng.below(3) as i32 - 1,
            from.y + dy,
            from.z + rng.below(3) as i32 - 1,
        )
    };
    let fits = |level: &dyn Level, q: BlockPos| {
        ctx.shapes.is_air(level.block(q)) && crate::support::can_survive(level, ctx, q, state)
    };
    let mut from = p;
    let mut target = step(from, rng);
    for _ in 0..4 {
        if fits(level, target) {
            from = target;
        }
        target = step(from, rng);
    }
    if fits(level, target) {
        set_block(level, ctx, target, state, Flags::SHAPES_ONLY);
    }
}

/// Cactus: like sugar cane, but a cactus that has grown half way (age 8)
/// may sprout a flower on top instead (one time in ten, one in four on a
/// full column).
fn cactus(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    let above = p.offset(Dir::Up);
    if !level.in_world(above) || !ctx.shapes.is_air(level.block(above)) {
        return;
    }
    let block = ctx.shapes.block_index(state);
    let age = int(ctx, state, "age");
    let mut height = 1;
    while ctx
        .shapes
        .block_index(level.block(BlockPos::new(p.x, p.y - height, p.z)))
        == block
    {
        height += 1;
        if height == 3 && age == 15 {
            return;
        }
    }
    if age == 8
        && let Some(flower) = ctx.shapes.default_state("minecraft:cactus_flower")
        && crate::support::can_survive(level, ctx, above, flower)
    {
        let chance = if height >= 3 { 0.25 } else { 0.1 };
        if rng.unit() <= chance {
            set_block(level, ctx, above, flower, Flags::ALL);
            return;
        }
    } else if age == 15 && height < 3 {
        let young = ctx.shapes.with(state, "age", "0");
        set_block(level, ctx, above, young, Flags::ALL);
        set_block(level, ctx, p, young, Flags::NONE);
        return;
    }
    if age < 15 {
        grow(level, ctx, p, state, age + 1);
    }
}

fn is_bamboo(ctx: Ctx, state: u32) -> bool {
    name(ctx, state) == "bamboo"
}

/// Bamboo blocks right below `p`, at most 15.
pub fn bamboo_below(level: &dyn Level, ctx: Ctx, p: BlockPos) -> u32 {
    let mut n = 0;
    while n < 16
        && is_bamboo(
            ctx,
            level.block(BlockPos::new(p.x, p.y - n as i32 - 1, p.z)),
        )
    {
        n += 1;
    }
    n
}

/// Bamboo blocks right above `p`, at most 15.
pub fn bamboo_above(level: &dyn Level, ctx: Ctx, p: BlockPos) -> u32 {
    let mut n = 0;
    while n < 16
        && is_bamboo(
            ctx,
            level.block(BlockPos::new(p.x, p.y + n as i32 + 1, p.z)),
        )
    {
        n += 1;
    }
    n
}

/// Bamboo at `p` (`height` tall with it) grows one block: the new top has
/// leaves, large ones once the stalk below has some (which then thin out
/// below it); stalks over 10 high may stop growing.
pub fn grow_bamboo(
    level: &mut dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    height: u32,
    rng: &mut Rng,
) {
    let below = level.block(p.offset(Dir::Down));
    let two_below = p.offset(Dir::Down).offset(Dir::Down);
    let below2 = level.block(two_below);
    let has_leaves = |s: u32| {
        ctx.shapes
            .property(s, "leaves")
            .is_some_and(|l| l != "none")
    };
    let mut leaves = "none";
    if height >= 1 {
        if !is_bamboo(ctx, below) || !has_leaves(below) {
            leaves = "small";
        } else {
            leaves = "large";
            if is_bamboo(ctx, below2) {
                let small = ctx.shapes.with(below, "leaves", "small");
                set_block(level, ctx, p.offset(Dir::Down), small, Flags::ALL);
                let bare = ctx.shapes.with(below2, "leaves", "none");
                set_block(level, ctx, two_below, bare, Flags::ALL);
            }
        }
    }
    let age = if int(ctx, state, "age") == 1 || is_bamboo(ctx, below2) {
        "1"
    } else {
        "0"
    };
    let stage = if (height >= 11 && rng.unit() < 0.25) || height == 15 {
        "1"
    } else {
        "0"
    };
    let Some(base) = ctx.shapes.default_state("minecraft:bamboo") else {
        return;
    };
    let top = ctx.shapes.with(
        ctx.shapes
            .with(ctx.shapes.with(base, "age", age), "leaves", leaves),
        "stage",
        stage,
    );
    set_block(level, ctx, p.offset(Dir::Up), top, Flags::ALL);
}

/// A bamboo shoot grows: bamboo with small leaves above it, and the shoot
/// becomes bamboo.
pub fn grow_shoot(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let Some(base) = ctx.shapes.default_state("minecraft:bamboo") else {
        return;
    };
    set_block(
        level,
        ctx,
        p.offset(Dir::Up),
        ctx.shapes.with(base, "leaves", "small"),
        Flags::ALL,
    );
    set_block(level, ctx, p, base, Flags::ALL);
}

/// Kelp and vines: the tip grows on into water (kelp) or air (vines), the
/// old tip becoming stem.
fn growing_tip(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    let tip = name(ctx, state);
    let (dir, chance, body) = match tip {
        "kelp" => (Dir::Up, 0.14, "minecraft:kelp_plant"),
        "twisting_vines" => (Dir::Up, 0.1, "minecraft:twisting_vines_plant"),
        "weeping_vines" => (Dir::Down, 0.1, "minecraft:weeping_vines_plant"),
        _ => (Dir::Down, 0.1, "minecraft:cave_vines_plant"),
    };
    let age = int(ctx, state, "age");
    if age >= 25 || rng.unit() >= chance {
        return;
    }
    let at = p.offset(dir);
    if !level.in_world(at) {
        return;
    }
    let there = level.block(at);
    let room = if tip == "kelp" {
        name(ctx, there) == "water" && int(ctx, there, "level") == 0
    } else {
        ctx.shapes.is_air(there)
    };
    if !room {
        return;
    }
    let mut new_tip = ctx.shapes.with(state, "age", &(age + 1).to_string());
    if tip == "cave_vines" {
        let berries = if rng.unit() < 0.11 { "true" } else { "false" };
        new_tip = ctx.shapes.with(new_tip, "berries", berries);
    }
    set_block(level, ctx, at, new_tip, Flags::ALL);
    if let Some(mut stem) = ctx.shapes.default_state(body) {
        if tip == "cave_vines" {
            let berries = ctx.shapes.property(state, "berries").unwrap_or("false");
            stem = ctx.shapes.with(stem, "berries", berries);
        }
        set_block(level, ctx, p, stem, Flags::ALL);
    }
}

fn face(ctx: Ctx, state: u32, d: Dir) -> bool {
    ctx.shapes.property(state, d.name()) == Some("true")
}

/// A wall (or ceiling) a vine can hold to: a block whose face towards the
/// vine is full.
fn holds_vine(level: &dyn Level, ctx: Ctx, at: BlockPos) -> bool {
    ctx.facts.blocks_motion(level.block(at))
}

/// Vines within 4 blocks sideways and 1 up or down stop it spreading when
/// there are five (itself counted).
fn can_spread(level: &dyn Level, ctx: Ctx, p: BlockPos, vine: u32) -> bool {
    let kind = ctx.shapes.block_index(vine);
    let mut left = 5;
    for dx in -4..=4 {
        for dy in -1..=1 {
            for dz in -4..=4 {
                let q = BlockPos::new(p.x + dx, p.y + dy, p.z + dz);
                if ctx.shapes.block_index(level.block(q)) == kind {
                    left -= 1;
                    if left <= 0 {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// A vine's random tick (minecraft.wiki, "Vines" → "Growth"): a quarter of
/// the time, with the rule `spread_vines` on, it tries one random way.
fn vine(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    if !level.spread_vines() || rng.below(4) != 0 {
        return;
    }
    let dir = Dir::ALL[rng.below(6) as usize];
    let set = |level: &mut dyn Level, at: BlockPos, base: u32, d: Dir| {
        let new = ctx.shapes.with(base, d.name(), "true");
        set_block(level, ctx, at, new, Flags::NONE);
    };
    let blank = ctx.shapes.default_state("minecraft:vine");
    let Some(blank) = blank else {
        return;
    };
    let horizontal = !matches!(dir, Dir::Up | Dir::Down);
    if horizontal && !face(ctx, state, dir) {
        if !can_spread(level, ctx, p, state) {
            return;
        }
        let rel = p.offset(dir);
        let there = level.block(rel);
        if ctx.shapes.is_air(there) {
            let (cw, ccw) = (dir.cw(), dir.ccw());
            let (flag, flag1) = (face(ctx, state, cw), face(ctx, state, ccw));
            let rel_cw = rel.offset(cw);
            let rel_ccw = rel.offset(ccw);
            if flag && holds_vine(level, ctx, rel_cw) {
                set(level, rel, blank, cw);
            } else if flag1 && holds_vine(level, ctx, rel_ccw) {
                set(level, rel, blank, ccw);
            } else {
                let opposite = dir.opposite();
                if flag
                    && ctx.shapes.is_air(level.block(rel_cw))
                    && holds_vine(level, ctx, p.offset(cw))
                {
                    set(level, rel_cw, blank, opposite);
                } else if flag1
                    && ctx.shapes.is_air(level.block(rel_ccw))
                    && holds_vine(level, ctx, p.offset(ccw))
                {
                    set(level, rel_ccw, blank, opposite);
                } else if rng.unit() < 0.05 && holds_vine(level, ctx, rel.offset(Dir::Up)) {
                    set(level, rel, blank, Dir::Up);
                }
            }
        } else if holds_vine(level, ctx, rel) {
            set(level, p, state, dir);
        }
        return;
    }
    let sides = [Dir::North, Dir::East, Dir::South, Dir::West];
    let has_side = |s: u32| sides.iter().any(|&d| face(ctx, s, d));
    if dir == Dir::Up && level.in_world(p.offset(Dir::Up)) {
        let above = p.offset(Dir::Up);
        if holds_vine(level, ctx, above) {
            set(level, p, state, Dir::Up);
            return;
        }
        if ctx.shapes.is_air(level.block(above)) {
            if !can_spread(level, ctx, p, state) {
                return;
            }
            let mut s = state;
            for d in sides {
                if rng.below(2) == 0 || !holds_vine(level, ctx, above.offset(d)) {
                    s = ctx.shapes.with(s, d.name(), "false");
                }
            }
            if has_side(s) {
                set_block(level, ctx, above, s, Flags::NONE);
            }
            return;
        }
    }
    let below = p.offset(Dir::Down);
    if level.in_world(below) {
        let below_state = level.block(below);
        let same = ctx.shapes.block_index(below_state) == ctx.shapes.block_index(state);
        if ctx.shapes.is_air(below_state) || same {
            let target = if same { below_state } else { blank };
            let mut s = target;
            for d in sides {
                if rng.below(2) == 0 && face(ctx, state, d) {
                    s = ctx.shapes.with(s, d.name(), "true");
                }
            }
            if s != target && has_side(s) {
                set_block(level, ctx, below, s, Flags::NONE);
            }
        }
    }
}

/// Whether grass may stay on the block at `p`: not under a full block of
/// light-stopping stuff or water (a single snow layer is fine).
fn can_be_grass(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let above = level.block(p.offset(Dir::Up));
    if name(ctx, above) == "snow" && int(ctx, above, "layers") == 1 {
        return true;
    }
    if name(ctx, above) == "water" && int(ctx, above, "level") == 0 {
        return false;
    }
    level.light_block(above) < 15
}

fn spreading(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    if !can_be_grass(level, ctx, p) {
        let dirt = ctx.shapes.default_state("minecraft:dirt").unwrap_or(state);
        set_block(level, ctx, p, dirt, Flags::ALL);
        return;
    }
    if level.brightness(p.offset(Dir::Up)) < 9 {
        return;
    }
    let plain = ctx.shapes.with(state, "snowy", "false");
    for _ in 0..4 {
        let q = BlockPos::new(
            p.x + rng.below(3) as i32 - 1,
            p.y + rng.below(5) as i32 - 3,
            p.z + rng.below(3) as i32 - 1,
        );
        if !level.in_world(q) || name(ctx, level.block(q)) != "dirt" {
            continue;
        }
        let up = level.block(q.offset(Dir::Up));
        if !can_be_grass(level, ctx, q) || name(ctx, up) == "water" {
            continue;
        }
        let snowy = matches!(name(ctx, up), "snow" | "snow_block" | "powder_snow");
        let new = ctx
            .shapes
            .with(plain, "snowy", if snowy { "true" } else { "false" });
        set_block(level, ctx, q, new, Flags::ALL);
    }
}

/// Farmland is wet within four blocks of water (or in rain), dries slowly
/// otherwise and turns back to dirt when dry with nothing growing on it.
fn farmland(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let moisture = int(ctx, state, "moisture");
    let near_water = (-4..=4).any(|dx| {
        (0..=1).any(|dy| {
            (-4..=4).any(|dz| {
                let s = level.block(BlockPos::new(p.x + dx, p.y + dy, p.z + dz));
                name(ctx, s) == "water" || ctx.shapes.property(s, "waterlogged") == Some("true")
            })
        })
    });
    if near_water || level.raining_at(p.offset(Dir::Up)) {
        if moisture < 7 {
            grow_moisture(level, ctx, p, state, 7);
        }
    } else if moisture > 0 {
        grow_moisture(level, ctx, p, state, moisture - 1);
    } else {
        let above = level.block(p.offset(Dir::Up));
        if !ctx.shapes.in_tag(above, "minecraft:maintains_farmland") {
            let dirt = ctx.shapes.default_state("minecraft:dirt").unwrap_or(state);
            set_block(level, ctx, p, dirt, Flags::ALL);
        }
    }
}

fn grow_moisture(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, moisture: u32) {
    let new = ctx.shapes.with(state, "moisture", &moisture.to_string());
    set_block(level, ctx, p, new, Flags::SHAPES_ONLY);
}

/// Lava sets fire to flammable blocks near it (wiki, "Lava" → "Fire").
fn lava(level: &mut dyn Level, ctx: Ctx, p: BlockPos, rng: &mut Rng) {
    if !level.fire_spreads_at(p) {
        return;
    }
    let flammable =
        |level: &dyn Level, q: BlockPos| crate::fire::flammability(ctx, level.block(q)).0 > 0;
    let tries = rng.below(3);
    if tries > 0 {
        let mut q = p;
        for _ in 0..tries {
            q = BlockPos::new(
                q.x + rng.below(3) as i32 - 1,
                q.y + 1,
                q.z + rng.below(3) as i32 - 1,
            );
            if !level.in_world(q) {
                return;
            }
            let s = level.block(q);
            if ctx.shapes.is_air(s) {
                if Dir::ALL.iter().any(|&d| flammable(level, q.offset(d))) {
                    if let Some(fire) = crate::fire::fire_state(level, ctx, q, 0) {
                        set_block(level, ctx, q, fire, Flags::ALL);
                    }
                    return;
                }
            } else if ctx.facts.blocks_motion(s) {
                return;
            }
        }
    } else {
        for _ in 0..3 {
            let q = BlockPos::new(
                p.x + rng.below(3) as i32 - 1,
                p.y,
                p.z + rng.below(3) as i32 - 1,
            );
            if !level.in_world(q) {
                return;
            }
            let up = q.offset(Dir::Up);
            if ctx.shapes.is_air(level.block(up))
                && flammable(level, q)
                && let Some(fire) = crate::fire::fire_state(level, ctx, up, 0)
            {
                set_block(level, ctx, up, fire, Flags::ALL);
            }
        }
    }
}
