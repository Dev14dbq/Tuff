//! Natural spawning and despawning, after the public description of vanilla's
//! (minecraft.wiki, "Spawn" and "Mob spawning"): each category has a cap
//! that grows with the chunks players keep loaded around them; monsters are
//! tried every tick in the dark, animals when a chunk first comes in range
//! and every 400 ticks after that; nothing spawns within 24 blocks of a
//! player, and monsters far from every player go away. What spawns where is
//! read from the biomes of the data pack.

use std::collections::{HashMap, HashSet};

use mc_entity::Collide as _;
use mc_entity::mob::{Category, Gear, Target, slot};
use mc_entity::nav::{Liquid, Terrain};
use mc_entity::{Entity, Kind, Mob, Species};
use mc_protocol::Position;
use serde_json::Value;

use super::{Ground, new_mob_in, sky_darken};
use crate::context::ServerContext;
use crate::tick::lock;

/// Chunks around a player that take part (the simulation distance, capped).
const RANGE: i32 = 8;
/// Monsters per 289 chunks (17 × 17 around one player), animals likewise.
const MONSTER_CAP: i32 = 70;
const CREATURE_CAP: i32 = 10;
const AMBIENT_CAP: i32 = 15;
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
    ambient: Vec<Entry>,
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
    /// The world seed (slime chunks), once read.
    seed: Option<Option<i64>>,
    /// When phantoms next look.
    phantoms_at: i64,
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
                ambient: entries(&spawns["ambient"]),
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
    nether: bool,
    seed: Option<i64>,
    /// Moon brightness tonight (1 full .. 0 new).
    moon: f64,
}

/// The block tag animals of a kind spawn on.
fn spawnable_on(species: Species) -> &'static str {
    match species {
        Species::Rabbit => "minecraft:rabbits_spawnable_on",
        Species::Wolf => "minecraft:wolves_spawnable_on",
        Species::Fox => "minecraft:foxes_spawnable_on",
        Species::Mooshroom => "minecraft:mooshrooms_spawnable_on",
        Species::Parrot => "minecraft:parrots_spawnable_on",
        // Land animals (Cursor)
        Species::Goat => "minecraft:goats_spawnable_on",
        _ => "minecraft:animals_spawnable_on",
    }
}

/// Java's `Random` (a 48-bit linear congruential generator), for slime
/// chunks.
struct JavaRandom(i64);

impl JavaRandom {
    fn new(seed: i64) -> Self {
        Self((seed ^ 0x5DEE_CE66D) & ((1 << 48) - 1))
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.0 = (self.0.wrapping_mul(0x5DEE_CE66D).wrapping_add(0xB)) & ((1 << 48) - 1);
        (self.0 >> (48 - bits)) as i32
    }

    fn next_int(&mut self, bound: i32) -> i32 {
        if bound & -bound == bound {
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits - val + (bound - 1) >= 0 {
                return val;
            }
        }
    }
}

/// Whether slimes spawn underground in this chunk (one in ten, by seed).
pub fn slime_chunk(seed: i64, cx: i32, cz: i32) -> bool {
    let (x, z) = (i64::from(cx), i64::from(cz));
    let s = seed
        .wrapping_add((x * x * 0x4c1906) as i32 as i64)
        .wrapping_add((x * 0x5ac0db) as i32 as i64)
        .wrapping_add((z * z) as i32 as i64 * 0x4307a7)
        .wrapping_add((z * 0x5f24f) as i32 as i64)
        ^ 0x3ad8025f;
    JavaRandom::new(s).next_int(10) == 0
}

