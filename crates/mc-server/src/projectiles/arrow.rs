//! Arrows: shot from a bow, flying, sticking in blocks, hurting what they
//! hit, and picked up again.

use mc_entity::extra::{Arrow, Pickup, Shot};
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::ItemStack;

use super::{hurt_player, knock, look, muzzle};
use crate::context::Event;
use crate::fx;
use crate::held;
use crate::hits::{self, Hit, Strike, Tick};
use crate::interact::Actor;

const GRAVITY: f64 = 0.05;
/// Ticks a stuck arrow lasts.
const LIFE: i32 = 1200;

/// Shoots the bow in `hand`, drawn for `ticks`.
pub fn fire_bow(ctx: &crate::context::ServerContext, actor: Actor, hand: i32, ticks: i32) {
    let Some(bow) = held::held(ctx, actor.uuid, hand).filter(|h| h.name == "minecraft:bow") else {
        return;
    };
    let creative = actor.game_mode == 1;
    let infinity = held::enchant(bow.comps.as_ref(), "infinity") > 0;
    let ammo = held::find(ctx, actor.uuid, super::is_arrow);
    if ammo.is_none() && !creative && !infinity {
        return;
    }
    let power = power(ticks);
    if power < 0.1 {
        return;
    }
    let (item, name, comps) = match &ammo {
        Some(a) => (a.stack.item, a.name.as_str(), a.comps.as_ref()),
        None => (
            ctx.items.id("minecraft:arrow").unwrap_or(0),
            "minecraft:arrow",
            None,
        ),
    };
    let mut arrow = Arrow::new(item);
    arrow.spectral = name == "minecraft:spectral_arrow";
    if name == "minecraft:tipped_arrow" {
        arrow.effects = super::potions::contents(comps);
    }
    arrow.crit = power >= 1.0;
    let level = held::enchant(bow.comps.as_ref(), "power");
    if level > 0 {
        arrow.damage += f64::from(level) * 0.5 + 0.5;
    }
    arrow.punch = held::enchant(bow.comps.as_ref(), "punch");
    arrow.flame = held::enchant(bow.comps.as_ref(), "flame") > 0;
    if creative || infinity {
        // Infinity arrows can only be picked up by creative players.
        arrow.pickup = Pickup::Creative;
    }
    let at = muzzle(&actor);
    let dir = look(actor.pose.yaw, actor.pose.pitch);
    super::shoot(
        ctx,
        Shot::Arrow(arrow),
        Some(actor.uuid),
        at,
        dir,
        power * 3.0,
        1.0,
    );
    fx::sound(
        ctx,
        "entity.arrow.shoot",
        fx::category::PLAYERS,
        at,
        1.0,
        1.0 / (hits::rnd() as f32 * 0.4 + 1.2) + power as f32 * 0.5,
    );
    if !creative {
        held::wear(ctx, actor.uuid, bow.slot, 1);
        if let (Some(a), false) = (&ammo, infinity) {
            held::consume(ctx, actor.uuid, a.slot, 1);
        }
    }
}

/// How hard a bow shoots after `ticks` of drawing: a full draw takes a
/// second, and the power rises faster at first.
pub fn power(ticks: i32) -> f64 {
    let f = (f64::from(ticks) / 20.0).min(1.0);
    (f * f + f * 2.0) / 3.0
}

