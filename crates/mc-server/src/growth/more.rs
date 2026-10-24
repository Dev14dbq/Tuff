//! Bone meal on plants other than crops and saplings (minecraft.wiki,
//! "Bone Meal" → "Fertilizer"): grass and moss spread, small plants grow
//! tall, mushrooms and fungi grow huge, vines and bamboo grow on, glow
//! lichen spreads, tall flowers and petals give a copy.

use mc_protocol::Position;
use mc_tick::{Dir, Level as _};

use super::unit;
use crate::level::{WorldLevel, to_pos};

fn at(p: Position, dx: i32, dy: i32, dz: i32) -> Position {
    Position::new(p.x + dx, p.y + dy, p.z + dz)
}

fn name_at(level: &WorldLevel, p: Position) -> String {
    let ctx = level.logic();
    ctx.shapes
        .block_name(level.block(to_pos(p)))
        .unwrap_or("")
        .trim_start_matches("minecraft:")
        .to_owned()
}

fn is_air(level: &WorldLevel, p: Position) -> bool {
    level.logic().shapes.is_air(level.block(to_pos(p)))
}

fn named(level: &WorldLevel, name: &str) -> Option<u32> {
    level
        .logic()
        .shapes
        .default_state(&format!("minecraft:{name}"))
}

/// Places a configured feature at `p` as worldgen would (`false` if it did
/// not fit or does not exist).
fn place(level: &mut WorldLevel, id: &str, p: Position, cleared: &[Position]) -> bool {
    let ctx = level.logic();
    let Some((changes, _)) = crate::commands::place::place_configured(
        level.context(),
        &format!("minecraft:{id}"),
        p,
        cleared,
    ) else {
        return false;
    };
    for (q, new) in changes {
        let new = mc_tick::placement_state(level, ctx, to_pos(q), new);
        level.set_block(q, new);
    }
    true
}

/// An item of the block at `p` pops out of it.
fn drop_copy(level: &mut WorldLevel, p: Position, item: &str) {
    let pos = [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ];
    if let Some(e) = crate::decor::dropped(level.context(), pos, &format!("minecraft:{item}"), 1, 0)
    {
        level.spawn(e);
    }
}

/// Places a two-block plant (lower half at `p`) if the space above is free.
fn tall(level: &mut WorldLevel, p: Position, name: &str) -> bool {
    let up = at(p, 0, 1, 0);
    if !is_air(level, up) {
        return false;
    }
    let Some(base) = named(level, name) else {
        return false;
    };
    let ctx = level.logic();
    let (lower, upper) = (
        ctx.shapes.with(base, "half", "lower"),
        ctx.shapes.with(base, "half", "upper"),
    );
    // Both halves at once: neither may see the other missing.
    mc_tick::set_block(level, ctx, to_pos(p), lower, mc_tick::Flags::NONE);
    mc_tick::set_block(level, ctx, to_pos(up), upper, mc_tick::Flags::NONE);
    true
}

/// The flower a grass block grows here: the first flower feature of the
/// biome.
fn flower_feature(level: &WorldLevel, p: Position) -> &'static str {
    let ctx = level.context();
    let biome = ctx
        .world
        .biome(p, &ctx.data.blocks)
        .and_then(|b| ctx.world.biome_name(b))
        .unwrap_or("")
        .trim_start_matches("minecraft:");
    match biome {
        "flower_forest" => "flower_flower_forest",
        "meadow" => "flower_meadow",
        "cherry_grove" => "flower_cherry",
        "pale_garden" => "flower_pale_garden",
        "plains" | "sunflower_plains" => "flower_plain",
        "swamp" => "flower_swamp",
        _ => "flower_default",
    }
}

