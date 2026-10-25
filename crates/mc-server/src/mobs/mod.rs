//! Mobs in the world: the tick that runs their goals, what they do to
//! players and each other, hits, deaths and drops, and what clients are told
//! about them. Behaviour itself (goals, paths, movement) lives in
//! `mc_entity::mob`; the rest here:
//! - [`hurt`]: damage to mobs, death, loot, experience, breeding and
//!   conversions;
//! - [`interact`]: what players do to mobs with items in hand;
//! - [`ride`]: saddled pigs and horses;
//! - [`spawn`]: natural spawning, despawning and how new mobs are dressed;
//! - [`meta`]: metadata and equipment for clients;
//! - [`nbt`]: mobs as `/summon` and `/data` see them.

pub mod air;
pub mod aqua;
pub mod bees;
pub(crate) mod blocks;
pub mod chest;
pub mod copper;
pub mod creaking;
mod crowd;
pub mod cube;
pub mod eggs;
pub mod endermite;
pub mod golems;
pub mod hurt;
mod illager;
pub mod infest;
pub mod interact;
mod land;
pub mod meta;
pub mod nbt;
pub mod ride;
pub(crate) mod spawn;
mod spawner;
mod terrain;
pub mod village;

use std::collections::HashMap;
use std::sync::Mutex;

use mc_entity::mob::{Category, Missile, MobEvent, Other, Senses, Target};
use mc_entity::{Entity, Kind, Mob, Species};
use mc_protocol::packets::play::{self, SetEntityData};

use crate::combat::Blow;
use crate::context::{Event, ServerContext};
use crate::explosion::Blast;
use crate::fx;
use crate::level::WorldLevel;
use crate::tick::lock;

pub use meta::{metadata_of, type_id};
pub use terrain::Ground;

/// How far from a player mobs think (blocks).
const ACTIVE_RANGE: f64 = 128.0;
/// How far a player reaches a mob (blocks, squared, with room for lag).
const REACH_SQ: f64 = 36.0;

pub const WOOL_COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// Who did something to a mob.
#[derive(Clone, Debug)]
pub struct Actor {
    pub uuid: u128,
    pub entity_id: i32,
    pub name: String,
    pub pos: [f64; 3],
    pub game_mode: i32,
    pub sneaking: bool,
}

#[derive(Debug)]
enum Pending {
    Hit {
        target: i32,
        by: Actor,
        blow: Blow,
    },
    Use {
        target: i32,
        by: Actor,
        hand: i32,
    },
    /// A bell rang.
    Bell {
        at: mc_protocol::Position,
    },
    /// Damage from something other than a blow (Thorns).
    Harm {
        target: i32,
        amount: f32,
        kind: String,
        player: Option<u128>,
    },
}

/// Mob state shared with connections: what players did to mobs, waiting for
/// the tick thread, where mobs have spawned, and players in fights.
#[derive(Debug, Default)]
pub struct MobState {
    /// By dimension index.
    pending: Mutex<HashMap<usize, Vec<Pending>>>,
    spawner: Mutex<HashMap<usize, spawn::Spawner>>,
    /// Players' fighting state (see [`crate::combat`]).
    pub fighters: Mutex<crate::combat::Fighters>,
    /// Ticks until each spawner (dimension, x, y, z) tries again.
    pub spawner_delays: Mutex<HashMap<(usize, i32, i32, i32), i32>>,
    /// Trial spawners' waves, and the configs they named (by data pack name).
    pub trials: Mutex<crate::trials::Trials>,
    pub trial_configs: Mutex<HashMap<String, crate::trials::Config>>,
    /// Health of villagers and wandering traders (full when not listed).
    villagers: Mutex<HashMap<i32, hurt::MerchantHealth>>,
    /// Villagers killed by a player: who and where, until the tick tells
    /// the villagers that saw (see [`village::witnesses`]).
    murders: Mutex<Vec<(usize, u128, [f64; 3])>>,
}

impl MobState {
    /// A player hit the entity with this id with `blow` (nothing happens
    /// if it is no mob or out of reach).
    pub fn hit(&self, dim: usize, target: i32, by: Actor, blow: Blow) {
        lock(&self.pending)
            .entry(dim)
            .or_default()
            .push(Pending::Hit { target, by, blow });
    }

    /// The mob with this id takes `amount` of `kind` (Thorns); `player` is
    /// who did it.
    pub fn harm(&self, dim: usize, target: i32, amount: f32, kind: &str, player: Option<u128>) {
        lock(&self.pending)
            .entry(dim)
            .or_default()
            .push(Pending::Harm {
                target,
                amount,
                kind: kind.to_owned(),
                player,
            });
    }

    /// A player used an item on the entity with this id.
    pub fn use_on(&self, dim: usize, target: i32, by: Actor, hand: i32) {
        lock(&self.pending)
            .entry(dim)
            .or_default()
            .push(Pending::Use { target, by, hand });
    }

    /// The player killed a villager at `at`: the villagers that saw it
    /// remember (hitting one is noted by the villager itself, in
    /// `hurt::merchant_hurt`; what each remembers is in
    /// [`mc_entity::gossip`]).
    pub fn offend(&self, dim: usize, player: u128, at: [f64; 3]) {
        lock(&self.murders).push((dim, player, at));
    }

