//! Fishing rods: the bobber is cast, floats, a fish bites after a while,
//! and reeling in at the right moment catches what the fishing loot table
//! gives. Simplified: no open-water check, and experience goes straight to
//! the player.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::{MetadataValue, SetEntityData};

use super::{look, spawn};
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held;
use crate::hits::{self, Strike, Tick};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Right click with a rod: cast, or reel in.
pub fn use_rod(ctx: &ServerContext, actor: Actor, hand: i32, yaw: f32, pitch: f32) -> Outcome {
    let mut outcome = Outcome::default();
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    let cast = lock(&ctx.shooting).bobbers.remove(&actor.uuid);
    if let Some(id) = cast {
        reel(ctx, actor, hand, id);
        return outcome;
    }
    let at = super::muzzle(&actor);
    let id = super::shoot(
        ctx,
        Shot::Bobber {
            hooked: None,
            wait: -1,
            bite: 0,
        },
        Some(actor.uuid),
        at,
        look(yaw, pitch),
        0.9,
        1.0,
    );
    lock(&ctx.shooting).bobbers.insert(actor.uuid, id);
    fx::sound(
        ctx,
        "entity.fishing_bobber.throw",
        fx::category::NEUTRAL,
        at,
        0.5,
        0.4 / (hits::rnd() as f32 * 0.4 + 0.8),
    );
    let _ = spawn;
    outcome
}