/// Grass spreads plants around: 128 tries, each a random walk out from the
/// block above (longer for later tries) over grass; free spots get short
/// grass or, one time in eight, the biome's flower, and short grass may
/// grow tall.
fn grass(level: &mut WorldLevel, p: Position) {
    let r = |n: u64| (crate::util::random_u64() % n) as i32;
    let above = at(p, 0, 1, 0);
    'tries: for i in 0..128 {
        let mut q = above;
        for _ in 0..i / 16 {
            q = at(q, r(3) - 1, (r(3) - 1) * r(3) / 2, r(3) - 1);
            let below = at(q, 0, -1, 0);
            let full = level.logic().shapes.support(level.block(to_pos(q)))
                & mc_tick::support::TOP_FULL
                != 0;
            if name_at(level, below) != "grass_block" || full {
                continue 'tries;
            }
        }
        if name_at(level, q) == "short_grass" && r(10) == 0 {
            tall(level, q, "tall_grass");
        }
        if is_air(level, q) {
            let feature = if r(8) == 0 {
                flower_feature(level, q)
            } else {
                "grass"
            };
            place(level, feature, q, &[]);
        }
    }
}

/// How many blocks a weeping or twisting vine grows: 1, and each more with
/// chance 0.826.
fn vine_growth() -> i32 {
    let mut n = 1;
    while unit(crate::util::random_u64()) < 0.826 {
        n += 1;
    }
    n
}

/// A growing tip (kelp, weeping and twisting vines) grows on from its end.
fn grow_tip(level: &mut WorldLevel, p: Position, head: &str, body: &str, dir: i32) -> bool {
    // From a stem, up (or down) to the tip.
    let mut tip = p;
    while name_at(level, tip) == body {
        tip = at(tip, 0, dir, 0);
    }
    if name_at(level, tip) != head {
        return false;
    }
    let ctx = level.logic();
    let tip_state = level.block(to_pos(tip));
    let free = |level: &WorldLevel, q: Position| {
        if head == "kelp" {
            let s = level.block(to_pos(q));
            ctx.shapes.block_name(s) == Some("minecraft:water")
                && ctx.shapes.property(s, "level") == Some("0")
        } else {
            is_air(level, q)
        }
    };
    let mut next = at(tip, 0, dir, 0);
    if !free(level, next) {
        return false;
    }
    let mut age: i32 = ctx
        .shapes
        .property(tip_state, "age")
        .and_then(|a| a.parse().ok())
        .unwrap_or(0);
    let count = if head == "kelp" { 1 } else { vine_growth() };
    let Some(stem) = named(level, body) else {
        return false;
    };
    let mut last = tip;
    for _ in 0..count {
        if !free(level, next) {
            break;
        }
        age = (age + 1).min(25);
        level.set_block(last, stem);
        level.set_block(next, ctx.shapes.with(tip_state, "age", &age.to_string()));
        last = next;
        next = at(next, 0, dir, 0);
    }
    true
}