impl Spots<'_> {
    /// Slimes: in swamps at the height of the surface on moonlit nights, or
    /// deep down in slime chunks.
    fn slime_fits(&self, x: i32, y: i32, z: i32) -> bool {
        let p = Position::new(x, y, z);
        let swamp = self
            .biome(x, y, z)
            .is_some_and(|b| in_biome_tag(self.ctx, &b, "minecraft:allows_surface_slime_spawns"));
        if swamp && (51..70).contains(&y) && rand() < 0.5 && rand() < self.moon {
            return u32::from(self.ctx.world.brightness(p)) <= below(8);
        }
        let chunk = self
            .seed
            .is_some_and(|seed| slime_chunk(seed, x >> 4, z >> 4));
        chunk && y < 40 && below(10) == 0
    }

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
        // Drowned rise from deep water.
        if species == Species::Drowned {
            let water = |dy| self.ground.liquid(x, y + dy, z) == Liquid::Water;
            return water(0)
                && water(1)
                && self.ctx.world.block_light(Position::new(x, y, z)) == 0
                && below(15) == 0;
        }
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
            // Golems come from villagers and players, never from here.
            Category::Misc => false,
            // Bats: in the dark, light 3 or less.
            Category::Ambient => {
                self.ctx
                    .world
                    .sky_light(p)
                    .max(self.ctx.world.block_light(p))
                    <= 3
            }
            Category::Creature => {
                // Its kind of ground underfoot, in the light.
                let below = self
                    .ctx
                    .world
                    .block(Position::new(x, y - 1, z), &self.ctx.data.blocks)
                    .and_then(|s| self.ctx.data.blocks.block_name(s))
                    .unwrap_or("");
                let ground = self
                    .ctx
                    .commands
                    .env
                    .tag("minecraft:block", spawnable_on(species));
                ground.iter().any(|g| g == below)
                    && self
                        .ctx
                        .world
                        .sky_light(p)
                        .max(self.ctx.world.block_light(p))
                        >= 9
            }
            Category::Monster => {
                if self.nether {
                    return self.ctx.world.block_light(p) <= 11;
                }
                if species == Species::Slime {
                    return self.slime_fits(x, y, z);
                }
                // Sulfur cubes spawn at any light level.
                if species == Species::SulfurCube {
                    return true;
                }
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
            Category::Misc => return,
            Category::Ambient => &spawns.ambient,
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
            Category::Creature | Category::Misc => top + 1,
            Category::Monster | Category::Ambient => {
                let min = self.ctx.world.min_y;
                min + below((top + 2 - min).max(1) as u32) as i32
            }
        };
        // Drowned start in the water column instead.
        let y = if entry.species == Species::Drowned {
            if self.ground.liquid(x, top, z) != Liquid::Water {
                return;
            }
            let bottom = (self.ctx.world.min_y..=top)
                .rev()
                .find(|&yy| self.ground.liquid(x, yy, z) != Liquid::Water)
                .unwrap_or(top);
            bottom + 1 + below((top - bottom).max(1) as u32) as i32
        } else {
            y
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
            let biome = self.biome(px, y, pz);
            out.push(new_mob_in(self.ctx, entry.species, pos, biome.as_deref()));
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
    let mut all = lock(&ctx.mobs.spawner);
    let spawner = all.entry(ctx.dim).or_default();
    let mut biome_table = spawner.biomes.take().unwrap_or_else(|| load_biomes(ctx));
    let out = spawn_round(
        ctx,
        spawner,
        &biome_table,
        ground,
        entities,
        targets,
        now,
        day_time,
        difficulty,
    );
    spawner.biomes = Some(std::mem::take(&mut biome_table));
    out
}

