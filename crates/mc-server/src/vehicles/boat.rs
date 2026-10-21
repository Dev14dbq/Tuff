//! Boats and rafts (minecraft.wiki, "Boat"). A boat someone sits in is
//! driven by their client, which sends where it is; one left alone floats
//! on water, drifts, and falls and slides on land.

use mc_entity::extra::{Craft, Vehicle};
use mc_entity::{Collide, Entity, Kind, move_entity};

use crate::context::ServerContext;
use crate::fx;
use crate::hits::{self, Tick};

struct Ground<'a>(&'a ServerContext);

impl Collide for Ground<'_> {
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        hits::stops(self.0, [x, y, z])
    }
}

/// How many can sit in a boat.
pub fn seats(chest: bool) -> usize {
    if chest { 1 } else { 2 }
}

/// The y of the water surface over `pos` (the top of the water block).
fn surface(ctx: &ServerContext, pos: [f64; 3]) -> f64 {
    let mut y = (pos[1] + 0.1).floor();
    while hits::in_water(ctx, [pos[0], y + 1.5, pos[2]]) && y < pos[1] + 8.0 {
        y += 1.0;
    }
    y + 0.9
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Vehicle(v) = &e.kind else {
        return;
    };
    let mut v = v.clone();
    step(t, e, &mut v);
    e.kind = Kind::Vehicle(v);
}

fn step(t: &mut Tick, e: &mut Entity, v: &mut Vehicle) {
    let ctx = t.ctx;
    let Craft::Boat { .. } = v.craft else {
        return;
    };
    e.age += 1;
    if v.wobble > 0 {
        v.wobble -= 1;
    }
    if v.damage > 0.0 {
        v.damage = (v.damage - 1.0).max(0.0);
    }
    // With a driver the client says where it goes.
    if !v.riders.is_empty() {
        return;
    }
    let before = (e.pos, e.vel);
    if hits::in_water(ctx, [e.pos[0], e.pos[1] + 0.1, e.pos[2]]) {
        // Floats, a little under the surface.
        let target = surface(ctx, e.pos) - 0.2;
        e.vel[1] = ((target - e.pos[1]) * 0.25).clamp(-0.08, 0.08);
        e.vel[0] *= 0.9;
        e.vel[2] *= 0.9;
    } else {
        e.vel[1] -= 0.04;
    }
    let (moved, on_ground) = move_entity(e.aabb(), e.vel, &Ground(ctx));
    for ((p, vel), m) in e.pos.iter_mut().zip(&mut e.vel).zip(moved) {
        *p += m;
        if (m - *vel).abs() > 1e-9 {
            *vel = 0.0;
        }
    }
    e.on_ground = on_ground;
    if on_ground {
        e.vel[0] *= 0.5;
        e.vel[2] *= 0.5;
    }
    for c in &mut e.vel {
        if c.abs() < 0.003 {
            *c = 0.0;
        }
    }
    if (e.pos, e.vel) != before {
        t.out.sync.push(fx::Sync {
            id: e.id,
            pos: e.pos,
            vel: e.vel,
            yaw: v.yaw,
            pitch: 0.0,
            on_ground,
        });
    }
}