/// Glow lichen covers one more face: beside it on the same block, on the
/// neighbouring block along the wall, or round the corner.
fn lichen(level: &mut WorldLevel, p: Position, state: u32) -> bool {
    let ctx = level.logic();
    let faces: Vec<Dir> = Dir::ALL
        .into_iter()
        .filter(|d| ctx.shapes.property(state, d.name()) == Some("true"))
        .collect();
    if faces.is_empty() {
        return false;
    }
    let sturdy = |level: &WorldLevel, q: Position, d: Dir| {
        let s = level.block(to_pos(q));
        !ctx.shapes.is_air(s) && ctx.shapes.face_attaches(s, d.opposite())
    };
    let mut tries: Vec<(Dir, Dir)> = faces
        .iter()
        .flat_map(|&f| {
            Dir::ALL
                .into_iter()
                .filter(move |&d| d != f && d != f.opposite())
                .map(move |d| (f, d))
        })
        .collect();
    // Shuffled.
    for i in (1..tries.len()).rev() {
        let j = (crate::util::random_u64() % (i as u64 + 1)) as usize;
        tries.swap(i, j);
    }
    let water_or_air = |level: &WorldLevel, q: Position| {
        let s = level.block(to_pos(q));
        ctx.shapes.is_air(s)
            || (ctx.shapes.block_name(s) == Some("minecraft:water")
                && ctx.shapes.property(s, "level") == Some("0"))
    };
    let new_lichen = |level: &WorldLevel, q: Position, face: Dir| {
        let s = level.block(to_pos(q));
        let base = if ctx.shapes.block_name(s) == Some("minecraft:glow_lichen") {
            s
        } else {
            let mut base = named(level, "glow_lichen")?;
            for d in Dir::ALL {
                base = ctx.shapes.with(base, d.name(), "false");
            }
            let wet = ctx.shapes.block_name(s) == Some("minecraft:water");
            ctx.shapes
                .with(base, "waterlogged", if wet { "true" } else { "false" })
        };
        (ctx.shapes.property(base, face.name()) == Some("false"))
            .then(|| ctx.shapes.with(base, face.name(), "true"))
    };
    for (face, d) in tries {
        let (dx, dy, dz) = d.offset();
        let (fx, fy, fz) = face.offset();
        let candidates = [
            // The same block, toward `d`.
            (p, d),
            // The block beside, on the same wall.
            (at(p, dx, dy, dz), face),
            // Round the corner.
            (at(p, dx + fx, dy + fy, dz + fz), d.opposite()),
        ];
        for (q, f) in candidates {
            let (ox, oy, oz) = f.offset();
            if q != p && !water_or_air(level, q) && name_at(level, q) != "glow_lichen" {
                continue;
            }
            if !sturdy(level, at(q, ox, oy, oz), f) {
                continue;
            }
            if let Some(new) = new_lichen(level, q, f) {
                level.set_block(q, new);
                return true;
            }
        }
    }
    false
}

/// A big dripleaf `height` tall (stems and a leaf on top) from `p` up,
/// through air or water only.
fn dripleaf(level: &mut WorldLevel, p: Position, facing: &str, height: i32) -> bool {
    let (Some(stem), Some(leaf)) = (
        named(level, "big_dripleaf_stem"),
        named(level, "big_dripleaf"),
    ) else {
        return false;
    };
    let shapes = &level.logic().shapes;
    let free = |level: &WorldLevel, q: Position| {
        let n = name_at(level, q);
        is_air(level, q) || n == "water" || q == p
    };
    let mut top = p;
    while top.y < p.y + height - 1 && free(level, at(top, 0, 1, 0)) {
        top = at(top, 0, 1, 0);
    }
    for y in p.y..=top.y {
        let q = Position::new(p.x, y, p.z);
        let wet = name_at(level, q) == "water"
            || name_at(level, q).contains("dripleaf")
                && shapes.property(level.block(to_pos(q)), "waterlogged") == Some("true");
        let base = if y == top.y { leaf } else { stem };
        let s = shapes.with(
            shapes.with(base, "facing", facing),
            "waterlogged",
            if wet { "true" } else { "false" },
        );
        level.set_block(q, s);
    }
    true
}

/// Bone meal on a living sea pickle (minecraft.wiki, "Sea Pickle"): in the
/// 5 by 4 (at most 6 in a row) blocks around it, two layers from the one
/// below it up, each water block over a coral block has 1 chance in 6 to
/// become a sea pickle of 1 to 4; the pickle itself becomes 4. The meal is
/// used up whatever happens.
fn sea_pickle(level: &mut WorldLevel, p: Position, state: u32) -> bool {
    let ctx = level.logic();
    if ctx.shapes.property(state, "waterlogged") != Some("true") {
        return false;
    }
    let corals = level
        .context()
        .commands
        .env
        .tag("minecraft:block", "minecraft:coral_blocks");
    let r = |n: u64| crate::util::random_u64() % n;
    let (mut width, mut back) = (2, 0);
    for (step, column) in (0..5).enumerate() {
        for z in 0..width {
            for y in [p.y - 1, p.y] {
                let q = Position::new(p.x - 2 + column, y, p.z - back + z);
                if r(6) != 0 || name_at(level, q) != "water" {
                    continue;
                }
                if !corals.contains(&format!("minecraft:{}", name_at(level, at(q, 0, -1, 0)))) {
                    continue;
                }
                if let Some(base) = named(level, "sea_pickle") {
                    let pickles = (1 + r(4)).to_string();
                    level.set_block(q, ctx.shapes.with(base, "pickles", &pickles));
                }
            }
        }
        if step < 2 {
            width += 2;
            back += 1;
        } else {
            width -= 2;
            back -= 1;
        }
    }
    level.set_block(p, ctx.shapes.with(state, "pickles", "4"));
    true
}

