//! The warden (minecraft.wiki, "Warden"): blind, it hears and smells.
//!
//! It digs out of the ground (134 ticks, unhurt but by `/kill`), then
//! listens: a vibration within 16 blocks makes its tendrils shiver and
//! angers it at whoever made it (35, a projectile 10), and it goes to see
//! where it came from; every 2 seconds at most. With nothing to do it
//! sniffs now and then (84 ticks) and gets 35 angrier at the nearest
//! creature within 6 blocks sideways and 20 up or down. Anger runs out by
//! one a second. At 80 it roars (84 ticks), the anger goes up by 20, and it
//! chases the most angry suspect; one that hits it angers it by 100 and,
//! within 5 blocks, gets no roar first. Its blows hit hard (30 on normal);
//! a target within 15 blocks sideways and 20 up or down that it has not hit
//! for 5 seconds, after 10 seconds of fighting, gets a sonic boom: 60 ticks,
//! fired at the 34th, 10 damage on normal through armor and walls, a big
//! push, 2 seconds before the next. Every 6 seconds players within 20
//! blocks get 13 seconds of Darkness; its heart beats faster the angrier it
//! is. A minute with nothing to disturb it and it digs back down and goes
//! (not a named one).

use mc_entity::mob::warden::{self as w, Doing};
use mc_entity::mob::{Mob, Target};
use mc_entity::{Entity, Kind, Species};

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::tick::lock;
use crate::vibrations::Heard;

const DARKNESS_EVERY: i64 = 120;
const DARKNESS_TICKS: i32 = 260;
const CALM_TICKS: i32 = 1200;
const HEARING_CD: i32 = 40;

fn rnd(n: u32) -> i32 {
    (crate::util::random_u64() % u64::from(n.max(1))) as i32
}

/// Whether it can hear now (not while it comes out or goes down, nor right
/// after it heard something).
pub fn can_hear(m: &Mob) -> bool {
    let q = &m.quirks.warden;
    !matches!(q.doing, Doing::Emerging | Doing::Digging) && q.hearing_cd == 0
}

/// A new warden comes out of the ground.
pub fn emerge(e: &mut Entity) {
    if let Kind::Mob(m) = &mut e.kind {
        m.quirks.warden.start(Doing::Emerging, w::EMERGE_TICKS);
        m.persistent = false;
        m.dirty = true;
    }
}

/// A vibration reached the warden.
pub fn heard(ctx: &ServerContext, id: i32, h: &Heard) {
    let mut entities = lock(&ctx.entities);
    let Some(e) = entities.iter_mut().find(|e| e.id == id && !e.removed) else {
        return;
    };
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.dying.is_some() || !can_hear(m) {
        return;
    }
    let q = &mut m.quirks.warden;
    q.hearing_cd = HEARING_CD;
    q.calm = 0;
    if let Some(by) = h.source.entity.filter(|&by| by != id) {
        let projectile = matches!(h.name, "projectile_land" | "projectile_shoot");
        q.anger_at(by, if projectile { 10 } else { 35 });
    }
    if q.angry_at().is_none() {
        q.investigate = Some(h.pos);
    }
    m.dirty = true;
    drop(entities);
    fx::status(ctx, id, 61);
    fx::sound(
        ctx,
        "entity.warden.tendril_clicks",
        fx::category::HOSTILE,
        pos,
        5.0,
        1.0,
    );
}

