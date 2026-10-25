//! Raids and illager patrols (minecraft.wiki, "Raid" and "Patrol").
//!
//! A player with Bad Omen who comes into a village (a section with a home,
//! meeting point or job site somebody holds, or one next to it) loses it for
//! Raid Omen of the same level for 30 seconds. When that runs out a raid
//! starts where they were: 3, 5 or 7 waves by difficulty, one more when the
//! omen was above level I. Each wave spawns together somewhere around the
//! village after 15 seconds of quiet, its first illager carrying the ominous
//! banner, and marches on the centre. Players within 96 blocks see the red
//! bar. The village wins when every wave is dead (the heroes get Hero of the
//! Village of the omen's level for 40 minutes), and loses when nobody holds a
//! point near the centre any more; either way the raid lingers 30 seconds
//! (the raiders cheer a loss) and ends. One that runs 40 minutes just ends.
//!
//! Patrols: every 10 to 11 minutes from day 5 on, in daylight, a one in five
//! chance of a few pillagers 24 to 47 blocks from a random player who is not
//! near a village, their leader with the banner. A patrol's leader that dies
//! outside a raid drops an ominous bottle.

use std::collections::{HashMap, HashSet};

use mc_entity::mob::{Gear, slot};
use mc_entity::{Entity, Kind, Species};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::{BossAction, BossEvent};

use crate::context::{Direct, Event, ServerContext};
use crate::effects::{self, Instance};
use crate::tick::lock;
use crate::util::random_u64;

/// Raid Omen's 30 seconds.
const OMEN_TICKS: i32 = 600;
/// Quiet before each wave.
const PREPARE_TICKS: i32 = 300;
/// How long a won or lost raid lingers.
const CELEBRATE_TICKS: i32 = 600;
/// A raid that has gone on this long just ends.
const MAX_TICKS: i64 = 48_000;
const HERO_TICKS: i32 = 48_000;
const MAX_LEVEL: i32 = 5;
/// Who sees the bar, and which raid a raider may join.
const RANGE: f64 = 96.0;
const RED: i32 = 2;
const NOTCHED_10: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ongoing,
    Victory,
    Loss,
}

#[derive(Debug)]
pub struct Raid {
    pub id: i32,
    pub center: [i32; 3],
    /// The omen's level (1 to 5).
    pub level: i32,
    pub status: Status,
    /// Waves spawned so far, of `waves`.
    pub spawned: i32,
    /// Ticks of quiet left before the next wave.
    pub prepare: i32,
    pub ticks: i64,
    celebrate: i32,
    /// The raiders' health when their wave came, for the bar.
    total_health: f32,
    pub heroes: HashSet<u128>,
    bar: u128,
    pub viewers: HashSet<u128>,
    progress: f32,
    name: Option<Tag>,
}

impl Raid {
    /// Waves on this difficulty: 3, 5 or 7, and the bonus one.
    pub fn waves(&self, difficulty: i32) -> i32 {
        groups(difficulty) + i32::from(self.level > 1)
    }
}

#[derive(Debug)]
struct Omen {
    center: [i32; 3],
    level: i32,
    left: i32,
}

#[derive(Debug, Default)]
pub struct Raids {
    next: i32,
    pub raids: Vec<Raid>,
    omens: HashMap<u128, Omen>,
    patrol_delay: i32,
}

/// Waves before the bonus one.
fn groups(difficulty: i32) -> i32 {
    match difficulty {
        1 => 3,
        2 => 5,
        _ => 7,
    }
}

/// Raiders of each kind per wave (index 1 to 7; the bonus wave uses the
/// index after the last normal one).
const WAVES: [(Species, [i32; 8]); 5] = [
    (Species::Vindicator, [0, 0, 2, 0, 1, 4, 2, 5]),
    (Species::Evoker, [0, 0, 0, 0, 0, 1, 1, 2]),
    (Species::Pillager, [0, 4, 3, 3, 4, 4, 4, 2]),
    (Species::Witch, [0, 0, 0, 0, 3, 0, 0, 1]),
    (Species::Ravager, [0, 0, 0, 1, 0, 1, 0, 2]),
];

fn below(n: i32) -> i32 {
    if n <= 0 {
        0
    } else {
        (random_u64() % n as u64) as i32
    }
}

