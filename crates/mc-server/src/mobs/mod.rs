//! Mobs in the world: the tick that runs their goals, hits, deaths and
//! drops, and what clients are told about them. Behaviour itself (goals,
//! paths, movement) lives in `mc_entity::mob`; spawning in [`spawn`].

mod spawn;
mod terrain;

use std::sync::Mutex;

use mc_entity::mob::{MobEvent, Senses, Target};
use mc_entity::{Entity, Kind, Mob, Species};
use mc_protocol::packets::play::{MetadataValue, SetEntityData, metadata};

use crate::context::{Event, ServerContext};
use crate::explosion::Blast;
use crate::level::WorldLevel;
use crate::tick::lock;

pub use terrain::Ground;

/// Entity metadata fields of mobs (indices observed from vanilla 26.3).
mod field {
    pub const FLAGS: u8 = 0;
    /// `Mob` flags: 4 aggressive.
    pub const MOB_FLAGS: u8 = 15;
    /// Sheep: colour in the low bits, 16 shorn.
    pub const SHEEP_WOOL: u8 = 17;
    pub const CREEPER_SWELL: u8 = 16;
    pub const CREEPER_POWERED: u8 = 17;
    pub const CREEPER_IGNITED: u8 = 18;
    /// Spider: 1 climbing.
    pub const SPIDER_FLAGS: u8 = 16;
}

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
}

#[derive(Debug)]
enum Pending {
    Hit { target: i32, by: Actor, damage: f32 },
    Use { target: i32, by: Actor, hand: i32 },
}

/// Mob state shared with connections: what players did to mobs, waiting for
/// the tick thread, and where mobs have spawned.
#[derive(Debug, Default)]
pub struct MobState {
    pending: Mutex<Vec<Pending>>,
    spawner: Mutex<spawn::Spawner>,
}

impl MobState {
    /// A player hit the entity with this id (nothing happens if it is no
    /// mob or out of reach).
    pub fn hit(&self, target: i32, by: Actor, damage: f32) {
        lock(&self.pending).push(Pending::Hit { target, by, damage });
    }

    /// A player used an item on the entity with this id.
    pub fn use_on(&self, target: i32, by: Actor, hand: i32) {
        lock(&self.pending).push(Pending::Use { target, by, hand });
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
    };
    if attack {
        let held = lock(&ctx.inventories)
            .get(&who.uuid)
            .and_then(|h| h.main_hand().map(|s| s.item));
        ctx.mobs
            .hit(target, by, crate::vitals::melee_damage(ctx, held));
    } else {
        ctx.mobs.use_on(target, by, hand);
    }
}

