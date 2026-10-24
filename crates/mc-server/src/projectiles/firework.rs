//! Firework rockets (minecraft.wiki, "Firework Rocket"): they rise,
//! speeding up (straight on, without speeding, when a crossbow shot them),
//! and burst when their flight ends or they hit something. Clients draw
//! the burst (entity event 17) from the rocket's item, which carries its
//! `fireworks` component. A rocket with stars hurts by 5 plus 2 a star:
//! the one it is attached to in full, everything else within 5 blocks it
//! can see less with distance. Used while gliding with an elytra, a rocket
//! carries its user along.

use mc_entity::extra::{Firework, Shot};
use mc_entity::{Entity, Kind};
use mc_nbt::Tag;
use mc_protocol::packets::play::ItemStack;

use super::spawn;
use crate::fx;
use crate::held::Held;
use crate::hits::{self, Hit, Strike, Tick};
use crate::interact::Actor;

/// Flight duration and the number of stars of a rocket stack.
pub(crate) fn read(stack: &ItemStack) -> (i32, usize) {
    let Some(Tag::Compound(f)) = stack.components.get("minecraft:fireworks") else {
        return (1, 0);
    };
    let flight = match f.get("flight_duration") {
        Some(Tag::Byte(v)) => i32::from(*v),
        Some(Tag::Int(v)) => *v,
        _ => 1,
    };
    let stars = match f.get("explosions") {
        Some(Tag::List(l)) => l.len(),
        _ => 0,
    };
    (flight, stars)
}

fn below(n: u64) -> i32 {
    (crate::util::random_u64() % n) as i32
}

/// The rocket a stack makes: it flies `10 × (flight + 1)` ticks and up to
/// 11 more.
pub fn rocket(stack: &ItemStack, boosts: Option<u128>, at_angle: bool) -> Firework {
    let (flight, stars) = read(stack);
    Firework {
        life: 10 * (flight + 1) + below(6) + below(7),
        boosts,
        stars: vec![Vec::new(); stars],
        shot_at_angle: at_angle,
        stack: Some(stack.with_count(1)),
    }
}

/// Uniform on (-1, 1) with a peak in the middle, times `spread`.
fn triangle(spread: f64) -> f64 {
    (hits::rnd() - hits::rnd()) * spread
}

/// Sends a rocket up from `pos`.
pub fn launch(
    ctx: &crate::context::ServerContext,
    actor: Actor,
    held: &Held,
    pos: [f64; 3],
    at_angle: bool,
) -> i32 {
    let shot = Shot::Firework(rocket(&held.stack, None, at_angle));
    let vel = [triangle(0.002_297), 0.05, triangle(0.002_297)];
    let id = spawn(ctx, shot, Some(actor.uuid), pos, vel);
    fx::sound(
        ctx,
        "entity.firework_rocket.launch",
        fx::category::AMBIENT,
        pos,
        3.0,
        1.0,
    );
    id
}