/// How many of a kind come in a wave: the table, and a random few more.
fn wave_count(species: Species, wave: i32, bonus: bool, difficulty: i32) -> i32 {
    let row = WAVES
        .iter()
        .find(|(s, _)| *s == species)
        .map_or([0; 8], |r| r.1);
    let index = if bonus { groups(difficulty) } else { wave };
    let base = row[index.clamp(0, 7) as usize];
    let easy = difficulty == 1;
    let extra = match species {
        Species::Witch if easy || wave <= 2 || wave == 4 => 0,
        Species::Witch => 1,
        Species::Pillager | Species::Vindicator if easy => below(2),
        Species::Pillager | Species::Vindicator if difficulty == 2 => 1,
        Species::Pillager | Species::Vindicator => 2,
        Species::Ravager => i32::from(!easy && bonus),
        _ => 0,
    };
    base + if extra > 0 { below(extra + 1) } else { 0 }
}

/// Whether a section with a held home, meeting point or job site is at or
/// next to this block's.
pub fn is_village(ctx: &ServerContext, p: [i32; 3]) -> bool {
    let section = |q: [i32; 3]| [q[0] >> 4, q[1] >> 4, q[2] >> 4];
    let at = section(p);
    lock(&ctx.poi)
        .near(p, 32, 32, |r| {
            let max = crate::poi::tickets_of(r.kind);
            max > 0 && r.free < max
        })
        .iter()
        .any(|(q, _)| {
            let s = section(*q);
            (0..3).all(|i| (s[i] - at[i]).abs() <= 1)
        })
}

fn block_of(pos: [f64; 3]) -> [i32; 3] {
    [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ]
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

fn centre(p: [i32; 3]) -> [f64; 3] {
    [
        f64::from(p[0]) + 0.5,
        f64::from(p[1]),
        f64::from(p[2]) + 0.5,
    ]
}

struct Player {
    uuid: u128,
    pos: [f64; 3],
    spectator: bool,
}

fn players(ctx: &ServerContext) -> Vec<Player> {
    lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            let q = p.pose?;
            Some(Player {
                uuid: p.profile.uuid,
                pos: [q.x, q.y, q.z],
                spectator: p.game_mode == 3,
            })
        })
        .collect()
}

fn amplifier_of(ctx: &ServerContext, uuid: u128, effect: &str) -> Option<i32> {
    lock(&ctx.vitals)
        .get(&uuid)
        .filter(|v| !v.dead)
        .and_then(|v| v.effects.active.get(effect))
        .map(|i| i.amplifier)
}

/// The ominous banner a captain wears.
pub fn ominous_banner(ctx: &ServerContext) -> Option<Gear> {
    let item = ctx.items.id("minecraft:white_banner")?;
    let layer = |pattern: &str, color: &str| {
        let mut c = Compound::new();
        c.insert("pattern", Tag::String(format!("minecraft:{pattern}")));
        c.insert("color", Tag::String(color.into()));
        Tag::Compound(c)
    };
    let patterns = vec![
        layer("rhombus", "cyan"),
        layer("stripe_bottom", "light_gray"),
        layer("stripe_center", "gray"),
        layer("border", "light_gray"),
        layer("stripe_middle", "black"),
        layer("half_horizontal", "light_gray"),
        layer("circle", "light_gray"),
        layer("border", "black"),
    ];
    let mut components = mc_protocol::components::Components::default();
    components.set("banner_patterns", Tag::List(patterns));
    let mut name = Compound::new();
    name.insert(
        "translate",
        Tag::String("block.minecraft.ominous_banner".into()),
    );
    components.set("item_name", Tag::Compound(name));
    components.set("rarity", Tag::String("uncommon".into()));
    let mut tooltip = Compound::new();
    tooltip.insert(
        "hidden_components",
        Tag::List(vec![Tag::String("minecraft:banner_patterns".into())]),
    );
    components.set("tooltip_display", Tag::Compound(tooltip));
    Some(Gear {
        item,
        count: 1,
        damage: 0,
        components,
    })
}

/// Makes a raider a captain: the banner on its head, always dropped.
fn make_captain(ctx: &ServerContext, e: &mut Entity) {
    let banner = ominous_banner(ctx);
    if let Kind::Mob(m) = &mut e.kind {
        m.quirks.illager.patrol_leader = true;
        m.gear[slot::HEAD] = banner;
        m.drop_chance[slot::HEAD] = 2.0;
    }
}

