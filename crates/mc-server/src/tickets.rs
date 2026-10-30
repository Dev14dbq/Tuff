//! Chunk tickets and the level each loaded chunk is at (minecraft.wiki,
//! "Chunk", the sections on tickets and load levels). A ticket names a
//! level for its own chunk; every chunk around it is one level higher per
//! step of chebyshev distance, out to 44, and a chunk keeps the lowest
//! level any ticket gives it. Rings are squares: a diagonal step counts the
//! same as a step along an axis.
//!
//! What a level allows, measured against vanilla 26.3
//! (`docs/specs/chunk-tickets-26.3.md`):
//!
//! | level | what runs |
//! |---|---|
//! | ≤ 31 | everything, including entities and random ticks |
//! | 32 | block entities, scheduled ticks, fluids; not entities |
//! | 33 | loaded and readable, nothing ticks |
//! | 34–44 | not kept in memory |
//!
//! Random ticks reach one chunk further than entities when
//! `simulation-distance` is greater than the view distance
//! (`min(sim, view + 1)`). A chunk of level 32 waits with its water, lava
//! and fire while a neighbour is only a border.

use std::collections::{HashMap, HashSet};

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use crate::context::ServerContext;
use crate::tick::lock;

/// Entity ticking and full simulation.
pub const ENTITY: i32 = 31;
/// Block entities, scheduled block ticks and fluids.
pub const BLOCK: i32 = 32;
/// In memory, nothing ticks.
pub const BORDER: i32 = 33;
/// Past this a chunk is not tracked.
pub const MAX_LEVEL: i32 = 44;
/// Ticks with no player, forceload, portal or ender pearl before the
/// dimension stops entity and block-entity ticks.
pub const IDLE_LIMIT: i32 = 300;

const PORTAL_LEVEL: i32 = 30;
const PORTAL_RADIUS: i32 = 3;
const PORTAL_TICKS: i32 = 300;
const SPAWN_LEVEL: i32 = 30;
const SPAWN_RADIUS: i32 = 3;
const SPAWN_TICKS: i32 = 20;
const PEARL_RADIUS: i32 = 2;
const DRAGON_LEVEL: i32 = 24;
const DRAGON_RADIUS: i32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Portal,
    Spawn,
    Search,
    Unknown,
}

#[derive(Clone, Debug)]
struct Timed {
    kind: Kind,
    x: i32,
    z: i32,
    level: i32,
    radius: i32,
    left: i32,
}

/// One dimension's tickets, the level of every chunk they reach, and how
/// long the dimension has been without a player, a forceload, a portal or
/// an ender pearl.
#[derive(Debug, Default)]
pub struct Map {
    timed: Vec<Timed>,
    /// Players who already received the spawn ticket for this visit.
    seen: HashSet<u128>,
    levels: HashMap<(i32, i32), i32>,
    random: HashSet<(i32, i32)>,
    /// The game loop has started applying levels. Before that, every loaded
    /// chunk still ticks, so tests that never run a tick are unchanged.
    live: bool,
    imported: bool,
    idle: i32,
    last_age: i64,
    /// What `levels`, `random` and `pin` were last built from; while it is
    /// the same the next rebuild would give the same result and is skipped.
    /// `None` after anything painted on its own (a granted ticket).
    built_from: Option<Painted>,
    /// The chunks kept in memory by the last build.
    pin: HashSet<(i32, i32)>,
}

/// Everything a rebuild of the levels reads (see [`Map::built_from`]).
#[derive(Debug, PartialEq)]
struct Painted {
    holders: Vec<(i32, i32)>,
    sim: i32,
    view: i32,
    forced: Vec<(i32, i32)>,
    pearls: Vec<(i32, i32)>,
    dragon: bool,
    timed: Vec<(i32, i32, i32, i32)>,
}

