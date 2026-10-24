//! Villagers on foot (minecraft.wiki, "Villager"): they stroll about near
//! where they stand, at a zombie's pace, and open the wooden doors in their
//! way, closing them again behind them. At night they lie down in a bed, and
//! they run from zombies and the like.

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

/// What a villager does to the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Open(Cell),
    Close(Cell),
    /// Lies down in the bed (its head) at the cell.
    Sleep(Cell),
    /// Gets up from the bed.
    Wake(Cell),
}

/// What is going on around a villager.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env<'a> {
    /// Night: time for bed.
    pub night: bool,
    /// The afternoon (9000 to 12000): villagers meet at their bell.
    pub gather: bool,
    /// Where zombies and other monsters it fears are.
    pub threats: &'a [[f64; 3]],
}

/// Blocks from the bell it is satisfied to be at in the afternoon.
const MEET: f64 = 4.0;
/// Blocks within which it sees a monster and runs.
const FEAR: f64 = 8.0;
/// Where a lying villager's body sits on a bed (its height).
const BED_TOP: f64 = 0.5625;

/// A walking villager's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Walk {
    pub nav: Nav,
    /// Doors it opened and has not closed yet.
    pub opened: Vec<Cell>,
    /// The bed it has chosen, and whether it lies in it.
    pub bed: Option<Cell>,
    pub asleep: bool,
    /// Ticks to wait before looking for a bed again.
    pub rest: u32,
    /// Game time it last lay down in a bed (golems come to villagers that
    /// slept in the last 24000 ticks).
    pub slept: Option<i64>,
    /// Consecutive ticks it has been running from a monster.
    pub panic_ticks: u32,
    /// Until when it knows of an iron golem nearby (no new one is called).
    pub golem_seen_until: i64,
    /// Game time it last gossiped.
    pub gossiped: Option<i64>,
    /// Game time it last talked with another villager (and told it what
    /// it has heard).
    pub chatted: Option<i64>,
    /// A bell rang: until when (game time) it hides in the bed at the cell.
    pub hiding: Option<(u32, Cell)>,
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
    (0..2).map(|dy| [c[0], c[1] + dy, c[2]]).find(|d| {
        matches!(
            w.door(d[0], d[1], d[2]),
            Some(Door {
                by_hand: true,
                open: false
            })
        )
    })
}

fn flat_dist(a: [f64; 3], c: Cell) -> f64 {
    ((f64::from(c[0]) + 0.5 - a[0]).powi(2) + (f64::from(c[2]) + 0.5 - a[2]).powi(2)).sqrt()
}