    /// A bell at `at` rang.
    pub fn bell(&self, dim: usize, at: mc_protocol::Position) {
        lock(&self.pending)
            .entry(dim)
            .or_default()
            .push(Pending::Bell { at });
    }

    /// Health of the villager or wandering trader with this id.
    pub fn villager_health(&self, id: i32) -> f32 {
        lock(&self.villagers)
            .get(&id)
            .copied()
            .unwrap_or_default()
            .health
    }

    /// Sets it (`None`: forgets it, the villager left the world).
    pub fn set_villager_health(&self, id: i32, health: Option<f32>) {
        let mut all = lock(&self.villagers);
        match health {
            Some(h) if h < hurt::MerchantHealth::default().health => {
                all.entry(id).or_default().health = h;
            }
            _ => {
                all.remove(&id);
            }
        }
    }
}

/// A player clicked a mob: a hit, or a use of the held item.
pub fn clicked(
    ctx: &ServerContext,
    who: &crate::interact::Actor,
    target: i32,
    hand: i32,
    attack: bool,
) {
    let name = lock(&ctx.players)
        .get(&who.uuid)
        .map(|p| p.profile.name.clone())
        .unwrap_or_default();
    let by = Actor {
        uuid: who.uuid,
        entity_id: who.entity_id,
        name,
        pos: [who.pose.x, who.pose.y, who.pose.z],
        game_mode: who.game_mode,
        sneaking: who.sneaking,
    };
    if attack {
        let blow = crate::combat::swing(ctx, who.uuid, who.pose);
        ctx.mobs.hit(ctx.dim, target, by, blow);
    } else {
        ctx.mobs.use_on(ctx.dim, target, by, hand);
    }
}

/// A new mob of this species (not yet in the world), as `/summon` makes
/// it: nothing rolled for where it spawns.
pub fn new_mob(ctx: &ServerContext, species: Species, pos: [f64; 3]) -> Entity {
    new_mob_in(ctx, species, pos, None)
}

/// A new mob dressed for the biome it spawns in.
pub fn new_mob_in(
    ctx: &ServerContext,
    species: Species,
    pos: [f64; 3],
    biome: Option<&str>,
) -> Entity {
    let mob = Mob::new(species, crate::util::random_u64());
    let mut e = Entity {
        id: if species == Species::EnderDragon {
            ctx.next_entity_ids(9)
        } else {
            ctx.next_entity_id()
        },
        uuid: crate::util::random_uuid(),
        kind: Kind::Mob(Box::new(mob)),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    spawn::dress(ctx, &mut e, biome);
    e
}

/// The metadata a mob has right now, or `None` for other entities.
pub fn metadata_packet(ctx: &ServerContext, entity: &Entity) -> Option<SetEntityData> {
    let Kind::Mob(m) = &entity.kind else {
        return None;
    };
    Some(SetEntityData {
        entity_id: entity.id,
        entries: metadata_of(ctx, m, lock(&ctx.time).age),
    })
}

/// One mob's change of place for clients.
#[derive(Clone, Debug, PartialEq)]
pub struct MobMove {
    pub id: i32,
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub on_ground: bool,
}

/// The sky's darkening by time of day (0 by day, 11 at night).
pub fn sky_darken(day_time: i64) -> u8 {
    let t = day_time.rem_euclid(24000);
    match t {
        0..=12000 => 0,
        12001..=13800 => ((t - 12000) * 11 / 1800) as u8,
        13801..=22200 => 11,
        22201..=24000 => ((24000 - t) * 11 / 1800) as u8,
        _ => 0,
    }
}

/// Ticks of the overworld clock (the first clock when it has no name).
pub fn day_time(ctx: &ServerContext) -> i64 {
    let time = lock(&ctx.time);
    time.clocks
        .iter()
        .find(|c| c.name == "minecraft:overworld")
        .or(time.clocks.first())
        .map_or(time.age, |c| c.ticks)
}

fn players(ctx: &ServerContext) -> Vec<Target> {
    let vitals = lock(&ctx.vitals);
    let inventories = lock(&ctx.inventories);
    let players = lock(&ctx.players);
    let mut out: Vec<Target> = players
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            let pose = p.pose?;
            let alive = vitals.get(&p.profile.uuid).is_none_or(|v| !v.dead);
            let hands = inventories.get(&p.profile.uuid);
            let item = |slot: usize| {
                hands
                    .and_then(|h| h.slots.get(slot).cloned().flatten())
                    .map_or(-1, |s| s.item)
            };
            let main = hands.map_or(-1, |h| item(h.hand_slot(0)));
            let gold = (5..9).any(|s| {
                let i = item(s);
                i >= 0
                    && ctx
                        .items
                        .name(i)
                        .is_some_and(|n| n.starts_with("minecraft:golden_"))
            });
            let mut t = Target::at(p.entity_id, p.profile.uuid, [pose.x, pose.y, pose.z]);
            t.targetable = matches!(p.game_mode, 0 | 2) && alive;
            t.present = p.game_mode != 3 && alive;
            t.held = [main, item(45)];
            t.head = item(5);
            t.gold = gold;
            t.yaw = pose.yaw;
            t.pitch = pose.pitch;
            // Aquatic (Claude): dolphins come to swimmers and boats.
            let wet = ctx
                .world
                .block(
                    mc_protocol::Position::new(
                        pose.x.floor() as i32,
                        (pose.y + 0.4).floor() as i32,
                        pose.z.floor() as i32,
                    ),
                    &ctx.data.blocks,
                )
                .and_then(|s| ctx.data.blocks.block_name(s))
                == Some("minecraft:water");
            t.swimming = wet && crate::stats::is_sprinting(ctx, t.uuid);
            t.boating = crate::stats::in_boat(ctx, t.id);
            Some(t)
        })
        .collect();
    drop((vitals, inventories, players));
    for t in &mut out {
        (t.hurt_by, t.hit) = crate::combat::recent(ctx, t.uuid);
    }
    out
}