impl Map {
    fn add(&mut self, kind: Kind, x: i32, z: i32, level: i32, radius: i32, left: i32) {
        if let Some(t) = self
            .timed
            .iter_mut()
            .find(|t| t.kind == kind && t.x == x && t.z == z)
        {
            t.left = t.left.max(left);
            t.level = t.level.min(level);
            t.radius = t.radius.max(radius);
            return;
        }
        self.timed.push(Timed {
            kind,
            x,
            z,
            level,
            radius,
            left,
        });
    }

    fn grant(&mut self, kind: Kind, x: i32, z: i32, level: i32, radius: i32, left: i32) {
        self.add(kind, x, z, level, radius, left);
        self.built_from = None;
        // Visible immediately. The next tick rebuilds levels from `timed`,
        // so a one-tick ticket added after this tick's rebuild still applies
        // until that rebuild drops it.
        self.paint(x, z, level, radius);
    }

    fn expire(&mut self) {
        for t in &mut self.timed {
            t.left -= 1;
        }
        self.timed.retain(|t| t.left > 0);
    }

    /// `level` on the square of chebyshev radius `radius`, then one higher
    /// per step outside it, through [`MAX_LEVEL`]. A border ticket does not
    /// spread: its neighbours would be inaccessible and are not kept.
    fn paint(&mut self, x: i32, z: i32, level: i32, radius: i32) {
        if level >= BORDER {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    self.relax(x + dx, z + dz, level);
                }
            }
            return;
        }
        let reach = MAX_LEVEL - level;
        let far = radius + reach;
        for dz in -far..=far {
            for dx in -far..=far {
                let outside = (dx.abs().max(dz.abs()) - radius).max(0);
                let at = level + outside;
                if at <= MAX_LEVEL {
                    self.relax(x + dx, z + dz, at);
                }
            }
        }
    }

    fn relax(&mut self, x: i32, z: i32, level: i32) {
        match self.levels.get_mut(&(x, z)) {
            Some(old) if *old <= level => {}
            Some(old) => *old = level,
            None => {
                self.levels.insert((x, z), level);
            }
        }
    }

    fn fill_random(&mut self, players: &[(i32, i32)], sim: i32, view: i32) {
        self.random.clear();
        for (&chunk, &level) in &self.levels {
            if level <= ENTITY {
                self.random.insert(chunk);
            }
        }
        // Wheat grows one chunk past the entities when simulation distance
        // is allowed to exceed the view (`chunk-tickets-26.3.md`).
        if sim > view {
            let r = sim.min(view + 1);
            for &(cx, cz) in players {
                for dz in -r..=r {
                    for dx in -r..=r {
                        let chunk = (cx + dx, cz + dz);
                        if self.levels.get(&chunk).copied().unwrap_or(99) <= BLOCK {
                            self.random.insert(chunk);
                        }
                    }
                }
            }
        }
    }

    fn portal_tags(&self) -> Vec<Tag> {
        self.timed
            .iter()
            .filter(|t| t.kind == Kind::Portal && t.left > 0)
            .map(|t| {
                let mut c = Compound::new();
                c.insert("chunk_pos", Tag::IntArray(vec![t.x, t.z]));
                c.insert("level", Tag::Int(t.level));
                c.insert("radius", Tag::Int(t.radius));
                c.insert("ticks_left", Tag::Int(t.left));
                c.insert("type", Tag::String("minecraft:portal".into()));
                Tag::Compound(c)
            })
            .collect()
    }

    fn import_portals(&mut self, tags: &[Tag]) {
        for tag in tags {
            let Tag::Compound(c) = tag else { continue };
            let Some(Tag::IntArray(p)) = c.get("chunk_pos") else {
                continue;
            };
            if p.len() < 2 {
                continue;
            }
            let (x, z) = (p[0], p[1]);
            let level = match c.get("level") {
                Some(Tag::Int(v)) => *v,
                _ => PORTAL_LEVEL,
            };
            let radius = match c.get("radius") {
                Some(Tag::Int(v)) => *v,
                _ => 0,
            };
            let left = match c.get("ticks_left") {
                Some(Tag::Int(v)) => *v,
                _ => PORTAL_TICKS,
            };
            if left > 0 {
                self.add(Kind::Portal, x, z, level, radius, left);
            }
        }
    }
}

