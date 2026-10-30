//! Minecart movement along rails (minecraft.wiki, "Minecart"). Each tick the
//! cart finds the rail it is on, turns its speed to run along it, is slowed
//! going up slopes and sped going down, pushed by powered rails (and
//! stopped by unpowered ones), and moves along the rail's line.

use mc_entity::extra::{Cart, Craft, Vehicle};
use mc_entity::{Collide, Entity, Kind, move_entity};

use crate::context::ServerContext;
use crate::fx;
use crate::hits::{self, Tick};

/// Fastest a cart goes along either axis, per tick (8 blocks a second).
pub const MAX_SPEED: f64 = 0.4;
const SLOPE: f64 = 0.0078125;

/// A rail's two ends as `[x, rise, z]` offsets from the block centre
/// (minecraft.wiki "Rail"); `rise` is 1 where the rail climbs.
pub fn exits(shape: &str) -> Option<[[i32; 3]; 2]> {
    Some(match shape {
        "north_south" => [[0, 0, -1], [0, 0, 1]],
        "east_west" => [[-1, 0, 0], [1, 0, 0]],
        "ascending_east" => [[-1, 0, 0], [1, 1, 0]],
        "ascending_west" => [[-1, 1, 0], [1, 0, 0]],
        "ascending_north" => [[0, 1, -1], [0, 0, 1]],
        "ascending_south" => [[0, 0, -1], [0, 1, 1]],
        "south_east" => [[0, 0, 1], [1, 0, 0]],
        "south_west" => [[0, 0, 1], [-1, 0, 0]],
        "north_west" => [[0, 0, -1], [-1, 0, 0]],
        "north_east" => [[0, 0, -1], [1, 0, 0]],
        _ => return None,
    })
}

/// A rail under the cart.
#[derive(Clone, Debug)]
pub struct Rail {
    pub cell: [i32; 3],
    pub shape: String,
    /// `powered_rail` / `activator_rail` / `detector_rail` / `rail`.
    pub name: &'static str,
    pub powered: bool,
}

fn rail_at(ctx: &ServerContext, cell: [i32; 3]) -> Option<Rail> {
    let state = hits::block_at(ctx, cell)?;
    let name = match ctx.data.blocks.block_name(state)? {
        "minecraft:rail" => "rail",
        "minecraft:powered_rail" => "powered_rail",
        "minecraft:activator_rail" => "activator_rail",
        "minecraft:detector_rail" => "detector_rail",
        _ => return None,
    };
    Some(Rail {
        cell,
        shape: ctx.data.blocks.property(state, "shape")?.to_owned(),
        name,
        powered: ctx.data.blocks.property(state, "powered") == Some("true"),
    })
}

/// The rail a cart at `pos` is on: the one under it, else the one in it.
pub fn find_rail(ctx: &ServerContext, pos: [f64; 3]) -> Option<Rail> {
    let c = pos.map(|v| v.floor() as i32);
    rail_at(ctx, [c[0], c[1] - 1, c[2]]).or_else(|| rail_at(ctx, c))
}

/// Where the rail's line is at the horizontal position `(x, z)`: the
/// snapped position on it.
fn on_line(rail: &Rail, x: f64, z: f64) -> [f64; 3] {
    let Some(ex) = exits(&rail.shape) else {
        return [x, f64::from(rail.cell[1]) + 0.0625, z];
    };
    let c = rail.cell;
    let end = |e: [i32; 3]| {
        [
            f64::from(c[0]) + 0.5 + f64::from(e[0]) * 0.5,
            f64::from(c[1]) + 0.0625 + f64::from(e[1]),
            f64::from(c[2]) + 0.5 + f64::from(e[2]) * 0.5,
        ]
    };
    let (p0, p1) = (end(ex[0]), end(ex[1]));
    let (dx, dz) = (p1[0] - p0[0], p1[2] - p0[2]);
    let len2 = dx * dx + dz * dz;
    let d = (((x - p0[0]) * dx + (z - p0[2]) * dz) / len2).clamp(-0.5, 1.5);
    [
        p0[0] + dx * d,
        p0[1] + (p1[1] - p0[1]) * d.clamp(0.0, 1.0),
        p0[2] + dz * d,
    ]
}

struct Ground<'a>(&'a ServerContext);