#[allow(clippy::too_many_arguments)]
fn spawn_round(
    ctx: &ServerContext,
    spawner: &mut Spawner,
    biomes: &HashMap<String, BiomeSpawns>,
    ground: &Ground,
    entities: &mut [Entity],
    targets: &[Target],
    now: i64,
    day_time: i64,
    difficulty: i32,
) -> Vec<Entity> {
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
    let (mut creatures, mut monsters, mut bats) = (0, 0, 0);
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
                Category::Ambient => bats += 1,
                Category::Misc => {}
            }
        }
    }
    let (creature_cap, monster_cap) = (CREATURE_CAP * n / 289, MONSTER_CAP * n / 289);
    if !ctx.game_rule_bool("minecraft:spawn_mobs") {
        return Vec::new();
    }
    let seed = *spawner.seed.get_or_insert_with(|| ctx.world.seed());
    let weather = lock(&ctx.weather);
    let phase = (day_time / 24000).rem_euclid(8);
    let spots = Spots {
        ctx,
        ground,
        targets,
        darken: sky_darken(day_time),
        raining: weather.raining && ctx.world.dimension == "minecraft:overworld",
        thundering: weather.thundering,
        nether: ctx.world.dimension == "minecraft:the_nether",
        seed,
        moon: [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75][phase as usize],
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
    // Phantoms come for players who have not slept for three days.
    phantoms(
        ctx,
        &mut spawner.phantoms_at,
        &spots,
        now,
        difficulty,
        &mut out,
    );
    // Bats, every tick like monsters, up to a cap of their own.
    if bats < AMBIENT_CAP * n / 289 {
        let chunk = list[below(list.len() as u32) as usize];
        if let Some(b) = in_biome(chunk.0 * 16 + 8, chunk.1 * 16 + 8, biomes) {
            spots.try_pack(&b, Category::Ambient, chunk, false, &mut out);
        }
    }
    // Monsters, every tick, in a few chunks picked at random.
    if difficulty > 0 && monsters < monster_cap && ctx.game_rule_bool("minecraft:spawn_monsters") {
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

/// Whether a biome is in a biome tag.
fn in_biome_tag(ctx: &ServerContext, biome: &str, tag: &str) -> bool {
    ctx.commands
        .env
        .tag("minecraft:worldgen/biome", tag)
        .iter()
        .any(|b| b == biome)
}

/// Whether a `biomes` field (an id, a `#tag` or a list) holds a biome.
fn biomes_hold(ctx: &ServerContext, field: &Value, biome: &str) -> bool {
    match field {
        Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => in_biome_tag(ctx, biome, tag),
            None => mc_command::args::full_id(s) == biome,
        },
        Value::Array(a) => a.iter().any(|v| biomes_hold(ctx, v, biome)),
        _ => false,
    }
}

/// Picks a data-driven variant the way spawning does: among the entries
/// whose spawn conditions hold, those of the highest priority, at random.
fn pick_variant(ctx: &ServerContext, species: Species, biome: Option<&str>, m: &mut Mob) -> i32 {
    let Some(reg) = super::meta::variant_registry(species) else {
        return 0;
    };
    let pack = ctx.data.data_pack();
    let mut best: (i64, Vec<i32>) = (i64::MIN, Vec::new());
    for (i, name) in super::meta::registry(ctx, reg).iter().enumerate() {
        let Ok(json) = pack.read_entry(reg, name) else {
            continue;
        };
        for cond in json["spawn_conditions"].as_array().into_iter().flatten() {
            let holds = match cond.get("condition") {
                None => true,
                Some(c) if c["type"] == "minecraft:biome" => {
                    biome.is_some_and(|b| biomes_hold(ctx, &c["biomes"], b))
                }
                Some(_) => false,
            };
            let priority = cond["priority"].as_i64().unwrap_or(0);
            if holds && priority >= best.0 {
                if priority > best.0 {
                    best = (priority, Vec::new());
                }
                best.1.push(i as i32);
            }
        }
    }
    if best.1.is_empty() {
        return 0;
    }
    best.1[m.below(best.1.len() as u32) as usize]
}

/// The type of villager a biome raises (zombie villagers too).
pub fn villager_type(biome: Option<&str>) -> &'static str {
    let b = biome.unwrap_or("");
    let has = |w: &[&str]| w.iter().any(|w| b.contains(w));
    if has(&["desert"]) {
        "minecraft:desert"
    } else if has(&["jungle", "bamboo"]) {
        "minecraft:jungle"
    } else if has(&["savanna", "badlands"]) {
        "minecraft:savanna"
    } else if has(&["snowy", "frozen", "ice", "grove", "jagged", "peaks"]) {
        "minecraft:snow"
    } else if has(&["swamp"]) {
        "minecraft:swamp"
    } else if has(&["taiga"]) {
        "minecraft:taiga"
    } else {
        "minecraft:plains"
    }
}

/// How hard the place is, after the wiki's "regional difficulty": the
/// difficulty scaled by how long the world has run and the moon.
pub fn regional_difficulty(difficulty: i32, world_age: i64, day_time: i64) -> f64 {
    if difficulty == 0 {
        return 0.0;
    }
    let time = ((world_age - 72_000) as f64 / 1_440_000.0).clamp(0.0, 1.0) * 0.25;
    let phase = (day_time / 24000).rem_euclid(8);
    let moon = [1.0, 0.75, 0.5, 0.25, 0.0, 0.25, 0.5, 0.75][phase as usize];
    let mut chunk = (moon * 0.25f64).min(time);
    if difficulty == 1 {
        chunk *= 0.5;
    }
    f64::from(difficulty) * (0.75 + time + chunk)
}

/// The part of regional difficulty that gives monsters gear (0 to 1).
pub fn special_multiplier(regional: f64) -> f64 {
    ((regional - 2.0) / 2.0).clamp(0.0, 1.0)
}