fn spawn(ctx: &ServerContext, e: Entity) {
    let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    lock(&ctx.entities).push(e);
}

/// One server tick of raids, omens and patrols.
pub fn tick(ctx: &ServerContext) {
    let players = players(ctx);
    omens(ctx, &players);
    raids(ctx, &players);
    patrols(ctx);
    spawn_patrol(ctx, &players);
}

/// Bad Omen turns into Raid Omen in a village; Raid Omen running out starts
/// (or strengthens) a raid.
fn omens(ctx: &ServerContext, players: &[Player]) {
    let allowed = ctx.difficulty() > 0 && ctx.game_rule_bool("raids");
    for p in players.iter().filter(|p| !p.spectator) {
        if !allowed {
            break;
        }
        let Some(amp) = amplifier_of(ctx, p.uuid, "bad_omen") else {
            continue;
        };
        let at = block_of(p.pos);
        if !is_village(ctx, at)
            || !crate::environment::flag(ctx, Some(at), crate::environment::CAN_START_RAID, true)
        {
            continue;
        }
        let (Some(bad), Some(raid)) = (effects::kind_of("bad_omen"), effects::kind_of("raid_omen"))
        else {
            continue;
        };
        effects::remove(ctx, p.uuid, Some(bad));
        effects::give(ctx, p.uuid, raid, Instance::new(amp, OMEN_TICKS, true));
        lock(&ctx.raids).omens.insert(
            p.uuid,
            Omen {
                center: at,
                level: (amp + 1).min(MAX_LEVEL),
                left: OMEN_TICKS,
            },
        );
    }
    let due: Vec<(u128, Omen)> = {
        let mut state = lock(&ctx.raids);
        let mut gone = Vec::new();
        for (uuid, omen) in &mut state.omens {
            omen.left -= 1;
            let online = players.iter().any(|p| p.uuid == *uuid);
            // Milk takes the omen and the raid with it.
            let kept = omen.left <= 2 || amplifier_of(ctx, *uuid, "raid_omen").is_some();
            if !online || !kept || omen.left <= 0 {
                gone.push(*uuid);
            }
        }
        gone.into_iter()
            .filter_map(|u| state.omens.remove(&u).map(|o| (u, o)))
            .filter(|(u, o)| o.left <= 0 && players.iter().any(|p| p.uuid == *u))
            .collect()
    };
    for (uuid, omen) in due {
        if let Some(k) = effects::kind_of("raid_omen") {
            effects::remove(ctx, uuid, Some(k));
        }
        if allowed {
            start(ctx, omen.center, omen.level, uuid);
        }
    }
}

/// A raid starts at `center` (or the one already there grows stronger).
pub fn start(ctx: &ServerContext, center: [i32; 3], level: i32, by: u128) -> Option<i32> {
    let mut state = lock(&ctx.raids);
    if let Some(r) = state.raids.iter_mut().find(|r| {
        r.status == Status::Ongoing && dist_sq(centre(r.center), centre(center)) < RANGE * RANGE
    }) {
        r.level = (r.level + level).min(MAX_LEVEL);
        return Some(r.id);
    }
    if !is_village(ctx, center) {
        return None;
    }
    state.next += 1;
    let id = state.next;
    let mut heroes = HashSet::new();
    heroes.insert(by);
    state.raids.push(Raid {
        id,
        center,
        level: level.clamp(1, MAX_LEVEL),
        status: Status::Ongoing,
        spawned: 0,
        prepare: PREPARE_TICKS,
        ticks: 0,
        celebrate: 0,
        total_health: 0.0,
        heroes,
        bar: crate::util::random_uuid(),
        viewers: HashSet::new(),
        progress: 0.0,
        name: None,
    });
    drop(state);
    crate::stats::custom(ctx, by, "raid_trigger", 1);
    Some(id)
}

/// Raiders of a raid still fighting, and their health.
fn alive(entities: &[Entity], id: i32) -> (usize, f32) {
    entities
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if m.dying.is_none() && m.quirks.illager.raid == Some(id) => {
                Some(m.health)
            }
            _ => None,
        })
        .fold((0, 0.0), |(n, h), x| (n + 1, h + x))
}

