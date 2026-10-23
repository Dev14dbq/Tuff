//! Firework rockets: they rise, speeding up, and burst when their flight
//! ends or they hit something. Clients draw the burst (entity status 17);
//! the stars' colours live in item components we cannot send yet, so
//! bursts show as plain ones. The damage counts the stars.

use mc_entity::extra::{Firework, Shot};
use mc_entity::{Entity, Kind};
use mc_nbt::Tag;
use mc_protocol::packets::play::ItemStack;

use super::spawn;
use crate::fx;
use crate::held::Held;
use crate::hits::{self, Hit, Strike, Tick};
use crate::interact::Actor;

/// Flight duration and the number of stars of a rocket stack's components.
fn read(comps: Option<&Tag>) -> (i32, usize) {
    let Some(Tag::Compound(c)) = comps else {
        return (1, 0);
    };
    let Some(Tag::Compound(f)) = c.get("minecraft:fireworks") else {
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

/// Sends a rocket up from `pos`.
pub fn launch(
    ctx: &crate::context::ServerContext,
    actor: Actor,
    held: &Held,
    pos: [f64; 3],
    at_angle: bool,
) -> i32 {
    let (flight, stars) = read(held.comps.as_ref());
    let life = 10 * (flight + 1) + (hits::rnd() * 5.0) as i32 + (hits::rnd() * 6.0) as i32;
    let shot = Shot::Firework(Firework {
        life,
        boosts: None,
        stars: vec![Vec::new(); stars],
        shot_at_angle: at_angle,
        stack: Some(ItemStack {
            count: 1,
            ..held.stack.clone()
        }),
    });
    let vel = [hits::gauss() * 0.0006, 0.05, hits::gauss() * 0.0006];
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
    let (stars, ended) = (f.stars.len(), f.life <= 0);
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
                burst = true;
                if let Strike::Target(i) = strike {
                    t.out.hits.push(Hit {
                        target: t.targets[i].id,
                        amount: if stars > 0 {
                            5.0 + 2.0 * stars as f32
                        } else {
                            0.0
                        },
                        by: owner,
                        with: Some(e.id),
                        kind: "minecraft:fireworks",
                    });
                }
            }
        }
    }
    if burst {
        explode(t, e, stars, owner);
        e.removed = true;
        return;
    }
    // Faster and faster, and up.
    e.vel = [e.vel[0] * 1.15, e.vel[1] + 0.04, e.vel[2] * 1.15];
    fx::particles(t.ctx, "minecraft:firework", e.pos, 0.0, 0.0, 1);
}

/// The burst hurts players within 5 blocks, by 5 plus 2 for each star.
fn explode(t: &mut Tick, e: &Entity, stars: usize, owner: Option<u128>) {
    fx::status(t.ctx, e.id, 17);
    if stars == 0 {
        return;
    }
    let amount = 5.0 + 2.0 * stars as f32;
    let attacker = owner.and_then(|o| hits::name_of(t.ctx, o));
    for w in t.players {
        let d2 = (w.pose.x - e.pos[0]).powi(2)
            + (w.pose.y + 0.9 - e.pos[1]).powi(2)
            + (w.pose.z - e.pos[2]).powi(2);
        if w.solid() && d2 <= 25.0 {
            crate::vitals::try_hurt_by_entity(
                t.ctx,
                w.uuid,
                amount,
                "minecraft:fireworks",
                "death.attack.fireworks",
                attacker.as_deref(),
                Some(e.id),
            );
        }
    }
}