fn chunk_of(v: f64) -> i32 {
    (v.floor() as i32) >> 4
}

/// The rounded view vanilla sends (`connection::in_view`).
fn in_view(radius: i32, dx: i32, dz: i32) -> bool {
    let beyond = |d: i32| i64::from((d.abs() - 2).max(0));
    beyond(dx).pow(2) + beyond(dz).pow(2) < i64::from(radius).pow(2)
}

struct Sources {
    players: Vec<(u128, i32, i32)>,
    /// The chunks of the players that hold chunks around them: everyone
    /// but spectators while `spectators_generate_chunks` is off (vanilla
    /// still sends those what is loaded, but they keep nothing loaded or
    /// ticking).
    holders: Vec<(i32, i32)>,
    pearls: Vec<(i32, i32)>,
    dragon: bool,
    sim: i32,
    view: i32,
    age: i64,
}

fn sources(ctx: &ServerContext) -> Sources {
    let spectators_hold = ctx.game_rule_bool("spectators_generate_chunks");
    let mut holders = Vec::new();
    let players = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            let pose = p.pose?;
            let at = (chunk_of(pose.x), chunk_of(pose.z));
            if p.game_mode != 3 || spectators_hold {
                holders.push(at);
            }
            Some((p.profile.uuid, at.0, at.1))
        })
        .collect();
    let pearls = lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            mc_entity::Kind::Projectile(p) if matches!(p.shot, mc_entity::extra::Shot::Pearl) => {
                Some((chunk_of(e.pos[0]), chunk_of(e.pos[2])))
            }
            _ => None,
        })
        .collect();
    let dragon = ctx.world.dimension == "minecraft:the_end"
        && lock(&ctx.bosses)
            .fight
            .as_ref()
            .is_some_and(|f| !f.dragon_killed || f.respawn.is_some());
    Sources {
        players,
        holders,
        pearls,
        dragon,
        sim: i32::from(ctx.config.simulation_distance),
        view: ctx.view_distance.load(std::sync::atomic::Ordering::Relaxed) as i32,
        age: lock(&ctx.time).age,
    }
}

/// Recomputes levels from whoever is here. Does not age tickets: the game
/// loop does that once per tick in [`begin`].
pub fn rebuild(ctx: &ServerContext) {
    sync(ctx, false);
}

/// One game tick of tickets: expiry, the idle counter, levels, and loading
/// anything the levels say must be in memory.
pub fn begin(ctx: &ServerContext) {
    sync(ctx, true);
    load_reached(ctx);
}

