//! Game events and the vibrations they make (minecraft.wiki, "Sculk
//! Sensor", "Sculk Shrieker", "Warden").
//!
//! Whatever the game does that can be heard (a step, a block placed, a door
//! opened, an arrow landing...) is a game event at a place, maybe with who
//! did it. Each tick the events go to whoever listens within range: sculk
//! sensors (8 blocks, calibrated ones 16), shriekers (the clicking of a
//! sensor within 8, when a player caused it) and wardens (16). A vibration
//! goes nowhere through wool; one made on wool (a step on a carpet, wool
//! placed) is not heard at all, nor are the quiet things a sneaking player
//! does. It flies a block a tick, a particle on the way, and does its thing
//! where it lands: a sensor turns on with a power by the distance and a
//! frequency by the event (a comparator reads it), clicks for the
//! shriekers, and makes an amethyst block beside it ring the same
//! frequency.
//!
//! A shrieker a player sets off shrieks for 4.5 seconds. A natural one (`can_summon`)
//! warns too: every player within 16 blocks goes up a level (to the
//! highest among them, plus one; at most 4) unless one of them was warned
//! in the last 10 seconds or a warden is within 48 blocks, and they get 13
//! seconds of Darkness. When the shriek ends a level below 4 answers with a
//! warden's sound from afar; at 4 a warden digs out within 5 blocks.
//! Levels drop by one after 10 minutes without a warning.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use mc_entity::{Kind, Species};
use mc_protocol::Position;

use crate::context::ServerContext;
use crate::effects::{self, Instance};
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;
use crate::util::random_u64;

/// Who made a game event.
#[derive(Clone, Copy, Debug, Default)]
pub struct Source {
    /// The entity that did it (a player's entity id too).
    pub entity: Option<i32>,
    /// The player behind it: the one who did it, or who shot it.
    pub player: Option<u128>,
    pub sneaking: bool,
    /// A warden: sensors do not hear wardens.
    pub warden: bool,
    /// The player was in creative or spectator (a warden ignores them).
    pub unheard_by_wardens: bool,
}

impl Source {
    pub fn player(ctx: &ServerContext, uuid: u128) -> Self {
        let sneaking = crate::stats::sneaking(ctx, uuid);
        let players = lock(&ctx.players);
        let Some(p) = players.get(&uuid) else {
            return Self::default();
        };
        Self {
            entity: Some(p.entity_id),
            player: Some(uuid),
            sneaking,
            warden: false,
            unheard_by_wardens: matches!(p.game_mode, 1 | 3),
        }
    }

    pub fn entity(id: i32) -> Self {
        Self {
            entity: Some(id),
            ..Self::default()
        }
    }
}

/// One game event.
#[derive(Clone, Debug)]
pub struct GameEvent {
    pub name: &'static str,
    pub pos: [f64; 3],
    pub source: Source,
    /// The block it is about (placed, broken, stepped on), if any.
    pub state: Option<u32>,
}

/// Who a vibration is on its way to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Listener {
    Block(Position),
    Mob(i32),
}

#[derive(Clone, Debug)]
struct Flight {
    to: Listener,
    event: GameEvent,
    frequency: u8,
    distance: f64,
    arrives: i64,
}

/// A vibration that reached a warden.
#[derive(Clone, Debug)]
pub struct Heard {
    pub name: &'static str,
    pub pos: [f64; 3],
    pub source: Source,
}

/// A player's warnings from shriekers (vanilla's `warden_spawn_tracker`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Warnings {
    pub level: i32,
    pub since: i32,
    pub cooldown: i32,
}

#[derive(Debug, Default)]
pub struct Vibrations {
    pending: Vec<GameEvent>,
    flights: Vec<Flight>,
    /// Blocks that hear, by chunk.
    listeners: HashMap<(i32, i32), HashSet<Position>>,
    /// The frequency each sensor heard last.
    frequencies: HashMap<Position, u8>,
    pub warnings: HashMap<u128, Warnings>,
    /// Shriekers due to answer: where, when, the warning level.
    answers: Vec<(Position, i64, i32)>,
}

