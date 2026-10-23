//! Villagers on foot (minecraft.wiki, "Villager"): they stroll about near
//! where they stand, at a zombie's pace, and open the wooden doors in their
//! way, closing them again behind them.

use crate::nav::{Cell, Door, Nav, PathParams, Terrain, standable};
use crate::physics::move_stepping;
use crate::{Entity, Kind};

/// Blocks a tick: a villager's 0.5 speed times its stroll's 0.5, squared and
/// scaled as `mob::body` does for walkers.
const SPEED: f64 = 0.25 * 0.25 * 2.2;
const GRAVITY: f64 = 0.08;
const STEP: f64 = 0.6;
/// A stroll starts one tick in this many.
const STROLL: u64 = 120;

/// What a villager does with a door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoorOp {
    Open(Cell),
    Close(Cell),
}

/// A walking villager's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Walk {
    pub nav: Nav,
    /// Doors it opened and has not closed yet.
    pub opened: Vec<Cell>,
    rng: u64,
}

impl Walk {
    fn next(&mut self, seed: u64) -> u64 {
        if self.rng == 0 {
            self.rng = seed | 1;
        }
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn params() -> PathParams {
    let mut p = PathParams::walker(2);
    p.doors = true;
    p.water_cost = f32::INFINITY;
    p
}

/// A closed hand-opened door in either of the two cells a villager's body
/// fills at `c`.
fn closed_door(w: &dyn Terrain, c: Cell) -> Option<Cell> {
    (0..2)
        .map(|dy| [c[0], c[1] + dy, c[2]])
        .find(|d| matches!(w.door(d[0], d[1], d[2]), Some(Door { by_hand: true, open: false })))
}

fn flat_dist(a: [f64; 3], c: Cell) -> f64 {
    ((f64::from(c[0]) + 0.5 - a[0]).powi(2) + (f64::from(c[2]) + 0.5 - a[2]).powi(2)).sqrt()
}

/// One tick of a villager or wandering trader: gravity, a stroll when not
/// `standing` (trading), and the doors on its way. Door changes are pushed
/// to `ops`.
pub fn step(entity: &mut Entity, world: &dyn Terrain, now: u32, standing: bool, ops: &mut Vec<DoorOp>) {
    let seed = u64::from(entity.id.unsigned_abs()) * 0x9E37_79B9 + 1;
    let (pos, on_ground) = (entity.pos, entity.on_ground);
    let Kind::Merchant(m) = &mut entity.kind else {
        return;
    };
    let walk = &mut m.walk;
    walk.nav.tick(pos, now);
    let from = [
        pos[0].floor() as i32,
        (pos[1] + 0.5).floor() as i32,
        pos[2].floor() as i32,
    ];
    if standing {
        walk.nav.stop();
    } else if walk.nav.is_done() && walk.next(seed) % STROLL == 0 {
        let p = params();
        for _ in 0..10 {
            let r = walk.next(seed);
            let c = [
                from[0] + (r % 21) as i32 - 10,
                from[1] + ((r >> 8) % 15) as i32 - 7,
                from[2] + ((r >> 16) % 21) as i32 - 10,
            ];
            if standable(world, c, &p) {
                walk.nav.stop();
                walk.nav.go_to(world, from, c, &p);
                break;
            }
        }
    }
    // Where it heads this tick.
    let mut dir = None;
    let mut jump = false;
    if let Some(c) = walk.nav.waypoint(pos, 0.4) {
        // Doors in the next two cells of the path open as it nears them.
        let index = walk.nav.index;
        for cell in walk.nav.path.iter().skip(index).take(2).copied().collect::<Vec<_>>() {
            if let Some(d) = closed_door(world, cell)
                && flat_dist(pos, d) < 2.0
                && !walk.opened.contains(&d)
            {
                walk.opened.push(d);
                ops.push(DoorOp::Open(d));
            }
        }
        let (dx, dz) = (f64::from(c[0]) + 0.5 - pos[0], f64::from(c[2]) + 0.5 - pos[2]);
        let len = dx.hypot(dz);
        if len > 0.05 {
            dir = Some([dx / len, dz / len]);
            jump = f64::from(c[1]) > pos[1] + STEP && on_ground && len < 1.0;
        }
    }
    // Doors it has left behind close.
    let near = |d: &Cell| flat_dist(pos, *d) < 2.0 && (f64::from(d[1]) - pos[1]).abs() < 2.5;
    let behind: Vec<Cell> = walk.opened.iter().copied().filter(|d| !near(d)).collect();
    walk.opened.retain(|d| near(d));
    ops.extend(behind.into_iter().map(DoorOp::Close));
    // Move.
    let mut vel = [0.0, entity.vel[1], 0.0];
    if let Some(d) = dir {
        vel[0] = d[0] * SPEED;
        vel[2] = d[1] * SPEED;
    }
    if jump {
        vel[1] = 0.42;
    }
    let (w, h) = entity.size();
    let aabb = crate::Aabb {
        min: [pos[0] - w / 2.0, pos[1], pos[2] - w / 2.0],
        max: [pos[0] + w / 2.0, pos[1] + h, pos[2] + w / 2.0],
    };
    let (moved, ground) = move_stepping(aabb, vel, world, STEP, on_ground);
    for axis in 0..3 {
        entity.pos[axis] += moved[axis];
        if (moved[axis] - vel[axis]).abs() > 1e-9 {
            vel[axis] = 0.0;
        }
    }
    entity.on_ground = ground;
    vel[1] = if ground && vel[1] <= 0.0 { 0.0 } else { (vel[1] - GRAVITY) * 0.98 };
    entity.vel = [0.0, vel[1], 0.0];
    entity.age += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::Liquid;
    use crate::physics::Collide;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A floor at y < 0 and a wall across x = 3 with a door in it at z = 0,
    /// that the test world opens and closes on request.
    struct Yard {
        open: RefCell<bool>,
    }

    impl Collide for Yard {
        fn solid(&self, x: i32, y: i32, z: i32) -> bool {
            y < 0 || x == 3 && (-8..=8).contains(&z) && (y < 3) && (z != 0 || y == 2 || !*self.open.borrow())
        }
    }

    impl Terrain for Yard {
        fn liquid(&self, _: i32, _: i32, _: i32) -> Liquid {
            Liquid::None
        }
        fn hazard(&self, _: i32, _: i32, _: i32) -> bool {
            false
        }
        fn sky_light(&self, _: i32, _: i32, _: i32) -> u8 {
            15
        }
        fn light(&self, _: i32, _: i32, _: i32) -> u8 {
            15
        }
        fn door(&self, x: i32, y: i32, z: i32) -> Option<Door> {
            (x == 3 && z == 0 && (0..2).contains(&y)).then(|| Door {
                by_hand: true,
                open: *self.open.borrow(),
            })
        }
    }

    fn villager(pos: [f64; 3]) -> Entity {
        Entity {
            id: 41,
            uuid: 1,
            kind: Kind::Merchant(Box::new(crate::trade::Merchant::villager())),
            pos,
            vel: [0.0; 3],
            on_ground: true,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }
    }

    #[test]
    fn a_villager_opens_a_door_to_go_through_and_closes_it_behind() {
        let world = Yard { open: RefCell::new(false) };
        let mut e = villager([1.5, 0.0, 0.5]);
        // Sent for a walk to the other side of the door.
        let Kind::Merchant(m) = &mut e.kind else { panic!() };
        m.walk.nav.go_to(&world, [1, 0, 0], [6, 0, 0], &params());
        assert!(m.walk.nav.path.contains(&[3, 0, 0]));
        let (mut opened, mut closed) = (None, None);
        let mut log: HashMap<&str, usize> = HashMap::new();
        let mut farthest = 0.0f64;
        for t in 0..300 {
            let mut ops = Vec::new();
            step(&mut e, &world, t, false, &mut ops);
            for op in ops {
                match op {
                    DoorOp::Open(_) => {
                        *world.open.borrow_mut() = true;
                        opened.get_or_insert(t);
                        *log.entry("open").or_default() += 1;
                    }
                    DoorOp::Close(_) => {
                        *world.open.borrow_mut() = false;
                        closed.get_or_insert(t);
                    }
                }
            }
            farthest = farthest.max(e.pos[0]);
            // Once through, it does not stroll back.
            if farthest > 4.5 && closed.is_some() {
                break;
            }
        }
        assert!(opened.is_some(), "never opened");
        assert!(farthest > 4.5, "stuck at {:?}", e.pos);
        assert!(closed > opened, "{opened:?} {closed:?}");
        assert!(!*world.open.borrow());
    }

    #[test]
    fn a_trading_villager_stands_still() {
        let world = Yard { open: RefCell::new(false) };
        let mut e = villager([1.5, 0.0, 0.5]);
        let Kind::Merchant(m) = &mut e.kind else { panic!() };
        m.walk.nav.go_to(&world, [1, 0, 0], [2, 0, 6], &params());
        for t in 0..60 {
            step(&mut e, &world, t, true, &mut Vec::new());
        }
        assert_eq!(e.pos, [1.5, 0.0, 0.5]);
    }
}