/// Whether the block is a full cube to walk into.
fn full_block(ctx: mc_tick::Ctx, state: u32) -> bool {
    let s = ctx.shapes.support(state);
    s & mc_tick::support::TOP_FULL != 0 && s & mc_tick::support::BOTTOM_FULL != 0
}

/// Whether a plant of `state` can stay at `p`; seagrass wants a sturdy top
/// under it, anything but magma.
fn survives(level: &WorldLevel, p: Position, state: u32) -> bool {
    let ctx = level.logic();
    if ctx.shapes.block_name(state) == Some("minecraft:seagrass") {
        let below = level.block(to_pos(at(p, 0, -1, 0)));
        return ctx.shapes.support(below) & mc_tick::support::TOP_FULL != 0
            && ctx.shapes.block_name(below) != Some("minecraft:magma_block");
    }
    mc_tick::support::can_survive(level, ctx, to_pos(p), state)
}

/// A water block with all of its fluid (a source or falling water).
fn full_water(level: &WorldLevel, p: Position) -> bool {
    let ctx = level.logic();
    let state = level.block(to_pos(p));
    ctx.shapes.block_name(state) == Some("minecraft:water")
        && ctx
            .shapes
            .property(state, "level")
            .and_then(|l| l.parse::<u32>().ok())
            .is_some_and(|l| l == 0 || l >= 8)
}

/// Bone meal on a solid face under water (minecraft.wiki, "Bone Meal" →
/// "Fertilizer"): from the water block in front of `face` of `clicked`, 128
/// tries (the later, the further they wander, through water) each plant
/// seagrass in a full water block, and in warm oceans coral, coral fans
/// and, on the first try, a wall coral on a horizontal face; seagrass
/// already there is grown now and then. Says whether the meal is used.
pub fn water_plants(level: &mut WorldLevel, clicked: Position, face: Option<Dir>) -> bool {
    let ctx = level.logic();
    let origin = match face {
        Some(f) => {
            let o = f.offset();
            at(clicked, o.0, o.1, o.2)
        }
        None => clicked,
    };
    if !full_water(level, origin) {
        return false;
    }
    let r = |n: u64| (crate::util::random_u64() % n) as i32;
    let env = &level.context().commands.env;
    let underwater = env.tag("minecraft:block", "minecraft:underwater_bonemeals");
    let walls = env.tag("minecraft:block", "minecraft:wall_corals");
    let server = level.context();
    let warm_at = |q: Position| {
        server
            .world
            .biome(q, &server.data.blocks)
            .and_then(|b| server.world.biome_name(b))
            == Some("minecraft:warm_ocean")
    };
    'tries: for i in 0..128 {
        let mut q = origin;
        let mut plant = "minecraft:seagrass".to_owned();
        for _ in 0..i / 16 {
            q = at(q, r(3) - 1, (r(3) - 1) * r(3) / 2, r(3) - 1);
            if full_block(ctx, level.block(to_pos(q))) {
                continue 'tries;
            }
        }
        let mut facing = None;
        if warm_at(q) {
            if i == 0 && face.is_some_and(Dir::is_horizontal) {
                if !walls.is_empty() {
                    plant = walls[r(walls.len() as u64) as usize].clone();
                    facing = face;
                }
            } else if r(4) == 0 && !underwater.is_empty() {
                plant = underwater[r(underwater.len() as u64) as usize].clone();
                facing = Some(Dir::HORIZONTAL[r(4) as usize]);
            }
        }
        let Some(mut state) = ctx.shapes.default_state(&plant) else {
            continue;
        };
        // A wall fan turns until it has something to hang on.
        if ctx.shapes.property(state, "facing").is_some() {
            let mut dir = facing.unwrap_or(Dir::North);
            for _ in 0..5 {
                state = ctx.shapes.with(state, "facing", dir.name());
                if survives(level, q, state) {
                    break;
                }
                dir = Dir::HORIZONTAL[r(4) as usize];
            }
        }
        if !survives(level, q, state) {
            continue;
        }
        if full_water(level, q) {
            level.set_block(q, state);
        } else if name_at(level, q) == "seagrass" && r(10) == 0 {
            let here = level.block(to_pos(q));
            let _ = bone_meal(level, q, here, "seagrass", crate::util::random_u64());
        }
    }
    true
}