/// One tick of a villager or wandering trader: gravity, a stroll when not
/// `standing` (trading), and the doors on its way. Door changes are pushed
/// to `ops`.
pub fn step(
    entity: &mut Entity,
    world: &dyn Terrain,
    now: u32,
    standing: bool,
    env: &Env,
    ops: &mut Vec<Op>,
) {
    let seed = u64::from(entity.id.unsigned_abs()) * 0x9E37_79B9 + 1;
    let (pos, on_ground) = (entity.pos, entity.on_ground);
    let Kind::Merchant(m) = &mut entity.kind else {
        return;
    };
    let home = m.home.map(|(x, y, z)| [x, y, z]);
    let meeting = m
        .meeting
        .filter(|_| env.gather && !m.wandering)
        .map(|(x, y, z)| [x, y, z]);
    let walk = &mut m.walk;
    let wandering = m.wandering;
    walk.nav.tick(pos, now);
    let from = [
        pos[0].floor() as i32,
        (pos[1] + 0.5).floor() as i32,
        pos[2].floor() as i32,
    ];
    let afraid = (!wandering)
        .then(|| {
            env.threats
                .iter()
                .filter(|t| {
                    (t[0] - pos[0]).hypot(t[2] - pos[2]) < FEAR && (t[1] - pos[1]).abs() < 4.0
                })
                .min_by(|a, b| {
                    let d = |t: &&[f64; 3]| (t[0] - pos[0]).hypot(t[2] - pos[2]);
                    d(a).total_cmp(&d(b))
                })
                .copied()
        })
        .flatten();
    walk.panic_ticks = if afraid.is_some() {
        walk.panic_ticks + 1
    } else {
        0
    };
    // A bell rang: it hides until the time is up.
    if walk.hiding.is_some_and(|(until, _)| now >= until) {
        walk.hiding = None;
    }
    let hiding = walk.hiding;
    // Getting up, from the day or a fright or a customer or a bell.
    if walk.asleep && (!env.night || afraid.is_some() || standing || hiding.is_some()) {
        walk.asleep = false;
        if let Some(b) = walk.bed.take() {
            ops.push(Op::Wake(b));
        }
        walk.rest = 200;
    }
    if walk.asleep {
        // Lies still where it is.
        entity.vel = [0.0; 3];
        entity.age += 1;
        return;
    }
    if !env.night && walk.bed.is_some() {
        walk.bed = None;
    }
    let mut hurry = 1.0;
    if let Some(t) = afraid {
        // Away from it, as far as it can.
        hurry = 1.5;
        if walk.nav.is_done() || now.is_multiple_of(20) {
            let (dx, dz) = (pos[0] - t[0], pos[2] - t[2]);
            let len = dx.hypot(dz).max(0.01);
            let p = params();
            for k in [12.0, 8.0, 5.0] {
                let c = [
                    from[0] + (dx / len * k).round() as i32,
                    from[1],
                    from[2] + (dz / len * k).round() as i32,
                ];
                if standable(world, c, &p) {
                    walk.nav.stop();
                    walk.nav.go_to(world, from, c, &p);
                    break;
                }
            }
        }
    } else if standing {
        walk.nav.stop();
    } else if let Some((_, bed)) = hiding {
        hurry = 1.5;
        if walk.nav.is_done() && flat_dist(pos, bed) > 2.0 {
            let p = params();
            if let Some(s) = beside(world, bed, &p) {
                walk.nav.go_to(world, from, s, &p);
            }
        }
    } else if let Some(bell) = meeting.filter(|b| flat_dist(pos, *b) > MEET) {
        // The afternoon: it goes to the bell.
        if walk.nav.is_done() {
            let p = params();
            if let Some(s) = beside(world, bell, &p) {
                walk.nav.go_to(world, from, s, &p);
            }
        }
    } else if env.night && !wandering {
        night_walk(walk, home, world, pos, from, now, seed, ops);
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
    if walk.asleep {
        entity.age += 1;
        return;
    }
    if let Some(c) = walk.nav.waypoint(pos, 0.4) {
        // Doors in the next two cells of the path open as it nears them.
        let index = walk.nav.index;
        for cell in walk
            .nav
            .path
            .iter()
            .skip(index)
            .take(2)
            .copied()
            .collect::<Vec<_>>()
        {
            if let Some(d) = closed_door(world, cell)
                && flat_dist(pos, d) < 2.0
                && !walk.opened.contains(&d)
            {
                walk.opened.push(d);
                ops.push(Op::Open(d));
            }
        }
        let (dx, dz) = (
            f64::from(c[0]) + 0.5 - pos[0],
            f64::from(c[2]) + 0.5 - pos[2],
        );
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
    ops.extend(behind.into_iter().map(Op::Close));
    // Move.
    let mut vel = [0.0, entity.vel[1], 0.0];
    if let Some(d) = dir {
        vel[0] = d[0] * SPEED * hurry;
        vel[2] = d[1] * SPEED * hurry;
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
    vel[1] = if ground && vel[1] <= 0.0 {
        0.0
    } else {
        (vel[1] - GRAVITY) * 0.98
    };
    entity.vel = [0.0, vel[1], 0.0];
    entity.age += 1;
}

/// Sets it walking to the cell, if it is not on its way somewhere.
pub fn head_for(entity: &mut Entity, world: &dyn Terrain, to: Cell) {
    let from = [
        entity.pos[0].floor() as i32,
        (entity.pos[1] + 0.5).floor() as i32,
        entity.pos[2].floor() as i32,
    ];
    let Kind::Merchant(m) = &mut entity.kind else {
        return;
    };
    if m.walk.nav.is_done() && !m.walk.asleep {
        m.walk.nav.go_to(world, from, to, &params());
    }
}

/// A cell to stand in beside the bed.
fn beside(world: &dyn Terrain, c: Cell, p: &PathParams) -> Option<Cell> {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .flat_map(|(dx, dz)| [0, -1, 1].map(|dy| [c[0] + dx, c[1] + dy, c[2] + dz]))
        .find(|s| standable(world, *s, p))
}

/// At night: goes to the bed it has claimed (see the village's points of
/// interest) and lies down in it. A villager with no bed stays up.
#[allow(clippy::too_many_arguments)]
fn night_walk(
    walk: &mut Walk,
    home: Option<Cell>,
    world: &dyn Terrain,
    pos: [f64; 3],
    from: Cell,
    now: u32,
    seed: u64,
    ops: &mut Vec<Op>,
) {
    let Some(bed) = walk.bed else {
        if walk.rest > 0 {
            walk.rest -= 1;
        } else if (u64::from(now) + seed).is_multiple_of(40) {
            let Some(c) = home.filter(|c| world.free_bed(c[0], c[1], c[2])) else {
                walk.rest = 100;
                return;
            };
            walk.bed = Some(c);
            let p = params();
            match beside(world, c, &p) {
                Some(s) => {
                    walk.nav.stop();
                    walk.nav.go_to(world, from, s, &p);
                }
                None => walk.bed = None,
            }
        }
        return;
    };
    if !world.free_bed(bed[0], bed[1], bed[2]) {
        walk.bed = None;
        walk.rest = 100;
        return;
    }
    let near = flat_dist(pos, bed) < 1.6 && (pos[1] - f64::from(bed[1])).abs() < 1.6;
    if near {
        walk.nav.stop();
        walk.asleep = true;
        ops.push(Op::Sleep(bed));
    } else if walk.nav.is_done() {
        // The way was too far or blocked: another time.
        walk.bed = None;
        walk.rest = 200;
    }
}

/// Where a villager lies in the bed at `bed`.
pub fn bed_spot(bed: Cell) -> [f64; 3] {
    [
        f64::from(bed[0]) + 0.5,
        f64::from(bed[1]) + BED_TOP,
        f64::from(bed[2]) + 0.5,
    ]
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
            y < 0
                || x == 3
                    && (-8..=8).contains(&z)
                    && (y < 3)
                    && (z != 0 || y == 2 || !*self.open.borrow())
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
        let world = Yard {
            open: RefCell::new(false),
        };
        let mut e = villager([1.5, 0.0, 0.5]);
        // Sent for a walk to the other side of the door.
        let Kind::Merchant(m) = &mut e.kind else {
            panic!()
        };
        m.walk.nav.go_to(&world, [1, 0, 0], [6, 0, 0], &params());
        assert!(m.walk.nav.path.contains(&[3, 0, 0]));
        let (mut opened, mut closed) = (None, None);
        let mut log: HashMap<&str, usize> = HashMap::new();
        let mut farthest = 0.0f64;
        for t in 0..300 {
            let mut ops = Vec::new();
            step(&mut e, &world, t, false, &Env::default(), &mut ops);
            for op in ops {
                match op {
                    Op::Open(_) => {
                        *world.open.borrow_mut() = true;
                        opened.get_or_insert(t);
                        *log.entry("open").or_default() += 1;
                    }
                    Op::Close(_) => {
                        *world.open.borrow_mut() = false;
                        closed.get_or_insert(t);
                    }
                    _ => {}
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
        let world = Yard {
            open: RefCell::new(false),
        };
        let mut e = villager([1.5, 0.0, 0.5]);
        let Kind::Merchant(m) = &mut e.kind else {
            panic!()
        };
        m.walk.nav.go_to(&world, [1, 0, 0], [2, 0, 6], &params());
        for t in 0..60 {
            step(&mut e, &world, t, true, &Env::default(), &mut Vec::new());
        }
        assert_eq!(e.pos, [1.5, 0.0, 0.5]);
    }
}
