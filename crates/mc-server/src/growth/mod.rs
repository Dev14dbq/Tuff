//! Saplings growing into trees (minecraft.wiki, "Sapling" → "Growth").
//!
//! The tree is a worldgen feature placed as `/place feature` places it. The
//! sapling is cleared first and comes back if the feature does not fit.
//! Spruce, jungle, dark oak and pale oak grow huge from four saplings in a
//! 2×2 square; dark oak and pale oak only grow that way. Oak, birch and cherry
//! use the variant with a bee nest when flowers are within 2 blocks.

use mc_protocol::Position;
use mc_tick::{BlockPos, Level as _};

use crate::level::{WorldLevel, to_position};

mod more;

/// What a sapling grows into.
struct Species {
    /// Features for the single sapling; empty if it cannot grow alone.
    single: &'static [&'static str],
    /// Features for four saplings in a square.
    mega: &'static [&'static str],
}

fn species(name: &str) -> Option<Species> {
    Some(match name {
        "oak_sapling" => Species {
            single: &["oak", "fancy_oak"],
            mega: &[],
        },
        "birch_sapling" => Species {
            single: &["birch"],
            mega: &[],
        },
        "spruce_sapling" => Species {
            single: &["spruce"],
            mega: &["mega_spruce", "mega_pine"],
        },
        "jungle_sapling" => Species {
            single: &["jungle_tree_no_vine"],
            mega: &["mega_jungle_tree"],
        },
        "acacia_sapling" => Species {
            single: &["acacia"],
            mega: &[],
        },
        "dark_oak_sapling" => Species {
            single: &[],
            mega: &["dark_oak"],
        },
        "pale_oak_sapling" => Species {
            single: &[],
            mega: &["pale_oak_bonemeal"],
        },
        "cherry_sapling" => Species {
            single: &["cherry"],
            mega: &[],
        },
        "azalea" | "flowering_azalea" => Species {
            single: &["azalea_tree"],
            mega: &[],
        },
        "mangrove_propagule" => Species {
            single: &["mangrove", "tall_mangrove"],
            mega: &[],
        },
        _ => return None,
    })
}

fn has_flowers(level: &WorldLevel, ctx: mc_tick::Ctx, p: BlockPos) -> bool {
    for dx in -2..=2 {
        for dy in -1..=1 {
            for dz in -2..=2 {
                let s = level.block(BlockPos::new(p.x + dx, p.y + dy, p.z + dz));
                if ctx.shapes.in_tag(s, "minecraft:flowers") {
                    return true;
                }
            }
        }
    }
    false
}