fn translated(key: &str) -> Tag {
    let mut c = Compound::new();
    c.insert("translate", Tag::String(key.into()));
    Tag::Compound(c)
}

/// "Raid - Raiders Remaining: n".
fn remaining(n: usize) -> Tag {
    let mut count = Compound::new();
    count.insert(
        "translate",
        Tag::String("event.minecraft.raid.raiders_remaining".into()),
    );
    count.insert("with", Tag::List(vec![Tag::String(n.to_string())]));
    let mut c = Compound::new();
    c.insert("text", Tag::String(String::new()));
    c.insert(
        "extra",
        Tag::List(vec![
            translated("event.minecraft.raid"),
            Tag::String(" - ".into()),
            Tag::Compound(count),
        ]),
    );
    Tag::Compound(c)
}

fn raids(ctx: &ServerContext, players: &[Player]) {
    if lock(&ctx.raids).raids.is_empty() {
        return;
    }
    let difficulty = ctx.difficulty();
    let allowed = difficulty > 0 && ctx.game_rule_bool("raids");
    let mut send: Vec<(u128, u128, BossAction)> = Vec::new();
    let mut waves: Vec<(i32, [i32; 3], i32, bool)> = Vec::new();
    let mut heroes: Vec<(u128, i32)> = Vec::new();
    let mut ended: Vec<i32> = Vec::new();
    let mut lost: Vec<i32> = Vec::new();
    {
        let entities = lock(&ctx.entities);
        let mut state = lock(&ctx.raids);
        for r in &mut state.raids {
            r.ticks += 1;
            let at = centre(r.center);
            let (count, health) = alive(&entities, r.id);
            if r.status == Status::Ongoing {
                if !allowed || r.ticks >= MAX_TICKS {
                    ended.push(r.id);
                    continue;
                }
                if r.ticks % 20 == 0 && !is_village(ctx, r.center) {
                    if r.spawned > 0 {
                        r.status = Status::Loss;
                        lost.push(r.id);
                    } else {
                        ended.push(r.id);
                        continue;
                    }
                }
            }
            for p in players.iter().filter(|p| !p.spectator) {
                if r.status == Status::Ongoing && dist_sq(p.pos, at) < RANGE * RANGE {
                    r.heroes.insert(p.uuid);
                }
            }
            let (progress, name) = match r.status {
                Status::Ongoing => {
                    let waves_total = r.waves(difficulty);
                    if count == 0 && r.spawned >= waves_total {
                        r.status = Status::Victory;
                        for &h in &r.heroes {
                            heroes.push((h, r.level));
                        }
                        (0.0, translated("event.minecraft.raid.victory.full"))
                    } else if count == 0 {
                        if r.prepare > 0 {
                            r.prepare -= 1;
                        } else {
                            r.spawned += 1;
                            let bonus = r.spawned > groups(difficulty);
                            waves.push((r.id, r.center, r.spawned, bonus));
                            r.prepare = PREPARE_TICKS;
                            r.total_health = 0.0;
                        }
                        (
                            ((PREPARE_TICKS - r.prepare) as f32 / PREPARE_TICKS as f32)
                                .clamp(0.0, 1.0),
                            translated("event.minecraft.raid"),
                        )
                    } else {
                        r.total_health = r.total_health.max(health);
                        let name = if count <= 2 {
                            remaining(count)
                        } else {
                            translated("event.minecraft.raid")
                        };
                        ((health / r.total_health.max(1.0)).clamp(0.0, 1.0), name)
                    }
                }
                Status::Victory | Status::Loss => {
                    r.celebrate += 1;
                    if r.celebrate >= CELEBRATE_TICKS {
                        ended.push(r.id);
                        continue;
                    }
                    let key = if r.status == Status::Victory {
                        "event.minecraft.raid.victory.full"
                    } else {
                        "event.minecraft.raid.defeat.full"
                    };
                    (r.progress, translated(key))
                }
            };
            let near: Vec<u128> = players
                .iter()
                .filter(|p| dist_sq(p.pos, at) < RANGE * RANGE)
                .map(|p| p.uuid)
                .collect();
            for &u in &near {
                if r.viewers.insert(u) {
                    send.push((
                        u,
                        r.bar,
                        BossAction::Add {
                            name: name.clone(),
                            progress,
                            color: RED,
                            overlay: NOTCHED_10,
                            flags: 0,
                        },
                    ));
                }
            }
            let gone: Vec<u128> = r
                .viewers
                .iter()
                .copied()
                .filter(|u| !near.contains(u))
                .collect();
            for u in gone {
                r.viewers.remove(&u);
                send.push((u, r.bar, BossAction::Remove));
            }
            if r.progress.to_bits() != progress.to_bits() {
                r.progress = progress;
                for &u in &r.viewers {
                    send.push((u, r.bar, BossAction::Progress(progress)));
                }
            }
            if r.name.as_ref() != Some(&name) {
                if r.name.is_some() {
                    for &u in &r.viewers {
                        send.push((u, r.bar, BossAction::Name(name.clone())));
                    }
                }
                r.name = Some(name);
            }
        }
        for id in &ended {
            if let Some(i) = state.raids.iter().position(|r| r.id == *id) {
                let r = state.raids.remove(i);
                for u in r.viewers {
                    send.push((u, r.bar, BossAction::Remove));
                }
            }
        }
    }
    for (uuid, id, action) in send {
        let _ = ctx.events.send(Event::Direct {
            uuid,
            packet: Direct::Boss(BossEvent { uuid: id, action }),
        });
    }
    if !lost.is_empty() || !ended.is_empty() {
        let mut entities = lock(&ctx.entities);
        for e in entities.iter_mut() {
            let Kind::Mob(m) = &mut e.kind else {
                continue;
            };
            let Some(id) = m.quirks.illager.raid else {
                continue;
            };
            if lost.contains(&id) {
                m.quirks.illager.celebrating = true;
            }
            if ended.contains(&id) {
                m.quirks.illager.raid = None;
                m.quirks.illager.celebrating = false;
            }
        }
    }
    for (raid, center, wave, bonus) in waves {
        spawn_wave(ctx, raid, center, wave, bonus, difficulty);
    }
    for (uuid, level) in heroes {
        let alive = lock(&ctx.vitals).get(&uuid).is_some_and(|v| !v.dead);
        if !alive || !players.iter().any(|p| p.uuid == uuid) {
            continue;
        }
        if let Some(k) = effects::kind_of("hero_of_the_village") {
            effects::give(ctx, uuid, k, Instance::new(level - 1, HERO_TICKS, true));
        }
        crate::stats::custom(ctx, uuid, "raid_win", 1);
        crate::criteria::plain(ctx, uuid, "minecraft:hero_of_the_village");
    }
    join(ctx);
}

