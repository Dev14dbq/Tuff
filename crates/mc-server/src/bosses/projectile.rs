//! The bosses' projectiles (minecraft.wiki, "Wither Skull", "Dragon
//! Fireball", "Area Effect Cloud"):
//! - both fly straight, speeding up by 0.1 a tick along their way and
//!   keeping 95% of their speed (a blue skull 73%, so it is slow);
//! - a skull hurts what it hits (8, scaled by difficulty; 5 magic without a
//!   wither behind it) and withers it (II, 10 seconds on normal, 40 on
//!   hard), then explodes with power 1 (the wither's: `mob_griefing`);
//! - a dragon fireball leaves a cloud of dragon's breath, 3 blocks wide and
//!   growing to 7 over 30 seconds, on the nearest creature within 4 blocks;
//! - breath clouds harm (at half strength: 3 for harming I, 6 for II)
//!   whoever stands in them, each once a second; the dragon's own breath
//!   on the portal is 5 wide for ten seconds.

use mc_entity::extra::{Projectile, Shot};
use mc_entity::{Entity, Kind};

use crate::hits::{self, Strike, Tick};

const BELOW_WORLD: f64 = -128.0;
/// Ticks a cloud waits before it acts.
const WAIT: u32 = 20;

/// A cloud of dragon's breath at `at`.
pub fn breath(
    ctx: &crate::context::ServerContext,
    owner: i32,
    at: [f64; 3],
    radius: f32,
    grow: f32,
    duration: i32,
    amplifier: i32,
) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(Projectile {
            shot: Shot::Breath {
                radius,
                grow,
                left: duration + WAIT as i32,
                amplifier,
                hit: Vec::new(),
            },
            owner: None,
            mob: Some((owner, "Ender Dragon")),
            yaw: 0.0,
            pitch: 0.0,
        }),
        pos: at,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    match p.shot {
        Shot::WitherSkull { dangerous } => fly(t, e, Some(dangerous)),
        Shot::DragonFireball => fly(t, e, None),
        Shot::Breath { .. } => cloud(t, e),
        _ => {}
    }
}