/// The warden is being hurt: not while it emerges or digs (only `/kill` and
/// the void get through); one that hurts it angers it by 100. `Some(false)`
/// keeps the hit off.
pub fn hurt(
    m: &mut Mob,
    by: Option<i32>,
    kind: &str,
    from: Option<[f64; 3]>,
    at: [f64; 3],
) -> Option<bool> {
    if m.species != Species::Warden {
        return None;
    }
    let q = &mut m.quirks.warden;
    if matches!(q.doing, Doing::Emerging | Doing::Digging)
        && !matches!(kind, "minecraft:generic_kill" | "minecraft:out_of_world")
    {
        return Some(false);
    }
    if let Some(by) = by {
        q.anger_at(by, 100);
        // Close by, it fights at once.
        let near = from.is_some_and(|f| {
            (f[0] - at[0]).powi(2) + (f[1] - at[1]).powi(2) + (f[2] - at[2]).powi(2) <= 25.0
        });
        if near && q.doing != Doing::Roaring {
            q.roared_at = Some(by);
            m.target = Some(by);
        }
    }
    None
}

/// The metadata of a warden: its anger, and the pose.
pub fn metadata(m: &Mob) -> Vec<(u8, mc_protocol::packets::play::MetadataValue)> {
    use mc_protocol::packets::play::MetadataValue;
    vec![
        (
            mc_protocol::packets::play::metadata::POSE,
            MetadataValue::Pose(m.quirks.warden.pose()),
        ),
        (16, MetadataValue::VarInt(m.quirks.warden.anger())),
    ]
}

/// Where a suspect is and whether it may still be fought: a targetable
/// player or a live mob that is no warden.
fn whereabouts(entities: &[Entity], targets: &[Target], id: i32) -> Option<[f64; 3]> {
    if let Some(t) = targets.iter().find(|t| t.id == id) {
        return t.targetable.then_some(t.pos);
    }
    entities
        .iter()
        .find(|e| e.id == id && !e.removed)
        .and_then(|e| match &e.kind {
            Kind::Mob(m) if m.dying.is_none() && m.species != Species::Warden => Some(e.pos),
            Kind::Merchant(_) => Some(e.pos),
            _ => None,
        })
}

fn flat_and_up(a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
    ((a[0] - b[0]).hypot(a[2] - b[2]), (a[1] - b[1]).abs())
}