impl Collide for Ground<'_> {
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        hits::stops(self.0, [x, y, z])
    }

    fn boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<mc_entity::Aabb>) {
        super::boat::shape_at(self.0, x, y, z, out);
    }
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
    let Craft::Minecart(cart) = v.craft.clone() else {
        return;
    };
    e.age += 1;
    if v.wobble > 0 {
        v.wobble -= 1;
    }
    if v.damage > 0.0 {
        v.damage = (v.damage - 1.0).max(0.0);
    }
    let rail = find_rail(ctx, e.pos);
    if let Some(r) = &rail
        && r.name == "activator_rail"
    {
        v.enabled = !r.powered;
    }
    // Activator rails shake riders off and light TNT.
    if let Some(r) = &rail
        && r.name == "activator_rail"
        && r.powered
    {
        super::eject(ctx, e.id, v);
        // A command block minecart runs at most every 4 ticks.
        if cart == Cart::Command && i64::from(e.age) - v.command.activated_at >= 4 {
            v.command.activated_at = i64::from(e.age);
            t.out.carts.push(e.id);
        }
        if cart == Cart::Tnt && v.fuse < 0 {
            v.fuse = 80;
            // It flashes as it burns.
            fx::status(ctx, e.id, 10);
            fx::sound(
                ctx,
                "entity.tnt.primed",
                fx::category::BLOCKS,
                e.pos,
                1.0,
                1.0,
            );
        }
    }
    if v.fuse >= 0 {
        v.fuse -= 1;
        if v.fuse <= 0 {
            e.removed = true;
            t.out.blasts.push(crate::explosion::Blast {
                center: [e.pos[0], e.pos[1] + 0.35, e.pos[2]],
                power: 4.0,
                source: e.id,
                owner: None,
                mob: false,
                dangerous: false,
                by: None,
                fire: false,
                respawn: None,
                block: false,
            });
            return;
        }
    }
    let occupied = !v.riders.is_empty();
    // A furnace cart burns coal to push itself.
    if cart == Cart::Furnace && v.fuel > 0 {
        v.fuel -= 1;
        if rail.is_some() {
            let speed = e.vel[0].hypot(e.vel[2]);
            let (dx, dz) = if speed > 1e-4 {
                (e.vel[0] / speed, e.vel[2] / speed)
            } else {
                (0.0, 0.0)
            };
            e.vel[0] += dx * 0.04;
            e.vel[2] += dz * 0.04;
        }
    }
    v.on_rail = rail.is_some();
    let water = hits::in_water(ctx, e.pos);
    let max = if water { MAX_SPEED / 2.0 } else { MAX_SPEED };
    let before = (e.pos, e.vel);
    match rail {
        Some(rail) => along(t.ctx, e, v, &rail, occupied, max),
        None => off_rail(ctx, e, max),
    }
    if (e.pos, e.vel) != before || e.age.is_multiple_of(20) {
        t.out.sync.push(fx::Sync {
            id: e.id,
            pos: e.pos,
            vel: e.vel,
            yaw: v.yaw,
            pitch: 0.0,
            on_ground: e.on_ground,
        });
    }
}