pub const SENSOR: &str = mc_tick::sculk::SENSOR;
pub const CALIBRATED: &str = mc_tick::sculk::CALIBRATED;
pub const SHRIEKER: &str = mc_tick::sculk::SHRIEKER;
pub const CATALYST: &str = "minecraft:sculk_catalyst";

/// Blocks that hear vibrations or watch deaths.
pub fn is_listener_block(name: &str) -> bool {
    matches!(name, SENSOR | CALIBRATED | SHRIEKER | CATALYST)
}

/// The frequency of a game event (0 for one that is no vibration).
pub fn frequency(name: &str) -> u8 {
    if let Some(n) = name.strip_prefix("resonate_") {
        return n.parse().unwrap_or(0);
    }
    match name {
        "step" | "swim" | "flap" => 1,
        "projectile_land" | "hit_ground" | "bounce" | "splash" => 2,
        "item_interact_finish" | "projectile_shoot" | "instrument_play" => 3,
        "entity_action" | "elytra_glide" | "unequip" => 4,
        "entity_dismount" | "equip" => 5,
        "entity_mount" | "entity_interact" | "shear" => 6,
        "entity_damage" => 7,
        "drink" | "eat" => 8,
        "container_close" | "block_close" | "block_deactivate" | "block_detach" => 9,
        "container_open" | "block_open" | "block_activate" | "block_attach" | "prime_fuse"
        | "note_block_play" => 10,
        "block_change" => 11,
        "block_destroy" | "fluid_pickup" => 12,
        "block_place" | "fluid_place" => 13,
        "entity_place" | "lightning_strike" | "teleport" => 14,
        "entity_die" | "explode" => 15,
        _ => 0,
    }
}

struct Tags {
    vibrations: HashSet<String>,
    warden: HashSet<String>,
    sneaking: HashSet<String>,
    occludes: HashSet<String>,
    dampens: HashSet<String>,
    resonators: HashSet<String>,
}

fn tags(ctx: &ServerContext) -> &'static Tags {
    static TAGS: OnceLock<Tags> = OnceLock::new();
    TAGS.get_or_init(|| {
        let env = &ctx.commands.env;
        let set = |registry: &str, tag: &str| -> HashSet<String> {
            env.tag(registry, tag).into_iter().collect()
        };
        Tags {
            vibrations: set("minecraft:game_event", "minecraft:vibrations"),
            warden: set("minecraft:game_event", "minecraft:warden_can_listen"),
            sneaking: set(
                "minecraft:game_event",
                "minecraft:ignore_vibrations_sneaking",
            ),
            occludes: set("minecraft:block", "minecraft:occludes_vibration_signals"),
            dampens: set("minecraft:block", "minecraft:dampens_vibrations"),
            resonators: set("minecraft:block", "minecraft:vibration_resonators"),
        }
    })
}

fn full(name: &str) -> String {
    format!("minecraft:{name}")
}

/// Something that can be heard happened.
pub fn emit(
    ctx: &ServerContext,
    name: &'static str,
    pos: [f64; 3],
    source: Source,
    state: Option<u32>,
) {
    lock(&ctx.vibrations).pending.push(GameEvent {
        name,
        pos,
        source,
        state,
    });
}

/// A game event at the centre of a block.
pub fn emit_at(
    ctx: &ServerContext,
    name: &'static str,
    p: Position,
    source: Source,
    state: Option<u32>,
) {
    emit(ctx, name, centre(p), source, state);
}

fn centre(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}

fn block_of(p: [f64; 3]) -> Position {
    Position::new(
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    )
}

/// What the comparator by a sensor reads: the frequency it heard last.
pub fn comparator(ctx: &ServerContext, p: Position) -> Option<u8> {
    let state = ctx.world.block_if_loaded(p)?;
    let name = ctx.data.blocks.block_name(state)?;
    if !matches!(name, SENSOR | CALIBRATED) {
        return None;
    }
    Some(
        lock(&ctx.vibrations)
            .frequencies
            .get(&p)
            .copied()
            .unwrap_or(0),
    )
}

