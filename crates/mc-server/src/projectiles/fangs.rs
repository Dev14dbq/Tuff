//! Evoker fangs (minecraft.wiki, "Evoker Fangs"): after their delay they
//! snap shut once, biting what stands in them for 6 (magic, the evoker's),
//! and are gone a second later. Illagers and the evoker's other friends are
//! spared.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind, Species};

use crate::hits::{Hit, Tick};

/// Their box: half a block wide, 0.8 high.
const HALF: f64 = 0.25;
const HEIGHT: f64 = 0.8;

/// New fangs at `pos`, snapping after `delay` ticks.
pub fn new(
    ctx: &crate::context::ServerContext,
    pos: [f64; 3],
    yaw: f32,
    delay: i32,
    evoker: (i32, &'static str),
) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(mc_entity::extra::Projectile {
            shot: Shot::Fangs {
                warmup: delay,
                life: 22,
                snapped: false,
            },
            owner: None,
            mob: Some(evoker),
            yaw,
            pitch: 0.0,
        }),
        pos,
        vel: [0.0; 3],
        on_ground: true,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Raiders do not bite each other.
fn spared(kind: Option<&str>) -> bool {
    kind.and_then(Species::from_id).is_some_and(|s| s.raider() || s == Species::Vex)
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let owner = p.mob.map(|(id, _)| id);
    let Shot::Fangs {
        warmup,
        life,
        snapped,
    } = &mut p.shot
    else {
        return;
    };
    e.age += 1;
    *warmup -= 1;
    if *warmup >= 0 {
        return;
    }
    if !*snapped {
        *snapped = true;
        // Clients play the snap and its sound.
        crate::fx::status(t.ctx, e.id, 4);
    }
    if *warmup == -8 {
        let area = mc_entity::Aabb {
            min: [e.pos[0] - HALF, e.pos[1], e.pos[2] - HALF],
            max: [e.pos[0] + HALF, e.pos[1] + HEIGHT, e.pos[2] + HALF],
        };
        let inside =
            |b: &mc_entity::Aabb| (0..3).all(|i| b.min[i] < area.max[i] && b.max[i] > area.min[i]);
        let amount = crate::damage::scale_for_difficulty(6.0, t.ctx.difficulty());
        for w in t.players {
            if w.solid() && inside(&w.aabb()) {
                crate::vitals::try_hurt_by_entity(
                    t.ctx,
                    w.uuid,
                    amount,
                    "minecraft:indirect_magic",
                    "death.attack.indirectMagic",
                    t.shooter.map(|(_, name)| name),
                    Some(e.id),
                );
            }
        }
        for g in t.targets {
            if Some(g.id) == owner || spared(g.kind) || !inside(&g.aabb) {
                continue;
            }
            t.out.hits.push(Hit {
                target: g.id,
                amount: 6.0,
                by: None,
                with: Some(e.id),
                kind: "minecraft:indirect_magic",
            });
        }
    }
    *life -= 1;
    if *life < 0 {
        e.removed = true;
    }
}
