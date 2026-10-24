//! The bosses and the End fight (minecraft.wiki, "Ender Dragon", "Wither",
//! "End Crystal", "End Gateway", "Exit portal"):
//! - [`crystal`]: end crystals, placed on obsidian or bedrock, healing the
//!   dragon and exploding when hit;
//! - [`dragon`]: the ender dragon, its phases, parts and death;
//! - [`fight`]: the dragon fight of the End (`ender_dragon_fight.dat`), the
//!   exit portal, gateways and bringing the dragon back;
//! - [`wither`]: building, spawning and fighting the wither;
//! - [`projectile`]: wither skulls, dragon fireballs and dragon's breath.
//!
//! Both bosses are mobs ([`Species::EnderDragon`], [`Species::Wither`]) whose
//! tick is here instead of in the goals of `mc_entity::mob`.

pub mod crystal;
pub mod dragon;
pub mod fight;
pub mod projectile;
pub mod wither;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use mc_entity::boss::Part;
use mc_entity::mob::Target;
use mc_entity::{Entity, Kind, Species};
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{BossAction, BossEvent};

use crate::context::{Direct, Event, ServerContext};
use crate::level::WorldLevel;
use crate::tick::lock;

/// Boss bar colours and flags, in protocol numbers.
const PINK: i32 = 0;
const PURPLE: i32 = 5;
const DARKEN: u8 = 1;
const MUSIC: u8 = 2;
const FOG: u8 = 4;

/// Bosses state kept with a dimension.
#[derive(Debug, Default)]
pub struct State {
    /// The End's dragon fight, read on the End's first tick.
    pub fight: Option<fight::Fight>,
    /// Boss bars shown, by the boss's UUID.
    bars: HashMap<u128, Bar>,
    /// Game tick until which each player cannot use a gateway again.
    pub cooldowns: HashMap<u128, i64>,
}

#[derive(Debug)]
struct Bar {
    id: u128,
    viewers: HashSet<u128>,
    progress: f32,
    name: Tag,
}

/// The id for a new entity of `kind`: a dragon takes nine in a row, for its
/// parts.
pub fn entity_id(ctx: &ServerContext, kind: &Kind) -> i32 {
    match kind {
        Kind::Mob(m) if m.species == Species::EnderDragon => ctx.next_entity_ids(9),
        _ => ctx.next_entity_id(),
    }
}