/// Where a wave comes from: 20 tries around the centre, far out first,
/// outside the village while it can.
fn spawn_point(ctx: &ServerContext, center: [i32; 3]) -> Option<[i32; 3]> {
    for attempt in 0..3 {
        let scale = if attempt == 0 { 2 } else { 2 - attempt };
        for _ in 0..20 {
            let angle = crate::util::random_f64() * std::f64::consts::TAU;
            let x = center[0] + (angle.cos() * 32.0 * f64::from(scale)).floor() as i32 + below(5);
            let z = center[2] + (angle.sin() * 32.0 * f64::from(scale)).floor() as i32 + below(5);
            if !ctx.world.chunk_loaded(x >> 4, z >> 4) {
                continue;
            }
            let Some(y) = crate::trading::surface_stand(ctx, x, z) else {
                continue;
            };
            if attempt < 2 && is_village(ctx, [x, y, z]) {
                continue;
            }
            return Some([x, y, z]);
        }
    }
    None
}

fn spawn_wave(
    ctx: &ServerContext,
    raid: i32,
    center: [i32; 3],
    wave: i32,
    bonus: bool,
    difficulty: i32,
) {
    let Some(at) = spawn_point(ctx, center) else {
        return;
    };
    let pos = centre(at);
    let target = centre(center);
    let mut captain = false;
    let mut health = 0.0;
    for (species, _) in WAVES {
        for _ in 0..wave_count(species, wave, bonus, difficulty) {
            let mut e = crate::mobs::new_mob(ctx, species, pos);
            if let Kind::Mob(m) = &mut e.kind {
                m.persistent = true;
                let q = &mut m.quirks.illager;
                q.raid = Some(raid);
                q.wave = wave;
                q.can_join_raid = true;
                q.patrol_target = Some(target);
                health += m.health;
            }
            if !captain && !matches!(species, Species::Witch | Species::Ravager) {
                make_captain(ctx, &mut e);
                captain = true;
            }
            spawn(ctx, e);
        }
    }
    if let Some(r) = lock(&ctx.raids).raids.iter_mut().find(|r| r.id == raid) {
        r.total_health = health;
    }
    crate::fx::sound(
        ctx,
        "event.raid.horn",
        crate::fx::category::NEUTRAL,
        pos,
        64.0,
        1.0,
    );
}