fn along(
    ctx: &ServerContext,
    e: &mut Entity,
    v: &mut Vehicle,
    rail: &Rail,
    occupied: bool,
    max: f64,
) {
    let Some(ex) = exits(&rail.shape) else {
        return;
    };
    // Put on the rail's line first.
    e.pos = on_line(rail, e.pos[0], e.pos[2]);
    let height = e.pos[1];
    e.on_ground = true;
    let (mut dx, mut dz) = (
        f64::from(ex[1][0] - ex[0][0]),
        f64::from(ex[1][2] - ex[0][2]),
    );
    let len = dx.hypot(dz);
    if e.vel[0] * dx + e.vel[2] * dz < 0.0 {
        dx = -dx;
        dz = -dz;
    }
    let speed = e.vel[0].hypot(e.vel[2]).min(2.0);
    e.vel = [speed * dx / len, 0.0, speed * dz / len];
    // Down a slope it speeds up, up it slows down.
    match rail.shape.as_str() {
        "ascending_east" => e.vel[0] -= SLOPE,
        "ascending_west" => e.vel[0] += SLOPE,
        "ascending_north" => e.vel[2] += SLOPE,
        "ascending_south" => e.vel[2] -= SLOPE,
        _ => {}
    }
    // Back along the line after the slope changed the speed.
    let speed = e.vel[0].hypot(e.vel[2]);
    if speed > 1e-9 {
        let keep = if e.vel[0] * dx + e.vel[2] * dz < 0.0 {
            -1.0
        } else {
            1.0
        };
        e.vel = [keep * speed * dx / len, 0.0, keep * speed * dz / len];
    }
    if rail.name == "powered_rail" {
        if rail.powered {
            if speed > 0.01 {
                e.vel[0] += e.vel[0] / speed * 0.06;
                e.vel[2] += e.vel[2] / speed * 0.06;
            } else {
                // At rest it is pushed away from a wall at the rail's end.
                let c = rail.cell;
                match rail.shape.as_str() {
                    "east_west" if hits::stops(ctx, [c[0] - 1, c[1], c[2]]) => e.vel[0] = 0.02,
                    "east_west" if hits::stops(ctx, [c[0] + 1, c[1], c[2]]) => e.vel[0] = -0.02,
                    "north_south" if hits::stops(ctx, [c[0], c[1], c[2] - 1]) => e.vel[2] = 0.02,
                    "north_south" if hits::stops(ctx, [c[0], c[1], c[2] + 1]) => e.vel[2] = -0.02,
                    _ => {}
                }
            }
        } else if speed < 0.03 {
            e.vel = [0.0; 3];
        } else {
            e.vel[0] *= 0.5;
            e.vel[2] *= 0.5;
        }
    }
    // It moves at most `max` a tick (with someone aboard, three quarters
    // of its speed), keeping the rest of its speed, then slows by itself
    // (less with someone aboard).
    let share = if occupied { 0.75 } else { 1.0 };
    let to = [
        e.pos[0] + (e.vel[0] * share).clamp(-max, max),
        e.pos[1],
        e.pos[2] + (e.vel[2] * share).clamp(-max, max),
    ];
    let drag = if occupied { 0.997 } else { 0.96 };
    let wet = if hits::in_water(ctx, e.pos) {
        0.95
    } else {
        1.0
    };
    e.vel[0] *= drag * wet;
    e.vel[2] *= drag * wet;
    // Stops against a block that is not a rail ahead of it, in its way.
    let ahead = [
        to[0].floor() as i32,
        e.pos[1].floor() as i32,
        to[2].floor() as i32,
    ];
    if find_rail(ctx, to).is_none() && hits::stops(ctx, ahead) {
        e.vel = [0.0; 3];
        return;
    }
    e.pos[0] = to[0];
    e.pos[2] = to[2];
    // Onto the line of the rail it is over now (the next one, at a join).
    match find_rail(ctx, e.pos) {
        Some(next) => e.pos = on_line(&next, e.pos[0], e.pos[2]),
        None => e.pos[1] = to[1],
    }
    // Height lost becomes speed, height gained costs it: 0.05 a block.
    let gain = (height - e.pos[1]) * 0.05;
    let speed = e.vel[0].hypot(e.vel[2]);
    if speed > 0.0 {
        let k = ((speed + gain) / speed).max(0.0);
        e.vel[0] *= k;
        e.vel[2] *= k;
    }
    if e.vel[0] != 0.0 || e.vel[2] != 0.0 {
        v.yaw = (e.vel[0].atan2(e.vel[2]).to_degrees() as f32 + 90.0) % 360.0;
    }
}

