//! Lightning bolts (minecraft.wiki, "Lightning"), here those Channeling
//! calls: thunder and the impact sound, fire where it strikes (on normal
//! and hard), 5 damage and fire to everything within 3 blocks while it
//! flashes, one to three flashes.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use crate::hits::{self, Hit, Tick};

/// The weather sound category.
const WEATHER: i32 = 3;

/// A new bolt at `pos`.
pub fn bolt(ctx: &crate::context::ServerContext, pos: [f64; 3], visual: bool) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(mc_entity::extra::Projectile {
            shot: Shot::Lightning {
                life: 2,
                flashes: 1 + (crate::util::random_u64() % 3) as i32,
                struck: Vec::new(),
                visual,
            },
            owner: None,
            mob: None,
            yaw: 0.0,
            pitch: 0.0,
        }),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Calls a bolt down at `pos` from an entity's tick.
pub fn strike(t: &mut Tick, pos: [f64; 3]) {
    let b = bolt(t.ctx, pos, false);
    t.out.spawn.push(b);
}

/// Fire where the bolt strikes and `extra` more around it (normal and
/// hard difficulty only).
fn fire(t: &mut Tick, pos: [f64; 3], extra: usize) {
    if t.ctx.difficulty() < 2 {
        return;
    }
    let c = pos.map(|v| v.floor() as i32);
    t.out.fires.push(c);
    for _ in 0..extra {
        let r = || (crate::util::random_u64() % 3) as i32 - 1;
        t.out.fires.push([c[0] + r(), c[1] + r(), c[2] + r()]);
    }
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Shot::Lightning {
        life,
        flashes,
        visual,
        ..
    } = &mut p.shot
    else {
        return;
    };
    let ctx = t.ctx;
    if e.age == 0 {
        crate::fx::sound(
            ctx,
            "entity.lightning_bolt.thunder",
            WEATHER,
            e.pos,
            10000.0,
            0.8 + hits::rnd() as f32 * 0.2,
        );
        crate::fx::sound(
            ctx,
            "entity.lightning_bolt.impact",
            WEATHER,
            e.pos,
            2.0,
            0.5 + hits::rnd() as f32 * 0.2,
        );
    }
    let visual = *visual;
    if e.age == 0 && !visual {
        fire(t, e.pos, 4);
    }
    e.age += 1;
    *life -= 1;
    if *life < 0 {
        if *flashes == 0 {
            e.removed = true;
            return;
        }
        if *life < -((crate::util::random_u64() % 10) as i32) {
            *flashes -= 1;
            *life = 1;
            if !visual {
                fire(t, e.pos, 0);
            }
        }
    }
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Shot::Lightning { life, struck, .. } = &mut p.shot else {
        return;
    };
    if *life < 0 || visual {
        return;
    }
    let area = mc_entity::Aabb {
        min: [e.pos[0] - 3.0, e.pos[1] - 3.0, e.pos[2] - 3.0],
        max: [e.pos[0] + 3.0, e.pos[1] + 9.0, e.pos[2] + 3.0],
    };
    let inside = |b: &mc_entity::Aabb| (0..3).all(|i| b.min[i] < area.max[i] && b.max[i] > area.min[i]);
    for w in t.players {
        if !w.solid() || struck.contains(&w.entity_id) || !inside(&w.aabb()) {
            continue;
        }
        struck.push(w.entity_id);
        crate::vitals::try_hurt_by_entity(
            ctx,
            w.uuid,
            5.0,
            "minecraft:lightning_bolt",
            "death.attack.lightningBolt",
            None,
            Some(e.id),
        );
        crate::worn::in_fire(ctx, w.uuid);
    }
    for g in t.targets {
        if struck.contains(&g.id) || !inside(&g.aabb) {
            continue;
        }
        struck.push(g.id);
        t.out.hits.push(Hit {
            target: g.id,
            amount: 5.0,
            by: None,
            with: Some(e.id),
            kind: "minecraft:lightning_bolt",
        });
    }
}