/// A new mob of this species (not yet in the world).
pub fn new_mob(ctx: &ServerContext, species: Species, pos: [f64; 3]) -> Entity {
    let mut mob = Mob::new(species, crate::util::random_u64());
    if species == Species::Sheep {
        let roll = mob.below(10_000);
        // The wiki's natural wool colours.
        mob.set_color(match roll {
            0..=15 => 6,
            16..=315 => 12,
            316..=815 => 7,
            816..=1315 => 8,
            1316..=1815 => 15,
            _ => 0,
        });
    }
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Mob(Box::new(mob)),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// The entity type id clients know the species by.
pub fn type_id(ctx: &ServerContext, species: Species) -> i32 {
    ctx.data
        .builtin
        .id("minecraft:entity_type", species.id())
        .unwrap_or(0)
}

/// All the metadata fields of a mob, as sent when it appears.
pub fn metadata_of(mob: &Mob) -> Vec<(u8, MetadataValue)> {
    let mut v = vec![
        (
            field::FLAGS,
            MetadataValue::Byte(if mob.burning > 0 { 1 } else { 0 }),
        ),
        (metadata::HEALTH, MetadataValue::Float(mob.health.max(0.0))),
        (
            field::MOB_FLAGS,
            MetadataValue::Byte(if mob.aggressive() { 4 } else { 0 }),
        ),
    ];
    match mob.species {
        Species::Sheep => v.push((
            field::SHEEP_WOOL,
            MetadataValue::Byte((mob.color as i8 & 15) | if mob.sheared { 16 } else { 0 }),
        )),
        Species::Creeper => {
            v.push((field::CREEPER_SWELL, MetadataValue::VarInt(mob.swell_dir)));
            v.push((field::CREEPER_POWERED, MetadataValue::Boolean(mob.powered)));
            v.push((field::CREEPER_IGNITED, MetadataValue::Boolean(mob.ignited)));
        }
        Species::Spider => v.push((
            field::SPIDER_FLAGS,
            MetadataValue::Byte(i8::from(mob.climbing)),
        )),
        _ => {}
    }
    v
}

/// The metadata a mob has right now, or `None` for other entities.
pub fn metadata_packet(entity: &Entity) -> Option<SetEntityData> {
    let Kind::Mob(m) = &entity.kind else {
        return None;
    };
    Some(SetEntityData {
        entity_id: entity.id,
        entries: metadata_of(m),
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
fn day_time(ctx: &ServerContext) -> i64 {
    let time = lock(&ctx.time);
    time.clocks
        .iter()
        .find(|c| c.name == "minecraft:overworld")
        .or(time.clocks.first())
        .map_or(time.age, |c| c.ticks)
}

fn players(ctx: &ServerContext) -> Vec<Target> {
    let vitals = lock(&ctx.vitals);
    lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            let pose = p.pose?;
            let alive = vitals.get(&p.profile.uuid).is_none_or(|v| !v.dead);
            Some(Target {
                id: p.entity_id,
                uuid: p.profile.uuid,
                pos: [pose.x, pose.y, pose.z],
                targetable: matches!(p.game_mode, 0 | 2) && alive,
            })
        })
        .collect()
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

struct Env<'a>(&'a ServerContext);

impl crate::loot::LootEnv for Env<'_> {
    fn tag(&self, registry: &str, tag: &str) -> Vec<String> {
        self.0.commands.env.tag(registry, tag)
    }
    fn max_stack(&self, item: i32) -> i32 {
        self.0.items.max_stack(item)
    }
    fn max_damage(&self, item: i32) -> i32 {
        self.0.items.get(item).map_or(0, |i| i.max_damage)
    }
    fn item_name(&self, item: i32) -> String {
        self.0.items.name(item).unwrap_or("").to_owned()
    }
    fn smelt(&self, item: i32) -> Option<i32> {
        self.0
            .recipes
            .cooking
            .iter()
            .find(|c| c.kind == "minecraft:smelting" && c.input.contains(&item))
            .map(|c| c.result.item)
    }
}

/// What a dead mob leaves, from the loot tables of the data pack.
fn loot(ctx: &ServerContext, entity: &Entity, by_player: bool) -> Vec<crate::loot::Drop> {
    let Kind::Mob(m) = &entity.kind else {
        return Vec::new();
    };
    let env = Env(ctx);
    let cx = crate::loot::LootCtx {
        origin: Some(entity.pos),
        this: Some(crate::loot::Ent {
            kind: m.species.id().to_owned(),
            on_fire: m.burning > 0,
        }),
        player_kill: by_player,
        ..Default::default()
    };
    let name = m.species.id().trim_start_matches("minecraft:");
    let mut drops = Vec::new();
    let mut roll = |table: &str| {
        if let Some(t) = ctx.loot.table(table) {
            drops.extend(ctx.loot.roll(&env, t, &cx));
        }
    };
    roll(&format!("minecraft:entities/{name}"));
    if m.species == Species::Sheep && !m.sheared {
        roll(&format!(
            "minecraft:entities/sheep/{}",
            WOOL_COLORS[usize::from(m.color & 15)]
        ));
    }
    drops
}

/// Starts a mob's death: the animation, and what it drops.
fn die(ctx: &ServerContext, entity: &mut Entity, by_player: bool, status: &mut Vec<(i32, u8)>) {
    let Kind::Mob(m) = &mut entity.kind else {
        return;
    };
    if m.dying.is_some() {
        return;
    }
    m.dying = Some(20);
    m.health = 0.0;
    m.target = None;
    m.nav.stop();
    status.push((entity.id, 3));
    let pos = entity.pos;
    // Experience for what a player killed: 5 for monsters, 1-3 for animals.
    if by_player && let Kind::Mob(m) = &mut entity.kind {
        let xp = match m.species.category() {
            mc_entity::mob::Category::Monster => 5,
            mc_entity::mob::Category::Creature => 1 + m.below(3) as i32,
        };
        crate::xp::spawn(ctx, [pos[0], pos[1] + 0.3, pos[2]], xp);
    }
    // Experience for what a player killed: 5 for monsters, 1-3 for animals.
    if by_player && let Kind::Mob(m) = &mut entity.kind {
        let xp = match m.species.category() {
            mc_entity::mob::Category::Monster => 5,
            mc_entity::mob::Category::Creature => 1 + m.below(3) as i32,
        };
        crate::xp::spawn(ctx, [pos[0], pos[1] + 0.3, pos[2]], xp);
    }
    for drop in loot(ctx, entity, by_player) {
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.3, pos[2]], drop.item, drop.count);
    }
}