fn off_rail(ctx: &ServerContext, e: &mut Entity, max: f64) {
    e.vel[0] = e.vel[0].clamp(-max, max);
    e.vel[2] = e.vel[2].clamp(-max, max);
    e.vel[1] -= 0.04;
    let (moved, on_ground) = move_entity(e.aabb(), e.vel, &Ground(ctx));
    for ((p, v), m) in e.pos.iter_mut().zip(&mut e.vel).zip(moved) {
        *p += m;
        if (m - *v).abs() > 1e-9 {
            *v = 0.0;
        }
    }
    e.on_ground = on_ground;
    if on_ground {
        e.vel[0] *= 0.5;
        e.vel[1] *= 0.5;
        e.vel[2] *= 0.5;
    } else {
        e.vel[0] *= 0.95;
        e.vel[2] *= 0.95;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;
    use mc_protocol::Position;

    fn rail(ctx: &ServerContext, x: i32, y: i32, name: &str, shape: &str, powered: bool) {
        let p = if name == "minecraft:rail" {
            vec![("shape", shape)]
        } else {
            vec![
                ("shape", shape),
                ("powered", if powered { "true" } else { "false" }),
            ]
        };
        let state = ctx.data.blocks.state_id(name, &p).expect("rail state");
        ctx.world
            .set_block(Position::new(x, y, 0), state, &ctx.data.blocks, &ctx.rules)
            .expect("set");
    }

    fn cart(ctx: &ServerContext, pos: [f64; 3], vel: [f64; 3]) -> Entity {
        let mut e = super::super::new(ctx, Craft::Minecart(Cart::Rideable), pos, 0.0);
        e.vel = vel;
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
            tick(&mut t, e);
        }
    }

    #[test]
    fn a_cart_rolls_along_a_rail_and_slows() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        for x in 100..140 {
            rail(&ctx, x, -60, "minecraft:rail", "east_west", false);
        }
        let mut e = cart(&ctx, [100.5, -59.9375, 0.5], [0.3, 0.0, 0.0]);
        run(&ctx, &mut e, 20);
        // It moves first and slows after: 0.3 × (1 + 0.96 + … + 0.96¹⁹)
        // = 4.1806 blocks.
        let far = 0.3 * (1.0 - 0.96f64.powi(20)) / 0.04;
        assert!((e.pos[0] - 100.5 - far).abs() < 1e-6, "{:?}", e.pos);
        assert!((e.pos[1] + 59.9375).abs() < 1e-6 && (e.pos[2] - 0.5).abs() < 1e-6);
        // Empty carts lose 4% a tick.
        assert!(e.vel[0] < 0.3 * 0.96f64.powi(19));
    }

    #[test]
    fn powered_rails_push_and_unpowered_ones_stop() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        for x in 200..230 {
            rail(&ctx, x, -60, "minecraft:powered_rail", "east_west", true);
        }
        let mut e = cart(&ctx, [200.5, -59.9375, 0.5], [0.1, 0.0, 0.0]);
        run(&ctx, &mut e, 30);
        // It keeps speeding up, but moves no more than 0.4 a tick.
        assert!(e.vel[0] > MAX_SPEED, "{:?}", e.vel);
        let at = e.pos[0];
        run(&ctx, &mut e, 1);
        assert!((e.pos[0] - at - MAX_SPEED).abs() < 1e-9, "{:?}", e.pos);
        for x in 300..330 {
            rail(&ctx, x, -60, "minecraft:powered_rail", "east_west", false);
        }
        let mut e = cart(&ctx, [300.5, -59.9375, 0.5], [0.3, 0.0, 0.0]);
        run(&ctx, &mut e, 20);
        assert!(e.vel[0] < 0.01, "{:?}", e.vel);
    }

    #[test]
    fn a_cart_goes_up_and_down_a_slope() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        rail(&ctx, 400, -60, "minecraft:rail", "east_west", false);
        let stone = ctx.data.blocks.default_state("minecraft:stone").unwrap();
        for k in 1..=3 {
            // Each rail climbs one more; stone holds it up.
            for y in -60..(-61 + k) {
                ctx.world.set_block(
                    Position::new(400 + k, y, 0),
                    stone,
                    &ctx.data.blocks,
                    &ctx.rules,
                );
            }
            rail(
                &ctx,
                400 + k,
                -61 + k,
                "minecraft:rail",
                "ascending_east",
                false,
            );
        }
        let mut e = cart(&ctx, [400.5, -59.9375, 0.5], [0.35, 0.0, 0.0]);
        run(&ctx, &mut e, 10);
        assert!(e.pos[1] > -59.5, "{:?}", e.pos);
    }

    #[test]
    fn a_cart_rolls_down_a_slope_from_rest() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // One rail up (rising to the west), then flat track east.
        rail(&ctx, 600, -60, "minecraft:rail", "ascending_west", false);
        for x in 601..620 {
            rail(&ctx, x, -60, "minecraft:rail", "east_west", false);
        }
        let mut e = cart(&ctx, [600.5, -59.4375, 0.5], [0.0; 3]);
        run(&ctx, &mut e, 400);
        // minecraft.wiki ("Minecart", distance of empty carts starting on a
        // downward slope): 2.77 blocks from a slope one high. The height
        // it loses turns into speed.
        assert!((e.pos[0] - 600.5 - 2.77).abs() < 0.01, "{:?}", e.pos);
        assert!(e.vel[0].abs() < 1e-3);
    }

    #[test]
    fn off_rails_a_cart_falls() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = cart(&ctx, [500.5, -50.0, 0.5], [0.0; 3]);
        run(&ctx, &mut e, 60);
        assert!(e.on_ground && (e.pos[1] + 60.0).abs() < 0.01, "{:?}", e.pos);
    }

    #[test]
    fn rails_have_two_ends() {
        for shape in [
            "north_south",
            "east_west",
            "ascending_east",
            "ascending_west",
            "ascending_north",
            "ascending_south",
            "south_east",
            "south_west",
            "north_west",
            "north_east",
        ] {
            let e = exits(shape).unwrap();
            assert_ne!(e[0], e[1], "{shape}");
        }
        assert!(exits("nonsense").is_none());
    }

    #[test]
    fn the_line_climbs_on_a_slope() {
        let rail = Rail {
            cell: [0, 10, 0],
            shape: "ascending_east".into(),
            name: "rail",
            powered: false,
        };
        let low = on_line(&rail, 0.0, 0.5);
        let high = on_line(&rail, 1.0, 0.5);
        assert!((low[1] - 10.0625).abs() < 1e-9);
        assert!((high[1] - 11.0625).abs() < 1e-9);
        // Off the line sideways, it is pulled onto it.
        assert!((on_line(&rail, 0.5, 0.9)[2] - 0.5).abs() < 1e-9);
    }
}
