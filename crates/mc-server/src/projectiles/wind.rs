//! Wind charges and wind bursts (minecraft.wiki, "Wind Charge", "Wind
//! Burst"): a thrown wind charge flies straight at 1.5 without slowing,
//! hurts what it hits by 1 and bursts; the burst (radius 1.2, a breeze's
//! 3) hurts nothing but pushes everything near away from it, by the share
//! of it the blast sees, 1.22 times as hard for a player's charge; it
//! flips levers, buttons, doors, trapdoors and fence gates it reaches.
//! A player it launches takes fall damage only below where it hit them.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use crate::context::{Event, ServerContext};
use crate::hits::{self, Hit, Strike, Tick};
use crate::interact::Actor;
use crate::tick::lock;

/// A burst of wind to set off once the entities are back together.
#[derive(Clone, Debug)]
pub struct Gust {
    pub center: [f64; 3],
    pub radius: f32,
    /// How much harder than an explosion it pushes.
    pub knockback: f32,
    /// `minecraft:sound_event` name.
    pub sound: &'static str,
    /// Whether it flips blocks.
    pub triggers: bool,
}

impl Gust {
    /// A thrown wind charge's (a player's) or a breeze's.
    pub fn charge(center: [f64; 3], breeze: bool) -> Self {
        if breeze {
            Gust {
                center,
                radius: 3.0,
                knockback: 1.0,
                sound: "entity.breeze.wind_burst",
                triggers: true,
            }
        } else {
            Gust {
                center,
                radius: 1.2,
                knockback: 1.22,
                sound: "entity.wind_charge.wind_burst",
                triggers: true,
            }
        }
    }
}