fn gear(ctx: &ServerContext, name: &str) -> Option<Gear> {
    Some(Gear {
        item: ctx.items.id(&format!("minecraft:{name}"))?,
        count: 1,
        damage: 0,
        components: Default::default(),
    })
}

/// Random armor for a monster, from boots up.
fn armor(ctx: &ServerContext, m: &mut Mob, special: f64, difficulty: i32) {
    if m.next_f64() >= 0.15 * special {
        return;
    }
    let mut tier = m.below(2);
    for _ in 0..3 {
        if m.next_f64() < 0.095 {
            tier += 1;
        }
    }
    let material = ["leather", "golden", "chainmail", "iron", "diamond"][tier as usize];
    let stop = if difficulty == 3 { 0.1 } else { 0.25 };
    for (i, (s, piece)) in [
        (slot::FEET, "boots"),
        (slot::LEGS, "leggings"),
        (slot::CHEST, "chestplate"),
        (slot::HEAD, "helmet"),
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 && m.next_f64() < stop {
            break;
        }
        if m.gear[s].is_none() {
            m.gear[s] = gear(ctx, &format!("{material}_{piece}"));
        }
    }
}

/// What a mob is born with: a sheep's wool, a variant fitting the biome,
/// a monster's weapon and armor, a chance of being a baby. `biome` is
/// where it spawns, when known.
pub fn dress(ctx: &ServerContext, e: &mut Entity, biome: Option<&str>) {
    use Species::*;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    let difficulty = ctx.difficulty();
    let day_time = super::day_time(ctx);
    let world_age = lock(&ctx.time).age;
    let special = special_multiplier(regional_difficulty(difficulty, world_age, day_time));
    let species = m.species;
    // Up to one zombie in ten may break doors, by regional difficulty.
    if matches!(species, Zombie | Husk | ZombieVillager) {
        m.breaks_doors = m.next_f64() < special * 0.1;
    }
    match species {
        Sheep => {
            let roll = m.below(10_000);
            // The wiki's natural wool colours.
            m.set_color(match roll {
                0..=15 => 6,
                16..=315 => 12,
                316..=815 => 7,
                816..=1315 => 8,
                1316..=1815 => 15,
                _ => 0,
            });
        }
        Cat | Cow | Wolf | Pig | Chicken => m.variant = pick_variant(ctx, species, biome, m),
        Horse => m.variant = m.below(7) as i32 | (m.below(5) as i32) << 8,
        Parrot => m.variant = m.below(5) as i32,
        Fox => {
            let snow = biome.is_some_and(|b| in_biome_tag(ctx, b, "minecraft:spawns_snow_foxes"));
            m.variant = i32::from(snow);
        }
        Rabbit => {
            let tag = |t| biome.is_some_and(|b| in_biome_tag(ctx, b, t));
            m.variant = if tag("minecraft:spawns_gold_rabbits") {
                4
            } else if tag("minecraft:spawns_white_rabbits") {
                if m.below(5) == 0 { 3 } else { 1 }
            } else {
                match m.below(100) {
                    0..=49 => 0,
                    50..=89 => 2,
                    _ => 5,
                }
            };
        }
        // Land animals (Cursor). Natural spawns only: a summon keeps the
        // defaults (both horns, not screaming, grown up).
        Goat if biome.is_some() => {
            if m.below(100) < 5 {
                m.set_baby(true);
            } else if m.below(10) == 0 {
                if m.below(2) == 0 {
                    m.quirks.land.left_horn = false;
                } else {
                    m.quirks.land.right_horn = false;
                }
            }
            if m.below(50) == 0 {
                m.quirks.land.screaming = true;
                m.quirks.land.ram_cd = m.rand_range(100, 300);
            }
        }
        _ => {}
    }
    if species.category() == Category::Monster {
        m.left_handed = m.next_f64() < 0.05;
        m.can_pick_up = m.next_f64() < 0.55 * special;
        if species.zombie() || species.skeleton() {
            armor(ctx, m, special, difficulty);
        }
    }
    let weapon = match species {
        Skeleton | Stray | Bogged | Parched => Some("bow"),
        WitherSkeleton => Some("stone_sword"),
        ZombifiedPiglin => Some("golden_sword"),
        // Land animals (Cursor)
        PiglinBrute => Some("golden_axe"),
        // Illagers and their kin
        Pillager => Some("crossbow"),
        Vindicator => Some("iron_axe"),
        Vex => Some("iron_sword"),
        Illusioner => Some("bow"),
        Piglin => Some(if m.below(2) == 0 {
            "crossbow"
        } else {
            "golden_sword"
        }),
        Drowned if m.next_f64() > 0.9 => Some(if m.below(16) < 10 {
            "trident"
        } else {
            "fishing_rod"
        }),
        Zombie | Husk | ZombieVillager
            if m.next_f64() < if difficulty == 3 { 0.05 } else { 0.01 } =>
        {
            Some(if m.below(3) == 0 {
                "iron_sword"
            } else {
                "iron_shovel"
            })
        }
        _ => None,
    };
    if let Some(w) = weapon
        && m.gear[slot::MAINHAND].is_none()
    {
        m.gear[slot::MAINHAND] = gear(ctx, w);
    }
    if species == ZombieVillager && m.villager.is_none() {
        m.villager = Some((villager_type(biome).to_owned(), None, 1));
    }
    if (species.zombie() || species == Piglin) && m.next_f64() < 0.05 {
        m.set_baby(true);
    }
}