/// Every mob and villager as the others see them.
fn others(entities: &[Entity]) -> Vec<Other> {
    // Whether a llama's caravan is held by a leashed llama at its head.
    let held = |mut at: i32| {
        for _ in 0..10 {
            let Some(Kind::Mob(m)) = entities.iter().find(|e| e.id == at).map(|e| &e.kind) else {
                return false;
            };
            if m.leash.is_some() {
                return true;
            }
            match m.caravan_head {
                Some(h) => at = h,
                None => return false,
            }
        }
        false
    };
    entities
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) => {
                let (w, h) = m.size();
                Some(Other {
                    leashed: m.leash.is_some(),
                    in_caravan: m.caravan_head.is_some(),
                    has_tail: m.caravan_tail,
                    chain_held: m.species.llama() && held(e.id),
                    id: e.id,
                    species: Some(m.species),
                    pos: e.pos,
                    width: w,
                    height: h,
                    baby: m.is_baby(),
                    in_love: m.in_love(),
                    owner: m.owner,
                    // An axolotl playing dead is not there for anything hunting it.
                    alive: !(m.dying.is_some()
                        || m.is_dead()
                        || e.removed
                        || m.species == Species::Axolotl && m.quirks.aqua.dead_play > 0),
                })
            }
            Kind::Merchant(m) => Some(Other {
                id: e.id,
                species: None,
                pos: e.pos,
                width: e.size().0,
                height: e.size().1,
                baby: m.is_baby(),
                in_love: false,
                owner: None,
                alive: !e.removed,
                leashed: false,
                in_caravan: false,
                has_tail: false,
                chain_held: false,
            }),
            _ => None,
        })
        .collect()
}

pub(crate) fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Item tags and names for the mobs, from the server's tables.
struct ItemSense<'a> {
    ctx: &'a ServerContext,
    tags: std::cell::RefCell<HashMap<String, Vec<String>>>,
}

impl mc_entity::mob::Items for ItemSense<'_> {
    fn tagged(&self, item: i32, tag: &str) -> bool {
        let Some(name) = self.ctx.items.name(item) else {
            return false;
        };
        let mut tags = self.tags.borrow_mut();
        let list = tags.entry(tag.to_owned()).or_insert_with(|| {
            self.ctx
                .commands
                .env
                .tag("minecraft:item", tag.trim_start_matches('#'))
        });
        list.iter().any(|n| n == name)
    }

    fn name(&self, item: i32) -> &str {
        self.ctx.items.name(item).unwrap_or("")
    }
}

/// The sound category of a mob's sounds.
pub fn category(species: Species) -> i32 {
    match species.category() {
        Category::Monster if species == Species::ZombieNautilus => fx::category::NEUTRAL,
        Category::Monster => 5,
        Category::Ambient => fx::category::AMBIENT,
        Category::Creature
        | Category::Misc
        | Category::WaterCreature
        | Category::WaterAmbient
        | Category::UndergroundWater
        | Category::Axolotls => fx::category::NEUTRAL,
    }
}