/// Bone meal on the block `state` named `name` at `p`; `None` if this
/// module does not know the block.
pub fn bone_meal(
    level: &mut WorldLevel,
    p: Position,
    state: u32,
    name: &str,
    roll: u64,
) -> Option<bool> {
    let ctx = level.logic();
    let up = at(p, 0, 1, 0);
    let prop = |k: &str| ctx.shapes.property(state, k).map(str::to_owned);
    Some(match name {
        "grass_block" => {
            if !is_air(level, up) {
                return Some(false);
            }
            grass(level, p);
            true
        }
        "moss_block" | "pale_moss_block" => {
            if !is_air(level, up) {
                return Some(false);
            }
            let feature = format!("{}_patch_bonemeal", name.trim_end_matches("_block"));
            place(level, &feature, up, &[]);
            true
        }
        "crimson_nylium" | "warped_nylium" => {
            if !is_air(level, up) {
                return Some(false);
            }
            place(level, "nylium_bonemeal", up, &[]);
            true
        }
        "short_grass" => tall(level, p, "tall_grass"),
        "fern" => tall(level, p, "large_fern"),
        "short_dry_grass" => {
            if let Some(s) = named(level, "tall_dry_grass") {
                level.set_block(p, s);
            }
            true
        }
        "sunflower" | "lilac" | "rose_bush" | "peony" => {
            drop_copy(level, p, name);
            true
        }
        "pink_petals" | "wildflowers" | "leaf_litter" => {
            let key = if name == "leaf_litter" {
                "segment_amount"
            } else {
                "flower_amount"
            };
            let n: i32 = prop(key).and_then(|v| v.parse().ok()).unwrap_or(4);
            if n < 4 {
                level.set_block(p, ctx.shapes.with(state, key, &(n + 1).to_string()));
            } else {
                drop_copy(level, p, name);
            }
            true
        }
        "brown_mushroom" | "red_mushroom" => {
            if unit(roll) < 0.4 {
                let air = ctx.shapes.air();
                level.set_block(p, air);
                if !place(level, &format!("huge_{name}"), p, &[p]) {
                    level.set_block(p, state);
                }
            }
            true
        }
        "crimson_fungus" | "warped_fungus" => {
            let nylium = format!("{}_nylium", name.trim_end_matches("_fungus"));
            if name_at(level, at(p, 0, -1, 0)) != nylium {
                return Some(false);
            }
            if unit(roll) < 0.4 {
                place(level, &format!("{name}_planted"), p, &[p]);
            }
            true
        }
        "azalea" | "flowering_azalea" => {
            if name_at(level, up) == "water" {
                return Some(false);
            }
            if unit(roll) < 0.45 {
                super::grow_tree(level, to_pos(p), state);
            }
            true
        }
        "cave_vines" | "cave_vines_plant" => {
            if prop("berries").as_deref() == Some("true") {
                return Some(false);
            }
            level.set_block(p, ctx.shapes.with(state, "berries", "true"));
            true
        }
        "kelp" | "kelp_plant" => grow_tip(level, p, "kelp", "kelp_plant", 1),
        "weeping_vines" | "weeping_vines_plant" => {
            grow_tip(level, p, "weeping_vines", "weeping_vines_plant", -1)
        }
        "twisting_vines" | "twisting_vines_plant" => {
            grow_tip(level, p, "twisting_vines", "twisting_vines_plant", 1)
        }
        "pale_hanging_moss" => {
            let down = at(p, 0, -1, 0);
            if !is_air(level, down) {
                return Some(false);
            }
            level.set_block(down, ctx.shapes.with(state, "tip", "true"));
            level.set_block(p, ctx.shapes.with(state, "tip", "false"));
            true
        }
        "glow_lichen" => lichen(level, p, state),
        "rooted_dirt" => {
            let down = at(p, 0, -1, 0);
            if !is_air(level, down) {
                return Some(false);
            }
            if let Some(roots) = named(level, "hanging_roots") {
                level.set_block(down, roots);
            }
            true
        }
        "seagrass" => {
            if name_at(level, up) != "water" {
                return Some(false);
            }
            let Some(base) = named(level, "tall_seagrass") else {
                return Some(false);
            };
            let (lower, upper) = (
                ctx.shapes.with(base, "half", "lower"),
                ctx.shapes.with(base, "half", "upper"),
            );
            mc_tick::set_block(level, ctx, to_pos(p), lower, mc_tick::Flags::NONE);
            mc_tick::set_block(level, ctx, to_pos(up), upper, mc_tick::Flags::NONE);
            true
        }
        "big_dripleaf" => {
            let facing = prop("facing").unwrap_or_else(|| "north".into());
            if !(is_air(level, up) || name_at(level, up) == "water") {
                return Some(false);
            }
            dripleaf(level, p, &facing, 2)
        }
        "small_dripleaf" => {
            let facing = prop("facing").unwrap_or_else(|| "north".into());
            let base = if prop("half").as_deref() == Some("upper") {
                at(p, 0, -1, 0)
            } else {
                p
            };
            let air = ctx.shapes.air();
            level.set_block(at(base, 0, 1, 0), air);
            dripleaf(level, base, &facing, 2 + (roll % 4) as i32)
        }
        "sea_pickle" => sea_pickle(level, p, state),
        "pitcher_crop" => {
            // Either half takes the meal for the crop: one stage, if it
            // is light enough and the upper half has room.
            let base = if prop("half").as_deref() == Some("upper") {
                at(p, 0, -1, 0)
            } else {
                p
            };
            let lower = level.block(to_pos(base));
            if name_at(level, base) != "pitcher_crop"
                || ctx.shapes.property(lower, "age") == Some("4")
            {
                return Some(false);
            }
            mc_tick::random::pitcher_grow(level, ctx, to_pos(base), lower, 1);
            true
        }
        "bamboo_sapling" => {
            if !is_air(level, up) {
                return Some(false);
            }
            mc_tick::random::grow_shoot(level, ctx, to_pos(p));
            true
        }
        "bamboo" => {
            let above = mc_tick::random::bamboo_above(level, ctx, to_pos(p)) as i32;
            let below = mc_tick::random::bamboo_below(level, ctx, to_pos(p)) as i32;
            let total = above + below + 1;
            let top = at(p, 0, above, 0);
            if total >= 16 || ctx.shapes.property(level.block(to_pos(top)), "stage") == Some("1") {
                return Some(false);
            }
            let mut rng = mc_tick::rng::Rng::new(roll);
            for i in 0..1 + (roll % 2) as i32 {
                let (above, total) = (above + i, total + i);
                let top = at(p, 0, above, 0);
                let top_state = level.block(to_pos(top));
                if total >= 16
                    || ctx.shapes.property(top_state, "stage") == Some("1")
                    || !is_air(level, at(top, 0, 1, 0))
                {
                    break;
                }
                mc_tick::random::grow_bamboo(
                    level,
                    ctx,
                    to_pos(top),
                    top_state,
                    total as u32,
                    &mut rng,
                );
            }
            true
        }
        _ => return None,
    })
}