fn add(a: [f64; 3], b: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

/// Whether a block is still holding a stuck arrow: the one it is in or one
/// beside it.
pub(super) fn held_by_block(ctx: &crate::context::ServerContext, pos: [f64; 3]) -> bool {
    let c = pos.map(|v| v.floor() as i32);
    [
        [0, 0, 0],
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ]
    .iter()
    .any(|d| hits::stops(ctx, [c[0] + d[0], c[1] + d[1], c[2] + d[2]]))
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let owner = p.owner;
    let Shot::Arrow(a) = &mut p.shot else {
        return;
    };
    let ctx = t.ctx;
    e.age += 1;
    if a.stuck > 0 {
        if !held_by_block(ctx, e.pos) {
            a.stuck = 0;
            e.vel = [0.0, -0.02, 0.0];
            return;
        }
        a.stuck += 1;
        if a.stuck >= LIFE {
            e.removed = true;
        } else if a.stuck > 7 && a.pickup != Pickup::No {
            pick_up(t, e);
        }
        return;
    }
    // Falls out of the world, or hangs over unloaded chunks.
    if e.pos[1] < -80.0 || e.age > 4 * LIFE as u32 {
        e.removed = true;
        return;
    }
    let from = e.pos;
    let to = add(from, e.vel, 1.0);
    let ignore = if e.age < 5 { owner } else { None };
    let skip = a.hit.clone();
    match hits::trace(t, from, to, 0.25, ignore, &skip) {
        None => e.pos = to,
        Some((f, strike)) => {
            e.pos = add(from, e.vel, f);
            match strike {
                Strike::Block => {
                    fx::sound(
                        ctx,
                        "entity.arrow.hit",
                        fx::category::NEUTRAL,
                        e.pos,
                        1.0,
                        1.2 / (hits::rnd() as f32 * 0.2 + 0.9),
                    );
                    a.stuck = 1;
                    a.crit = false;
                    e.vel = [0.0; 3];
                    return;
                }
                Strike::Player(i) => {
                    let victim = t.players[i];
                    let speed = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt();
                    let amount = damage(a, speed);
                    let hurt = hurt_player(
                        t,
                        &victim,
                        amount,
                        "minecraft:arrow",
                        "death.attack.arrow",
                        owner,
                        e.id,
                    );
                    if hurt && a.flame {
                        crate::worn::ignite(ctx, victim.uuid, 100);
                    }
                    if hurt {
                        let strength = if a.punch > 0 {
                            0.4 + f64::from(a.punch) * 0.6
                        } else {
                            0.4
                        };
                        knock(ctx, &victim, add(victim.pose_vec(), e.vel, -1.0), strength);
                        for (name, amp, dur) in a.effects.clone() {
                            if let Some(kind) = crate::effects::kind_of(&name) {
                                let ticks = if kind.instant { 1 } else { (dur / 8).max(1) };
                                crate::effects::give(
                                    ctx,
                                    victim.uuid,
                                    kind,
                                    crate::effects::Instance::new(amp, ticks, true),
                                );
                            }
                        }
                        if a.spectral
                            && let Some(kind) = crate::effects::kind_of("glowing")
                        {
                            crate::effects::give(
                                ctx,
                                victim.uuid,
                                kind,
                                crate::effects::Instance::new(0, 200, true),
                            );
                        }
                    }
                    if a.pierce > 0 {
                        a.pierce -= 1;
                        a.hit.push(victim.entity_id);
                    } else {
                        e.removed = true;
                    }
                }
                Strike::Target(i) => {
                    let target = t.targets[i];
                    let speed = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt();
                    t.out.hits.push(Hit {
                        target: target.id,
                        amount: damage(a, speed),
                        by: owner,
                        with: Some(e.id),
                        kind: "minecraft:arrow",
                    });
                    if a.pierce > 0 {
                        a.pierce -= 1;
                        a.hit.push(target.id);
                    } else {
                        e.removed = true;
                    }
                }
            }
            if e.removed {
                return;
            }
        }
    }
    let drag = if hits::in_water(ctx, e.pos) {
        0.6
    } else {
        0.99
    };
    e.vel = e.vel.map(|c| c * drag);
    e.vel[1] -= GRAVITY;
}

/// Damage of a hit at `speed`: the speed times the base damage, rounded up,
/// and a critical arrow adds up to half as much again plus 2.
fn damage(a: &Arrow, speed: f64) -> f32 {
    let base = (speed * a.damage).ceil().clamp(0.0, 2.147e9);
    let extra = if a.crit {
        (hits::rnd() * (base / 2.0 + 2.0)).floor()
    } else {
        0.0
    };
    (base + extra) as f32
}

/// A player in reach picks the arrow up.
fn pick_up(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::Arrow(a) = &p.shot else {
        return;
    };
    let b = e.aabb();
    for who in t.players {
        if who.mode == 3 {
            continue;
        }
        let w = who.aabb();
        let near = (0..3).all(|i| {
            let m = if i == 1 { 0.5 } else { 1.0 };
            w.min[i] - m < b.max[i] && w.max[i] + m > b.min[i]
        });
        if !near {
            continue;
        }
        let creative = who.mode == 1;
        if a.pickup == Pickup::Creative && !creative {
            continue;
        }
        if a.pickup == Pickup::Yes || creative {
            let left = held::give(t.ctx, who.uuid, &ItemStack::new(a.item, 1));
            if left > 0 && a.pickup == Pickup::Yes && !creative {
                continue;
            }
        }
        let _ = t.ctx.events.send(Event::ItemTaken {
            item_entity: e.id,
            collector: who.entity_id,
            count: 1,
            slots: Vec::new(),
        });
        e.removed = true;
        return;
    }
}