/// Applies what players did since the last tick.
fn apply_pending(ctx: &ServerContext, entities: &mut [Entity], pending: Vec<Pending>, now: i64) {
    for p in pending {
        match p {
            Pending::Hit { target, by, blow } => {
                // A dragon is hit in one of its parts.
                let part = crate::bosses::part_owner(entities, target);
                let target = part.map_or(target, |(id, _)| id);
                let Some(i) = entities.iter().position(|e| e.id == target) else {
                    continue;
                };
                let e = &entities[i];
                if let Kind::Merchant(_) = &e.kind {
                    if !e.removed && dist_sq(by.pos, e.pos) <= REACH_SQ {
                        hurt::player_blow(ctx, entities, i, &by, &blow, now);
                    }
                    continue;
                }
                let Kind::Mob(m) = &e.kind else {
                    continue;
                };
                let at = match (part, m.dragon()) {
                    (Some((_, p)), Some(d)) => d.parts[p as usize],
                    _ => e.pos,
                };
                if m.dying.is_some() || dist_sq(by.pos, at) > REACH_SQ {
                    continue;
                }
                if let Some((_, p)) = part {
                    crate::bosses::dragon::aim_at(&mut entities[i], p);
                }
                hurt::player_blow(ctx, entities, i, &by, &blow, now);
            }
            Pending::Bell { at } => village::ring(ctx, entities, at, now),
            Pending::Harm {
                target,
                amount,
                kind,
                player,
            } => {
                let Some(e) = entities.iter_mut().find(|e| e.id == target) else {
                    continue;
                };
                let mut h = hurt::Harm::new(amount, &kind);
                h.player = player;
                h.by = player.and_then(|p| lock(&ctx.players).get(&p).map(|p| p.entity_id));
                if player.is_some() {
                    h.killer = Some("minecraft:player".into());
                }
                hurt::harm(ctx, e, &h);
            }
            Pending::Use { target, by, hand } => {
                // Those on leads (and what holds them), for leads to move.
                let leashed: Vec<Entity> = {
                    let links = lock(&ctx.leashes);
                    entities
                        .iter()
                        .filter(|e| links.links.contains_key(&e.id))
                        .cloned()
                        .collect()
                };
                let Some(e) = entities.iter_mut().find(|e| e.id == target) else {
                    continue;
                };
                if dist_sq(by.pos, e.pos) > REACH_SQ {
                    continue;
                }
                let held = crate::held::held(ctx, by.uuid, hand).map(|h| h.stack);
                interact::used(ctx, e, &by, hand, &leashed);
                if held.is_some() {
                    let after = crate::criteria::entity(ctx, e);
                    crate::criteria::entity_and_item(
                        ctx,
                        by.uuid,
                        "minecraft:player_interacted_with_entity",
                        &after,
                        held.as_ref(),
                    );
                }
            }
        }
    }
}

/// What mobs did this tick that needs the whole world.
#[derive(Default)]
struct Outcome {
    moves: Vec<MobMove>,
    data: Vec<SetEntityData>,
    removed: Vec<i32>,
    blasts: Vec<Blast>,
    /// New entities (babies, converted mobs, split slimes).
    fresh: Vec<Entity>,
}