/// Phantoms: every minute or two, each player in the open at night who has
/// not slept for three days or more (their `time_since_rest` statistic)
/// may draw a few from the sky.
fn phantoms(
    ctx: &ServerContext,
    next_at: &mut i64,
    spots: &Spots,
    now: i64,
    difficulty: i32,
    out: &mut Vec<Entity>,
) {
    if now < *next_at {
        return;
    }
    *next_at = now + 1200 + i64::from(below(1200));
    // Only under a sky that turns night.
    if ctx.world.dimension != "minecraft:overworld"
        || difficulty == 0
        || spots.darken < 5
        || !ctx.game_rule_bool("minecraft:spawn_phantoms")
    {
        return;
    }
    let day_time = super::day_time(ctx);
    let world_age = lock(&ctx.time).age;
    let regional = regional_difficulty(difficulty, world_age, day_time);
    for t in spots.targets.iter().filter(|t| t.targetable) {
        let unrest = i64::from(crate::stats::get(
            ctx,
            t.uuid,
            crate::stats::CUSTOM,
            "time_since_rest",
        ));
        let p = Position::new(
            t.pos[0].floor() as i32,
            t.pos[1].floor() as i32,
            t.pos[2].floor() as i32,
        );
        if p.y < ctx.world.sea_level
            || ctx.world.sky_light(p) < 15
            || regional <= rand() * 3.0
            || unrest < 72_000
            || i64::from(below(unrest.min(u32::MAX as i64) as u32)) < 72_000
        {
            continue;
        }
        let up = 20 + below(15) as i32;
        let at = [
            f64::from(p.x + below(21) as i32 - 10) + 0.5,
            f64::from(p.y + up),
            f64::from(p.z + below(21) as i32 - 10) + 0.5,
        ];
        if spots.ground.solid(at[0] as i32, at[1] as i32, at[2] as i32) {
            continue;
        }
        for _ in 0..1 + below(difficulty as u32 + 1) {
            out.push(new_mob_in(ctx, Species::Phantom, at, None));
        }
    }
}

fn is_monster(e: &Entity) -> bool {
    matches!(&e.kind, Kind::Mob(m) if m.species.category() == Category::Monster)
}