enum Deed {
    Sound(&'static str, f32),
    Status(i8),
    Boom(i32),
    Darkness,
    Gone,
    Sniffed,
}

/// One tick of every warden.
pub fn tick(ctx: &ServerContext, entities: &mut [Entity], targets: &[Target], now: i64) {
    let mut deeds: Vec<(usize, Deed)> = Vec::new();
    for i in 0..entities.len() {
        if entities[i].removed {
            continue;
        }
        let pos = entities[i].pos;
        let suspects_at: Vec<(i32, Option<[f64; 3]>)> = match &entities[i].kind {
            Kind::Mob(m) if m.species == Species::Warden && m.dying.is_none() => m
                .quirks
                .warden
                .suspects
                .iter()
                .map(|s| (s.id, whereabouts(entities, targets, s.id)))
                .collect(),
            _ => continue,
        };
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        let before = (m.quirks.warden.pose(), m.quirks.warden.anger());
        let named = m.custom_name.is_some() || m.persistent;
        let q = &mut m.quirks.warden;
        q.hearing_cd = (q.hearing_cd - 1).max(0);
        q.sniff_cd = (q.sniff_cd - 1).max(0);
        q.boom_cd = (q.boom_cd - 1).max(0);
        q.touch_cd = (q.touch_cd - 1).max(0);
        q.since_attack += 1;
        // Suspects gone or out of reach are forgotten; anger fades.
        q.suspects.retain(|s| {
            suspects_at
                .iter()
                .any(|(id, at)| *id == s.id && at.is_some())
        });
        if now % 20 == 0 {
            for s in &mut q.suspects {
                s.anger -= 1;
            }
            q.suspects.retain(|s| s.anger > 0);
        }
        // What it is doing runs its course.
        if q.busy() {
            q.left -= 1;
            if q.doing == Doing::Booming
                && q.left == w::BOOM_TICKS - w::BOOM_FIRES
                && let Some(t) = m.target
            {
                deeds.push((i, Deed::Boom(t)));
            }
            if q.left <= 0 {
                match q.doing {
                    Doing::Digging => deeds.push((i, Deed::Gone)),
                    Doing::Sniffing => deeds.push((i, Deed::Sniffed)),
                    Doing::Roaring => {
                        m.target = q.roared_at;
                    }
                    Doing::Booming => q.boom_cd = 40,
                    _ => {}
                }
                q.doing = Doing::Nothing;
            }
        }
        let q = &mut m.quirks.warden;
        // The one to fight.
        match q.angry_at() {
            Some(id) if !q.busy() => {
                if q.roared_at != Some(id) {
                    q.roared_at = Some(id);
                    if let Some(s) = q.suspects.iter_mut().find(|s| s.id == id) {
                        s.anger = (s.anger + 20).min(150);
                    }
                    q.start(Doing::Roaring, w::ROAR_TICKS);
                    q.investigate = None;
                    m.target = None;
                    deeds.push((i, Deed::Sound("entity.warden.roar", 3.0)));
                } else {
                    m.target = Some(id);
                }
            }
            None => {
                m.target = None;
                q.roared_at = None;
                q.fighting = 0;
            }
            _ => {}
        }
        let q = &mut m.quirks.warden;
        if let Some(t) = m.target {
            q.calm = 0;
            q.fighting += 1;
            let at = suspects_at
                .iter()
                .find(|(id, _)| *id == t)
                .and_then(|(_, a)| *a);
            if let Some(at) = at {
                let (flat, up) = flat_and_up(pos, at);
                let close = flat <= 2.5 && up <= 3.0;
                if close && m.attack_cooldown > 0 {
                    q.since_attack = 0;
                }
                if !q.busy()
                    && q.boom_cd == 0
                    && q.fighting >= 200
                    && q.since_attack >= 100
                    && flat <= 15.0
                    && up <= 20.0
                {
                    q.start(Doing::Booming, w::BOOM_TICKS);
                    q.since_attack = 0;
                    deeds.push((i, Deed::Status(62)));
                    deeds.push((i, Deed::Sound("entity.warden.sonic_charge", 3.0)));
                }
            }
        } else if !q.busy() {
            q.calm += 1;
            if q.calm >= CALM_TICKS && !named {
                q.start(Doing::Digging, w::DIG_TICKS);
                deeds.push((i, Deed::Sound("entity.warden.dig", 5.0)));
            } else if q.sniff_cd == 0 && q.investigate.is_none() {
                q.start(Doing::Sniffing, w::SNIFF_TICKS);
                q.sniff_cd = 100 + rnd(100);
                deeds.push((i, Deed::Sound("entity.warden.sniff", 5.0)));
            }
        }
        // Its heart, its voice, the darkness about it.
        let anger = q.anger();
        q.heartbeat -= 1;
        if q.heartbeat <= 0 {
            q.heartbeat = 40 - (30.0 * (f64::from(anger) / 80.0).clamp(0.0, 1.0)).floor() as i32;
            deeds.push((i, Deed::Sound("entity.warden.heartbeat", 5.0)));
        }
        if !q.busy() && rnd(200) == 0 {
            let voice = if anger >= 80 {
                "entity.warden.angry"
            } else if anger >= 40 {
                "entity.warden.agitated"
            } else {
                "entity.warden.ambient"
            };
            deeds.push((i, Deed::Sound(voice, 3.0)));
        }
        if now % DARKNESS_EVERY == 0 && !matches!(q.doing, Doing::Emerging | Doing::Digging) {
            deeds.push((i, Deed::Darkness));
        }
        if (m.quirks.warden.pose(), m.quirks.warden.anger()) != before {
            m.dirty = true;
        }
    }
    for (i, deed) in deeds {
        let (id, pos) = (entities[i].id, entities[i].pos);
        match deed {
            Deed::Sound(name, volume) => {
                fx::sound(ctx, name, fx::category::HOSTILE, pos, volume, 1.0);
            }
            Deed::Status(s) => fx::status(ctx, id, s),
            Deed::Boom(target) => boom(ctx, entities, i, target, targets),
            Deed::Darkness => darkness(ctx, pos, targets),
            Deed::Gone => {
                entities[i].removed = true;
                let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
            }
            Deed::Sniffed => sniffed(entities, i, targets),
        }
    }
}

/// After a sniff: the nearest creature within 6 blocks sideways and 20 up
/// or down angers it by 35.
fn sniffed(entities: &mut [Entity], i: usize, targets: &[Target]) {
    let pos = entities[i].pos;
    let mut best: Option<(f64, i32)> = None;
    let mut consider = |id: i32, at: [f64; 3]| {
        let (flat, up) = flat_and_up(pos, at);
        if flat <= 6.0 && up <= 20.0 {
            let d = flat * flat + up * up;
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, id));
            }
        }
    };
    for t in targets.iter().filter(|t| t.targetable) {
        consider(t.id, t.pos);
    }
    for e in entities.iter().filter(|e| !e.removed) {
        if let Kind::Mob(m) = &e.kind
            && m.species != Species::Warden
            && m.dying.is_none()
        {
            consider(e.id, e.pos);
        }
    }
    if let Some((_, id)) = best
        && let Kind::Mob(m) = &mut entities[i].kind
    {
        m.quirks.warden.anger_at(id, 35);
    }
}

