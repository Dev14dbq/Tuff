//! Snowballs, eggs, ender pearls and splash potions in flight.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use super::{knock, potions};
use crate::context::Event;
use crate::fx;
use crate::hits::{self, Strike, Tick};

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let (shot, owner) = (p.shot.clone(), p.owner);
    let ctx = t.ctx;
    e.age += 1;
    // A pearl dies with the thrower leaving; anything lost in the sky goes.
    let gone = matches!(shot, Shot::Pearl)
        && owner.is_some_and(|o| !t.players.iter().any(|w| w.uuid == o));
    if e.pos[1] < -80.0 || e.age > 6000 || gone {
        e.removed = true;
        return;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let ignore = if e.age < 5 { owner } else { None };
    match hits::trace(t, from, to, 0.125, ignore, &[]) {
        None => e.pos = to,
        Some((f, strike)) => {
            e.pos = [
                from[0] + e.vel[0] * f,
                from[1] + e.vel[1] * f,
                from[2] + e.vel[2] * f,
            ];
            hits::hit_block(ctx, &strike, e.pos, false);
            let who = match &strike {
                Strike::Player(i) => Some(t.players[*i]),
                _ => None,
            };
            land(t, e, &shot, owner, who, &strike);
            e.removed = true;
            return;
        }
    }
    let gravity = if matches!(shot, Shot::Potion { .. }) {
        0.05
    } else {
        0.03
    };
    let drag = if hits::in_water(ctx, e.pos) {
        0.8
    } else {
        0.99
    };
    e.vel = e.vel.map(|c| c * drag);
    e.vel[1] -= gravity;
}

fn land(
    t: &mut Tick,
    e: &Entity,
    shot: &Shot,
    owner: Option<u128>,
    who: Option<hits::Who>,
    strike: &Strike,
) {
    let ctx = t.ctx;
    match shot {
        Shot::Snowball | Shot::Egg => {
            // Particles of the thing breaking; no damage, but it pushes.
            fx::status(ctx, e.id, 3);
            if let Some(w) = who {
                knock(
                    ctx,
                    &w,
                    [w.pose.x - e.vel[0], 0.0, w.pose.z - e.vel[2]],
                    0.4,
                );
            }
            if let Strike::Target(i) = strike {
                t.out.hits.push(hits::Hit {
                    target: t.targets[*i].id,
                    amount: 0.0,
                    by: owner,
                    with: Some(e.id),
                    kind: "minecraft:thrown",
                });
            }
        }
        Shot::Pearl => {
            fx::particles(ctx, "minecraft:portal", e.pos, 0.3, 0.1, 32);
            let Some(o) = owner else { return };
            let Some(me) = t.players.iter().find(|w| w.uuid == o) else {
                return;
            };
            // Back out of the wall it hit.
            let n = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt().max(1e-9);
            let spot = [
                e.pos[0] - e.vel[0] / n * 0.3,
                e.pos[1] - e.vel[1] / n * 0.3,
                e.pos[2] - e.vel[2] / n * 0.3,
            ];
            let to = fit(ctx, spot);
            let pose = crate::context::Pose {
                x: to[0],
                y: to[1],
                z: to[2],
                on_ground: false,
                ..me.pose
            };
            let _ = ctx.events.send(Event::Teleport { uuid: o, pose });
            // Landing hurts: 5 points, as a fall.
            crate::vitals::hurt(ctx, o, 5.0, "minecraft:ender_pearl", "death.attack.fall");
            fx::sound(
                ctx,
                "entity.player.teleport",
                fx::category::PLAYERS,
                to,
                1.0,
                1.0,
            );
        }
        Shot::Potion {
            effects,
            color,
            lingering,
            ..
        } => {
            potions::shatter(t, e.pos, effects, *color, *lingering, who.as_ref());
        }
        _ => {}
    }
}

/// A spot a player fits at, near where a pearl came down: backed out of
/// walls and floors, with room for their body.
fn fit(ctx: &crate::context::ServerContext, at: [f64; 3]) -> [f64; 3] {
    let mut p = at;
    for _ in 0..4 {
        let (x, z) = (p[0].floor() as i32, p[2].floor() as i32);
        let feet = p[1].floor() as i32;
        if !hits::stops(ctx, [x, feet, z]) && !hits::stops(ctx, [x, feet + 1, z]) {
            return p;
        }
        p[1] += 1.0;
        p[1] = p[1].floor();
    }
    at
}
