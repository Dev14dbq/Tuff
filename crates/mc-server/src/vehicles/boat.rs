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

    fn boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<mc_entity::Aabb>) {
        shape_at(self.0, x, y, z, out);
    }
}

pub(crate) fn shape_at(
    ctx: &ServerContext,
    x: i32,
    y: i32,
    z: i32,
    out: &mut Vec<mc_entity::Aabb>,
) {
    let Some(state) = hits::block_at(ctx, [x, y, z]) else {
        return;
    };
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(state).unwrap_or("");
    crate::mobs::hitbox::push(
        name,
        &|k| blocks.property(state, k),
        [x, y, z],
        hits::stops(ctx, [x, y, z]).then_some((0.0, 1.0)),
        out,
    );
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

/// Height of a boat's box.
const HEIGHT: f64 = 0.5625;

/// How slippery the ground under the boat is (minecraft.wiki, "Boat"):
/// the average over the blocks its bottom rests on, 0.98 for ice, 0.989
/// for blue ice, 0.6 for the rest.
fn ground_slip(ctx: &ServerContext, e: &Entity) -> f64 {
    let b = e.aabb();
    let y = (b.min[1] - 0.001).floor() as i32;
    let (mut sum, mut n) = (0.0, 0);
    for x in b.min[0].floor() as i32..=(b.max[0] - 1e-7).floor() as i32 {
        for z in b.min[2].floor() as i32..=(b.max[2] - 1e-7).floor() as i32 {
            let Some(state) = hits::block_at(ctx, [x, y, z]) else {
                continue;
            };
            if !hits::stops(ctx, [x, y, z]) {
                continue;
            }
            sum += match ctx.data.blocks.block_name(state) {
                Some("minecraft:ice" | "minecraft:packed_ice" | "minecraft:frosted_ice") => 0.98,
                Some("minecraft:blue_ice") => 0.989,
                _ => 0.6,
            };
            n += 1;
        }
    }
    if n == 0 { 0.9 } else { sum / f64::from(n) }
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
    // Under water nobody can stay in it.
    let under = hits::in_water(ctx, [e.pos[0], e.pos[1] + HEIGHT + 0.001, e.pos[2]]);
    if under && !v.riders.is_empty() {
        super::eject(ctx, e.id, v);
    }
    // With a driver the client says where it goes.
    if !v.riders.is_empty() {
        return;
    }
    let before = (e.pos, e.vel);
    let floating = hits::in_water(ctx, [e.pos[0], e.pos[1] + 0.1, e.pos[2]]);
    // Friction first: 0.9 on water and in the air, 0.45 under water, the
    // ground's slipperiness on land.
    let slip = if under {
        0.45
    } else if floating || !e.on_ground {
        0.9
    } else {
        ground_slip(ctx, e)
    };
    e.vel[0] *= slip;
    e.vel[2] *= slip;
    if floating {
        // Floats, a little under the surface.
        let target = surface(ctx, e.pos) - 0.2;
        e.vel[1] = ((target - e.pos[1]) * 0.25).clamp(-0.08, 0.08);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;
    use crate::tick::lock;
    use mc_protocol::Position;

    fn boat(ctx: &ServerContext, pos: [f64; 3], vel: [f64; 3]) -> Entity {
        let mut e = super::super::new(
            ctx,
            Craft::Boat {
                wood: "oak".into(),
                chest: false,
            },
            pos,
            0.0,
        );
        e.vel = vel;
        e.on_ground = true;
        e
    }

    fn run(ctx: &ServerContext, e: &mut Entity, ticks: usize) {
        let mut out = hits::Out::default();
        for _ in 0..ticks {
            let mut t = Tick {
                ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            super::super::tick(&mut t, e);
        }
    }

    fn floor(ctx: &ServerContext, x0: i32, name: &str) {
        let state = ctx.data.blocks.default_state(name).unwrap();
        for x in x0..x0 + 40 {
            for z in -1..2 {
                ctx.world.set_block(
                    Position::new(x, -61, z),
                    state,
                    &ctx.data.blocks,
                    &ctx.rules,
                );
            }
        }
    }

    #[test]
    fn a_boat_slides_far_on_ice_and_stops_on_land() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        floor(&ctx, 0, "minecraft:stone");
        floor(&ctx, 100, "minecraft:packed_ice");
        floor(&ctx, 200, "minecraft:blue_ice");
        let mut slid = Vec::new();
        for x0 in [0, 100, 200] {
            let start = f64::from(x0) + 2.0;
            let mut e = boat(&ctx, [start, -60.0, 0.5], [0.5, 0.0, 0.0]);
            run(&ctx, &mut e, 30);
            slid.push(e.pos[0] - start);
        }
        // Friction comes first each tick: 0.5 × (0.6 + 0.6² + …) ≈ 0.75
        // on stone, 0.5 × 0.98 × (1 − 0.98³⁰) / 0.02 ≈ 11.2 on ice.
        assert!((slid[0] - 0.75).abs() < 0.01, "{slid:?}");
        let ice = 0.5 * 0.98 * (1.0 - 0.98f64.powi(30)) / 0.02;
        assert!((slid[1] - ice).abs() < 0.01, "{slid:?}");
        assert!(slid[2] > slid[1], "{slid:?}");
    }

    #[test]
    fn hits_shake_it_and_paddles_turn() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = boat(&ctx, [0.5, -60.0, 0.5], [0.0; 3]);
        let mut fresh = Vec::new();
        let blow = crate::hits::Hit {
            target: e.id,
            amount: 1.0,
            by: None,
            with: None,
            kind: "minecraft:player_attack",
        };
        super::super::hurt(&ctx, &mut e, &blow, &mut fresh);
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!((v.wobble, v.hurt_dir, v.damage), (10, -1, 10.0));
        super::super::hurt(&ctx, &mut e, &blow, &mut fresh);
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!((v.hurt_dir, v.damage), (1, 20.0));
        run(&ctx, &mut e, 5);
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!((v.wobble, v.damage), (5, 15.0));
        assert!(!e.removed);
        // A driver's paddles show.
        if let Kind::Vehicle(v) = &mut e.kind {
            v.riders.push(4);
        }
        lock(&ctx.entities).push(e.clone());
        let actor = crate::interact::Actor {
            uuid: 9,
            entity_id: 4,
            game_mode: 0,
            pose: crate::context::Pose {
                x: 0.5,
                y: -60.0,
                z: 0.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
            sneaking: false,
        };
        super::super::act(
            &ctx,
            actor,
            super::super::Act::Paddle {
                left: true,
                right: false,
            },
        );
        let all = lock(&ctx.entities).clone();
        let Kind::Vehicle(v) = &all[0].kind else {
            panic!()
        };
        assert_eq!(v.paddles, [true, false]);
        assert!(super::super::meta(v).iter().any(|(i, _)| *i == 11));
        // Getting off puts the player beside the boat, the way they look
        // (south), on the ground.
        super::super::act(&ctx, actor, super::super::Act::Dismount);
        let all = lock(&ctx.entities).clone();
        let Kind::Vehicle(v) = &all[0].kind else {
            panic!()
        };
        assert!(v.riders.is_empty() && v.paddles == [false; 2]);
    }

    #[test]
    fn under_water_its_riders_get_out() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let water = ctx.data.blocks.default_state("minecraft:water").unwrap();
        for y in -60..-57 {
            ctx.world.set_block(
                Position::new(300, y, 0),
                water,
                &ctx.data.blocks,
                &ctx.rules,
            );
        }
        let mut e = boat(&ctx, [300.5, -60.0, 0.5], [0.0; 3]);
        if let Kind::Vehicle(v) = &mut e.kind {
            v.riders.push(4);
        }
        run(&ctx, &mut e, 1);
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert!(v.riders.is_empty());
    }
}