fn sync(ctx: &ServerContext, expire: bool) {
    let imported = lock(&ctx.tickets).imported;
    let mut forced = lock(&ctx.forced);
    let forced_chunks = forced.list();
    let saved = if imported {
        Vec::new()
    } else {
        forced.take_portals()
    };
    drop(forced);
    let src = sources(ctx);

    let mut map = lock(&ctx.tickets);
    if !map.imported {
        map.import_portals(&saved);
        map.imported = true;
    }
    if expire && map.last_age != src.age {
        map.last_age = src.age;
        map.expire();
        let active = !src.players.is_empty()
            || !forced_chunks.is_empty()
            || !src.pearls.is_empty()
            || map.timed.iter().any(|t| t.kind == Kind::Portal);
        if active {
            map.idle = 0;
        } else {
            map.idle = map.idle.saturating_add(1);
        }
    }
    let ids: HashSet<u128> = src.players.iter().map(|p| p.0).collect();
    map.seen.retain(|u| ids.contains(u));
    for &(uuid, x, z) in &src.players {
        if map.seen.insert(uuid) {
            map.grant(Kind::Spawn, x, z, SPAWN_LEVEL, SPAWN_RADIUS, SPAWN_TICKS);
        }
    }
    let painted = Painted {
        holders: src.holders.clone(),
        sim: src.sim,
        view: src.view,
        forced: forced_chunks.clone(),
        pearls: src.pearls.clone(),
        dragon: src.dragon,
        timed: map
            .timed
            .iter()
            .map(|t| (t.x, t.z, t.level, t.radius))
            .collect(),
    };
    if map.built_from.as_ref() == Some(&painted) {
        // The same tickets paint the same levels.
        map.live = map.live || expire;
        let pin = map.pin.clone();
        drop(map);
        ctx.world.set_pinned(pin);
        return;
    }
    map.levels.clear();
    let s = src.sim.min(src.view).max(0);
    let player_chunks = &src.holders;
    for &(cx, cz) in player_chunks {
        map.paint(cx, cz, ENTITY - s, 0);
        let reach = src.view + 1;
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                if in_view(src.view, dx, dz) {
                    map.paint(cx + dx, cz + dz, BORDER, 0);
                }
            }
        }
    }
    for &(x, z) in &forced_chunks {
        map.paint(x, z, ENTITY, 0);
    }
    for &(x, z) in &src.pearls {
        map.paint(x, z, ENTITY, PEARL_RADIUS);
    }
    if src.dragon {
        map.paint(0, 0, DRAGON_LEVEL, DRAGON_RADIUS);
    }
    let timed = map.timed.clone();
    for t in &timed {
        map.paint(t.x, t.z, t.level, t.radius);
    }
    map.fill_random(player_chunks, src.sim, src.view);
    map.live = map.live || expire;
    let pin: HashSet<(i32, i32)> = map
        .levels
        .iter()
        .filter(|(_, level)| **level <= BORDER)
        .map(|(&c, _)| c)
        .collect();
    map.pin = pin.clone();
    map.built_from = Some(painted);
    drop(map);
    ctx.world.set_pinned(pin);
}

fn load_reached(ctx: &ServerContext) {
    let needed: Vec<(i32, i32)> = {
        let map = lock(&ctx.tickets);
        map.levels
            .iter()
            .filter(|(_, level)| **level <= BORDER)
            .map(|(&c, _)| c)
            .collect()
    };
    let y = ctx.world.min_y;
    let blocks = &ctx.data.blocks;
    for (x, z) in needed {
        if !ctx.world.chunk_loaded(x, z) {
            // Something needed this chunk before it was in memory.
            unknown(ctx, x, z);
            let _ = ctx.world.block(Position::new(x << 4, y, z << 4), blocks);
        }
    }
}

/// Levels are in force (a tick has started). Before that, loaded chunks
/// behave as they did without tickets.
pub fn limiting(ctx: &ServerContext) -> bool {
    lock(&ctx.tickets).live
}

/// No player, forceload, portal or pearl for [`IDLE_LIMIT`] ticks.
pub fn asleep(ctx: &ServerContext) -> bool {
    let map = lock(&ctx.tickets);
    map.live && map.idle >= IDLE_LIMIT
}

pub fn entities_run(ctx: &ServerContext, x: i32, z: i32) -> bool {
    let map = lock(&ctx.tickets);
    if !map.live || map.idle >= IDLE_LIMIT {
        return !map.live;
    }
    map.levels.get(&(x, z)).copied().unwrap_or(99) <= ENTITY
}

pub fn blocks_run(ctx: &ServerContext, x: i32, z: i32) -> bool {
    let map = lock(&ctx.tickets);
    if !map.live || map.idle >= IDLE_LIMIT {
        return !map.live;
    }
    map.levels.get(&(x, z)).copied().unwrap_or(99) <= BLOCK
}

/// Whether this dimension has any chunk where entities tick. Forceload
/// keeps that true with nobody online.
pub fn has_entity_chunks(ctx: &ServerContext) -> bool {
    let map = lock(&ctx.tickets);
    map.live && map.levels.values().any(|level| *level <= ENTITY)
}