/// Runs the mobs for one tick: what players did, goals and movement,
/// deaths, spawning and despawning.
pub fn tick(ctx: &ServerContext, level: &mut WorldLevel) {
    let pending = lock(&ctx.mobs.pending).remove(&ctx.dim).unwrap_or_default();
    let targets = players(ctx);
    if targets.is_empty() {
        return;
    }
    let mut entities = std::mem::take(&mut *lock(&ctx.entities));
    let now = lock(&ctx.time).age;
    let day = day_time(ctx);
    // Only the overworld has a sun and weather.
    let overworld = ctx.world.dimension == "minecraft:overworld";
    let raining = overworld && lock(&ctx.weather).raining;
    let sunny = overworld && sky_darken(day) == 0 && !raining;
    // Bright outside: rain dims the sky a little, a thunderstorm too much.
    let bright = overworld && sky_darken(day) < 4 && !lock(&ctx.weather).thundering;
    let griefing = ctx.game_rule_bool("minecraft:mob_griefing");
    let difficulty = ctx.difficulty();
    let mut out = Outcome::default();

    apply_pending(ctx, &mut entities, pending, now);
    village::witnesses(ctx, &mut entities);
    village::scorn(&mut entities, now);
    crate::decor::leash::tell_mobs(ctx, &mut entities);
    if now % 20 == 0 {
        blocks::close_idle(ctx, level, &entities, &targets);
        blocks::free_beds(ctx, level, &entities);
    }
    chest::mirror(ctx, &mut entities);
    air::tick(ctx, &mut entities, &targets, now);
    {
        let home = raining || !bright;
        bees::bees(ctx, level, &mut entities, now, home);
        bees::hives(ctx, level, home);
        creaking::tick(ctx, level, &mut entities, now, day);
        copper::tick(ctx, level, &mut entities, now);
        cube::tick(ctx, &mut entities, &targets, now);
    }
    illager::tend(&mut entities);

    let ground = Ground::new(ctx);
    let items = ItemSense {
        ctx,
        tags: Default::default(),
    };
    let mut events = Vec::new();
    crowd::tick(ctx, &mut entities, &targets);
    let mut seen = others(&entities);
    for i in 0..entities.len() {
        let e = &mut entities[i];
        let Kind::Mob(m) = &e.kind else {
            continue;
        };
        if e.removed {
            continue;
        }
        // The bosses think wherever their chunk is loaded.
        let near = m.species.boss()
            || targets
                .iter()
                .any(|t| dist_sq(t.pos, e.pos) <= ACTIVE_RANGE * ACTIVE_RANGE);
        let (cx, cz) = (
            (e.pos[0].floor() as i32) >> 4,
            (e.pos[2].floor() as i32) >> 4,
        );
        if !near || !ctx.world.chunk_loaded(cx, cz) && !m.species.boss() {
            continue;
        }
        // Monsters do not exist in peaceful.
        if difficulty == 0
            && m.species.category() == Category::Monster
            && m.species != Species::EnderDragon
            && m.species != Species::SulfurCube
        {
            e.removed = true;
            out.removed.push(e.id);
            continue;
        }
        if e.on_ground {
            aqua::trample_eggs(ctx, level, m, e.pos, None);
        }
        if !m.riders.is_empty() && now % 10 == 0 {
            aqua::breath(ctx, m, &targets);
        }
        let before = (
            e.pos,
            e.on_ground,
            m.yaw,
            m.pitch,
            m.head_yaw,
            m.burning > 0,
        );
        // Flowing water and lava carry it (not what flies).
        if !m.species.flies() && !ride::steered(e) {
            let b = e.aabb();
            crate::currents::push(ctx, b.min, b.max, &mut e.vel);
        }
        // Ridden by a player: they steer, it only lives.
        let senses = Senses {
            world: &ground,
            players: &targets,
            mobs: &seen,
            items: &items,
            tick: now as u64,
            difficulty,
            sunny,
            raining,
            nether: ctx.world.dimension == "minecraft:the_nether",
            day: bright,
            griefing,
        };
        events.clear();
        if m.species.boss() {
            crate::bosses::tick_mob(ctx, level, &mut entities, i, &targets, now, &mut out.blasts);
        } else if ride::steered(e) {
            ride::tick(ctx, e, &senses, &mut events);
        } else {
            mc_entity::mob::tick(e, &senses, &mut events);
        }
        let e = &mut entities[i];
        let id = e.id;
        // Keep what the others see of it up to date.
        if let Some(o) = seen.iter_mut().find(|o| o.id == id)
            && let Kind::Mob(m) = &e.kind
        {
            o.pos = e.pos;
            o.in_love = m.in_love();
            o.baby = m.is_baby();
        }
        for ev in events.drain(..) {
            handle(
                ctx,
                level,
                &mut entities,
                i,
                ev,
                &targets,
                difficulty,
                now,
                &mut out,
            );
        }
        let e = &mut entities[i];
        if e.removed {
            out.removed.push(e.id);
            continue;
        }
        crate::mob_effects::tick(ctx, e, now);
        // The dragon dies its own way.
        if let Kind::Mob(m) = &e.kind
            && m.is_dead()
            && m.dying.is_none()
            && m.species != Species::EnderDragon
        {
            hurt::die(ctx, e, None, 0);
        }
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        // A burning mob in a water or powder snow cauldron goes out.
        if m.burning > 0 && !m.species.fire_immune() {
            let cell = mc_tick::BlockPos::new(
                e.pos[0].floor() as i32,
                e.pos[1].floor() as i32,
                e.pos[2].floor() as i32,
            );
            let top = e.pos[1] + m.size().1;
            let logic = level.logic();
            if mc_tick::cauldron::put_out(level, logic, cell, e.pos[1], top) {
                m.burning = 0;
            }
        }
        if (e.pos, e.on_ground, m.yaw, m.pitch, m.head_yaw)
            != (before.0, before.1, before.2, before.3, before.4)
        {
            out.moves.push(MobMove {
                id: e.id,
                pos: e.pos,
                yaw: m.yaw,
                pitch: m.pitch,
                head_yaw: m.head_yaw,
                on_ground: e.on_ground,
            });
        }
        if std::mem::take(&mut m.dirty) || (m.burning > 0) != before.5 {
            out.data.push(SetEntityData {
                entity_id: e.id,
                entries: metadata_of(ctx, m, now),
            });
        }
        if e.removed {
            // The body is gone: a puff of smoke first.
            fx::status(ctx, e.id, 60);
            out.removed.push(e.id);
        }
    }

    for (id, _) in crate::explosion::explode_all(ctx, level, &mut entities, &out.blasts) {
        out.removed.push(id);
    }
    // Mobs hurt by the blast.
    for e in entities.iter_mut() {
        // The dragon dies its own way.
        if let Kind::Mob(m) = &e.kind
            && m.is_dead()
            && m.dying.is_none()
            && m.species != Species::EnderDragon
        {
            hurt::die(ctx, e, None, 0);
        }
    }
    interact::pick_up(ctx, &mut entities, &mut out.data, now);
    aqua::play(ctx, &mut entities, now);
    interact::admire(ctx, &mut entities);

    let spawned = spawn::tick(ctx, &ground, &mut entities, &targets, now, day, difficulty);
    out.fresh.extend(spawned);
    golems::villagers(ctx, &ground, &mut entities, now, &mut out.fresh);
    village::claims(ctx, &mut entities, now);
    village::tend(ctx, &ground, &mut entities, now, &mut out.fresh);
    let made = spawner::tick(ctx, &ground, &entities, &targets, difficulty);
    out.fresh.extend(made);
    let made = crate::trials::tick(ctx, &ground, &entities, &targets, difficulty, now);
    out.fresh.extend(made);
    crate::trials::tick_vaults(ctx, &targets, now);
    let leashed: std::collections::HashSet<i32> =
        lock(&ctx.leashes).links.keys().copied().collect();
    out.removed.extend(spawn::despawn(
        &mut entities,
        &targets,
        difficulty,
        &leashed,
    ));
    drop(ground);

    for e in entities.iter().filter(|e| e.removed) {
        if matches!(e.kind, Kind::Mob(_)) && !out.removed.contains(&e.id) {
            out.removed.push(e.id);
        }
    }
    village::release_removed(ctx, &mut entities);
    entities.retain(|e| !e.removed);
    for e in &out.fresh {
        let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    }
    entities.append(&mut out.fresh);
    aqua::carry(ctx, &mut entities);
    let mut stored = lock(&ctx.entities);
    entities.append(&mut stored);
    *stored = entities;
    drop(stored);

    if !out.moves.is_empty() {
        let _ = ctx.events.send(Event::MobsMoved(out.moves));
    }
    if !out.data.is_empty() {
        let _ = ctx.events.send(Event::EntitiesData(out.data));
    }
    if !out.removed.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(out.removed));
    }
}

