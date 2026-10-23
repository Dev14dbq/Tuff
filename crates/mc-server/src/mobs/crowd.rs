//! Mobs push one another (and players push mobs) apart, and too many in one
//! spot crush each other: past the `max_entity_cramming` game rule (24),
//! each takes 6 damage one tick in four (minecraft.wiki, "Entity cramming").

use std::collections::HashMap;

use mc_entity::mob::Target;
use mc_entity::{Entity, Kind};

use super::hurt;
use crate::context::ServerContext;

/// How hard two overlapping bodies push apart each tick (vanilla's 0.05).
const PUSH: f64 = 0.05;
const PLAYER_WIDTH: f64 = 0.6;
const PLAYER_HEIGHT: f64 = 1.8;

#[derive(Clone, Copy)]
struct Body {
    i: usize,
    pos: [f64; 3],
    half: f64,
    height: f64,
}

fn overlap(a: &Body, b: &Body) -> bool {
    (a.pos[0] - b.pos[0]).abs() < a.half + b.half
        && (a.pos[2] - b.pos[2]).abs() < a.half + b.half
        && a.pos[1] < b.pos[1] + b.height
        && b.pos[1] < a.pos[1] + a.height
}

/// The push one body gets from another (vanilla's `Entity.push`).
fn push_from(me: [f64; 3], other: [f64; 3]) -> Option<[f64; 2]> {
    let (dx, dz) = (me[0] - other[0], me[2] - other[2]);
    let mut d = dx.abs().max(dz.abs());
    if d < 0.01 {
        // Right on top of each other: any way at all.
        let a = (crate::util::random_u64() % 360) as f64 * std::f64::consts::PI / 180.0;
        return Some([a.cos() * PUSH, a.sin() * PUSH]);
    }
    d = d.sqrt();
    let f = (1.0 / d).min(1.0);
    Some([dx / d * f * PUSH, dz / d * f * PUSH])
}

/// Pushes overlapping mobs apart and crushes crowds. Ridden mobs and their
/// riders are left alone.
pub fn tick(ctx: &ServerContext, entities: &mut [Entity], targets: &[Target], now: i64) {
    let mut bodies = Vec::new();
    for (i, e) in entities.iter().enumerate() {
        if let Kind::Mob(m) = &e.kind
            && m.dying.is_none()
            && m.riders.is_empty()
            && !m.species.flies()
        {
            let (w, h) = m.size();
            bodies.push(Body {
                i,
                pos: e.pos,
                half: w / 2.0,
                height: h,
            });
        }
    }
    if bodies.is_empty() {
        return;
    }
    // Two-block buckets, so each body looks at its neighbours only.
    let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (k, b) in bodies.iter().enumerate() {
        let key = (
            (b.pos[0] / 2.0).floor() as i32,
            (b.pos[2] / 2.0).floor() as i32,
        );
        grid.entry(key).or_default().push(k);
    }
    let limit = ctx
        .game_rules
        .lock()
        .expect("game rules lock")
        .int("minecraft:max_entity_cramming");
    let mut pushes = vec![[0.0f64; 2]; bodies.len()];
    let mut crowd = vec![0usize; bodies.len()];
    for (k, a) in bodies.iter().enumerate() {
        let (gx, gz) = (
            (a.pos[0] / 2.0).floor() as i32,
            (a.pos[2] / 2.0).floor() as i32,
        );
        for x in gx - 1..=gx + 1 {
            for z in gz - 1..=gz + 1 {
                for &j in grid.get(&(x, z)).into_iter().flatten() {
                    if j == k || !overlap(a, &bodies[j]) {
                        continue;
                    }
                    crowd[k] += 1;
                    if let Some(p) = push_from(a.pos, bodies[j].pos) {
                        pushes[k][0] += p[0];
                        pushes[k][1] += p[1];
                    }
                }
            }
        }
        // Players walking into it.
        for t in targets.iter().filter(|t| t.present) {
            let player = Body {
                i: usize::MAX,
                pos: t.pos,
                half: PLAYER_WIDTH / 2.0,
                height: PLAYER_HEIGHT,
            };
            if overlap(a, &player)
                && let Some(p) = push_from(a.pos, t.pos)
            {
                pushes[k][0] += p[0];
                pushes[k][1] += p[1];
            }
        }
    }
    for (k, b) in bodies.iter().enumerate() {
        let e = &mut entities[b.i];
        e.vel[0] += pushes[k][0];
        e.vel[2] += pushes[k][1];
        if limit > 0 && crowd[k] >= limit as usize - 1 && (now + b.i as i64) % 4 == 0 {
            hurt::harm(ctx, e, &hurt::Harm::new(6.0, "minecraft:cramming"));
        }
    }
}