/// Pulls the bobber in: with a bite the catch comes flying.
fn reel(ctx: &ServerContext, actor: Actor, hand: i32, id: i32) {
    let found = {
        let mut entities = lock(&ctx.entities);
        entities.iter_mut().find(|e| e.id == id).map(|e| {
            e.removed = true;
            let (bite, hooked) = match &e.kind {
                Kind::Projectile(p) => match &p.shot {
                    Shot::Bobber { bite, hooked, .. } => (*bite, *hooked),
                    _ => (0, None),
                },
                _ => (0, None),
            };
            (e.pos, bite, hooked)
        })
    };
    let Some((pos, bite, hooked)) = found else {
        return;
    };
    let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
    let rod = held::held(ctx, actor.uuid, hand).filter(|h| h.name == "minecraft:fishing_rod");
    let mut wear = 0;
    if bite > 0 {
        wear = 1;
        let luck = rod
            .as_ref()
            .map_or(0, |r| held::enchant(r.comps.as_ref(), "luck_of_the_sea"));
        catch(ctx, actor, pos, luck);
        fx::sound(
            ctx,
            "entity.fishing_bobber.retrieve",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
    } else if hooked.is_some() {
        wear = 3;
    }
    if let Some(r) = rod
        && wear > 0
        && actor.game_mode != 1
    {
        held::wear(ctx, actor.uuid, r.slot, wear);
    }
}

/// Rolls the fishing table and throws the result at the player.
fn catch(ctx: &ServerContext, actor: Actor, at: [f64; 3], luck: i32) {
    let Some(table) = ctx.loot.table("minecraft:gameplay/fishing").cloned() else {
        return;
    };
    let cx = crate::loot::LootCtx {
        origin: Some(at),
        luck: luck as f32,
        ..crate::loot::LootCtx::default()
    };
    let env = crate::commands::loot::Env(ctx);
    for drop in ctx.loot.roll(&env, &table, &cx) {
        let d = [
            actor.pose.x - at[0],
            actor.pose.y - at[1],
            actor.pose.z - at[2],
        ];
        let flat = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let entity = Entity {
            id: ctx.next_entity_id(),
            uuid: crate::util::random_uuid(),
            kind: Kind::stack(&drop.stack(), 10),
            pos: at,
            vel: [d[0] * 0.1, d[1] * 0.1 + flat.sqrt() * 0.08, d[2] * 0.1],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        };
        let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
        lock(&ctx.entities).push(entity);
    }
    // 1 to 6 points of experience.
    let points = 1 + (hits::rnd() * 6.0) as i32;
    let packet = {
        let mut all = lock(&ctx.vitals);
        all.get_mut(&actor.uuid).map(|v| {
            v.add_xp_points(points);
            v.experience_packet()
        })
    };
    if let Some(packet) = packet {
        let _ = ctx.events.send(Event::Direct {
            uuid: actor.uuid,
            packet: crate::context::Direct::Experience(packet),
        });
    }
}

/// The y of the water surface above `p`.
fn surface(ctx: &ServerContext, p: [f64; 3]) -> f64 {
    let (x, z) = (p[0].floor() as i32, p[2].floor() as i32);
    let mut y = p[1].floor() as i32;
    while hits::in_water(
        ctx,
        [
            f64::from(x) + 0.5,
            f64::from(y + 1) + 0.5,
            f64::from(z) + 0.5,
        ],
    ) && y < p[1] as i32 + 8
    {
        y += 1;
    }
    f64::from(y) + 0.9
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Some(owner) = p.owner else {
        e.removed = true;
        return;
    };
    let Shot::Bobber { hooked, wait, bite } = &mut p.shot else {
        return;
    };
    let ctx = t.ctx;
    e.age += 1;
    let me = t.players.iter().find(|w| w.uuid == owner);
    // The line goes when the thrower does, lets go of the rod, or is far.
    let rod = me.is_some_and(|w| {
        [0, 1]
            .iter()
            .any(|&h| held::held(ctx, w.uuid, h).is_some_and(|h| h.name == "minecraft:fishing_rod"))
    });
    let far = me.is_none_or(|w| {
        (w.pose.x - e.pos[0]).powi(2)
            + (w.pose.y - e.pos[1]).powi(2)
            + (w.pose.z - e.pos[2]).powi(2)
            > 1024.0
    });
    if !rod || far {
        e.removed = true;
        lock(&ctx.shooting).bobbers.remove(&owner);
        return;
    }
    if let Some(id) = *hooked {
        // Carried by what it hooked.
        let at = t
            .players
            .iter()
            .find(|w| w.entity_id == id)
            .map(|w| [w.pose.x, w.pose.y + 0.9, w.pose.z])
            .or_else(|| {
                t.targets.iter().find(|g| g.id == id).map(|g| {
                    [
                        (g.aabb.min[0] + g.aabb.max[0]) / 2.0,
                        (g.aabb.min[1] + g.aabb.max[1]) / 2.0,
                        (g.aabb.min[2] + g.aabb.max[2]) / 2.0,
                    ]
                })
            });
        match at {
            Some(at) => {
                e.pos = at;
                e.vel = [0.0; 3];
                return;
            }
            None => *hooked = None,
        }
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    match hits::trace(t, from, to, 0.125, Some(owner), &[]) {
        None => e.pos = to,
        Some((f, strike)) => {
            e.pos = [
                from[0] + e.vel[0] * f,
                from[1] + e.vel[1] * f,
                from[2] + e.vel[2] * f,
            ];
            match strike {
                Strike::Block => e.vel = [0.0; 3],
                Strike::Player(i) => {
                    let id = t.players[i].entity_id;
                    *hooked = Some(id);
                    t.out.data.push(SetEntityData {
                        entity_id: e.id,
                        entries: vec![(8, MetadataValue::VarInt(id + 1))],
                    });
                    return;
                }
                Strike::Target(i) => {
                    let id = t.targets[i].id;
                    *hooked = Some(id);
                    t.out.data.push(SetEntityData {
                        entity_id: e.id,
                        entries: vec![(8, MetadataValue::VarInt(id + 1))],
                    });
                    return;
                }
            }
        }
    }
    if hits::in_water(ctx, e.pos) {
        // Floats at the surface, drifting little.
        let top = surface(ctx, e.pos);
        e.vel[0] *= 0.5;
        e.vel[2] *= 0.5;
        e.vel[1] = ((top - 0.2 - e.pos[1]) * 0.2).clamp(-0.1, 0.1);
        if *wait < 0 {
            *wait = 100 + (hits::rnd() * 500.0) as i32;
        }
        if *bite > 0 {
            *bite -= 1;
            if *bite == 0 {
                *wait = 100 + (hits::rnd() * 500.0) as i32;
                t.out.data.push(SetEntityData {
                    entity_id: e.id,
                    entries: vec![(9, MetadataValue::Boolean(false))],
                });
            }
        } else {
            *wait -= 1;
            if *wait <= 0 {
                *bite = 20 + (hits::rnd() * 20.0) as i32;
                fx::sound(
                    ctx,
                    "entity.fishing_bobber.splash",
                    fx::category::NEUTRAL,
                    e.pos,
                    0.25,
                    1.0,
                );
                fx::particles(ctx, "minecraft:fishing", e.pos, 0.3, 0.1, 8);
                t.out.data.push(SetEntityData {
                    entity_id: e.id,
                    entries: vec![(9, MetadataValue::Boolean(true))],
                });
                // The bobber dips.
                e.vel[1] -= 0.2;
            }
        }
    } else {
        *wait = -1;
        *bite = 0;
        if e.vel != [0.0; 3] {
            e.vel = e.vel.map(|c| c * 0.92);
            e.vel[1] -= 0.03;
        }
    }
}