/// Carries out one thing the mob at `i` did.
#[allow(clippy::too_many_arguments)]
fn handle(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    i: usize,
    ev: MobEvent,
    targets: &[Target],
    difficulty: i32,
    now: i64,
    out: &mut Outcome,
) {
    let id = entities[i].id;
    let pos = entities[i].pos;
    let Kind::Mob(m) = &mut entities[i].kind else {
        return;
    };
    let species = m.species;
    match ev {
        MobEvent::Attack { target, damage } => {
            hurt::mob_attacks(ctx, entities, i, target, damage, targets, difficulty, now);
            if species == Species::Bee {
                air::bee_stung(&mut entities[i]);
            }
        }
        MobEvent::Shoot {
            target,
            aim,
            missile,
        } => shoot(ctx, &entities[i], target, aim, missile, difficulty),
        MobEvent::Explode { power } => {
            entities[i].removed = true;
            out.blasts.push(Blast {
                center: [pos[0], pos[1] + 0.0625, pos[2]],
                power,
                source: id,
                owner: None,
                mob: true,
                dangerous: false,
                by: None,
                fire: false,
                respawn: None,
            });
        }
        MobEvent::Lay { item } => hurt::lay(ctx, &entities[i], item),
        MobEvent::Graze { cell, block } => {
            let to = if block {
                ctx.data.blocks.default_state("minecraft:dirt")
            } else {
                Some(ctx.shapes.air())
            };
            if let Some(state) = to {
                level.set_block(mc_protocol::Position::new(cell[0], cell[1], cell[2]), state);
            }
        }
        MobEvent::Status(s) => fx::status(ctx, id, s as i8),
        MobEvent::Hurt { amount, kind } => {
            let harm = hurt::Harm::new(amount, kind);
            hurt::harm(ctx, &mut entities[i], &harm);
        }
        MobEvent::Landed { distance } => {
            trample_farmland(ctx, level, m, pos, distance);
            aqua::trample_eggs(ctx, level, m, pos, Some(distance));
        }
        MobEvent::Breed { partner } => {
            if let Some(baby) = hurt::breed(ctx, entities, i, partner) {
                out.fresh.push(baby);
            }
        }
        MobEvent::Sound {
            name,
            volume,
            pitch,
        } => fx::sound(ctx, &name, category(species), pos, volume, pitch),
        MobEvent::Swing => fx::emit(
            ctx,
            fx::Fx::Swing(play::SwingAnimation {
                entity_id: id,
                hand: 0,
                kind: 1,
                duration: 6,
            }),
        ),
        MobEvent::Convert(to) => {
            if let Some(new) = hurt::convert(ctx, &mut entities[i], to) {
                out.fresh.push(new);
            }
        }
        MobEvent::Cured => hurt::cure(ctx, &mut entities[i]),
        MobEvent::FlowerTaken(child) => village::flower_taken(ctx, child),
        MobEvent::CaravanJoin { head } | MobEvent::CaravanLeave { head } => {
            let join = matches!(ev, MobEvent::CaravanJoin { .. });
            if let Some(Kind::Mob(h)) = entities
                .iter_mut()
                .find(|e| e.id == head)
                .map(|e| &mut e.kind)
            {
                h.caravan_tail = join;
            }
        }
        MobEvent::Alert { target } => {
            let range = species.follow_range();
            for (j, o) in entities.iter_mut().enumerate() {
                if j == i || dist_sq(o.pos, pos) > range * range {
                    continue;
                }
                if let Kind::Mob(om) = &mut o.kind
                    && om.species == species
                    && om.target.is_none()
                    && !om.is_tame()
                {
                    om.revenge = Some((target, 100));
                }
            }
        }
        MobEvent::BreakDoor { cell } => blocks::break_door(ctx, level, id, cell),
        MobEvent::DoorCrack { cell, stage } => blocks::crack(ctx, id, cell, stage),
        MobEvent::DoorBash { cell } => blocks::bash(ctx, cell),
        MobEvent::OpenDoor { cell, open } => blocks::open_door(ctx, level, cell, open),
        MobEvent::TakeBlock { cell } => blocks::take(ctx, level, &mut entities[i], cell),
        MobEvent::PlaceBlock { cell } => blocks::place(ctx, level, entities, i, cell, targets),
        MobEvent::Teleported => {
            if species == Species::Enderman {
                fx::sound(ctx, "entity.enderman.teleport", 5, pos, 1.0, 1.0);
            }
        }
        MobEvent::Heal(amount) => {
            m.health = (m.health + amount).min(m.max_health);
            m.dirty = true;
        }
        MobEvent::Wear { slot, amount } => hurt::wear_gear(ctx, &mut entities[i], slot, amount),
        MobEvent::Buck => ride::eject(ctx, &mut entities[i]),
        MobEvent::Trail => golems::snow_trail(ctx, level, &entities[i]),
        MobEvent::Vanish => entities[i].removed = true,
        MobEvent::Hide { cell } => infest::hide(ctx, level, entities, i, cell),
        MobEvent::WakeFriends => infest::wake_friends(ctx, level, entities, i, &mut out.fresh),
        // Land animals (Cursor)
        MobEvent::Ram {
            target,
            damage,
            knockback,
        } => land::ram(ctx, entities, i, target, damage, knockback, targets),
        MobEvent::Horn => land::horn(ctx, &mut entities[i]),
        // Aquatic (Claude)
        MobEvent::Ink => aqua::ink(ctx, &entities[i]),
        MobEvent::Sting {
            target,
            damage,
            poison,
        } => aqua::sting(
            ctx, entities, i, target, damage, poison, targets, difficulty,
        ),
        MobEvent::MagicAttack { target, damage } => {
            aqua::magic(ctx, entities, i, target, damage, targets, difficulty);
        }
        MobEvent::Curse => aqua::curse(ctx, &entities[i], targets),
        MobEvent::Eat { target } => aqua::eat(ctx, entities, i, target),
        MobEvent::LayEgg { cell, count } => aqua::lay_egg(ctx, level, cell, count),
        MobEvent::LaySpawn { cell } => aqua::lay_spawn(ctx, level, cell),
        MobEvent::Grace { player } => aqua::grace(ctx, player, targets),
        MobEvent::Fangs(spots) => illager::fangs(ctx, &entities[i], spots, &mut out.fresh),
        MobEvent::SummonVexes => illager::summon_vexes(ctx, &entities[i], &mut out.fresh),
        MobEvent::Wololo { sheep } => illager::wololo(ctx, entities, sheep),
        MobEvent::Blind { target } => illager::blind(ctx, entities, targets, target),
        MobEvent::Roar => illager::roar(ctx, entities, i, targets, difficulty),
        MobEvent::Crush => illager::crush(ctx, level, &entities[i]),
    }
}

