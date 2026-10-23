//! Monster spawners, after minecraft.wiki ("Monster Spawner"): with a player
//! within 16 blocks, every 10 to 40 seconds a spawner puts up to four of its
//! kind within four blocks sideways and one up or down, unless six of them
//! are already around it.

use mc_entity::mob::{Category, Target};
use mc_entity::nav::{PathParams, standable};
use mc_entity::{Entity, Kind, Species};
use mc_nbt::Tag;
use mc_protocol::Position;

use super::{Ground, new_mob};
use crate::context::ServerContext;
use crate::fx;
use crate::tick::lock;

const PLAYER_RANGE: f64 = 16.0;
const SPAWN_RANGE: i32 = 4;
const SPAWN_COUNT: u32 = 4;
const MAX_NEARBY: usize = 6;
const MIN_DELAY: i32 = 200;
const MAX_DELAY: i32 = 800;

fn below(n: u32) -> u32 {
    (crate::util::random_u64() >> 33) as u32 % n.max(1)
}

fn short(data: &mc_nbt::Compound, key: &str) -> Option<i32> {
    match data.get(key)? {
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Int(v) => Some(*v),
        _ => None,
    }
}

/// The kind a spawner makes (`SpawnData.entity.id`).
pub fn kind_of(data: &mc_nbt::Compound) -> Option<Species> {
    let Some(Tag::Compound(spawn)) = data.get("SpawnData") else {
        return None;
    };
    let Some(Tag::Compound(entity)) = spawn.get("entity") else {
        return None;
    };
    match entity.get("id")? {
        Tag::String(id) => Species::from_id(&mc_command::args::full_id(id)),
        _ => None,
    }
}

/// What a spawner makes this round: one of its `SpawnPotentials` by weight
/// when it has any, else its `SpawnData`.
fn choose(data: &mc_nbt::Compound) -> Option<Species> {
    if let Some(Tag::List(list)) = data.get("SpawnPotentials") {
        let entries: Vec<(u32, Species)> = list
            .iter()
            .filter_map(|e| {
                let Tag::Compound(e) = e else { return None };
                let weight = short(e, "weight").unwrap_or(1).max(0) as u32;
                let Some(Tag::Compound(d)) = e.get("data") else {
                    return None;
                };
                let wrapped = {
                    let mut c = mc_nbt::Compound::new();
                    c.insert("SpawnData", Tag::Compound(d.clone()));
                    c
                };
                Some((weight, kind_of(&wrapped)?))
            })
            .collect();
        let total: u32 = entries.iter().map(|e| e.0).sum();
        if total > 0 {
            let mut roll = below(total);
            for (w, s) in entries {
                if roll < w {
                    return Some(s);
                }
                roll -= w;
            }
        }
    }
    kind_of(data)
}

/// Runs the spawners near players; returns the mobs they made.
pub fn tick(
    ctx: &ServerContext,
    ground: &Ground,
    entities: &[Entity],
    targets: &[Target],
    difficulty: i32,
) -> Vec<Entity> {
    let mut out = Vec::new();
    // The game rule `spawner_blocks_work` switches all of them off.
    if !ctx.game_rule_bool("spawner_blocks_work") {
        return out;
    }
    let spawners = ctx.world.block_entities_of("minecraft:mob_spawner");
    let mut delays = lock(&ctx.mobs.spawner_delays);
    delays.retain(|&(dim, x, y, z), _| {
        dim != ctx.dim || spawners.iter().any(|b| (b.x, b.y, b.z) == (x, y, z))
    });
    for be in spawners {
        let center = [
            f64::from(be.x) + 0.5,
            f64::from(be.y) + 0.5,
            f64::from(be.z) + 0.5,
        ];
        let reach = f64::from(short(&be.data, "RequiredPlayerRange").unwrap_or(PLAYER_RANGE as i32));
        let near = targets.iter().any(|t| {
            t.targetable
                && (0..3)
                    .map(|i| (t.pos[i] - center[i]).powi(2))
                    .sum::<f64>()
                    < reach * reach
        });
        if !near {
            continue;
        }
        let Some(species) = choose(&be.data) else {
            continue;
        };
        if species.category() == Category::Monster && difficulty == 0 {
            continue;
        }
        let key = (ctx.dim, be.x, be.y, be.z);
        let delay = delays
            .entry(key)
            .or_insert_with(|| short(&be.data, "Delay").unwrap_or(20));
        if *delay > 0 {
            *delay -= 1;
            continue;
        }
        let min = short(&be.data, "MinSpawnDelay").unwrap_or(MIN_DELAY);
        let max = short(&be.data, "MaxSpawnDelay").unwrap_or(MAX_DELAY);
        *delay = min + below((max - min).max(1) as u32) as i32;
        let range = short(&be.data, "SpawnRange").unwrap_or(SPAWN_RANGE);
        let count = short(&be.data, "SpawnCount").map_or(SPAWN_COUNT, |c| c.max(0) as u32);
        let max_nearby = short(&be.data, "MaxNearbyEntities").map_or(MAX_NEARBY, |c| c.max(0) as usize);
        let around = |e: &Entity| {
            matches!(&e.kind, Kind::Mob(m) if m.species == species)
                && (e.pos[0] - center[0]).abs() <= f64::from(range) + 0.5
                && (e.pos[1] - center[1]).abs() <= 4.5
                && (e.pos[2] - center[2]).abs() <= f64::from(range) + 0.5
        };
        let mut nearby = entities.iter().filter(|e| around(e)).count();
        let (w, h) = species.size();
        let params = PathParams::walker(h.ceil() as i32);
        let mut made = false;
        for _ in 0..count {
            if nearby >= max_nearby {
                break;
            }
            let x = be.x + below(2 * range as u32 + 1) as i32 - range;
            let y = be.y + below(3) as i32 - 1;
            let z = be.z + below(2 * range as u32 + 1) as i32 - range;
            let fits = species.flies() || standable(ground, [x, y, z], &params);
            let dark = species.category() != Category::Monster
                || ctx.world.block_light(Position::new(x, y, z)) <= 11;
            if !fits || !dark || w > 2.0 {
                continue;
            }
            let pos = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
            let mut e = new_mob(ctx, species, pos);
            if let Kind::Mob(m) = &mut e.kind {
                m.yaw = below(360) as f32;
            }
            fx::level_event(ctx, 2004, pos, 0);
            out.push(e);
            nearby += 1;
            made = true;
        }
        if made {
            fx::level_event(ctx, 2004, center, 0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, weight: i16) -> Tag {
        let mut entity = mc_nbt::Compound::new();
        entity.insert("id", Tag::from(id));
        let mut data = mc_nbt::Compound::new();
        data.insert("entity", Tag::Compound(entity));
        let mut e = mc_nbt::Compound::new();
        e.insert("weight", Tag::Short(weight));
        e.insert("data", Tag::Compound(data));
        Tag::Compound(e)
    }

    #[test]
    fn potentials_are_picked_by_weight() {
        let mut d = mc_nbt::Compound::new();
        d.insert(
            "SpawnPotentials",
            Tag::List(vec![entry("minecraft:zombie", 0), entry("minecraft:skeleton", 5)]),
        );
        for _ in 0..20 {
            assert_eq!(choose(&d), Species::from_id("minecraft:skeleton"));
        }
        assert_eq!(choose(&mc_nbt::Compound::new()), None);
    }
}