/// The names in a block tag (`minecraft:dragon_immune`), read once.
pub(crate) fn block_tag(ctx: &ServerContext, tag: &'static str) -> &'static HashSet<String> {
    static TAGS: OnceLock<std::sync::Mutex<HashMap<&'static str, &'static HashSet<String>>>> =
        OnceLock::new();
    let tags = TAGS.get_or_init(Default::default);
    let mut tags = lock(tags);
    tags.entry(tag).or_insert_with(|| {
        Box::leak(Box::new(
            ctx.commands
                .env
                .tag("minecraft:block", tag)
                .into_iter()
                .collect(),
        ))
    })
}

/// A uniform number in [0, 1).
fn rnd() -> f64 {
    crate::hits::rnd()
}

fn below(n: i32) -> i32 {
    ((rnd() * f64::from(n.max(1))) as i32).min(n.max(1) - 1)
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// The dragon owning the part with entity id `id`: the dragon's id and the
/// part.
pub fn part_owner(entities: &[Entity], id: i32) -> Option<(i32, Part)> {
    entities.iter().find_map(|e| match &e.kind {
        Kind::Mob(m) if !e.removed => m.dragon()?.part_of(id).map(|p| (e.id, p)),
        _ => None,
    })
}

/// The parts of every dragon, as projectiles see them.
pub fn part_targets(entities: &[Entity]) -> Vec<crate::hits::Target> {
    let mut out = Vec::new();
    for e in entities.iter().filter(|e| !e.removed) {
        let Kind::Mob(m) = &e.kind else {
            continue;
        };
        let Some(d) = m.dragon() else {
            continue;
        };
        if m.dying.is_some() || d.death_time > 0 {
            continue;
        }
        for p in Part::ALL {
            out.push(crate::hits::Target {
                id: d.part_base + p as i32,
                aabb: d.part_box(p),
                kind: Some("minecraft:ender_dragon"),
            });
        }
    }
    out
}

/// One tick of the boss at `i` (from the mobs' tick); explosions go into
/// `blasts`.
pub fn tick_mob(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    i: usize,
    players: &[Target],
    now: i64,
    blasts: &mut Vec<crate::explosion::Blast>,
) {
    let e = &mut entities[i];
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    m.age += 1;
    m.invulnerable = (m.invulnerable - 1).max(0);
    if let Some((_, ticks)) = &mut m.revenge {
        *ticks -= 1;
        if *ticks <= 0 {
            m.revenge = None;
        }
    }
    if let Some((_, ticks)) = &mut m.hurt_by_player {
        *ticks -= 1;
        if *ticks <= 0 {
            m.hurt_by_player = None;
        }
    }
    // A dead wither lies there for a second.
    if let Some(left) = m.dying {
        m.dying = Some(left - 1);
        if left <= 1 {
            e.removed = true;
        }
        return;
    }
    let species = m.species;
    if species == Species::Wither && e.pos[1] < -128.0 {
        let h = crate::mobs::hurt::Harm::new(4.0, "minecraft:out_of_world");
        crate::mobs::hurt::harm(ctx, e, &h);
    }
    match species {
        Species::EnderDragon => dragon::tick(ctx, level, entities, i, players, now),
        Species::Wither => wither::tick(ctx, level, entities, i, players, now, blasts),
        _ => {}
    }
}

/// A hit on a boss: `Some` when the boss settled it (the dragon always
/// does), `None` to let the common path go on.
pub fn harm(ctx: &ServerContext, e: &mut Entity, h: &crate::mobs::hurt::Harm) -> Option<bool> {
    let Kind::Mob(m) = &e.kind else {
        return None;
    };
    match m.species {
        Species::EnderDragon => Some(dragon::harm(ctx, e, h)),
        Species::Wither => wither::harm(ctx, e, h),
        _ => None,
    }
}

/// What the bosses do after the mobs' tick: crystals, the dragon fight,
/// gateways and boss bars.
pub fn tick(ctx: &ServerContext, level: &mut WorldLevel) {
    let now = lock(&ctx.time).age;
    let mut entities = std::mem::take(&mut *lock(&ctx.entities));
    let mut fresh = Vec::new();
    for nbt in ctx.world.take_generated_entities() {
        if let Some(e) = crate::summon::from_saved(ctx, &nbt) {
            fresh.push(e);
        }
    }
    crystal::tick_all(ctx, level, &mut entities);
    if ctx.world.dimension == "minecraft:the_end" {
        fight::tick(ctx, level, &mut entities, &mut fresh, now);
    }
    let removed: Vec<i32> = entities
        .iter()
        .filter(|e| e.removed && matches!(e.kind, Kind::Crystal(_)))
        .map(|e| e.id)
        .collect();
    entities.retain(|e| !(e.removed && matches!(e.kind, Kind::Crystal(_))));
    bars(ctx, &entities);
    for e in &fresh {
        let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    }
    entities.append(&mut fresh);
    let mut stored = lock(&ctx.entities);
    entities.append(&mut stored);
    *stored = entities;
    drop(stored);
    if !removed.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(removed));
    }
    if ctx.world.dimension == "minecraft:the_end" {
        fight::gateways(ctx, level, now);
    }
}

fn translated(key: &str) -> Tag {
    let mut c = Compound::new();
    c.insert("translate", Tag::String(key.into()));
    Tag::Compound(c)
}

fn named(name: &Option<String>, key: &str) -> Tag {
    match name {
        Some(n) => {
            let mut c = Compound::new();
            c.insert("text", Tag::String(n.clone()));
            Tag::Compound(c)
        }
        None => translated(key),
    }
}