fn shears(ctx: &ServerContext) -> Option<i32> {
    ctx.items.id("minecraft:shears")
}

/// Wears the shears in `hand` by one use.
fn wear_shears(ctx: &ServerContext, who: &Actor, hand: i32) {
    let mut inventories = lock(&ctx.inventories);
    let Some(hands) = inventories.get_mut(&who.uuid) else {
        return;
    };
    let slot = hands.hand_slot(hand);
    let Some(stack) = &mut hands.slots[slot] else {
        return;
    };
    let max = ctx.items.max_damage(stack.item);
    stack.damage += 1;
    if max > 0 && stack.damage >= max {
        hands.slots[slot] = None;
        hands.components.remove(&slot);
        let _ = ctx.events.send(Event::HandBroke {
            uuid: who.uuid,
            entity_id: who.entity_id,
            status: if hand == 1 { 48 } else { 47 },
        });
    }
    let _ = ctx.events.send(Event::InventorySlot {
        uuid: who.uuid,
        slot: slot as i16,
        item: hands.slots[slot].clone(),
    });
}

/// Applies what players did since the last tick.
fn apply_pending(
    ctx: &ServerContext,
    entities: &mut [Entity],
    pending: Vec<Pending>,
    status: &mut Vec<(i32, u8)>,
) {
    for p in pending {
        match p {
            Pending::Hit { target, by, damage } => {
                let Some(e) = entities.iter_mut().find(|e| e.id == target) else {
                    continue;
                };
                let Kind::Mob(m) = &mut e.kind else {
                    continue;
                };
                if m.dying.is_some() || dist_sq(by.pos, e.pos) > REACH_SQ {
                    continue;
                }
                if m.hurt(damage, Some(by.entity_id)) {
                    status.push((e.id, 2));
                    // Knocked back from the attacker (strength 0.4).
                    let (dx, dz) = (e.pos[0] - by.pos[0], e.pos[2] - by.pos[2]);
                    let len = (dx * dx + dz * dz).sqrt().max(1e-4);
                    e.vel[0] = e.vel[0] / 2.0 + dx / len * 0.4;
                    e.vel[2] = e.vel[2] / 2.0 + dz / len * 0.4;
                    e.vel[1] = (e.vel[1] / 2.0 + 0.4).min(0.4);
                    e.on_ground = false;
                    if m.is_dead() {
                        die(ctx, e, true, status);
                    }
                }
            }
            Pending::Use { target, by, hand } => {
                let Some(e) = entities.iter_mut().find(|e| e.id == target) else {
                    continue;
                };
                let pos = e.pos;
                let Kind::Mob(m) = &mut e.kind else {
                    continue;
                };
                if !m.can_shear() || dist_sq(by.pos, pos) > REACH_SQ {
                    continue;
                }
                let held = lock(&ctx.inventories)
                    .get(&by.uuid)
                    .and_then(|h| h.slots[h.hand_slot(hand)].as_ref().map(|s| s.item));
                if held.is_none() || held != shears(ctx) {
                    continue;
                }
                m.shear();
                let color = WOOL_COLORS[usize::from(m.color & 15)];
                let count = 1 + m.below(3) as i32;
                status.push((e.id, 0));
                wear_shears(ctx, &by, hand);
                if let Some(wool) = ctx.items.id(&format!("minecraft:{color}_wool")) {
                    crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 1.0, pos[2]], wool, count);
                }
            }
        }
    }
}