/// Catches the listening blocks up with chunks read and blocks changed.
fn sync(ctx: &ServerContext, v: &mut Vibrations) {
    let log = ctx.world.take_sculk_log();
    let blocks = &ctx.data.blocks;
    let hears = |s: u32| blocks.block_name(s).is_some_and(is_listener_block);
    for (cx, cz) in log.loaded {
        let found = ctx.world.peek_chunk(cx, cz, |chunk| {
            let mut out = HashSet::new();
            for (i, section) in chunk.sections.iter().enumerate() {
                if !section.blocks.may_contain(hears) {
                    continue;
                }
                let base = chunk.min_y + 16 * i as i32;
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            if hears(section.block(x, y, z)) {
                                out.insert(Position::new(
                                    cx * 16 + x as i32,
                                    base + y as i32,
                                    cz * 16 + z as i32,
                                ));
                            }
                        }
                    }
                }
            }
            out
        });
        if let Some(found) = found {
            v.listeners.insert((cx, cz), found);
        }
    }
    for p in log.changed {
        let set = v.listeners.entry((p.x >> 4, p.z >> 4)).or_default();
        match ctx.world.block_if_loaded(p) {
            Some(s) if hears(s) => {
                set.insert(p);
            }
            _ => {
                set.remove(&p);
                v.frequencies.remove(&p);
            }
        }
    }
}

impl Vibrations {
    /// Catalysts (or any listening block) within `range` of `at`, nearest
    /// first.
    pub fn catalysts_near(&self, at: [f64; 3], range: f64) -> Vec<Position> {
        let mut out = near(self, at, range);
        out.sort_by(|a, b| dist(centre(*a), at).total_cmp(&dist(centre(*b), at)));
        out
    }
}

/// Blocks that hear within `range` of `at`.
fn near(v: &Vibrations, at: [f64; 3], range: f64) -> Vec<Position> {
    let r = range.ceil() as i32;
    let (bx, bz) = (at[0].floor() as i32, at[2].floor() as i32);
    let mut out = Vec::new();
    for cx in (bx - r) >> 4..=(bx + r) >> 4 {
        for cz in (bz - r) >> 4..=(bz + r) >> 4 {
            if let Some(set) = v.listeners.get(&(cx, cz)) {
                out.extend(
                    set.iter()
                        .copied()
                        .filter(|p| dist(centre(*p), at) <= range),
                );
            }
        }
    }
    out
}

/// Whether wool stands between the two points (the cells on the line,
/// stepped finely; the end cells do not count).
fn occluded(ctx: &ServerContext, from: [f64; 3], to: [f64; 3]) -> bool {
    let occludes = &tags(ctx).occludes;
    let length = dist(from, to);
    let steps = (length / 0.1).ceil().max(1.0) as i32;
    let (start, end) = (block_of(from), block_of(to));
    let mut last = start;
    for i in 1..steps {
        let t = f64::from(i) / f64::from(steps);
        let p = block_of([
            from[0] + (to[0] - from[0]) * t,
            from[1] + (to[1] - from[1]) * t,
            from[2] + (to[2] - from[2]) * t,
        ]);
        if p == last || p == start || p == end {
            continue;
        }
        last = p;
        let blocked = ctx
            .world
            .block_if_loaded(p)
            .and_then(|s| ctx.data.blocks.block_name(s))
            .is_some_and(|n| occludes.contains(n));
        if blocked {
            return true;
        }
    }
    false
}

/// Whether an event may be heard at all: not on wool, not the quiet
/// things a sneaking player does.
fn audible(ctx: &ServerContext, e: &GameEvent) -> bool {
    let t = tags(ctx);
    if e.source.sneaking && t.sneaking.contains(&full(e.name)) {
        return false;
    }
    let damped = e
        .state
        .and_then(|s| ctx.data.blocks.block_name(s))
        .is_some_and(|n| t.dampens.contains(n));
    !damped
}