/// A gliding player uses a rocket: it carries them.
pub fn boost(ctx: &crate::context::ServerContext, actor: Actor, held: &Held) -> i32 {
    let shot = Shot::Firework(rocket(&held.stack, Some(actor.uuid), false));
    let pos = [actor.pose.x, actor.pose.y, actor.pose.z];
    let id = spawn(ctx, shot, Some(actor.uuid), pos, [0.0; 3]);
    fx::sound(
        ctx,
        "entity.firework_rocket.launch",
        fx::category::AMBIENT,
        pos,
        3.0,
        1.0,
    );
    id
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let owner = p.owner;
    let Shot::Firework(f) = &mut p.shot else {
        return;
    };
    e.age += 1;
    f.life -= 1;
    let (stars, ended, boosts, straight) = (f.stars.len(), f.life <= 0, f.boosts, f.shot_at_angle);
    if let Some(rider) = boosts {
        // It goes where its rider goes; the rider's client flies itself.
        match t.players.iter().find(|w| w.uuid == rider) {
            Some(w) => e.pos = [w.pose.x, w.pose.y, w.pose.z],
            None => {
                e.removed = true;
                return;
            }
        }
        if ended {
            explode(t, e, stars, owner, boosts);
            e.removed = true;
        }
        return;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let mut burst = ended;
    if !ended {
        match hits::trace(
            t,
            from,
            to,
            0.125,
            if e.age < 5 { owner } else { None },
            &[],
        ) {
            None => e.pos = to,
            Some((frac, strike)) => {
                e.pos = [
                    from[0] + e.vel[0] * frac,
                    from[1] + e.vel[1] * frac,
                    from[2] + e.vel[2] * frac,
                ];
                // A block stops a rocket without stars; anything else
                // sets it off.
                burst = !matches!(strike, Strike::Block) || stars > 0;
            }
        }
    }
    if burst {
        explode(t, e, stars, owner, None);
        e.removed = true;
        return;
    }
    if !straight {
        // Faster and faster, and up.
        e.vel = [e.vel[0] * 1.15, e.vel[1] + 0.04, e.vel[2] * 1.15];
    }
    fx::particles(t.ctx, "minecraft:firework", e.pos, 0.0, 0.0, 1);
}

/// Damage of a burst of `stars` stars at `distance`: 5 + 2 a star, less
/// the farther (none beyond 5 blocks).
pub fn damage(stars: usize, distance: f64) -> f32 {
    if stars == 0 || distance > 5.0 {
        return 0.0;
    }
    let f = 5.0 + 2.0 * stars as f64;
    (f * ((5.0 - distance) / 5.0).sqrt()) as f32
}

/// Whether a ray from the rocket to the feet or the middle of a box is
/// clear of blocks.
fn sees(t: &Tick, from: [f64; 3], b: &mc_entity::Aabb) -> bool {
    let x = (b.min[0] + b.max[0]) / 2.0;
    let z = (b.min[2] + b.max[2]) / 2.0;
    [b.min[1], (b.min[1] + b.max[1]) / 2.0]
        .iter()
        .any(|&y| hits::first_solid(t.ctx, from, [x, y, z]).is_none())
}

/// The burst: clients draw it; it hurts the rider in full and the others
/// near by distance.
fn explode(t: &mut Tick, e: &Entity, stars: usize, owner: Option<u128>, rider: Option<u128>) {
    fx::status(t.ctx, e.id, 17);
    if stars == 0 {
        return;
    }
    let full = 5.0 + 2.0 * stars as f32;
    let attacker = owner.and_then(|o| hits::name_of(t.ctx, o));
    let hurt = |uuid: u128, amount: f32| {
        crate::vitals::try_hurt_by_entity(
            t.ctx,
            uuid,
            amount,
            "minecraft:fireworks",
            "death.attack.fireworks",
            attacker.as_deref(),
            Some(e.id),
        );
    };
    for w in t.players {
        if !w.solid() {
            continue;
        }
        if Some(w.uuid) == rider {
            hurt(w.uuid, full);
            continue;
        }
        let d = ((w.pose.x - e.pos[0]).powi(2)
            + (w.pose.y - e.pos[1]).powi(2)
            + (w.pose.z - e.pos[2]).powi(2))
        .sqrt();
        if d <= 5.0 && sees(t, e.pos, &w.aabb()) {
            hurt(w.uuid, damage(stars, d));
        }
    }
    for g in t.targets {
        if g.kind.is_none() {
            continue;
        }
        let b = g.aabb;
        let feet = [
            (b.min[0] + b.max[0]) / 2.0,
            b.min[1],
            (b.min[2] + b.max[2]) / 2.0,
        ];
        let d = ((feet[0] - e.pos[0]).powi(2)
            + (feet[1] - e.pos[1]).powi(2)
            + (feet[2] - e.pos[2]).powi(2))
        .sqrt();
        if d <= 5.0 && sees(t, e.pos, &b) {
            t.out.hits.push(Hit {
                target: g.id,
                amount: damage(stars, d),
                by: owner,
                with: Some(e.id),
                kind: "minecraft:fireworks",
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{flat, join, wear};
    use crate::tick::lock;

    #[test]
    fn burst_damage_falls_with_distance() {
        assert_eq!(damage(0, 0.0), 0.0);
        assert_eq!(damage(2, 0.0), 9.0);
        assert!((damage(2, 3.75) - 4.5).abs() < 1e-5);
        assert_eq!(damage(2, 5.5), 0.0);
    }

    #[test]
    fn flight_follows_the_duration_and_crossbow_rockets_go_straight() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let stack = super::super::crossbow::rocket_stack(&ctx, 1, 0);
        for _ in 0..50 {
            let r = rocket(&stack, None, false);
            assert!((20..=31).contains(&r.life), "{}", r.life);
        }
        let mut e = Entity {
            id: 1,
            uuid: 1,
            kind: Kind::Projectile(mc_entity::extra::Projectile {
                shot: Shot::Firework(rocket(&stack, None, true)),
                owner: None,
                mob: None,
                yaw: 0.0,
                pitch: 0.0,
            }),
            pos: [0.5, -50.0, 0.5],
            vel: [0.0, 0.0, 1.6],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        };
        let mut up = e.clone();
        if let Kind::Projectile(p) = &mut up.kind {
            p.shot = Shot::Firework(rocket(&stack, None, false));
        }
        up.vel = [0.0, 0.05, 0.0];
        let mut out = hits::Out::default();
        for _ in 0..5 {
            let mut t = Tick {
                ctx: &ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
            tick(&mut t, &mut up);
        }
        assert_eq!(e.vel, [0.0, 0.0, 1.6]);
        assert!((up.vel[1] - 0.25).abs() < 1e-9);
    }

    #[test]
    fn a_boosting_rocket_rides_with_its_glider_and_hurts_them() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let u = 0x9101;
        join(&ctx, u, 9201, [0.5, -40.0, 0.5], 0);
        if let Some(p) = lock(&ctx.players).get_mut(&u)
            && let Some(pose) = &mut p.pose
        {
            pose.on_ground = false;
        }
        // No elytra, no gliding.
        crate::worn::start_gliding(&ctx, u);
        assert!(!lock(&ctx.vitals).get(&u).unwrap().gliding);
        wear(&ctx, u, 6, crate::testkit::stack(&ctx, "elytra", &[]));
        crate::worn::start_gliding(&ctx, u);
        assert!(lock(&ctx.vitals).get(&u).unwrap().gliding);
        let rocket_item = super::super::crossbow::rocket_stack(&ctx, 1, 1);
        wear(&ctx, u, 36, rocket_item);
        let p = lock(&ctx.players).get(&u).cloned().unwrap();
        let actor = Actor {
            uuid: u,
            entity_id: 9201,
            game_mode: 0,
            pose: p.pose.unwrap(),
            sneaking: false,
        };
        assert!(super::super::use_item(&ctx, actor, 0, 0.0, 0.0).is_some());
        assert!(lock(&ctx.inventories).get(&u).unwrap().slots[36].is_none());
        let mut e = lock(&ctx.entities).pop().unwrap();
        let who = [hits::Who {
            uuid: u,
            entity_id: 9201,
            mode: 0,
            pose: p.pose.unwrap(),
        }];
        let mut out = hits::Out::default();
        let health = lock(&ctx.vitals).get(&u).unwrap().health;
        for _ in 0..40 {
            let mut t = Tick {
                ctx: &ctx,
                players: &who,
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
            if e.removed {
                break;
            }
            assert_eq!(e.pos, [0.5, -40.0, 0.5]);
        }
        assert!(e.removed);
        let after = lock(&ctx.vitals).get(&u).unwrap().health;
        assert_eq!(health - after, 7.0);
    }
}