/// A player throws a wind charge (0.5 s before the next one).
pub fn throw(ctx: &ServerContext, actor: &Actor, slot: usize) -> bool {
    let now = lock(&ctx.time).age;
    {
        let mut st = lock(&ctx.shooting);
        let until = st
            .cooldowns
            .entry((actor.uuid, "minecraft:wind_charge"))
            .or_default();
        if *until > now {
            return false;
        }
        *until = now + 10;
    }
    let at = [actor.pose.x, actor.pose.y + 1.62, actor.pose.z];
    let dir = super::look(actor.pose.yaw, actor.pose.pitch);
    super::shoot(
        ctx,
        Shot::WindCharge { breeze: false },
        Some(actor.uuid),
        at,
        dir,
        1.5,
        1.0,
    );
    crate::fx::sound(
        ctx,
        "entity.wind_charge.throw",
        crate::fx::category::NEUTRAL,
        at,
        0.5,
        0.4 / (hits::rnd() as f32 * 0.4 + 0.8),
    );
    if actor.game_mode != 1 {
        crate::held::consume(ctx, actor.uuid, slot, 1);
    }
    true
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::WindCharge { breeze } = p.shot else {
        return;
    };
    let owner = p.owner;
    e.age += 1;
    if e.pos[1] > f64::from(t.ctx.world.min_y + t.ctx.world.height as i32 + 30) || e.age > 20 * 60 {
        t.out.gusts.push(Gust::charge(e.pos, breeze));
        e.removed = true;
        return;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let ignore = if e.age < 5 { owner } else { None };
    let skip: Vec<i32> = t.shooter.map(|s| vec![s.0]).unwrap_or_default();
    match hits::trace(t, from, to, 0.15625, ignore, &skip) {
        None => {
            e.pos = to;
            if hits::in_water(t.ctx, e.pos) {
                e.vel = e.vel.map(|c| c * 0.8);
            }
        }
        Some((f, strike)) => {
            e.pos = [
                from[0] + e.vel[0] * f,
                from[1] + e.vel[1] * f,
                from[2] + e.vel[2] * f,
            ];
            match strike {
                Strike::Player(i) => {
                    let victim = t.players[i];
                    super::hurt_player(
                        t,
                        &victim,
                        1.0,
                        "minecraft:wind_charge",
                        "death.attack.mob",
                        owner,
                        e.id,
                    );
                }
                Strike::Target(i) => t.out.hits.push(Hit {
                    target: t.targets[i].id,
                    amount: 1.0,
                    by: owner,
                    with: Some(e.id),
                    kind: "minecraft:wind_charge",
                }),
                Strike::Block { .. } => {
                    hits::hit_block(t.ctx, &strike, e.pos, false);
                    crate::pots::projectile(t.ctx, from, to);
                }
            }
            // Burst a little short of what it hit.
            let len = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt().max(1e-9);
            let back = e.vel.map(|c| c / len * 0.25);
            let at = [e.pos[0] - back[0], e.pos[1] - back[1], e.pos[2] - back[2]];
            t.out.gusts.push(Gust::charge(at, breeze));
            e.removed = true;
        }
    }
}

/// The share of rays from points of the box to `center` that no block
/// stops (vanilla's "seen percent").
fn seen(ctx: &ServerContext, center: [f64; 3], b: &mc_entity::Aabb) -> f64 {
    let size = [
        b.max[0] - b.min[0],
        b.max[1] - b.min[1],
        b.max[2] - b.min[2],
    ];
    let step = size.map(|s| 1.0 / (s * 2.0 + 1.0));
    if step.iter().any(|s| *s <= 0.0) {
        return 0.0;
    }
    let ox = (1.0 - (1.0 / step[0]).floor() * step[0]) / 2.0;
    let oz = (1.0 - (1.0 / step[2]).floor() * step[2]) / 2.0;
    let (mut clear, mut all) = (0, 0);
    let mut x = 0.0;
    while x <= 1.0 {
        let mut y = 0.0;
        while y <= 1.0 {
            let mut z = 0.0;
            while z <= 1.0 {
                let p = [
                    b.min[0] + size[0] * x + ox,
                    b.min[1] + size[1] * y,
                    b.min[2] + size[2] * z + oz,
                ];
                if hits::first_solid(ctx, p, center).is_none() {
                    clear += 1;
                }
                all += 1;
                z += step[2];
            }
            y += step[1];
        }
        x += step[0];
    }
    if all == 0 {
        0.0
    } else {
        f64::from(clear) / f64::from(all)
    }
}

/// Sets off a gust: pushes the entities in `entities` and the players,
/// flips blocks, and shows clients the burst.
pub fn burst(ctx: &ServerContext, entities: &mut [Entity], g: &Gust) {
    let c = g.center;
    let reach = f64::from(g.radius) * 2.0;
    let mult = f64::from(g.knockback);
    let push = |b: &mc_entity::Aabb, pos: [f64; 3], eye: f64| -> Option<[f64; 3]> {
        let d = ((pos[0] - c[0]).powi(2) + (pos[1] - c[1]).powi(2) + (pos[2] - c[2]).powi(2))
            .sqrt()
            / reach;
        if d > 1.0 {
            return None;
        }
        let dir = [pos[0] - c[0], eye - c[1], pos[2] - c[2]];
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        if len == 0.0 {
            return None;
        }
        let k = (1.0 - d) * seen(ctx, c, b) * mult;
        Some(dir.map(|v| v / len * k))
    };
    for e in entities.iter_mut() {
        if e.removed
            || !matches!(
                e.kind,
                Kind::Mob(_) | Kind::Item { .. } | Kind::Tnt { .. } | Kind::Vehicle(_)
            )
        {
            continue;
        }
        let b = e.aabb();
        let eye = match e.kind {
            Kind::Tnt { .. } => e.pos[1],
            _ => e.pos[1] + e.size().1 * 0.85,
        };
        if let Some(v) = push(&b, e.pos, eye) {
            for (a, d) in e.vel.iter_mut().zip(v) {
                *a += d;
            }
            if matches!(e.kind, Kind::Item { .. }) {
                let _ = ctx
                    .events
                    .send(Event::EntitySync(vec![crate::dropped::row(e)]));
            }
        }
    }
    let mut knockback = Vec::new();
    let players: Vec<(u128, i32, crate::context::Pose, bool)> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| Some((p.profile.uuid, p.game_mode, p.pose?, p.flying)))
        .collect();
    for (uuid, mode, pose, flying) in players {
        if mode == 3 || mode == 1 && flying {
            continue;
        }
        let b = mc_entity::Aabb {
            min: [pose.x - 0.3, pose.y, pose.z - 0.3],
            max: [pose.x + 0.3, pose.y + 1.8, pose.z + 0.3],
        };
        if let Some(v) = push(&b, [pose.x, pose.y, pose.z], pose.y + 1.62) {
            let k = 1.0 - crate::commands::gear::value(ctx, uuid, "explosion_knockback_resistance");
            knockback.push((uuid, v.map(|x| x * k)));
            if let Some(vi) = lock(&ctx.vitals).get_mut(&uuid) {
                vi.impulse_y = Some(pose.y);
                vi.fall_distance = 0.0;
            }
        }
    }
    if g.triggers {
        trigger(ctx, g);
    }
    let particle = if g.radius < 2.0 {
        "minecraft:gust_emitter_small"
    } else {
        "minecraft:gust_emitter_large"
    };
    let _ = ctx.events.send(Event::Explosion {
        center: c,
        radius: g.radius,
        block_count: 0,
        particle: ctx
            .data
            .builtin
            .id("minecraft:particle_type", particle)
            .unwrap_or(ctx.explosion_particle),
        sound: ctx
            .data
            .builtin
            .id("minecraft:sound_event", &format!("minecraft:{}", g.sound))
            .unwrap_or(ctx.explosion_sound),
        knockback,
    });
}