/// The particle of a vibration flying to a block or an entity.
fn particle(ctx: &ServerContext, from: [f64; 3], to: Listener, ticks: i32) {
    use mc_protocol::Write;
    let Some(id) = ctx
        .data
        .builtin
        .id("minecraft:particle_type", "minecraft:vibration")
    else {
        return;
    };
    let mut data = Vec::new();
    data.put_varint(id);
    match to {
        Listener::Block(p) => {
            data.put_varint(0);
            data.put_position(p);
        }
        Listener::Mob(entity) => {
            data.put_varint(1);
            data.put_varint(entity);
            data.put_f32(2.5);
        }
    }
    data.put_varint(ticks);
    crate::fx::emit(
        ctx,
        crate::fx::Fx::Particles(mc_protocol::packets::play::LevelParticles {
            particle: data,
            force: false,
            always_show: false,
            pos: from,
            delta: [0.0; 3],
            speed: 0.0,
            count: 1,
        }),
    );
}

fn listening_warden(m: &mc_entity::mob::Mob) -> bool {
    m.species == Species::Warden && m.dying.is_none() && crate::mobs::warden::can_hear(m)
}

/// One tick: new events find their listeners, vibrations that arrived act.
pub fn tick(ctx: &ServerContext) {
    let now = lock(&ctx.time).age;
    let (events, arrived) = {
        let mut v = lock(&ctx.vibrations);
        sync(ctx, &mut v);
        let events = std::mem::take(&mut v.pending);
        let (arrived, flying): (Vec<Flight>, Vec<Flight>) = std::mem::take(&mut v.flights)
            .into_iter()
            .partition(|f| f.arrives <= now);
        v.flights = flying;
        (events, arrived)
    };
    if !events.is_empty() {
        route(ctx, events, now);
    }
    for f in arrived {
        land(ctx, f);
    }
    warnings(ctx, now);
}

/// Sends each event to the listeners that hear it: each listener takes the
/// nearest one of the tick.
fn route(ctx: &ServerContext, events: Vec<GameEvent>, now: i64) {
    let t = tags(ctx);
    let blocks = &ctx.data.blocks;
    let logic = WorldLevel::new(ctx).logic();
    let mut best: HashMap<Listener, (f64, usize)> = HashMap::new();
    let wardens: Vec<(i32, [f64; 3])> = lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if listening_warden(m) => {
                Some((e.id, [e.pos[0], e.pos[1] + 2.5, e.pos[2]]))
            }
            _ => None,
        })
        .collect();
    {
        let v = lock(&ctx.vibrations);
        let busy: HashSet<Listener> = v.flights.iter().map(|f| f.to).collect();
        let level = WorldLevel::new(ctx);
        for (i, e) in events.iter().enumerate() {
            let name = full(e.name);
            let freq = frequency(e.name);
            let offer = |to: Listener, d: f64, best: &mut HashMap<Listener, (f64, usize)>| {
                if busy.contains(&to) {
                    return;
                }
                let better = best.get(&to).is_none_or(|(bd, _)| d < *bd);
                if better {
                    best.insert(to, (d, i));
                }
            };
            if e.name == "sculk_sensor_tendrils_clicking" {
                // Shriekers hear the sensors a player set off.
                let Some(player) = e.source.player else {
                    continue;
                };
                let spectator = lock(&ctx.players)
                    .get(&player)
                    .is_none_or(|p| p.game_mode == 3);
                if spectator {
                    continue;
                }
                for p in near(&v, e.pos, 8.0) {
                    let state = ctx.world.block_if_loaded(p).unwrap_or(0);
                    if blocks.block_name(state) != Some(SHRIEKER)
                        || blocks.property(state, "shrieking") == Some("true")
                    {
                        continue;
                    }
                    let d = dist(e.pos, centre(p));
                    if !occluded(ctx, e.pos, centre(p)) {
                        offer(Listener::Block(p), d, &mut best);
                    }
                }
            }
            if !audible(ctx, e) {
                // A sneaking player kept quiet near something that hears.
                if e.source.sneaking
                    && let Some(uuid) = e.source.player
                    && !near(&v, e.pos, 16.0).is_empty()
                {
                    crate::criteria::plain(ctx, uuid, "minecraft:avoid_vibration");
                }
                continue;
            }
            if t.vibrations.contains(&name) && freq > 0 && !e.source.warden {
                for p in near(&v, e.pos, 16.0) {
                    let state = ctx.world.block_if_loaded(p).unwrap_or(0);
                    if !mc_tick::sculk::listening(logic, state) {
                        continue;
                    }
                    let range = mc_tick::sculk::range(logic, state);
                    let d = dist(e.pos, centre(p));
                    if d > range || block_of(e.pos) == p && e.name != "step" {
                        continue;
                    }
                    if mc_tick::sculk::is_calibrated(logic, state) {
                        let want = mc_tick::sculk::calibration(&level, logic, to_pos(p), state);
                        if want > 0 && want != freq {
                            continue;
                        }
                    }
                    if !occluded(ctx, e.pos, centre(p)) {
                        offer(Listener::Block(p), d, &mut best);
                    }
                }
            }
            if t.warden.contains(&name) && !e.source.warden && !e.source.unheard_by_wardens {
                for &(id, ear) in &wardens {
                    if e.source.entity == Some(id) {
                        continue;
                    }
                    let d = dist(e.pos, ear);
                    if d <= 16.0 && !occluded(ctx, e.pos, ear) {
                        offer(Listener::Mob(id), d, &mut best);
                    }
                }
            }
        }
    }
    let mut v = lock(&ctx.vibrations);
    for (to, (d, i)) in best {
        let e = &events[i];
        let ticks = d.floor() as i64;
        particle(ctx, e.pos, to, ticks as i32);
        v.flights.push(Flight {
            to,
            event: e.clone(),
            frequency: frequency(e.name),
            distance: d,
            arrives: now + ticks.max(1),
        });
    }
}