/// Raiders that may join a raid do when they come within reach of one.
fn join(ctx: &ServerContext) {
    let open: Vec<(i32, [f64; 3], i32)> = lock(&ctx.raids)
        .raids
        .iter()
        .filter(|r| r.status == Status::Ongoing && r.spawned > 0)
        .map(|r| (r.id, centre(r.center), r.spawned))
        .collect();
    if open.is_empty() {
        return;
    }
    let mut added: Vec<(i32, f32)> = Vec::new();
    for e in lock(&ctx.entities).iter_mut().filter(|e| !e.removed) {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        let q = &m.quirks.illager;
        if !m.species.raider() || !q.can_join_raid || q.raid.is_some() || m.dying.is_some() {
            continue;
        }
        let Some((id, at, wave)) = open
            .iter()
            .filter(|(_, at, _)| dist_sq(pos, *at) < RANGE * RANGE)
            .min_by(|a, b| dist_sq(pos, a.1).total_cmp(&dist_sq(pos, b.1)))
            .copied()
        else {
            continue;
        };
        let q = &mut m.quirks.illager;
        q.raid = Some(id);
        q.wave = wave;
        q.patrolling = false;
        q.patrol_target = Some(at);
        m.persistent = true;
        added.push((id, m.health));
    }
    if !added.is_empty() {
        let mut state = lock(&ctx.raids);
        for (id, h) in added {
            if let Some(r) = state.raids.iter_mut().find(|r| r.id == id) {
                r.total_health += h;
            }
        }
    }
}

/// A patrol keeps together: those that follow take their leader's way.
fn patrols(ctx: &ServerContext) {
    if lock(&ctx.time).age % 20 != 0 {
        return;
    }
    let mut entities = lock(&ctx.entities);
    let leaders: Vec<([f64; 3], [f64; 3])> = entities
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            Kind::Mob(m)
                if m.quirks.illager.patrol_leader
                    && m.quirks.illager.patrolling
                    && m.quirks.illager.raid.is_none()
                    && m.dying.is_none() =>
            {
                Some((e.pos, m.quirks.illager.patrol_target?))
            }
            _ => None,
        })
        .collect();
    if leaders.is_empty() {
        return;
    }
    for e in entities.iter_mut().filter(|e| !e.removed) {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        let q = &mut m.quirks.illager;
        if q.patrol_leader || !q.patrolling || q.raid.is_some() {
            continue;
        }
        if let Some((_, to)) = leaders
            .iter()
            .filter(|(at, _)| dist_sq(*at, pos) <= 16.0 * 16.0)
            .min_by(|a, b| dist_sq(a.0, pos).total_cmp(&dist_sq(b.0, pos)))
        {
            q.patrol_target = Some(*to);
        }
    }
}

/// Every 10 to 11 minutes, maybe a patrol.
fn spawn_patrol(ctx: &ServerContext, players: &[Player]) {
    if ctx.world.dimension != "minecraft:overworld" || !ctx.game_rule_bool("spawn_patrols") {
        return;
    }
    {
        let mut state = lock(&ctx.raids);
        state.patrol_delay -= 1;
        if state.patrol_delay > 0 {
            return;
        }
        state.patrol_delay += 12_000 + below(1200);
    }
    let day = crate::mobs::day_time(ctx);
    if day / 24_000 < 5 || crate::mobs::sky_darken(day) >= 4 || below(5) != 0 {
        return;
    }
    let candidates: Vec<&Player> = players.iter().filter(|p| !p.spectator).collect();
    if candidates.is_empty() || ctx.difficulty() == 0 || !ctx.game_rule_bool("spawn_mobs") {
        return;
    }
    let p = candidates[below(candidates.len() as i32) as usize];
    try_patrol(ctx, p.pos);
}