/// Runs the mobs for one tick: what players did, goals and movement,
/// deaths, spawning and despawning.
pub fn tick(ctx: &ServerContext, level: &mut WorldLevel) {
    let pending = std::mem::take(&mut *lock(&ctx.mobs.pending));
    let targets = players(ctx);
    if targets.is_empty() {
        return;
    }
    let mut entities = std::mem::take(&mut *lock(&ctx.entities));
    let now = lock(&ctx.time).age;
    let day = day_time(ctx);
    let sunny = sky_darken(day) == 0 && !lock(&ctx.weather).raining;
    let difficulty = ctx.difficulty();
    let mut status: Vec<(i32, u8)> = Vec::new();
    let mut moves: Vec<MobMove> = Vec::new();
    let mut data: Vec<SetEntityData> = Vec::new();
    let mut removed: Vec<i32> = Vec::new();
    let mut blasts: Vec<Blast> = Vec::new();

    apply_pending(ctx, &mut entities, pending, &mut status);

    let ground = Ground::new(ctx);
    let senses = Senses {
        world: &ground,
        players: &targets,
        tick: now as u64,
        difficulty,
        sunny,
    };
    let mut events = Vec::new();
    for e in entities.iter_mut() {
        let Kind::Mob(m) = &e.kind else {
            continue;
        };
        let near = targets
            .iter()
            .any(|t| dist_sq(t.pos, e.pos) <= ACTIVE_RANGE * ACTIVE_RANGE);
        let (cx, cz) = (
            (e.pos[0].floor() as i32) >> 4,
            (e.pos[2].floor() as i32) >> 4,
        );
        if !near || !ctx.world.chunk_loaded(cx, cz) {
            continue;
        }
        // Monsters do not exist in peaceful.
        if difficulty == 0 && m.species.category() == mc_entity::mob::Category::Monster {
            e.removed = true;
            removed.push(e.id);
            continue;
        }
        let before = (
            e.pos,
            e.on_ground,
            m.yaw,
            m.pitch,
            m.head_yaw,
            m.burning > 0,
        );
        events.clear();
        mc_entity::mob::tick(e, &senses, &mut events);
        let id = e.id;
        for ev in events.drain(..) {
            match ev {
                MobEvent::Attack { target, damage } => {
                    attack(
                        ctx,
                        e,
                        target,
                        damage,
                        &targets,
                        difficulty,
                        "minecraft:mob_attack",
                        "death.attack.mob",
                    );
                }
                MobEvent::Shoot { target, damage } => {
                    attack(
                        ctx,
                        e,
                        target,
                        damage,
                        &targets,
                        difficulty,
                        "minecraft:arrow",
                        "death.attack.arrow",
                    );
                }
                MobEvent::Explode { power } => {
                    e.removed = true;
                    removed.push(id);
                    blasts.push(Blast {
                        center: [e.pos[0], e.pos[1] + 0.0625, e.pos[2]],
                        power,
                        source: id,
                        owner: None,
                    });
                }
                MobEvent::Egg => {
                    if let Some(egg) = ctx.items.id("minecraft:egg") {
                        crate::tick::spawn_item_at(
                            ctx,
                            [e.pos[0], e.pos[1] + 0.3, e.pos[2]],
                            egg,
                            1,
                        );
                    }
                }
                MobEvent::Graze { cell, block } => {
                    let to = if block {
                        ctx.data.blocks.default_state("minecraft:dirt")
                    } else {
                        Some(ctx.shapes.air())
                    };
                    if let Some(state) = to {
                        level.set_block(
                            mc_protocol::Position::new(cell[0], cell[1], cell[2]),
                            state,
                        );
                    }
                }
                MobEvent::Status(s) => status.push((id, s)),
                MobEvent::Hurt { amount, .. } => {
                    if let Kind::Mob(m) = &mut e.kind
                        && m.hurt(amount, None)
                    {
                        status.push((id, 2));
                    }
                }
            }
        }
        if e.removed {
            continue;
        }
        if let Kind::Mob(m) = &e.kind
            && m.is_dead()
            && m.dying.is_none()
        {
            die(ctx, e, false, &mut status);
        }
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if (e.pos, e.on_ground, m.yaw, m.pitch, m.head_yaw)
            != (before.0, before.1, before.2, before.3, before.4)
        {
            moves.push(MobMove {
                id: e.id,
                pos: e.pos,
                yaw: m.yaw,
                pitch: m.pitch,
                head_yaw: m.head_yaw,
                on_ground: e.on_ground,
            });
        }
        if std::mem::take(&mut m.dirty) || (m.burning > 0) != before.5 {
            data.push(SetEntityData {
                entity_id: e.id,
                entries: metadata_of(m),
            });
        }
        if e.removed {
            removed.push(e.id);
        }
    }

    for (id, _) in crate::explosion::explode_all(ctx, level, &mut entities, &blasts) {
        removed.push(id);
    }
    // Mobs hurt by the blast.
    for e in entities.iter_mut() {
        if let Kind::Mob(m) = &e.kind
            && m.is_dead()
            && m.dying.is_none()
        {
            die(ctx, e, false, &mut status);
        }
    }

    let spawned = spawn::tick(ctx, &ground, &mut entities, &targets, now, day, difficulty);
    for e in &spawned {
        let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    }
    entities.extend(spawned);
    removed.extend(spawn::despawn(&mut entities, &targets, difficulty));
    drop(ground);

    for e in entities.iter().filter(|e| e.removed) {
        if matches!(e.kind, Kind::Mob(_)) && !removed.contains(&e.id) {
            removed.push(e.id);
        }
    }
    entities.retain(|e| !e.removed);
    let mut stored = lock(&ctx.entities);
    entities.append(&mut stored);
    *stored = entities;
    drop(stored);

    if !moves.is_empty() {
        let _ = ctx.events.send(Event::MobsMoved(moves));
    }
    if !status.is_empty() {
        let _ = ctx.events.send(Event::MobsStatus(status));
    }
    if !data.is_empty() {
        let _ = ctx.events.send(Event::EntitiesData(data));
    }
    if !removed.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(removed));
    }
}

