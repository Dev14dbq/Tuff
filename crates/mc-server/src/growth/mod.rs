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

/// A roll as a number in 0..1.
fn unit(roll: u64) -> f64 {
    (roll >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> Option<crate::context::ServerContext> {
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
}
