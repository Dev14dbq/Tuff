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
    Crop { max: u8 },
    /// Pumpkin and melon stems.
    Stem,
    /// Sugar cane and cactus grow up to three high.
    Column,
    NetherWart,
    SweetBerries,
    Cocoa,
    /// Kelp, weeping, twisting and cave vines growing at the tip.
    GrowingTip,
    /// Grass blocks and mycelium spread onto dirt and die in the dark.
    Spreading,
    Leaves,
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
            "sugar_cane" | "cactus" => Self::Column,
            "nether_wart" => Self::NetherWart,
            "sweet_berry_bush" => Self::SweetBerries,
            "cocoa" => Self::Cocoa,
            "kelp" | "weeping_vines" | "twisting_vines" | "cave_vines" => Self::GrowingTip,
            "grass_block" | "mycelium" => Self::Spreading,
            "ice" => Self::Ice,
            "snow" => Self::SnowLayer,
            "farmland" => Self::Farmland,
            "lava" => Self::Lava,
            _ if name.ends_with("_leaves") => Self::Leaves,
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
        Behaviour::Leaves => {
            if crate::leaves::decays(ctx, state) {
                level.destroyed(p, state);
                set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
            }
        }
        Behaviour::Ice => {
            if level.block_light(p) > 11 - level.light_block(state).min(11) {
                let below = level.block(p.offset(Dir::Down));
                let holds = ctx.facts.blocks_motion(below) || crate::fluids::fluid_of(ctx, below).is_some();
                let water = ctx.shapes.default_state("minecraft:water").unwrap_or(ctx.shapes.air());
                set_block(level, ctx, p, if holds { water } else { ctx.shapes.air() }, Flags::ALL);
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
                g = if int(ctx, s, "moisture") > 0 { 3.0 } else { 1.0 };
            }
            if dx != 0 || dz != 0 {
                g /= 4.0;
            }
            speed += g;
        }
    }
    let same = |dx: i32, dz: i32| {
        ctx.shapes.block_index(level.block(BlockPos::new(p.x + dx, p.y, p.z + dz))) == block
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
    while ctx.shapes.block_index(level.block(BlockPos::new(p.x, p.y - height, p.z))) == block {
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
        let new = ctx.shapes.with(plain, "snowy", if snowy { "true" } else { "false" });
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
    let flammable = |level: &dyn Level, q: BlockPos| crate::fire::flammability(ctx, level.block(q)).0 > 0;
    let tries = rng.below(3);
    if tries > 0 {
        let mut q = p;
        for _ in 0..tries {
            q = BlockPos::new(q.x + rng.below(3) as i32 - 1, q.y + 1, q.z + rng.below(3) as i32 - 1);
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
            let q = BlockPos::new(p.x + rng.below(3) as i32 - 1, p.y, p.z + rng.below(3) as i32 - 1);
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