/// A vibration reached its listener.
fn land(ctx: &ServerContext, f: Flight) {
    match f.to {
        Listener::Mob(id) => {
            let heard = Heard {
                name: f.event.name,
                pos: f.event.pos,
                source: f.event.source,
            };
            crate::mobs::warden::heard(ctx, id, &heard);
        }
        Listener::Block(p) => {
            let Some(state) = ctx.world.block_if_loaded(p) else {
                return;
            };
            let blocks = &ctx.data.blocks;
            match blocks.block_name(state) {
                Some(SENSOR | CALIBRATED) => sensed(ctx, p, state, &f),
                Some(SHRIEKER) => {
                    if let Some(player) = f.event.source.player {
                        shriek(ctx, p, player);
                    }
                }
                _ => {}
            }
        }
    }
}

/// A sensor heard a vibration: on, clicking, ringing the amethyst beside.
fn sensed(ctx: &ServerContext, p: Position, state: u32, f: &Flight) {
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    let range = mc_tick::sculk::range(logic, state);
    let power = mc_tick::sculk::strength(f.distance, range);
    if !mc_tick::sculk::activate(&mut level, logic, to_pos(p), power) {
        return;
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx
            .events
            .send(crate::context::Event::BlocksChanged { by: 0, changes });
    }
    lock(&ctx.vibrations).frequencies.insert(p, f.frequency);
    let source = f.event.source;
    emit_at(
        ctx,
        "sculk_sensor_tendrils_clicking",
        p,
        source,
        Some(state),
    );
    let resonators = &tags(ctx).resonators;
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        let q = Position::new(p.x + dx, p.y + dy, p.z + dz);
        let rings = ctx
            .world
            .block_if_loaded(q)
            .and_then(|s| ctx.data.blocks.block_name(s))
            .is_some_and(|n| resonators.contains(n));
        if rings && let Some(name) = resonate(f.frequency) {
            emit_at(ctx, name, q, source, None);
            crate::fx::sound(
                ctx,
                "block.amethyst_block.resonate",
                crate::fx::category::BLOCKS,
                centre(q),
                1.0,
                1.0,
            );
        }
    }
}