/// Blocks the burst flips: levers, buttons, doors (by their lower half),
/// trapdoors and fence gates within its radius, not iron ones and none
/// that `blocks_wind_charge_explosions` shields.
fn trigger(ctx: &ServerContext, g: &Gust) {
    let r = f64::from(g.radius);
    let immune = ctx
        .commands
        .env
        .tag("minecraft:block", "minecraft:blocks_wind_charge_explosions");
    let blocks = &ctx.data.blocks;
    let mut level = crate::level::WorldLevel::new(ctx);
    let lo = g.center.map(|v| (v - r).floor() as i32);
    let hi = g.center.map(|v| (v + r).floor() as i32);
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let d = (f64::from(x) + 0.5 - g.center[0]).powi(2)
                    + (f64::from(y) + 0.5 - g.center[1]).powi(2)
                    + (f64::from(z) + 0.5 - g.center[2]).powi(2);
                if d > r * r {
                    continue;
                }
                let p = mc_protocol::Position::new(x, y, z);
                let Some(state) = ctx.world.block(p, blocks) else {
                    continue;
                };
                let name = blocks.block_name(state).unwrap_or("");
                if immune.iter().any(|m| m == name) || name.starts_with("minecraft:iron_") {
                    continue;
                }
                let flips = name == "minecraft:lever"
                    || name.ends_with("_button")
                    || name.ends_with("_trapdoor")
                    || name.ends_with("_fence_gate")
                    || name.ends_with("_door") && blocks.property(state, "half") == Some("lower");
                if flips {
                    let logic = level.logic();
                    mc_tick::use_block(&mut level, logic, crate::level::to_pos(p));
                }
            }
        }
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{block, flat, join, set, wear};

    #[test]
    fn a_wind_charge_flies_straight_and_bursts() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, 0x8001, 8101, [0.5, -60.0, 0.5], 0);
        let item = ctx.items.id("minecraft:wind_charge").unwrap();
        wear(
            &ctx,
            0x8001,
            36,
            mc_protocol::packets::play::ItemStack::new(item, 2),
        );
        let p = lock(&ctx.players).get(&0x8001).cloned().unwrap();
        let actor = Actor {
            uuid: 0x8001,
            entity_id: 8101,
            game_mode: 0,
            pose: p.pose.unwrap(),
            sneaking: false,
        };
        assert!(throw(&ctx, &actor, 36));
        // Half a second before the next.
        assert!(!throw(&ctx, &actor, 36));
        assert_eq!(
            lock(&ctx.inventories).get(&0x8001).unwrap().slots[36]
                .as_ref()
                .unwrap()
                .count,
            1
        );
        let mut e = lock(&ctx.entities).pop().unwrap();
        e.vel = [0.0, 0.0, 1.5];
        e.pos = [0.5, -59.0, 0.5];
        let mut out = hits::Out::default();
        for _ in 0..3 {
            let mut t = Tick {
                ctx: &ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
        }
        // No gravity, no drag.
        assert_eq!(e.vel, [0.0, 0.0, 1.5]);
        assert!((e.pos[1] + 59.0).abs() < 1e-9);
        set(&ctx, [0, -59, 6], "minecraft:stone");
        for _ in 0..5 {
            let mut t = Tick {
                ctx: &ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
            if e.removed {
                break;
            }
        }
        assert!(e.removed);
        assert_eq!(out.gusts.len(), 1);
        assert!((out.gusts[0].radius - 1.2).abs() < 1e-6);
        assert!(out.gusts[0].center[2] < 6.0);
    }

    #[test]
    fn a_gust_pushes_and_flips_levers() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, 0x8002, 8102, [40.5, -60.0, 40.5], 1);
        let lever = ctx
            .data
            .blocks
            .default_state("minecraft:lever")
            .map(|s| ctx.shapes.with(s, "face", "floor"))
            .unwrap();
        let mut level = crate::level::WorldLevel::new(&ctx);
        level.set_block(mc_protocol::Position::new(0, -60, 0), lever);
        let _ = level.into_changes();
        assert_eq!(block(&ctx, [0, -60, 0]), "minecraft:lever");
        let stone = ctx.items.id("minecraft:stone").unwrap();
        let mut items = vec![Entity {
            id: 1,
            uuid: 1,
            kind: Kind::stack(&mc_protocol::packets::play::ItemStack::new(stone, 1), 0),
            pos: [1.5, -60.0, 0.5],
            vel: [0.0; 3],
            on_ground: true,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }];
        burst(&ctx, &mut items, &Gust::charge([0.5, -59.8, 0.5], false));
        assert!(items[0].vel[0] > 0.1, "{:?}", items[0].vel);
        let state = ctx
            .world
            .block(mc_protocol::Position::new(0, -60, 0), &ctx.data.blocks)
            .unwrap();
        assert_eq!(ctx.data.blocks.property(state, "powered"), Some("true"));
    }
}