/// A mob's blow or arrow at a player.
#[allow(clippy::too_many_arguments)]
fn attack(
    ctx: &ServerContext,
    mob: &Entity,
    target: i32,
    damage: f32,
    targets: &[Target],
    difficulty: i32,
    damage_type: &str,
    death_key: &str,
) {
    let Kind::Mob(m) = &mob.kind else {
        return;
    };
    let Some(t) = targets.iter().find(|t| t.id == target) else {
        return;
    };
    let amount = crate::damage::scale_for_difficulty(damage, difficulty);
    crate::vitals::try_hurt_by_entity(
        ctx,
        t.uuid,
        amount,
        damage_type,
        death_key,
        Some(m.species.display_name()),
        Some(mob.id),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darkness_follows_the_day() {
        assert_eq!(sky_darken(1000), 0);
        assert_eq!(sky_darken(18000), 11);
        assert!(sky_darken(13000) > 0 && sky_darken(13000) < 11);
        assert_eq!(sky_darken(24000 + 100), 0);
    }

    /// A superflat server context, `None` without the game data.
    fn flat() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    /// Puts a survival player at `pos` and loads the chunks around.
    fn join(ctx: &ServerContext, pos: [f64; 3]) -> u128 {
        let uuid = 0xfeed;
        for cx in -2..=2 {
            for cz in -2..=2 {
                let _ = ctx.world.block(
                    mc_protocol::Position::new(cx * 16, 0, cz * 16),
                    &ctx.data.blocks,
                );
            }
        }
        lock(&ctx.players).insert(
            uuid,
            crate::context::OnlinePlayer {
                profile: mc_protocol::packets::GameProfile {
                    uuid,
                    name: "Steve".into(),
                    properties: Vec::new(),
                },
                game_mode: 0,
                entity_id: 7000,
                skin_parts: 0,
                pose: Some(crate::context::Pose {
                    x: pos[0],
                    y: pos[1],
                    z: pos[2],
                    yaw: 0.0,
                    pitch: 0.0,
                    on_ground: true,
                }),
                dimension: "minecraft:overworld".into(),
                main_hand: None,
                flying: false,
                address: None,
                extra: Default::default(),
            },
        );
        lock(&ctx.vitals).insert(uuid, crate::vitals::Vitals::default());
        uuid
    }

    fn run(ctx: &ServerContext, ticks: usize) {
        for _ in 0..ticks {
            lock(&ctx.time).age += 1;
            let mut level = WorldLevel::new(ctx);
            tick(ctx, &mut level);
        }
    }

    fn is_monster_entity(e: &Entity) -> bool {
        matches!(&e.kind, Kind::Mob(m) if m.species.category() == mc_entity::mob::Category::Monster)
    }

    fn mob_of(ctx: &ServerContext, id: i32) -> Option<Entity> {
        lock(&ctx.entities).iter().find(|e| e.id == id).cloned()
    }

    #[test]
    fn a_zombie_comes_for_the_player_and_hurts_them() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let uuid = join(&ctx, [0.5, -60.0, 0.5]);
        let zombie = new_mob(&ctx, Species::Zombie, [12.5, -60.0, 0.5]);
        let id = zombie.id;
        lock(&ctx.entities).push(zombie);
        run(&ctx, 200);
        let health = lock(&ctx.vitals)[&uuid].health;
        assert!(health < 20.0, "health {health}");
        let z = mob_of(&ctx, id).expect("zombie still there");
        assert!(z.pos[0] < 6.0, "{:?}", z.pos);
    }

    #[test]
    fn hitting_a_cow_kills_it_and_it_drops_loot() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let _ = join(&ctx, [0.5, -60.0, 0.5]);
        let cow = new_mob(&ctx, Species::Cow, [2.5, -60.0, 0.5]);
        let id = cow.id;
        lock(&ctx.entities).push(cow);
        run(&ctx, 5);
        let by = Actor {
            uuid: 0xfeed,
            entity_id: 7000,
            name: "Steve".into(),
            pos: [0.5, -60.0, 0.5],
        };
        ctx.mobs.hit(id, by.clone(), 4.0);
        run(&ctx, 1);
        let c = mob_of(&ctx, id).unwrap();
        let Kind::Mob(m) = &c.kind else { panic!() };
        assert_eq!(m.health, 6.0);
        // The animal runs.
        let before = c.pos;
        run(&ctx, 40);
        let after = mob_of(&ctx, id).unwrap().pos;
        assert!(
            (after[0] - before[0]).hypot(after[2] - before[2]) > 1.5,
            "{before:?} {after:?}"
        );
        let near = Actor { pos: after, ..by };
        ctx.mobs.hit(id, near, 100.0);
        run(&ctx, 1);
        let items = lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(e.kind, Kind::Item { .. }))
            .count();
        assert!(items > 0, "no drops");
        // The body goes after a second.
        run(&ctx, 25);
        assert!(mob_of(&ctx, id).is_none());
    }

    #[test]
    fn animals_and_monsters_spawn_naturally() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let _ = join(&ctx, [0.5, -60.0, 0.5]);
        run(&ctx, 400);
        let all: Vec<_> = lock(&ctx.entities).clone();
        let animals = all
            .iter()
            .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species.category() == mc_entity::mob::Category::Creature))
            .count();
        // A superflat plains world: animals on the grass.
        assert!(animals > 0, "no animals among {}", all.len());
        // Monsters never appear within 24 blocks of the player.
        for e in all.iter().filter(|e| is_monster_entity(e)) {
            let d = ((e.pos[0] - 0.5).powi(2) + (e.pos[2] - 0.5).powi(2)).sqrt();
            assert!(d > 20.0 || e.age > 0, "spawned too close: {:?}", e.pos);
        }
    }

    #[test]
    fn shears_take_the_wool() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let uuid = join(&ctx, [0.5, -60.0, 0.5]);
        let mut sheep = new_mob(&ctx, Species::Sheep, [1.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut sheep.kind {
            m.set_color(0);
        }
        let id = sheep.id;
        lock(&ctx.entities).push(sheep);
        let shears = ctx.items.id("minecraft:shears").unwrap();
        {
            let mut inv = lock(&ctx.inventories);
            let hands = inv.entry(uuid).or_default();
            let slot = hands.hand_slot(0);
            hands.slots[slot] = Some(mc_protocol::packets::play::ItemStack::new(shears, 1));
        }
        let by = Actor {
            uuid,
            entity_id: 7000,
            name: "Steve".into(),
            pos: [0.5, -60.0, 0.5],
        };
        ctx.mobs.use_on(id, by.clone(), 0);
        run(&ctx, 1);
        let Kind::Mob(m) = &mob_of(&ctx, id).unwrap().kind else {
            panic!()
        };
        assert!(m.sheared);
        let wool = ctx.items.id("minecraft:white_wool").unwrap();
        let dropped: i32 = lock(&ctx.entities)
            .iter()
            .filter_map(|e| match e.kind {
                Kind::Item { item, count, .. } if item == wool => Some(count),
                _ => None,
            })
            .sum();
        assert!((1..=3).contains(&dropped), "{dropped}");
        let worn = lock(&ctx.inventories)[&uuid].slots[36]
            .as_ref()
            .map(|s| s.damage);
        assert_eq!(worn, Some(1));
        // Shearing again does nothing.
        ctx.mobs.use_on(id, by, 0);
        run(&ctx, 1);
        let again: i32 = lock(&ctx.entities)
            .iter()
            .filter_map(|e| match e.kind {
                Kind::Item { item, count, .. } if item == wool => Some(count),
                _ => None,
            })
            .sum();
        assert_eq!(again, dropped);
    }

    #[test]
    fn a_creeper_blows_a_hole_near_the_player() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let uuid = join(&ctx, [0.5, -60.0, 0.5]);
        let creeper = new_mob(&ctx, Species::Creeper, [5.5, -60.0, 0.5]);
        let id = creeper.id;
        lock(&ctx.entities).push(creeper);
        run(&ctx, 160);
        assert!(
            mob_of(&ctx, id).is_none(),
            "the creeper should have blown up"
        );
        assert!(lock(&ctx.vitals)[&uuid].health < 20.0);
        let ground = ctx
            .world
            .block(mc_protocol::Position::new(0, -61, 0), &ctx.data.blocks)
            .unwrap();
        let _ = ground;
    }

    #[test]
    fn metadata_describes_the_species() {
        let mut sheep = Mob::new(Species::Sheep, 3);
        sheep.set_color(14);
        sheep.shear();
        let v = metadata_of(&sheep);
        assert!(v.contains(&(field::SHEEP_WOOL, MetadataValue::Byte(14 | 16))));
        let creeper = Mob::new(Species::Creeper, 3);
        assert!(
            metadata_of(&creeper)
                .iter()
                .any(|(i, _)| *i == field::CREEPER_IGNITED)
        );
    }
}