fn resonate(frequency: u8) -> Option<&'static str> {
    const NAMES: [&str; 15] = [
        "resonate_1",
        "resonate_2",
        "resonate_3",
        "resonate_4",
        "resonate_5",
        "resonate_6",
        "resonate_7",
        "resonate_8",
        "resonate_9",
        "resonate_10",
        "resonate_11",
        "resonate_12",
        "resonate_13",
        "resonate_14",
        "resonate_15",
    ];
    NAMES.get(usize::from(frequency).checked_sub(1)?).copied()
}

/// Ticks of Darkness a natural shrieker gives.
const DARKNESS_TICKS: i32 = 260;
/// Ticks between two warnings of one player.
const WARN_COOLDOWN: i32 = 200;
/// Ticks without a warning before a level is forgotten.
const FORGET_TICKS: i32 = 12_000;

fn players_near(ctx: &ServerContext, at: [f64; 3], range: f64) -> Vec<u128> {
    lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension && p.game_mode != 3)
        .filter(|p| p.pose.is_some_and(|q| dist([q.x, q.y, q.z], at) <= range))
        .map(|p| p.profile.uuid)
        .collect()
}

fn warden_near(ctx: &ServerContext, at: [f64; 3], range: f64) -> bool {
    lock(&ctx.entities).iter().any(|e| {
        !e.removed
            && matches!(&e.kind, Kind::Mob(m) if m.species == Species::Warden && m.dying.is_none())
            && dist(e.pos, at) <= range
    })
}

/// A player set off the shrieker at `p` (through a sensor, or by stepping
/// on it); whether it shrieked.
pub fn shriek(ctx: &ServerContext, p: Position, player: u128) -> bool {
    let Some(state) = ctx.world.block_if_loaded(p) else {
        return false;
    };
    let blocks = &ctx.data.blocks;
    if blocks.block_name(state) != Some(SHRIEKER)
        || blocks.property(state, "shrieking") == Some("true")
    {
        return false;
    }
    let summons = blocks.property(state, "can_summon") == Some("true");
    let at = centre(p);
    let mut warning = 0;
    if summons {
        if warden_near(ctx, at, 48.0) {
            return false;
        }
        let mut near = players_near(ctx, at, 16.0);
        if !near.contains(&player) {
            near.push(player);
        }
        let mut v = lock(&ctx.vibrations);
        if near
            .iter()
            .any(|u| v.warnings.get(u).is_some_and(|w| w.cooldown > 0))
        {
            return false;
        }
        let top = near
            .iter()
            .map(|u| v.warnings.get(u).map_or(0, |w| w.level))
            .max()
            .unwrap_or(0);
        warning = (top + 1).min(4);
        for u in near {
            v.warnings.insert(
                u,
                Warnings {
                    level: warning,
                    since: 0,
                    cooldown: WARN_COOLDOWN,
                },
            );
        }
    }
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    if !mc_tick::sculk::shriek(&mut level, logic, to_pos(p)) {
        return false;
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx
            .events
            .send(crate::context::Event::BlocksChanged { by: 0, changes });
    }
    emit_at(ctx, "shriek", p, Source::player(ctx, player), None);
    if summons {
        if let Some(darkness) = effects::kind_of("darkness") {
            for u in players_near(ctx, at, 40.0) {
                let long = lock(&ctx.vitals)
                    .get(&u)
                    .and_then(|v| v.effects.active.get("darkness"))
                    .is_some_and(|i| i.duration > 200);
                if !long {
                    effects::give(ctx, u, darkness, Instance::new(0, DARKNESS_TICKS, false));
                }
            }
        }
        let now = lock(&ctx.time).age;
        lock(&ctx.vibrations)
            .answers
            .push((p, now + mc_tick::sculk::SHRIEK_TICKS, warning));
    }
    true
}

