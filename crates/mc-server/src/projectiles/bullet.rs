//! A shulker's bullet (minecraft.wiki, "Shulker"): it homes on its target,
//! moving along one axis at a time, hurts by 4 and levitates the target for
//! ten seconds. A blow shoots it down.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use crate::hits::{self, Hit, Strike, Tick};

/// Blocks per tick.
const SPEED: f64 = 0.15;
/// Ticks before it burns out.
const LIFE: u32 = 200;

fn goal(t: &Tick, id: i32) -> Option<[f64; 3]> {
    if let Some(p) = t.players.iter().find(|p| p.entity_id == id) {
        return Some([p.pose.x, p.pose.y + 0.9, p.pose.z]);
    }
    t.targets.iter().find(|x| x.id == id).map(|x| {
        [
            (x.aabb.min[0] + x.aabb.max[0]) / 2.0,
            (x.aabb.min[1] + x.aabb.max[1]) / 2.0,
            (x.aabb.min[2] + x.aabb.max[2]) / 2.0,
        ]
    })
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::ShulkerBullet { target } = p.shot else {
        return;
    };
    let owner = p.owner;
    e.age += 1;
    if e.age > LIFE {
        e.removed = true;
        return;
    }
    if let Some(at) = goal(t, target) {
        let d = [at[0] - e.pos[0], at[1] - e.pos[1], at[2] - e.pos[2]];
        let axis = (0..3)
            .max_by(|&a, &b| d[a].abs().total_cmp(&d[b].abs()))
            .unwrap_or(0);
        let mut v = [0.0; 3];
        v[axis] = SPEED * d[axis].signum();
        e.vel = v;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let skip: Vec<i32> = t.shooter.map(|(id, _)| id).into_iter().collect();
    let Some((f, strike)) = hits::trace(t, from, to, 0.15625, owner, &skip) else {
        e.pos = to;
        return;
    };
    e.pos = [
        from[0] + e.vel[0] * f,
        from[1] + e.vel[1] * f,
        from[2] + e.vel[2] * f,
    ];
    e.removed = true;
    let levitation = || vec![("minecraft:levitation".to_owned(), 0, 200)];
    match strike {
        Strike::Player(i) => {
            let victim = t.players[i];
            if super::hurt_player(
                t,
                &victim,
                4.0,
                "minecraft:mob_projectile",
                "death.attack.mob",
                owner,
                e.id,
            ) && let Some(kind) = crate::effects::kind_of("levitation")
            {
                crate::effects::give(
                    t.ctx,
                    victim.uuid,
                    kind,
                    crate::effects::Instance::new(0, 200, true),
                );
            }
        }
        Strike::Target(i) => {
            let id = t.targets[i].id;
            t.out.hits.push(Hit {
                target: id,
                amount: 4.0,
                by: owner,
                with: Some(e.id),
                kind: "minecraft:mob_projectile",
            });
            t.out.doses.push(hits::Dose {
                target: id,
                effects: levitation(),
                strength: 1.0,
                splash: false,
                by: owner,
            });
        }
        Strike::Block { .. } => {}
    }
}
