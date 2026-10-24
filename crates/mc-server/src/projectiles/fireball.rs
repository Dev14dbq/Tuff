//! Fireballs (minecraft.wiki, "Fireball", "Small Fireball"): they fly
//! straight without falling. A ghast's explodes where it lands (power 1,
//! with fire); a blaze's hurts and sets on fire what it hits, or lights the
//! block it lands on.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use crate::hits::{self, Strike, Tick};

/// Ticks a fireball flies before it burns out.
const LIFE: u32 = 300;

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::Fireball { small } = p.shot else {
        return;
    };
    let owner = p.owner;
    e.age += 1;
    if e.age > LIFE || e.pos[1] < -80.0 {
        e.removed = true;
        return;
    }
    let from = e.pos;
    let to = [from[0] + e.vel[0], from[1] + e.vel[1], from[2] + e.vel[2]];
    let skip: Vec<i32> = t.shooter.map(|(id, _)| id).into_iter().collect();
    let half = if small { 0.15625 } else { 0.5 };
    let Some((f, strike)) = hits::trace(t, from, to, half, owner, &skip) else {
        e.pos = to;
        // Fireballs keep their speed through the air and slow in water.
        if hits::in_water(t.ctx, e.pos) {
            e.vel = e.vel.map(|c| c * 0.8);
        }
        return;
    };
    e.pos = [
        from[0] + e.vel[0] * f,
        from[1] + e.vel[1] * f,
        from[2] + e.vel[2] * f,
    ];
    e.removed = true;
    let (damage, kind, key) = if small {
        (5.0, "minecraft:fireball", "death.attack.fireball")
    } else {
        (6.0, "minecraft:fireball", "death.attack.fireball")
    };
    match strike {
        Strike::Player(i) => {
            let victim = t.players[i];
            let hurt = super::hurt_player(t, &victim, damage, kind, key, owner, e.id);
            if small && hurt {
                crate::worn::ignite(t.ctx, victim.uuid, 100);
            }
        }
        Strike::Target(i) => {
            t.out.hits.push(hits::Hit {
                target: t.targets[i].id,
                amount: damage,
                by: owner,
                with: Some(e.id),
                kind,
            });
        }
        Strike::Block => {}
    }
    if small {
        // Light the spot it hit.
        let back = [
            e.pos[0] - e.vel[0] * 0.1,
            e.pos[1] - e.vel[1] * 0.1,
            e.pos[2] - e.vel[2] * 0.1,
        ];
        t.out.fires.push([
            back[0].floor() as i32,
            back[1].floor() as i32,
            back[2].floor() as i32,
        ]);
    } else {
        t.out.blasts.push(crate::explosion::Blast {
            center: e.pos,
            power: 1.0,
            source: e.id,
            owner,
            // A ghast's: the mob rules apply.
            mob: true,
            dangerous: false,
            by: None,
        });
    }
}