/// Warnings fade; shriekers whose shriek ended answer.
fn warnings(ctx: &ServerContext, now: i64) {
    let due = {
        let mut v = lock(&ctx.vibrations);
        for w in v.warnings.values_mut() {
            w.cooldown = (w.cooldown - 1).max(0);
            w.since += 1;
            if w.since >= FORGET_TICKS && w.level > 0 {
                w.level -= 1;
                w.since = 0;
            }
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut v.answers)
            .into_iter()
            .partition(|a| a.1 <= now);
        v.answers = later;
        due
    };
    for (p, _, warning) in due {
        answer(ctx, p, warning);
    }
}

fn below(n: i32) -> i32 {
    (random_u64() % n.max(1) as u64) as i32
}

/// The shrieker's answer: a warden's sound from afar, nearer each level;
/// at the fourth, a warden.
fn answer(ctx: &ServerContext, p: Position, warning: i32) {
    let at = centre(p);
    if warning >= 4 {
        if ctx.difficulty() > 0 && ctx.game_rule_bool("spawn_wardens") {
            summon(ctx, p);
        }
        return;
    }
    let sound = match warning {
        1 => "entity.warden.nearby_close",
        2 => "entity.warden.nearby_closer",
        3 => "entity.warden.nearby_closest",
        _ => return,
    };
    let from = [
        at[0] + f64::from(below(21) - 10),
        at[1] + f64::from(below(7) - 3),
        at[2] + f64::from(below(21) - 10),
    ];
    crate::fx::sound(ctx, sound, crate::fx::category::HOSTILE, from, 5.0, 1.0);
}

/// Twenty tries at a spot within 5 blocks sideways and 6 up or down: the
/// highest place with ground that is not leaves under it and room for a
/// warden.
pub fn summon(ctx: &ServerContext, p: Position) -> Option<i32> {
    let blocks = &ctx.data.blocks;
    let rules = &ctx.rules;
    let state = |q: Position| ctx.world.block_if_loaded(q);
    for _ in 0..20 {
        let x = p.x + below(11) - 5;
        let z = p.z + below(11) - 5;
        for y in (p.y - 6..=p.y + 6).rev() {
            let (Some(floor), Some(feet)) = (
                state(Position::new(x, y - 1, z)),
                state(Position::new(x, y, z)),
            ) else {
                continue;
            };
            if !rules.blocks_motion(floor) || rules.is_leaves(floor) || blocks.is_fluid(floor) {
                continue;
            }
            let room = (0..3).all(|dy| {
                state(Position::new(x, y + dy, z)).is_some_and(|s| !rules.blocks_motion(s))
            });
            if !room || blocks.is_fluid(feet) {
                continue;
            }
            let pos = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
            let mut e = crate::mobs::new_mob(ctx, Species::Warden, pos);
            crate::mobs::warden::emerge(&mut e);
            let id = e.id;
            let _ = ctx
                .events
                .send(crate::context::Event::EntitySpawned(e.clone()));
            lock(&ctx.entities).push(e);
            return Some(id);
        }
    }
    None
}

/// A player stepped on a block (a shrieker shrieks).
pub fn stepped_on(ctx: &ServerContext, p: Position, player: u128) {
    let is_shrieker = ctx
        .world
        .block_if_loaded(p)
        .and_then(|s| ctx.data.blocks.block_name(s))
        == Some(SHRIEKER);
    if is_shrieker {
        shriek(ctx, p, player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequencies_follow_the_wiki_table() {
        assert_eq!(frequency("step"), 1);
        assert_eq!(frequency("projectile_land"), 2);
        assert_eq!(frequency("eat"), 8);
        assert_eq!(frequency("container_open"), 10);
        assert_eq!(frequency("block_place"), 13);
        assert_eq!(frequency("explode"), 15);
        assert_eq!(frequency("resonate_7"), 7);
        assert_eq!(frequency("shriek"), 0);
        assert_eq!(resonate(15), Some("resonate_15"));
        assert_eq!(resonate(0), None);
    }
}