/// Darkness for 13 seconds on players within 20 blocks who have less.
fn darkness(ctx: &ServerContext, at: [f64; 3], targets: &[Target]) {
    let Some(kind) = crate::effects::kind_of("darkness") else {
        return;
    };
    for t in targets.iter().filter(|t| t.present) {
        if super::dist_sq(t.pos, at) > 400.0 {
            continue;
        }
        let long = lock(&ctx.vitals)
            .get(&t.uuid)
            .and_then(|v| v.effects.active.get("darkness"))
            .is_some_and(|i| i.duration > 200);
        if !long {
            crate::effects::give(
                ctx,
                t.uuid,
                kind,
                crate::effects::Instance::new(0, DARKNESS_TICKS, false),
            );
        }
    }
}

/// The sonic boom goes off: through everything, at the target.
fn boom(ctx: &ServerContext, entities: &mut [Entity], i: usize, target: i32, targets: &[Target]) {
    let from = {
        let e = &entities[i];
        [e.pos[0], e.pos[1] + 1.6, e.pos[2]]
    };
    let to = targets
        .iter()
        .find(|t| t.id == target)
        .map(|t| t.pos)
        .or_else(|| entities.iter().find(|e| e.id == target).map(|e| e.pos));
    let Some(to) = to else {
        return;
    };
    let to = [to[0], to[1] + 1.0, to[2]];
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
    let steps = len.floor() as i32 + 7;
    for k in 1..steps {
        let t = f64::from(k) / len;
        if t > 1.0 {
            break;
        }
        fx::particles(
            ctx,
            "minecraft:sonic_boom",
            [from[0] + d[0] * t, from[1] + d[1] * t, from[2] + d[2] * t],
            0.0,
            0.0,
            1,
        );
    }
    fx::sound(
        ctx,
        "entity.warden.sonic_boom",
        fx::category::HOSTILE,
        from,
        3.0,
        1.0,
    );
    super::aqua::hurt_with(
        ctx,
        entities,
        i,
        target,
        10.0,
        "minecraft:sonic_boom",
        "death.attack.sonicBoom",
        targets,
        ctx.difficulty(),
        2.5,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anger_rises_to_150_and_the_angriest_is_fought() {
        let mut q = w::Warden::default();
        q.anger_at(1, 35);
        q.anger_at(2, 70);
        assert_eq!(q.angry_at(), None);
        q.anger_at(1, 50);
        assert_eq!(q.angry_at(), Some(1));
        q.anger_at(2, 500);
        assert_eq!(q.anger(), 150);
        assert_eq!(q.angry_at(), Some(2));
    }
}
