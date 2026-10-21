//! Natural spawning and despawning, after the public description of vanilla's
//! (minecraft.wiki, "Spawn" and "Mob spawning"): each category has a cap
//! that grows with the chunks players keep loaded around them; monsters are
//! tried every tick in the dark, animals when a chunk first comes in range
//! and every 400 ticks after that; nothing spawns within 24 blocks of a
//! player, and monsters far from every player go away. What spawns where is
//! read from the biomes of the data pack.

use std::collections::{HashMap, HashSet};

use mc_entity::Collide as _;
use mc_entity::mob::{Category, Target};
use mc_entity::nav::{Liquid, Terrain};
use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;
use serde_json::Value;

use super::{Ground, new_mob, sky_darken};
use crate::context::ServerContext;
use crate::tick::lock;

/// Chunks around a player that take part (the simulation distance, capped).
const RANGE: i32 = 8;
/// Monsters per 289 chunks (17 × 17 around one player), animals likewise.
const MONSTER_CAP: i32 = 70;
const CREATURE_CAP: i32 = 10;
/// Chunks tried for monsters each tick (vanilla tries all of them).
const MONSTER_TRIES: usize = 3;
/// Ticks between looks at which chunks are in range.
const RANGE_PERIOD: i64 = 20;
/// How far above a player the ground is looked for.
const SCAN_UP: i32 = 48;
/// Ticks between rounds of animal spawning.
const ANIMAL_PERIOD: i64 = 400;
/// Closest to a player that anything spawns, in blocks.
const MIN_PLAYER_DISTANCE: f64 = 24.0;

#[derive(Clone, Debug)]
struct Entry {
    species: Species,
    weight: u32,
    min: u32,
    max: u32,
}

#[derive(Clone, Debug, Default)]
struct BiomeSpawns {
    creature: Vec<Entry>,
    monster: Vec<Entry>,
    /// Chance of animals when a chunk is generated.
    probability: f64,
}

#[derive(Debug, Default)]
pub struct Spawner {
    biomes: Option<HashMap<String, BiomeSpawns>>,
    /// Chunks that had their animals at generation time.
    populated: HashSet<(i32, i32)>,
    /// Chunks in range of a player, as of `ranged_at`.
    eligible: Vec<(i32, i32)>,
    ranged_at: i64,
    /// The spawns of the biome at the middle of a chunk.
    chunk_biomes: HashMap<(i32, i32), Option<BiomeSpawns>>,
}

fn count(v: &Value) -> (u32, u32) {
    match v {
        Value::Number(n) => {
            let n = n.as_u64().unwrap_or(1) as u32;
            (n, n)
        }
        obj => (
            obj["min_inclusive"].as_u64().unwrap_or(1) as u32,
            obj["max_inclusive"].as_u64().unwrap_or(1) as u32,
        ),
    }
}