/// Chunks that receive random ticks, in a stable order.
pub fn random_chunks(ctx: &ServerContext) -> Vec<(i32, i32)> {
    let mut chunks: Vec<(i32, i32)> = lock(&ctx.tickets).random.iter().copied().collect();
    chunks.sort_unstable();
    chunks
}

/// A scheduled tick that has to wait: the chunk is not block-ticking, or it
/// is the lazy ring and this is water, lava or fire beside a border.
pub fn scheduled_waits(ctx: &ServerContext, x: i32, z: i32, fluid: bool, fire: bool) -> bool {
    let map = lock(&ctx.tickets);
    if !map.live {
        return false;
    }
    let level = map.levels.get(&(x, z)).copied().unwrap_or(99);
    if level > BLOCK {
        return true;
    }
    if level < BLOCK {
        return false;
    }
    let edge = [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .any(|(dx, dz)| map.levels.get(&(x + dx, z + dz)).copied().unwrap_or(99) > BLOCK);
    drop(map);
    edge && (fluid || fire)
}

/// The player is arriving or respawning: their neighbourhood stays fully
/// loaded for a second.
pub fn player_spawn(ctx: &ServerContext, x: f64, z: f64) {
    lock(&ctx.tickets).grant(
        Kind::Spawn,
        chunk_of(x),
        chunk_of(z),
        SPAWN_LEVEL,
        SPAWN_RADIUS,
        SPAWN_TICKS,
    );
}

/// One tick of access while a spawn point is being looked at.
pub fn spawn_search(ctx: &ServerContext, x: i32, z: i32) {
    lock(&ctx.tickets).grant(Kind::Search, x >> 4, z >> 4, BORDER, 0, 1);
}

/// One tick for a chunk something asked for before it was ready.
pub fn unknown(ctx: &ServerContext, x: i32, z: i32) {
    lock(&ctx.tickets).grant(Kind::Unknown, x, z, BORDER, 0, 1);
}

/// An entity came through a portal or an end gateway. The far side stays
/// loaded for 15 seconds, saved with the forceload tickets.
pub fn portal(ctx: &ServerContext, x: f64, z: f64) {
    lock(&ctx.tickets).grant(
        Kind::Portal,
        chunk_of(x),
        chunk_of(z),
        PORTAL_LEVEL,
        PORTAL_RADIUS,
        PORTAL_TICKS,
    );
}

/// Writes forceload and portal tickets. Imports anything still only in the
/// file so a save before the first tick does not drop it.
pub fn save(ctx: &ServerContext) {
    sync(ctx, false);
    let extra = lock(&ctx.tickets).portal_tags();
    lock(&ctx.forced).save_with(&extra);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_at(map: &Map, x: i32, z: i32) -> Option<i32> {
        map.levels.get(&(x, z)).copied()
    }

    #[test]
    fn one_ticket_spreads_in_squares() {
        let mut map = Map::default();
        map.paint(0, 0, ENTITY, 0);
        assert_eq!(level_at(&map, 0, 0), Some(31));
        assert_eq!(level_at(&map, 1, 0), Some(32));
        assert_eq!(
            level_at(&map, 1, 1),
            Some(32),
            "a diagonal step is one step"
        );
        assert_eq!(level_at(&map, 2, 0), Some(33));
        assert_eq!(level_at(&map, 13, 0), Some(44));
        assert_eq!(level_at(&map, 14, 0), None);
    }

    #[test]
    fn a_chunk_keeps_the_lower_level() {
        let mut map = Map::default();
        map.paint(0, 0, ENTITY, 0);
        map.paint(2, 0, 30, 0);
        assert_eq!(level_at(&map, 1, 0), Some(31));
        assert_eq!(level_at(&map, 2, 0), Some(30));
    }

    #[test]
    fn tickets_expire_and_the_level_goes_with_them() {
        let mut map = Map::default();
        map.add(Kind::Unknown, 0, 0, BORDER, 0, 1);
        map.paint(0, 0, BORDER, 0);
        assert_eq!(level_at(&map, 0, 0), Some(33));
        map.expire();
        let timed = map.timed.clone();
        map.levels.clear();
        for t in &timed {
            map.paint(t.x, t.z, t.level, t.radius);
        }
        assert!(timed.is_empty());
        assert_eq!(level_at(&map, 0, 0), None);
    }

    #[test]
    fn simulation_is_a_square_capped_by_the_view() {
        let mut map = Map::default();
        // sim 4, view 10: entities out to 4, blocks one further, random
        // ticks stop with the entities.
        map.paint(0, 0, ENTITY - 4, 0);
        map.fill_random(&[(0, 0)], 4, 10);
        assert!(level_at(&map, 4, -4).unwrap() <= ENTITY);
        assert_eq!(level_at(&map, 5, 0), Some(BLOCK));
        assert!(map.random.contains(&(4, -4)));
        assert!(!map.random.contains(&(5, 0)));
    }

    #[test]
    fn random_ticks_reach_the_extra_ring_when_simulation_exceeds_view() {
        let mut map = Map::default();
        // view 3, sim 8: entities to 3, wheat and furnaces to 4.
        map.paint(0, 0, ENTITY - 3, 0);
        map.fill_random(&[(0, 0)], 8, 3);
        assert!(level_at(&map, 3, 0).unwrap() <= ENTITY);
        assert_eq!(level_at(&map, 4, 0), Some(BLOCK));
        assert!(map.random.contains(&(4, 0)));
        assert!(map.levels.get(&(4, 0)).is_none_or(|l| *l > ENTITY));
    }

    #[test]
    fn forceload_ticks_with_nobody_there() {
        let mut map = Map::default();
        map.paint(100, -40, ENTITY, 0);
        map.fill_random(&[], 10, 10);
        assert!(map.random.contains(&(100, -40)));
        assert_eq!(level_at(&map, 100, -40), Some(ENTITY));
        assert_eq!(level_at(&map, 101, -40), Some(BLOCK));
    }

    #[test]
    fn a_spectator_holds_chunks_only_with_spectators_generate_chunks() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        crate::testkit::join(&ctx, 1, 70, [1000.5, -60.0, 1000.5], 3);
        // Let the ticket of the join run out.
        for _ in 0..=SPAWN_TICKS {
            lock(&ctx.time).age += 1;
            begin(&ctx);
        }
        assert!(entities_run(&ctx, chunk_of(1000.5), chunk_of(1000.5)));
        ctx.game_rules.lock().expect("rules").set(
            "spectators_generate_chunks",
            crate::rules::Value::Bool(false),
        );
        rebuild(&ctx);
        assert!(!entities_run(&ctx, chunk_of(1000.5), chunk_of(1000.5)));
    }

    #[test]
    fn a_portal_covers_a_square_then_spreads() {
        let mut map = Map::default();
        map.paint(0, 0, PORTAL_LEVEL, PORTAL_RADIUS);
        assert_eq!(level_at(&map, 3, 3), Some(PORTAL_LEVEL));
        assert_eq!(level_at(&map, 4, 0), Some(PORTAL_LEVEL + 1));
    }

    #[test]
    fn the_view_circle_is_loaded_and_does_not_tick() {
        let mut map = Map::default();
        map.paint(0, 0, ENTITY - 4, 0);
        let reach = 11;
        for dz in -reach..=reach {
            for dx in -reach..=reach {
                if in_view(10, dx, dz) {
                    map.paint(dx, dz, BORDER, 0);
                }
            }
        }
        // On the axis, view 10 still includes chunk 11, which the
        // simulation square does not reach.
        assert_eq!(level_at(&map, 0, 11), Some(BORDER));
        assert!(level_at(&map, 0, 4).unwrap() <= ENTITY);
    }
}
