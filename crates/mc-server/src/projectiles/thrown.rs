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
    // What spat it does not catch its own spit.
    let skip: Vec<i32> = t.shooter.map(|(id, _)| id).into_iter().collect();
    match hits::trace(t, from, to, 0.125, ignore, &skip) {
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
            if matches!(strike, Strike::Block { .. }) && matches!(shot, Shot::Snowball | Shot::Egg)
            {
                crate::pots::projectile(ctx, from, to);
            }
            land(t, e, &shot, owner, who, &strike);
            e.removed = true;
            return;
        }
    }
    let gravity = if matches!(shot, Shot::Bottle) {
        0.07
    } else if matches!(shot, Shot::Potion { .. }) {
        0.05
    } else if matches!(shot, Shot::Spit) {
        0.06
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
                    // Only blazes feel a snowball.
                    amount: if matches!(shot, Shot::Snowball)
                        && t.targets[*i].kind == Some("minecraft:blaze")
                    {
                        3.0
                    } else {
                        0.0
                    },
                    by: owner,
                    with: Some(e.id),
                    kind: "minecraft:thrown",
                });
            }
        }
        Shot::Spit => match strike {
            Strike::Player(i) => {
                let victim = t.players[*i];
                super::hurt_player(
                    t,
                    &victim,
                    1.0,
                    "minecraft:spit",
                    "death.attack.spit",
                    owner,
                    e.id,
                );
            }
            Strike::Target(i) => {
                t.out.hits.push(hits::Hit {
                    target: t.targets[*i].id,
                    amount: 1.0,
                    by: owner,
                    with: Some(e.id),
                    kind: "minecraft:spit",
                });
            }
            Strike::Block { .. } => {}
        },
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
            crate::mobs::endermite::from_pearl(ctx, [me.pose.x, me.pose.y, me.pose.z]);
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
        Shot::Bottle => {
            // Splash particles of water's colour, then 3 to 11 points.
            fx::level_event(ctx, 2002, e.pos, 0x385dc6);
            crate::xp::spawn(ctx, e.pos, bottle_points());
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

/// What a bottle o' enchanting is worth: 3 and two rolls of 0 to 4
/// (minecraft.wiki, "Bottle o' Enchanting").
fn bottle_points() -> i32 {
    3 + (hits::rnd() * 5.0) as i32 + (hits::rnd() * 5.0) as i32
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::{spawn, tests::flat};
    use crate::tick::lock;

    fn orb_points(ctx: &crate::context::ServerContext) -> i32 {
        lock(&ctx.entities)
            .iter()
            .map(|e| match e.kind {
                Kind::ExperienceOrb { value, count } => value * count,
                _ => 0,
            })
            .sum()
    }

    #[test]
    fn a_bottle_is_worth_three_to_eleven_points() {
        let (mut low, mut high) = (i32::MAX, 0);
        for _ in 0..2000 {
            let n = bottle_points();
            (low, high) = (low.min(n), high.max(n));
        }
        assert_eq!((low, high), (3, 11));
    }

    #[test]
    fn a_thrown_bottle_breaks_on_the_ground_into_orbs() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut rx = ctx.events.subscribe();
        let id = spawn(
            &ctx,
            Shot::Bottle,
            None,
            [0.5, -58.0, 0.5],
            [0.0, -0.5, 0.0],
        );
        let mut e = {
            let mut entities = lock(&ctx.entities);
            let i = entities.iter().position(|e| e.id == id).unwrap();
            entities.swap_remove(i)
        };
        let mut out = hits::Out::default();
        for _ in 0..20 {
            let mut t = Tick {
                ctx: &ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            crate::projectiles::tick(&mut t, &mut e);
            if e.removed {
                break;
            }
        }
        assert!(e.removed);
        let points = orb_points(&ctx);
        assert!((3..=11).contains(&points), "{points}");
        // The splash is shown in water's colour.
        let mut splashed = false;
        while let Some(ev) = rx.try_changed() {
            if let Event::Fx(fx::Fx::Level(l)) = ev {
                splashed |= l.event == 2002 && l.data == 0x385dc6;
            }
        }
        assert!(splashed);
    }
}