/// A patrol 24 to 47 blocks from `near` (on each axis, either way); how many came.
pub fn try_patrol(ctx: &ServerContext, near: [f64; 3]) -> usize {
    let at = block_of(near);
    // Not near a village: within two sections of a held point.
    let close = lock(&ctx.poi)
        .near(at, 48, 48, |r| {
            let max = crate::poi::tickets_of(r.kind);
            max > 0 && r.free < max
        })
        .iter()
        .any(|(q, _)| (0..3).all(|i| ((q[i] >> 4) - (at[i] >> 4)).abs() <= 2));
    if close {
        return 0;
    }
    let sign = || if below(2) == 0 { -1 } else { 1 };
    let x = at[0] + (24 + below(24)) * sign();
    let z = at[2] + (24 + below(24)) * sign();
    if !ctx.world.chunk_loaded(x >> 4, z >> 4) {
        return 0;
    }
    let biome = ctx
        .world
        .biome(Position::new(x, at[1], z), &ctx.data.blocks)
        .and_then(|id| ctx.world.biome_name(id));
    if biome.is_some_and(|b| b.ends_with("mushroom_fields")) {
        return 0;
    }
    let age = lock(&ctx.time).age;
    let day = crate::mobs::day_time(ctx);
    let regional = crate::mobs::spawn::regional_difficulty(ctx.difficulty(), age, day);
    let size = regional.ceil() as i32 + 1;
    let mut spawned = 0;
    let (mut px, mut pz) = (x, z);
    for i in 0..size {
        let Some(y) = crate::trading::surface_stand(ctx, px, pz) else {
            break;
        };
        let pos = centre([px, y, pz]);
        let mut e = crate::mobs::new_mob(ctx, Species::Pillager, pos);
        if let Kind::Mob(m) = &mut e.kind {
            m.persistent = true;
            let q = &mut m.quirks.illager;
            q.patrolling = true;
            q.can_join_raid = true;
        }
        if i == 0 {
            make_captain(ctx, &mut e);
            if let Kind::Mob(m) = &mut e.kind {
                let dx = f64::from(below(1000) - 500);
                let dz = f64::from(below(1000) - 500);
                m.quirks.illager.patrol_target = Some([pos[0] + dx, pos[1], pos[2] + dz]);
            }
        }
        spawn(ctx, e);
        spawned += 1;
        px += below(5) - below(5);
        pz += below(5) - below(5);
    }
    spawned
}

/// A patrol's captain killed outside a raid leaves an ominous bottle of a
/// random level.
pub fn captain_died(ctx: &ServerContext, m: &mc_entity::mob::Mob, pos: [f64; 3]) {
    let q = &m.quirks.illager;
    if !m.species.raider() || !q.patrol_leader || q.raid.is_some() {
        return;
    }
    let Some(item) = ctx.items.id("minecraft:ominous_bottle") else {
        return;
    };
    let mut components = mc_protocol::components::Components::default();
    components.set("ominous_bottle_amplifier", Tag::Int(below(5)));
    crate::mobs::hurt::spawn_stack(ctx, pos, item, 1, 0, components);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waves_follow_the_wiki_table() {
        assert_eq!((groups(1), groups(2), groups(3)), (3, 5, 7));
        for _ in 0..50 {
            // Normal, wave 1: four pillagers and maybe one more, no witch.
            assert!((4..=5).contains(&wave_count(Species::Pillager, 1, false, 2)));
            assert_eq!(wave_count(Species::Witch, 1, false, 2), 0);
            assert_eq!(wave_count(Species::Evoker, 4, false, 2), 0);
            // Wave 4: three witches and never a fourth; hard wave 5: one
            // evoker, a ravager, perhaps a witch.
            assert_eq!(wave_count(Species::Witch, 4, false, 3), 3);
            assert_eq!(wave_count(Species::Evoker, 5, false, 3), 1);
            assert_eq!(wave_count(Species::Ravager, 5, false, 3), 1);
            assert!((0..=1).contains(&wave_count(Species::Witch, 5, false, 3)));
            // The bonus wave on normal uses the sixth column; a ravager more.
            assert_eq!(wave_count(Species::Evoker, 6, true, 2), 1);
            assert!((1..=2).contains(&wave_count(Species::Ravager, 6, true, 2)));
            // Easy never brings a bonus ravager.
            assert_eq!(wave_count(Species::Ravager, 4, true, 1), 1);
        }
    }
}