/// The feature a single sapling picks: oak is fancy one time in ten, and
/// oak, birch and cherry have bee-nest versions near flowers.
fn pick(name: &str, options: &[&'static str], flowers: bool, roll: u32) -> String {
    let base = match name {
        "oak_sapling" => options[usize::from(roll.is_multiple_of(10))],
        // Mostly the tall one.
        "mangrove_propagule" => options[usize::from(roll % 100 < 85)],
        _ => options[(roll as usize) % options.len()],
    };
    let bees = flowers && matches!(name, "oak_sapling" | "birch_sapling" | "cherry_sapling");
    if bees {
        format!("minecraft:{base}_bees_005")
    } else {
        format!("minecraft:{base}")
    }
}

/// A random tick grows the sapling `state` at `p`. Returns whether a tree
/// stands there now.
pub fn grow_tree(level: &mut WorldLevel, p: BlockPos, state: u32) -> bool {
    let ctx = level.logic();
    let Some(name) = ctx
        .shapes
        .block_name(state)
        .map(|n| n.trim_start_matches("minecraft:").to_owned())
    else {
        return false;
    };
    let Some(species) = species(&name) else {
        return false;
    };
    let roll = (crate::util::random_u64() >> 32) as u32;
    // Four saplings of one kind around a corner of this one.
    let same = |level: &WorldLevel, x: i32, z: i32| {
        ctx.shapes
            .block_index(level.block(BlockPos::new(x, p.y, z)))
            == ctx.shapes.block_index(state)
    };
    let square = (!species.mega.is_empty())
        .then(|| {
            (-1..=0)
                .flat_map(|dx| (-1..=0).map(move |dz| (dx, dz)))
                .find(|&(dx, dz)| {
                    (0..2).all(|i| (0..2).all(|j| same(level, p.x + dx + i, p.z + dz + j)))
                })
        })
        .flatten();
    let (id, origin, cleared) = if let Some((dx, dz)) = square {
        let corner = Position::new(p.x + dx, p.y, p.z + dz);
        let cleared = (0..2)
            .flat_map(|i| (0..2).map(move |j| Position::new(corner.x + i, p.y, corner.z + j)))
            .collect::<Vec<_>>();
        let id = format!(
            "minecraft:{}",
            species.mega[(roll as usize) % species.mega.len()]
        );
        (id, corner, cleared)
    } else if species.single.is_empty() {
        return false;
    } else {
        let flowers = has_flowers(level, ctx, p);
        (
            pick(&name, species.single, flowers, roll),
            to_position(p),
            vec![to_position(p)],
        )
    };
    let Some((changes, entities)) =
        crate::commands::place::place_configured(level.context(), &id, origin, &cleared)
    else {
        return false;
    };
    for (at, new) in changes {
        let new =
            mc_tick::placement_state(level, ctx, mc_tick::BlockPos::new(at.x, at.y, at.z), new);
        if level.set_block(at, new) {
            crate::containers::placed(level.context(), at, new);
        }
    }
    for (at, nbt) in entities {
        if let Some(s) = level
            .context()
            .world
            .block(at, &level.context().data.blocks)
        {
            crate::commands::world::put_block_data(level.context(), at, s, nbt);
        }
    }
    true
}

/// The highest `age` of the crops bone meal speeds up, and how much it adds.
fn crop_age(name: &str) -> Option<(i32, bool)> {
    Some(match name {
        "wheat" | "carrots" | "potatoes" | "melon_stem" | "pumpkin_stem" => (7, true),
        "beetroots" | "sweet_berry_bush" => (3, false),
        "cocoa" => (2, false),
        "torchflower_crop" => (1, false),
        _ => return None,
    })
}

/// Bone meal on the block at `p` (minecraft.wiki, "Bone Meal"): crops and
/// stems age by two to five stages (other plants by one), saplings grow
/// 45% of the time. Returns whether the block could use it, so whether the
/// meal is used up.
pub fn bone_meal(level: &mut WorldLevel, p: Position) -> bool {
    let ctx = level.logic();
    let state = level.block(BlockPos::new(p.x, p.y, p.z));
    let Some(name) = ctx
        .shapes
        .block_name(state)
        .map(|n| n.trim_start_matches("minecraft:").to_owned())
    else {
        return false;
    };
    let roll = crate::util::random_u64();
    if name.ends_with("_sapling") || name == "mangrove_propagule" {
        if ctx.shapes.property(state, "stage").is_none() {
            return false;
        }
        if ctx.shapes.property(state, "hanging") == Some("true") {
            let age: i32 = ctx
                .shapes
                .property(state, "age")
                .and_then(|a| a.parse().ok())
                .unwrap_or(4);
            if age >= 4 {
                return false;
            }
            level.set_block(p, ctx.shapes.with(state, "age", &(age + 1).to_string()));
            return true;
        }
        if unit(roll) >= 0.45 {
            return true;
        }
        if ctx.shapes.property(state, "stage") == Some("0") {
            level.set_block(p, ctx.shapes.with(state, "stage", "1"));
        } else {
            grow_tree(level, BlockPos::new(p.x, p.y, p.z), state);
        }
        return true;
    }
    if let Some(used) = more::bone_meal(level, p, state, &name, roll) {
        return used;
    }
    let Some((max, wide)) = crop_age(&name) else {
        return false;
    };
    let age: i32 = ctx
        .shapes
        .property(state, "age")
        .and_then(|a| a.parse().ok())
        .unwrap_or(max);
    if age >= max {
        return false;
    }
    let add = if wide { 2 + (roll % 4) as i32 } else { 1 };
    let new = (age + add).min(max);
    level.set_block(p, ctx.shapes.with(state, "age", &new.to_string()));
    true
}

/// Bone meal on the `face` (if any) of a solid block under water: the
/// water in front of it grows seagrass and, in warm oceans, corals.
pub fn bone_meal_water(
    level: &mut WorldLevel,
    clicked: Position,
    face: Option<mc_tick::Dir>,
) -> bool {
    more::water_plants(level, clicked, face)
}

/// A roll as a number in 0..1.
fn unit(roll: u64) -> f64 {
    (roll >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> Option<std::sync::Arc<crate::context::ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        crate::context::ServerContext::new(crate::Config::new(dir), data).ok()
    }

    #[test]
    fn a_ripe_sapling_becomes_a_tree() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let sapling = ctx
            .data
            .blocks
            .state_id("minecraft:oak_sapling", &[("stage", "1")])
            .unwrap();
        let at = Position::new(0, -60, 0);
        let mut level = WorldLevel::new(&ctx);
        level.set_block(at, sapling);
        assert!(grow_tree(
            &mut level,
            mc_tick::BlockPos::new(0, -60, 0),
            sapling
        ));
        drop(level);
        let name = |p| {
            ctx.world
                .block(p, &ctx.data.blocks)
                .and_then(|s| ctx.data.blocks.block_name(s))
                .map(str::to_owned)
        };
        assert_eq!(name(at).as_deref(), Some("minecraft:oak_log"));
        assert!(
            name(Position::new(0, -57, 0))
                .is_some_and(|n| n.ends_with("_log") || n.ends_with("_leaves"))
        );
    }

    #[test]
    fn random_ticks_advance_the_stage_then_grow() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, -60, 0);
        let sapling = ctx
            .data
            .blocks
            .state_id("minecraft:birch_sapling", &[("stage", "0")])
            .unwrap();
        let mut level = WorldLevel::new(&ctx);
        level.set_block(at, sapling);
        let logic = level.logic();
        let mut rng = mc_tick::rng::Rng::new(1);
        let mut stage1 = None;
        for _ in 0..500 {
            let state = mc_tick::Level::block(&level, mc_tick::BlockPos::new(0, -60, 0));
            if ctx.data.blocks.property(state, "stage") == Some("1") && stage1.is_none() {
                stage1 = Some(state);
            }
            mc_tick::random::tick(
                &mut level,
                logic,
                mc_tick::BlockPos::new(0, -60, 0),
                state,
                &mut rng,
            );
            if ctx.data.blocks.block_name(mc_tick::Level::block(
                &level,
                mc_tick::BlockPos::new(0, -60, 0),
            )) == Some("minecraft:birch_log")
            {
                break;
            }
        }
        assert!(stage1.is_some(), "never reached stage 1");
        let now = mc_tick::Level::block(&level, mc_tick::BlockPos::new(0, -60, 0));
        assert_eq!(ctx.data.blocks.block_name(now), Some("minecraft:birch_log"));
    }

    fn put(level: &mut WorldLevel, p: [i32; 3], name: &str, props: &[(&str, &str)]) {
        let state = level.context().data.blocks.state_id(name, props).unwrap();
        level.set_block(Position::new(p[0], p[1], p[2]), state);
    }

    fn look(level: &WorldLevel, p: [i32; 3]) -> String {
        let s = mc_tick::Level::block(level, mc_tick::BlockPos::new(p[0], p[1], p[2]));
        level
            .context()
            .data
            .blocks
            .describe(s)
            .unwrap()
            .trim_end_matches("[]")
            .to_owned()
    }

    fn random_tick(level: &mut WorldLevel, p: [i32; 3], rng: &mut mc_tick::rng::Rng) {
        let logic = level.logic();
        let at = mc_tick::BlockPos::new(p[0], p[1], p[2]);
        let state = mc_tick::Level::block(level, at);
        mc_tick::random::tick(level, logic, at, state, rng);
    }

    #[test]
    fn a_pitcher_crop_grows_to_two_blocks_by_random_ticks_and_bone_meal() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut level = WorldLevel::new(&ctx);
        put(
            &mut level,
            [0, -61, 0],
            "minecraft:farmland",
            &[("moisture", "7")],
        );
        fn crop<'a>(age: &'a str, half: &'a str) -> [(&'a str, &'a str); 2] {
            [("age", age), ("half", half)]
        }
        put(
            &mut level,
            [0, -60, 0],
            "minecraft:pitcher_crop",
            &crop("0", "lower"),
        );
        let mut rng = mc_tick::rng::Rng::new(9);
        let mut seen = vec![look(&level, [0, -60, 0])];
        for _ in 0..4000 {
            random_tick(&mut level, [0, -60, 0], &mut rng);
            let now = look(&level, [0, -60, 0]);
            if seen.last() != Some(&now) {
                seen.push(now);
            }
        }
        // Each stage in turn; from the fourth the upper half is there.
        assert_eq!(seen.len(), 5, "{seen:?}");
        assert!(seen[4].contains("age=4") && seen[4].contains("half=lower"));
        assert!(look(&level, [0, -59, 0]).contains("age=4"));
        assert!(look(&level, [0, -59, 0]).contains("half=upper"));
        // The upper half does not tick, and the full crop stays.
        random_tick(&mut level, [0, -59, 0], &mut rng);
        assert!(look(&level, [0, -59, 0]).contains("half=upper"));

        // Bone meal: one stage; blocked above, the third stage waits.
        put(
            &mut level,
            [4, -61, 0],
            "minecraft:farmland",
            &[("moisture", "7")],
        );
        put(
            &mut level,
            [4, -60, 0],
            "minecraft:pitcher_crop",
            &crop("1", "lower"),
        );
        put(&mut level, [4, -58, 0], "minecraft:stone", &[]);
        assert!(bone_meal(&mut level, Position::new(4, -60, 0)));
        assert!(look(&level, [4, -60, 0]).contains("age=2"));
        put(&mut level, [4, -59, 0], "minecraft:stone", &[]);
        assert!(bone_meal(&mut level, Position::new(4, -60, 0)));
        assert!(look(&level, [4, -60, 0]).contains("age=2"));
        put(&mut level, [4, -59, 0], "minecraft:air", &[]);
        assert!(bone_meal(&mut level, Position::new(4, -60, 0)));
        assert!(look(&level, [4, -60, 0]).contains("age=3"));
        assert!(look(&level, [4, -59, 0]).contains("half=upper"));
        // From the upper half too, up to the end.
        assert!(bone_meal(&mut level, Position::new(4, -59, 0)));
        assert!(look(&level, [4, -60, 0]).contains("age=4"));
        assert!(!bone_meal(&mut level, Position::new(4, -60, 0)));
    }

    #[test]
    fn pointed_dripstone_grows_down_and_up_under_a_water_source() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut level = WorldLevel::new(&ctx);
        let tip = [("vertical_direction", "down"), ("thickness", "tip")];
        put(
            &mut level,
            [10, -47, 0],
            "minecraft:water",
            &[("level", "0")],
        );
        put(&mut level, [10, -48, 0], "minecraft:dripstone_block", &[]);
        put(
            &mut level,
            [10, -49, 0],
            "minecraft:pointed_dripstone",
            &tip,
        );
        // The floor is five blocks under the tip.
        put(&mut level, [10, -55, 0], "minecraft:stone", &[]);
        let mut rng = mc_tick::rng::Rng::new(4);
        for _ in 0..40_000 {
            random_tick(&mut level, [10, -49, 0], &mut rng);
        }
        // Down from the top: pieces of one stalactite, then they meet the
        // stalagmite growing from the floor and merge.
        let column: Vec<String> = (-54..=-49)
            .rev()
            .map(|y| look(&level, [10, y, 0]))
            .collect();
        assert!(
            column[0].contains("vertical_direction=down") && !column[0].contains("thickness=tip"),
            "{column:?}"
        );
        assert!(column.iter().any(|s| s.contains("thickness=tip_merge")));
        let merged: Vec<_> = column.iter().filter(|s| s.contains("tip_merge")).collect();
        assert_eq!(merged.len(), 2, "{column:?}");
        assert!(look(&level, [10, -54, 0]).contains("vertical_direction=up"));
        // Without a water source it does not grow.
        let mut dry = WorldLevel::new(&ctx);
        put(&mut dry, [20, -48, 0], "minecraft:dripstone_block", &[]);
        put(&mut dry, [20, -49, 0], "minecraft:pointed_dripstone", &tip);
        for _ in 0..40_000 {
            random_tick(&mut dry, [20, -49, 0], &mut rng);
        }
        assert!(look(&dry, [20, -50, 0]).ends_with("air"));
    }

    /// Water over `x0..=x1`, `y0..=y1`, `z0..=z1`.
    fn flood(level: &mut WorldLevel, x: (i32, i32), y: (i32, i32), z: (i32, i32)) {
        for x in x.0..=x.1 {
            for y in y.0..=y.1 {
                for z in z.0..=z.1 {
                    put(level, [x, y, z], "minecraft:water", &[("level", "0")]);
                }
            }
        }
    }

    #[test]
    fn bone_meal_spreads_sea_pickles_over_coral_blocks() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut level = WorldLevel::new(&ctx);
        flood(&mut level, (-6, 6), (-60, -58), (-6, 6));
        for x in -6..=6 {
            for z in -6..=6 {
                put(&mut level, [x, -61, z], "minecraft:tube_coral_block", &[]);
            }
        }
        let pickle = |pickles: &str, wet: &str| {
            [
                ("pickles", pickles.to_owned()),
                ("waterlogged", wet.to_owned()),
            ]
        };
        let set_pickle = |level: &mut WorldLevel, pickles: &str, wet: &str| {
            let props = pickle(pickles, wet);
            let props: Vec<(&str, &str)> = props.iter().map(|(k, v)| (*k, v.as_str())).collect();
            put(level, [0, -60, 0], "minecraft:sea_pickle", &props);
        };
        // A dead one (out of the water) does not take the meal.
        set_pickle(&mut level, "1", "false");
        assert!(!bone_meal(&mut level, Position::new(0, -60, 0)));
        set_pickle(&mut level, "1", "true");
        for _ in 0..20 {
            assert!(bone_meal(&mut level, Position::new(0, -60, 0)));
        }
        assert!(look(&level, [0, -60, 0]).contains("pickles=4"));
        let mut near = 0;
        for x in -6..=6 {
            for z in -6..=6 {
                for y in [-60, -59, -58] {
                    if look(&level, [x, y, z]).contains("sea_pickle") && (x, y, z) != (0, -60, 0) {
                        near += 1;
                        // Within 2 blocks along x, 3 along z, on the layer
                        // of the pickle (and the one under it, coral).
                        assert!(
                            x.abs() <= 2 && (-2..=3).contains(&z) && y == -60,
                            "{x} {y} {z}"
                        );
                    }
                }
            }
        }
        assert!(near >= 3, "{near}");
        // Over a plain floor there is nothing to grow on.
        let mut bare = WorldLevel::new(&ctx);
        flood(&mut bare, (20, 26), (-60, -58), (-3, 3));
        put(
            &mut bare,
            [23, -60, 0],
            "minecraft:sea_pickle",
            &[("pickles", "1"), ("waterlogged", "true")],
        );
        assert!(bone_meal(&mut bare, Position::new(23, -60, 0)));
        assert!(!look(&bare, [22, -60, 0]).contains("sea_pickle"));
    }

    #[test]
    fn bone_meal_on_a_floor_under_water_plants_seagrass_and_in_warm_oceans_coral() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut level = WorldLevel::new(&ctx);
        flood(&mut level, (-4, 4), (-60, -57), (-4, 4));
        // The first try plants right in front of the face clicked.
        assert!(bone_meal_water(
            &mut level,
            Position::new(0, -61, 0),
            Some(mc_tick::Dir::Up)
        ));
        assert!(look(&level, [0, -60, 0]).contains("seagrass"));
        let count = |level: &WorldLevel, what: &str| {
            let mut n = 0;
            for x in -4..=4 {
                for z in -4..=4 {
                    for y in -60..=-57 {
                        if look(level, [x, y, z]).contains(what) {
                            n += 1;
                        }
                    }
                }
            }
            n
        };
        assert!(count(&level, "seagrass") >= 2);
        assert_eq!(count(&level, "coral"), 0);
        // Not into plain air or a block without water in front.
        assert!(!bone_meal_water(
            &mut level,
            Position::new(30, -61, 0),
            Some(mc_tick::Dir::Up)
        ));
        // Warm ocean: corals too.
        let Some(warm) = ctx.world.biome_id("minecraft:warm_ocean") else {
            eprintln!("no warm ocean in this world");
            return;
        };
        ctx.world.fill_biome(
            (-1, -1),
            [-4, -16, -4],
            [-1, 8, -1],
            warm,
            &|_| true,
            &ctx.data.blocks,
        );
        ctx.world.fill_biome(
            (0, 0),
            [0, -16, 0],
            [3, 8, 3],
            warm,
            &|_| true,
            &ctx.data.blocks,
        );
        for x in 1..=3 {
            bone_meal_water(&mut level, Position::new(x, -61, 1), Some(mc_tick::Dir::Up));
        }
        assert!(count(&level, "coral") > 0);
    }
}