/// A skull (`Some(dangerous)`) or a dragon fireball (`None`).
fn fly(t: &mut Tick, e: &mut Entity, skull: Option<bool>) {
    e.age += 1;
    if e.pos[1] < BELOW_WORLD {
        e.removed = true;
        return;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let skip: Vec<i32> = t.shooter.map(|(id, _)| id).into_iter().collect();
    let half = if skull.is_some() { 0.15625 } else { 0.5 };
    let Some((f, strike)) = hits::trace(t, from, to, half, None, &skip) else {
        e.pos = to;
        let inertia = if hits::in_water(t.ctx, e.pos) {
            0.8
        } else if skull == Some(true) {
            0.73
        } else {
            0.95
        };
        let len = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt();
        if len > 1e-9 {
            for c in &mut e.vel {
                *c = (*c + *c / len * 0.1) * inertia;
            }
        }
        return;
    };
    e.pos = [
        from[0] + e.vel[0] * f,
        from[1] + e.vel[1] * f,
        from[2] + e.vel[2] * f,
    ];
    e.removed = true;
    if matches!(strike, Strike::Block { .. }) {
        crate::pots::projectile(t.ctx, from, to);
    }
    match skull {
        Some(dangerous) => skull_hit(t, e, strike, dangerous),
        None => fireball_hit(t, e),
    }
}

fn skull_hit(t: &mut Tick, e: &Entity, strike: Strike, dangerous: bool) {
    let wither = t.shooter;
    let difficulty = t.ctx.difficulty();
    let seconds = match difficulty {
        2 => 10,
        3 => 40,
        _ => 0,
    };
    let (amount, kind) = if wither.is_some() {
        (8.0, "minecraft:wither_skull")
    } else {
        (5.0, "minecraft:magic")
    };
    match strike {
        Strike::Player(i) => {
            let victim = t.players[i];
            let scaled = if wither.is_some() {
                crate::damage::scale_for_difficulty(amount, difficulty)
            } else {
                amount
            };
            let key = if wither.is_some() {
                "death.attack.witherSkull"
            } else {
                "death.attack.magic"
            };
            if crate::projectiles::hurt_player(t, &victim, scaled, kind, key, None, e.id)
                && seconds > 0
                && let Some(k) = crate::effects::kind_of("minecraft:wither")
            {
                crate::effects::give(
                    t.ctx,
                    victim.uuid,
                    k,
                    crate::effects::Instance::new(1, 20 * seconds, true),
                );
            }
        }
        Strike::Target(i) => {
            t.out.hits.push(hits::Hit {
                target: t.targets[i].id,
                amount,
                by: None,
                with: Some(e.id),
                kind,
            });
            // A mob it hits withers too.
            if seconds > 0 && t.targets[i].kind.is_some() {
                t.out.doses.push(hits::Dose {
                    target: t.targets[i].id,
                    effects: vec![("minecraft:wither".into(), 1, 20 * seconds)],
                    strength: 1.0,
                    splash: false,
                    by: None,
                });
            }
        }
        Strike::Block { .. } => {
            hits::hit_block(t.ctx, &strike, e.pos, false);
        }
    }
    t.out.blasts.push(crate::explosion::Blast {
        center: e.pos,
        power: 1.0,
        source: wither.map_or(e.id, |(id, _)| id),
        owner: None,
        mob: true,
        dangerous,
        by: Some("minecraft:wither"),
        fire: false,
        respawn: None,
    });
}

/// A fireball leaves a growing cloud of breath, on a creature near.
fn fireball_hit(t: &mut Tick, e: &Entity) {
    let mut at = e.pos;
    let near = t
        .players
        .iter()
        .filter(|w| w.solid())
        .map(|w| w.pose_vec())
        .chain(t.targets.iter().map(|g| {
            let b = g.aabb;
            [
                (b.min[0] + b.max[0]) / 2.0,
                b.min[1],
                (b.min[2] + b.max[2]) / 2.0,
            ]
        }))
        .filter(|p| {
            (p[0] - at[0]).abs() <= 4.5
                && (p[1] - at[1]).abs() <= 2.5
                && (p[2] - at[2]).abs() <= 4.5
        })
        .find(|p| super::dist_sq(*p, at) < 16.0);
    if let Some(p) = near {
        at = p;
    }
    let owner = t.shooter.map_or(0, |(id, _)| id);
    crate::fx::level_event(t.ctx, 2006, at, 1);
    t.out
        .spawn
        .push(breath(t.ctx, owner, at, 3.0, (7.0 - 3.0) / 600.0, 600, 1));
}

/// A cloud: it grows, harms those in it once a second each, and goes.
fn cloud(t: &mut Tick, e: &mut Entity) {
    let age = e.age;
    e.age += 1;
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Shot::Breath {
        radius,
        grow,
        left,
        amplifier,
        hit,
    } = &mut p.shot
    else {
        return;
    };
    *left -= 1;
    if *left <= 0 {
        e.removed = true;
        return;
    }
    if age < WAIT {
        return;
    }
    if *grow != 0.0 {
        *radius += *grow;
        if *radius < 0.5 {
            e.removed = true;
            return;
        }
        if age.is_multiple_of(5) {
            t.out.data.push(mc_protocol::packets::play::SetEntityData {
                entity_id: e.id,
                entries: vec![(8, mc_protocol::packets::play::MetadataValue::Float(*radius))],
            });
        }
    }
    if !age.is_multiple_of(5) {
        return;
    }
    hit.retain(|(_, until)| *until > age);
    let r2 = f64::from(*radius).powi(2);
    let damage = (3 << *amplifier) as f32;
    let attacker = t.shooter.map(|(_, name)| name);
    for who in t.players {
        let (dx, dz) = (who.pose.x - e.pos[0], who.pose.z - e.pos[2]);
        let inside =
            dx * dx + dz * dz <= r2 && who.pose.y + 1.8 > e.pos[1] && who.pose.y < e.pos[1] + 0.5;
        if !who.solid() || !inside || hit.iter().any(|(u, _)| *u == who.uuid) {
            continue;
        }
        hit.push((who.uuid, age + 20));
        crate::vitals::try_hurt_by_entity(
            t.ctx,
            who.uuid,
            damage,
            "minecraft:indirect_magic",
            "death.attack.indirectMagic",
            attacker,
            Some(e.id),
        );
    }
}