/// Shows each boss's bar to those who should see it: the dragon's to
/// players within 192 blocks of the portal (as long as it belongs to the
/// fight), the wither's to players near it.
fn bars(ctx: &ServerContext, entities: &[Entity]) {
    let players: Vec<(u128, [f64; 3])> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| p.pose.map(|q| (p.profile.uuid, [q.x, q.y, q.z])))
        .collect();
    let mut state = lock(&ctx.bosses);
    let mut alive = HashSet::new();
    let mut send = Vec::new();
    for e in entities.iter().filter(|e| !e.removed) {
        let Kind::Mob(m) = &e.kind else {
            continue;
        };
        let (progress, color, flags, name, near): (f32, i32, u8, Tag, Vec<u128>) =
            if let Some(d) = m.dragon() {
                if !d.in_fight {
                    continue;
                }
                let near = players
                    .iter()
                    .filter(|(_, p)| dist_sq(*p, [0.0, 128.0, 0.0]) <= 192.0 * 192.0)
                    .map(|(u, _)| *u)
                    .collect();
                (
                    (m.health / m.max_health).clamp(0.0, 1.0),
                    PINK,
                    MUSIC | FOG,
                    named(&m.custom_name, "entity.minecraft.ender_dragon"),
                    near,
                )
            } else if let Some(w) = m.wither() {
                let near = players
                    .iter()
                    .filter(|(_, p)| (p[0] - e.pos[0]).abs().max((p[2] - e.pos[2]).abs()) <= 160.0)
                    .map(|(u, _)| *u)
                    .collect();
                let progress = if w.invulnerable > 0 {
                    1.0 - w.invulnerable as f32 / wither::SPAWN_TICKS as f32
                } else {
                    m.health / m.max_health
                };
                (
                    progress.clamp(0.0, 1.0),
                    PURPLE,
                    DARKEN,
                    named(&m.custom_name, "entity.minecraft.wither"),
                    near,
                )
            } else {
                continue;
            };
        if m.dying.is_some() {
            continue;
        }
        alive.insert(e.uuid);
        let bar = state.bars.entry(e.uuid).or_insert_with(|| Bar {
            id: crate::util::random_uuid(),
            viewers: HashSet::new(),
            progress,
            name: name.clone(),
        });
        let add = BossAction::Add {
            name: name.clone(),
            progress,
            color,
            overlay: 0,
            flags,
        };
        for &u in &near {
            if bar.viewers.insert(u) {
                send.push((u, bar.id, add.clone()));
            }
        }
        let gone: Vec<u128> = bar
            .viewers
            .iter()
            .copied()
            .filter(|u| !near.contains(u))
            .collect();
        for u in gone {
            bar.viewers.remove(&u);
            send.push((u, bar.id, BossAction::Remove));
        }
        if bar.progress.to_bits() != progress.to_bits() {
            bar.progress = progress;
            for &u in &bar.viewers {
                send.push((u, bar.id, BossAction::Progress(progress)));
            }
        }
        if bar.name != name {
            bar.name = name.clone();
            for &u in &bar.viewers {
                send.push((u, bar.id, BossAction::Name(name.clone())));
            }
        }
    }
    let ended: Vec<u128> = state
        .bars
        .keys()
        .copied()
        .filter(|k| !alive.contains(k))
        .collect();
    for k in ended {
        if let Some(bar) = state.bars.remove(&k) {
            for u in bar.viewers {
                send.push((u, bar.id, BossAction::Remove));
            }
        }
    }
    drop(state);
    for (uuid, id, action) in send {
        let _ = ctx.events.send(Event::Direct {
            uuid,
            packet: Direct::Boss(BossEvent { uuid: id, action }),
        });
    }
}

/// The players who see a boss's bar now (tests).
#[cfg(test)]
pub fn bar_viewers(ctx: &ServerContext, boss: u128) -> Vec<u128> {
    lock(&ctx.bosses)
        .bars
        .get(&boss)
        .map(|b| b.viewers.iter().copied().collect())
        .unwrap_or_default()
}

/// Mob metadata of the bosses: the dragon's phase, the wither's heads and
/// spawn ticks.
pub fn metadata(m: &mc_entity::Mob) -> Vec<(u8, mc_protocol::packets::play::MetadataValue)> {
    use mc_protocol::packets::play::MetadataValue as V;
    if let Some(d) = m.dragon() {
        return vec![(16, V::VarInt(d.phase.id()))];
    }
    if let Some(w) = m.wither() {
        return vec![
            (16, V::VarInt(w.heads[0])),
            (17, V::VarInt(w.heads[1])),
            (18, V::VarInt(w.heads[2])),
            (19, V::VarInt(w.invulnerable)),
        ];
    }
    Vec::new()
}

/// What `/data` and saving keep of a boss (`DragonPhase`, `Invul`).
pub fn save_nbt(m: &mc_entity::Mob, out: &mut Compound) {
    if let Some(d) = m.dragon() {
        out.insert("DragonPhase", Tag::Int(d.phase.id()));
        out.insert("DragonDeathTime", Tag::Int(d.death_time));
    }
    if let Some(w) = m.wither() {
        out.insert("Invul", Tag::Int(w.invulnerable));
    }
}

/// Reads [`save_nbt`]'s fields back.
pub fn load_nbt(m: &mut mc_entity::Mob, nbt: &Compound) {
    let int = |k: &str| match nbt.get(k) {
        Some(Tag::Int(v)) => Some(*v),
        Some(Tag::Short(v)) => Some(i32::from(*v)),
        Some(Tag::Byte(v)) => Some(i32::from(*v)),
        _ => None,
    };
    if let Some(d) = m.dragon_mut() {
        if let Some(p) = int("DragonPhase") {
            d.phase = mc_entity::boss::Phase::from_id(p);
        }
        if let Some(t) = int("DragonDeathTime") {
            d.death_time = t;
        }
    }
    if let Some(w) = m.wither_mut()
        && let Some(t) = int("Invul")
    {
        w.invulnerable = t.max(0);
    }
}

#[cfg(test)]
mod tests;