/// Marks the mobs that despawn (minecraft.wiki, "Mob spawning",
/// "Despawning") for removal; returns their ids. On peaceful every monster
/// goes. Otherwise a monster that is not persistent (named, tamed, it picked
/// something up), not leashed (`leashed` holds those ids) and neither riding
/// nor ridden goes at once beyond 128 blocks of every player, and beyond 32
/// blocks with one chance in 800 a tick once it has been idle for 600 ticks;
/// a player within 32 blocks keeps it from being idle.
pub fn despawn(
    entities: &mut [Entity],
    targets: &[Target],
    difficulty: i32,
    leashed: &std::collections::HashSet<i32>,
) -> Vec<i32> {
    let riding: std::collections::HashSet<i32> = entities
        .iter()
        .flat_map(|e| match &e.kind {
            Kind::Mob(m) => m.riders.clone(),
            Kind::Vehicle(v) => v.riders.clone(),
            _ => Vec::new(),
        })
        .collect();
    let mut gone = Vec::new();
    for e in entities.iter_mut() {
        let id = e.id;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if !matches!(m.species.category(), Category::Monster | Category::Ambient) || e.removed {
            continue;
        }
        if difficulty == 0
            && m.species.category() == Category::Monster
            && m.species != Species::SulfurCube
        {
            e.removed = true;
            gone.push(id);
            continue;
        }
        if m.persistent
            || m.custom_name.is_some()
            || m.owner.is_some()
            || !m.riders.is_empty()
            || riding.contains(&id)
            || leashed.contains(&id)
        {
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
        if nearest < 32.0 * 32.0 {
            m.idle = 0;
            continue;
        }
        if nearest > 128.0 * 128.0 || (m.idle > 600 && below(800) == 0) {
            e.removed = true;
            gone.push(id);
        }
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_entity::Mob;

    #[test]
    fn slime_chunks_match_known_seed() {
        // One in ten chunks, the same for the same seed.
        let n = (0..100)
            .flat_map(|x| (0..100).map(move |z| (x, z)))
            .filter(|&(x, z)| slime_chunk(12345, x, z))
            .count();
        assert!((800..1200).contains(&n), "{n}");
        assert_eq!(slime_chunk(7, 3, -4), slime_chunk(7, 3, -4));
    }

    #[test]
    fn regional_difficulty_grows_with_time() {
        assert_eq!(regional_difficulty(0, 0, 0), 0.0);
        assert!((regional_difficulty(2, 0, 0) - 1.5).abs() < 1e-9);
        let late = regional_difficulty(3, 10_000_000, 0);
        // Hard, years in, full moon: 3 × (0.75 + 0.25 + 0.25).
        assert!((late - 3.75).abs() < 1e-9, "{late}");
        assert!((special_multiplier(late) - 0.875).abs() < 1e-9);
        assert_eq!(special_multiplier(1.5), 0.0);
    }

    #[test]
    fn villager_types_follow_biomes() {
        assert_eq!(villager_type(Some("minecraft:desert")), "minecraft:desert");
        assert_eq!(
            villager_type(Some("minecraft:snowy_plains")),
            "minecraft:snow"
        );
        assert_eq!(villager_type(None), "minecraft:plains");
    }

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
            {"type": "minecraft:warden", "count": 4, "weight": 5},
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
        let mk = |species, pos: [f64; 3], idle: i32| {
            let mut m = Mob::new(species, 5);
            m.idle = idle;
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
        let player = Target::at(9, 9, [0.0; 3]);
        let mut v = vec![
            mk(Species::Zombie, [200.0, 0.0, 0.0], 10),
            mk(Species::Zombie, [10.0, 0.0, 0.0], 10),
            mk(Species::Cow, [200.0, 0.0, 0.0], 10),
        ];
        let none = std::collections::HashSet::new();
        let gone = despawn(&mut v, std::slice::from_ref(&player), 2, &none);
        assert_eq!(gone.len(), 1);
        assert!(v[0].removed && !v[1].removed && !v[2].removed);
        // Named, leashed or idle-but-near mobs stay; peaceful takes them all.
        let named = |mut e: Entity| {
            if let Kind::Mob(m) = &mut e.kind {
                m.custom_name = Some("Bob".into());
            }
            e
        };
        let mut v = vec![
            named(mk(Species::Zombie, [200.0, 0.0, 0.0], 10)),
            mk(Species::Zombie, [0.0, 0.0, 200.0], 10),
            mk(Species::Skeleton, [20.0, 0.0, 0.0], 5000),
        ];
        v[1].id = 2;
        v[2].id = 3;
        let leashed: std::collections::HashSet<i32> = [2].into_iter().collect();
        for _ in 0..5000 {
            assert!(despawn(&mut v, std::slice::from_ref(&player), 2, &leashed).is_empty());
        }
        assert!(matches!(&v[2].kind, Kind::Mob(m) if m.idle == 0));
        assert_eq!(despawn(&mut v, &[player], 0, &leashed).len(), 3);
        // Far and idle: sooner or later.
        let mut v = vec![mk(Species::Zombie, [50.0, 0.0, 0.0], 601)];
        let mut ticks = 0;
        while despawn(&mut v, &[Target::at(9, 9, [0.0; 3])], 2, &none).is_empty() {
            ticks += 1;
            assert!(ticks < 100_000);
        }
    }
}