fn entries(list: &Value) -> Vec<Entry> {
    list.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let species = Species::from_id(e["type"].as_str()?)?;
                    let (min, max) = count(&e["count"]);
                    Some(Entry {
                        species,
                        weight: e["weight"].as_u64().unwrap_or(1) as u32,
                        min,
                        max,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn load_biomes(ctx: &ServerContext) -> HashMap<String, BiomeSpawns> {
    let pack = ctx.data.data_pack();
    let mut out = HashMap::new();
    let Ok(names) = pack.registry_entries("minecraft:worldgen/biome") else {
        return out;
    };
    for name in names {
        let Ok(json) = pack.read_entry("minecraft:worldgen/biome", &name) else {
            continue;
        };
        let attrs = &json["attributes"];
        let spawns =
            &attrs["minecraft:gameplay/natural_mob_spawns"]["argument"]["spawns_by_category"];
        out.insert(
            name,
            BiomeSpawns {
                creature: entries(&spawns["creature"]),
                monster: entries(&spawns["monster"]),
                probability: attrs["minecraft:gameplay/creature_world_gen_spawn_probability"]
                    .as_f64()
                    .unwrap_or(0.1),
            },
        );
    }
    out
}

fn rand() -> f64 {
    (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

fn below(n: u32) -> u32 {
    (crate::util::random_u64() >> 33) as u32 % n.max(1)
}

fn pick(list: &[Entry]) -> Option<&Entry> {
    let total: u32 = list.iter().map(|e| e.weight).sum();
    if total == 0 {
        return None;
    }
    let mut roll = below(total);
    list.iter().find(|e| {
        if roll < e.weight {
            true
        } else {
            roll -= e.weight;
            false
        }
    })
}

struct Spots<'a> {
    ctx: &'a ServerContext,
    ground: &'a Ground<'a>,
    targets: &'a [Target],
    darken: u8,
    raining: bool,
    thundering: bool,
}

impl Spots<'_> {
    fn biome(&self, x: i32, y: i32, z: i32) -> Option<String> {
        let id = self
            .ctx
            .world
            .biome(Position::new(x, y, z), &self.ctx.data.blocks)?;
        let name = self.ctx.world.biome_name(id)?;
        Some(if name.contains(':') {
            name.to_owned()
        } else {
            format!("minecraft:{name}")
        })
    }

    /// The topmost cell with something to stand on, scanning down from a
    /// little above the nearest player.
    fn surface(&self, x: i32, z: i32) -> Option<i32> {
        let top = self.ctx.world.min_y + self.ctx.world.height as i32 - 1;
        let near = self.targets.iter().min_by(|a, b| {
            let d =
                |t: &Target| (t.pos[0] - f64::from(x)).powi(2) + (t.pos[2] - f64::from(z)).powi(2);
            d(a).total_cmp(&d(b))
        })?;
        let start = (near.pos[1].floor() as i32 + SCAN_UP).min(top);
        if self.ground.solid(x, start, z) {
            return None;
        }
        (self.ctx.world.min_y..=start)
            .rev()
            .find(|&y| self.ground.solid(x, y, z) || self.ground.liquid(x, y, z) != Liquid::None)
    }

    /// Whether a mob of this species may spawn standing in this cell.
    fn fits(&self, species: Species, x: i32, y: i32, z: i32, check_players: bool) -> bool {
        if !self.ground.loaded(x, z) {
            return false;
        }
        let (w, h) = species.size();
        let half = (w / 2.0).ceil() as i32;
        let _ = half;
        let height = h.ceil() as i32;
        // Room for the body, not in a liquid or fire, on firm ground.
        for dy in 0..height {
            if self.ground.solid(x, y + dy, z)
                || self.ground.liquid(x, y + dy, z) != Liquid::None
                || self.ground.hazard(x, y + dy, z)
            {
                return false;
            }
        }
        if !self.ground.solid(x, y - 1, z) || self.ground.hazard(x, y - 1, z) {
            return false;
        }
        if check_players {
            let c = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
            let too_close = self.targets.iter().any(|t| {
                (t.pos[0] - c[0]).powi(2) + (t.pos[1] - c[1]).powi(2) + (t.pos[2] - c[2]).powi(2)
                    < MIN_PLAYER_DISTANCE * MIN_PLAYER_DISTANCE
            });
            if too_close {
                return false;
            }
        }
        let p = Position::new(x, y, z);
        match species.category() {
            Category::Creature => {
                // Grass underfoot, in the light.
                let below = self
                    .ctx
                    .world
                    .block(Position::new(x, y - 1, z), &self.ctx.data.blocks)
                    .and_then(|s| self.ctx.data.blocks.block_name(s));
                below == Some("minecraft:grass_block")
                    && self
                        .ctx
                        .world
                        .sky_light(p)
                        .max(self.ctx.world.block_light(p))
                        >= 9
            }
            Category::Monster => {
                if self.ctx.world.block_light(p) > 0 {
                    return false;
                }
                let darken = if self.thundering {
                    self.darken.max(10)
                } else if self.raining {
                    self.darken.max(3)
                } else {
                    self.darken
                };
                let sky = self.ctx.world.sky_light(p).saturating_sub(darken);
                u32::from(sky) <= below(8)
            }
        }
    }

    /// Tries to start a pack at a random place in a chunk. Spawned mobs go
    /// to `out`.
    fn try_pack(
        &self,
        spawns: &BiomeSpawns,
        category: Category,
        chunk: (i32, i32),
        at_generation: bool,
        out: &mut Vec<Entity>,
    ) {
        let list = match category {
            Category::Creature => &spawns.creature,
            Category::Monster => &spawns.monster,
        };
        let Some(entry) = pick(list) else {
            return;
        };
        let x = chunk.0 * 16 + below(16) as i32;
        let z = chunk.1 * 16 + below(16) as i32;
        if !self.ground.loaded(x, z) {
            return;
        }
        let Some(top) = self.surface(x, z) else {
            return;
        };
        // Animals on the surface; monsters anywhere from bedrock up, which
        // is mostly caves.
        let y = match category {
            Category::Creature => top + 1,
            Category::Monster => {
                let min = self.ctx.world.min_y;
                min + below((top + 2 - min).max(1) as u32) as i32
            }
        };
        if self.ground.solid(x, y, z) {
            return;
        }
        let size = entry.min + below(entry.max - entry.min + 1);
        let (mut px, mut pz) = (x, z);
        let mut spawned = 0;
        for _ in 0..size.max(1) * 4 {
            if spawned >= size {
                break;
            }
            px += below(6) as i32 - below(6) as i32;
            pz += below(6) as i32 - below(6) as i32;
            if !self.fits(entry.species, px, y, pz, !at_generation) {
                continue;
            }
            let pos = [f64::from(px) + 0.5, f64::from(y), f64::from(pz) + 0.5];
            out.push(new_mob(self.ctx, entry.species, pos));
            spawned += 1;
        }
    }
}

/// Spawns what the rules allow this tick. Returns the new mobs.
pub fn tick(
    ctx: &ServerContext,
    ground: &Ground,
    entities: &mut [Entity],
    targets: &[Target],
    now: i64,
    day_time: i64,
    difficulty: i32,
) -> Vec<Entity> {
    let mut sp = lock(&ctx.mobs.spawner);
    let spawner = &mut *sp;
    let biomes = spawner.biomes.get_or_insert_with(|| load_biomes(ctx));
    // Chunks within range of a player that are loaded.
    if now - spawner.ranged_at >= RANGE_PERIOD || spawner.eligible.is_empty() {
        let mut chunks: HashSet<(i32, i32)> = HashSet::new();
        for t in targets {
            let (cx, cz) = (
                (t.pos[0].floor() as i32) >> 4,
                (t.pos[2].floor() as i32) >> 4,
            );
            for x in cx - RANGE..=cx + RANGE {
                for z in cz - RANGE..=cz + RANGE {
                    if ctx.world.chunk_loaded(x, z) {
                        chunks.insert((x, z));
                    }
                }
            }
        }
        spawner.eligible = chunks.into_iter().collect();
        spawner.ranged_at = now;
    }
    if spawner.eligible.is_empty() {
        return Vec::new();
    }
    let list = spawner.eligible.clone();
    let chunks: HashSet<(i32, i32)> = list.iter().copied().collect();
    let n = chunks.len() as i32;
    let (mut creatures, mut monsters) = (0, 0);
    for e in entities.iter() {
        if let Kind::Mob(m) = &e.kind
            && m.dying.is_none()
            && chunks.contains(&(
                (e.pos[0].floor() as i32 >> 4),
                (e.pos[2].floor() as i32 >> 4),
            ))
        {
            match m.species.category() {
                Category::Creature => creatures += 1,
                Category::Monster => monsters += 1,
            }
        }
    }
    let (creature_cap, monster_cap) = (CREATURE_CAP * n / 289, MONSTER_CAP * n / 289);
    let weather = lock(&ctx.weather);
    let spots = Spots {
        ctx,
        ground,
        targets,
        darken: sky_darken(day_time),
        raining: weather.raining,
        thundering: weather.thundering,
    };
    drop(weather);
    let mut out = Vec::new();
    let chunk_biomes = &mut spawner.chunk_biomes;
    let mut in_biome = |x: i32, z: i32, biomes: &HashMap<String, BiomeSpawns>| {
        chunk_biomes
            .entry((x >> 4, z >> 4))
            .or_insert_with(|| {
                let y = spots.surface(x, z)?;
                spots.biome(x, y, z).and_then(|b| biomes.get(&b).cloned())
            })
            .clone()
    };

    // Animals that come with a chunk.
    for &chunk in &list {
        if spawner.populated.insert(chunk) {
            let Some(b) = in_biome(chunk.0 * 16 + 8, chunk.1 * 16 + 8, biomes) else {
                continue;
            };
            let mut packs = 0;
            while rand() < b.probability && packs < 4 {
                spots.try_pack(&b, Category::Creature, chunk, true, &mut out);
                packs += 1;
            }
        }
    }
    // Animals, now and then.
    if now % ANIMAL_PERIOD == 0 {
        for &chunk in &list {
            if creatures + out.len() as i32 >= creature_cap.max(1) {
                break;
            }
            if let Some(b) = in_biome(chunk.0 * 16 + 8, chunk.1 * 16 + 8, biomes) {
                spots.try_pack(&b, Category::Creature, chunk, false, &mut out);
            }
        }
    }
    // Monsters, every tick, in a few chunks picked at random.
    if difficulty > 0 && monsters < monster_cap {
        for _ in 0..MONSTER_TRIES {
            let chunk = list[below(list.len() as u32) as usize];
            if let Some(b) = in_biome(chunk.0 * 16 + 8, chunk.1 * 16 + 8, biomes) {
                spots.try_pack(&b, Category::Monster, chunk, false, &mut out);
            }
            if monsters + out.iter().filter(|e| is_monster(e)).count() as i32 >= monster_cap {
                break;
            }
        }
    }
    out
}

fn is_monster(e: &Entity) -> bool {
    matches!(&e.kind, Kind::Mob(m) if m.species.category() == Category::Monster)
}

/// Marks monsters that are too far from every player for removal; returns
/// their ids.
pub fn despawn(entities: &mut [Entity], targets: &[Target], difficulty: i32) -> Vec<i32> {
    let mut gone = Vec::new();
    for e in entities.iter_mut() {
        let Kind::Mob(m) = &e.kind else {
            continue;
        };
        if m.species.category() != Category::Monster || m.persistent || e.removed {
            continue;
        }
        let nearest = targets
            .iter()
            .map(|t| {
                (t.pos[0] - e.pos[0]).powi(2)
                    + (t.pos[1] - e.pos[1]).powi(2)
                    + (t.pos[2] - e.pos[2]).powi(2)
            })
            .fold(f64::MAX, f64::min);
        // Beyond 128 blocks at once; beyond 32 after half a minute, one
        // chance in 800 each tick.
        let go =
            nearest > 128.0 * 128.0 || (m.age > 600 && nearest > 32.0 * 32.0 && below(800) == 0);
        if go || difficulty == 0 {
            e.removed = true;
            gone.push(e.id);
        }
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_entity::Mob;

    #[test]
    fn counts_read_both_forms() {
        assert_eq!(count(&serde_json::json!(4)), (4, 4));
        assert_eq!(
            count(
                &serde_json::json!({"type": "minecraft:uniform", "min_inclusive": 2, "max_inclusive": 6})
            ),
            (2, 6)
        );
    }

    #[test]
    fn only_known_species_are_kept() {
        let list = serde_json::json!([
            {"type": "minecraft:cow", "count": 4, "weight": 8},
            {"type": "minecraft:horse", "count": 4, "weight": 5},
            {"type": "minecraft:zombie", "count": {"min_inclusive": 1, "max_inclusive": 2}, "weight": 90},
        ]);
        let e = entries(&list);
        assert_eq!(e.len(), 2);
        assert_eq!(
            (e[1].species, e[1].weight, e[1].min, e[1].max),
            (Species::Zombie, 90, 1, 2)
        );
    }

    #[test]
    fn weighted_pick_follows_weights() {
        let list = entries(&serde_json::json!([
            {"type": "minecraft:cow", "count": 1, "weight": 1},
            {"type": "minecraft:pig", "count": 1, "weight": 99},
        ]));
        let pigs = (0..1000)
            .filter(|_| pick(&list).is_some_and(|e| e.species == Species::Pig))
            .count();
        assert!(pigs > 900, "{pigs}");
        assert!(pick(&[]).is_none());
    }

    #[test]
    fn far_monsters_go_and_animals_stay() {
        let mk = |species, pos: [f64; 3], age: u32| {
            let mut m = Mob::new(species, 5);
            m.age = age;
            Entity {
                id: 1,
                uuid: 1,
                kind: Kind::Mob(Box::new(m)),
                pos,
                vel: [0.0; 3],
                on_ground: true,
                age: 0,
                removed: false,
                tags: Vec::new(),
            }
        };
        let player = Target {
            id: 9,
            uuid: 9,
            pos: [0.0; 3],
            targetable: true,
        };
        let mut v = vec![
            mk(Species::Zombie, [200.0, 0.0, 0.0], 10),
            mk(Species::Zombie, [10.0, 0.0, 0.0], 10),
            mk(Species::Cow, [200.0, 0.0, 0.0], 10),
        ];
        let gone = despawn(&mut v, &[player], 2);
        assert_eq!(gone.len(), 1);
        assert!(v[0].removed && !v[1].removed && !v[2].removed);
    }
}