/// A living mob landed after falling `fell` blocks. Farmland gives way
/// under a fall, the higher the likelier (wiki, "Farmland" → "Trampling").
/// A mob does it only while `mob_griefing` is on, and only when its box
/// (width × width × height) is bigger than 0.512. A chicken, a rabbit or
/// a baby is too light.
fn trample_farmland(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    m: &Mob,
    pos: [f64; 3],
    fell: f64,
) {
    if m.is_dead() || !ctx.game_rule_bool("mob_griefing") {
        return;
    }
    let (w, h) = m.size();
    if w * w * h <= 0.512 {
        return;
    }
    let roll = (crate::util::random_u64() >> 40) as f64 / (1u64 << 24) as f64;
    if roll >= fell - 0.5 {
        return;
    }
    let below = mc_protocol::Position::new(
        pos[0].floor() as i32,
        (pos[1] - 0.2).floor() as i32,
        pos[2].floor() as i32,
    );
    let logic = level.logic();
    mc_tick::random::trample(level, logic, crate::level::to_pos(below));
}

/// A mob shoots or throws at its target.
fn shoot(
    ctx: &ServerContext,
    e: &Entity,
    target: i32,
    aim: [f64; 3],
    missile: Missile,
    difficulty: i32,
) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let me = (e.id, m.species.display_name());
    let eye = [e.pos[0], e.pos[1] + m.eye_height(), e.pos[2]];
    let d = [aim[0] - eye[0], aim[1] - eye[1], aim[2] - eye[2]];
    let flat = d[0].hypot(d[2]);
    let spawn = |shot, from: [f64; 3], dir: [f64; 3], speed: f64, inaccuracy: f64| {
        let len = dir.iter().map(|c| c * c).sum::<f64>().sqrt().max(1e-9);
        let spread = 0.0075 * inaccuracy;
        let vel = dir.map(|c| (c / len + crate::hits::gauss() * spread) * speed);
        crate::projectiles::spawn_by_mob(ctx, shot, me, from, vel)
    };
    match missile {
        Missile::Arrow { .. } | Missile::Bolt | Missile::Tipped(..) => {
            let (damage, slowness, tipped) = match missile {
                Missile::Arrow { damage, slowness } => (damage, slowness, None),
                Missile::Tipped(effect, ticks) => (2.0, false, Some((effect, ticks))),
                _ => (2.0, false, None),
            };
            let Some(item) = ctx.items.id("minecraft:arrow") else {
                return;
            };
            let mut a = mc_entity::extra::Arrow::new(item);
            a.damage = damage + f64::from(difficulty) * 0.11 + crate::hits::gauss() * 0.25;
            a.pickup = mc_entity::extra::Pickup::No;
            a.hit.push(e.id);
            if slowness {
                a.effects.push(("minecraft:slowness".into(), 0, 600 * 8));
            }
            if let Some((effect, ticks)) = tipped {
                a.effects.push((effect.into(), 0, ticks * 8));
            }
            let from = [eye[0], eye[1] - 0.1, eye[2]];
            let dir = [d[0], aim[1] - 0.6 - from[1] + flat * 0.2, d[2]];
            spawn(
                mc_entity::extra::Shot::Arrow(a),
                from,
                dir,
                1.6,
                f64::from(14 - difficulty * 4),
            );
        }
        Missile::Trident => {
            let Some(item) = ctx.items.id("minecraft:trident") else {
                return;
            };
            let mut a = mc_entity::extra::Arrow::new(item);
            a.damage = 8.0;
            a.trident = true;
            a.pickup = mc_entity::extra::Pickup::No;
            a.hit.push(e.id);
            let from = [eye[0], eye[1] - 0.1, eye[2]];
            let dir = [d[0], aim[1] - 0.6 - from[1] + flat * 0.2, d[2]];
            spawn(
                mc_entity::extra::Shot::Arrow(a),
                from,
                dir,
                1.6,
                f64::from(14 - difficulty * 4),
            );
            fx::sound(ctx, "entity.drowned.shoot", 5, e.pos, 1.0, 1.0);
        }
        Missile::Snowball => {
            let from = [e.pos[0], e.pos[1] + m.eye_height() - 0.1, e.pos[2]];
            // Aims at the target's chest, with the drop of the throw.
            let dir = [d[0], aim[1] - 1.1 - from[1] + flat * 0.2, d[2]];
            spawn(mc_entity::extra::Shot::Snowball, from, dir, 1.6, 12.0);
        }
        Missile::Spit => {
            let from = [e.pos[0], e.pos[1] + m.eye_height() - 0.1, e.pos[2]];
            let dir = [
                aim[0] - from[0],
                aim[1] - from[1] + flat * 0.2,
                aim[2] - from[2],
            ];
            spawn(mc_entity::extra::Shot::Spit, from, dir, 1.5, 10.0);
        }
        Missile::Potion(name) => {
            let Some(item) = ctx.items.id("minecraft:splash_potion") else {
                return;
            };
            let effects = crate::projectiles::potions::effects_of(name);
            let color = crate::projectiles::potions::color(&effects);
            let from = [e.pos[0], e.pos[1] + m.eye_height() - 0.1, e.pos[2]];
            let dir = [d[0], aim[1] - 1.1 - from[1] + flat * 0.2, d[2]];
            spawn(
                mc_entity::extra::Shot::Potion {
                    item,
                    lingering: false,
                    effects,
                    color,
                },
                from,
                dir,
                0.75,
                8.0,
            );
            fx::sound(
                ctx,
                "entity.witch.throw",
                5,
                e.pos,
                1.0,
                0.8 + crate::hits::rnd() as f32 * 0.4,
            );
        }
        Missile::Fireball { .. } => {
            let yaw = f64::from(m.yaw).to_radians();
            let from = [
                e.pos[0] - yaw.sin() * 4.0,
                e.pos[1] + 2.0,
                e.pos[2] + yaw.cos() * 4.0,
            ];
            let dir = [aim[0] - from[0], aim[1] - from[1], aim[2] - from[2]];
            spawn(
                mc_entity::extra::Shot::Fireball { small: false },
                from,
                dir,
                1.0,
                0.0,
            );
        }
        // Flying mobs (flying-mobs)
        Missile::WindCharge => {
            let from = [e.pos[0], e.pos[1] + m.size().1 / 2.0, e.pos[2]];
            spawn(
                mc_entity::extra::Shot::WindCharge { breeze: true },
                from,
                [d[0], aim[1] - from[1], d[2]],
                0.7,
                5.0 - f64::from(difficulty),
            );
        }
        Missile::ShulkerBullet => {
            let from = [e.pos[0], e.pos[1] + 0.5, e.pos[2]];
            spawn(
                mc_entity::extra::Shot::ShulkerBullet { target },
                from,
                [aim[0] - from[0], aim[1] - from[1], aim[2] - from[2]],
                0.15,
                0.0,
            );
        }
        Missile::SmallFireball => {
            let spread = flat.sqrt() * 0.5;
            let dir = [
                d[0] + crate::hits::gauss() * spread * 0.3,
                d[1],
                d[2] + crate::hits::gauss() * spread * 0.3,
            ];
            let from = [e.pos[0], e.pos[1] + m.size().1 / 2.0 + 0.5, e.pos[2]];
            spawn(
                mc_entity::extra::Shot::Fireball { small: true },
                from,
                dir,
                1.0,
                0.0,
            );
            fx::level_event(ctx, 1018, e.pos, 0);
        }
    }
}

#[cfg(test)]
mod tests;
