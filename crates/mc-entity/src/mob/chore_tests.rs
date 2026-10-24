use std::collections::HashMap;

use super::*;
use crate::nav::{Door, Liquid, Perch};
use crate::physics::Collide;

/// A flat floor (y < 0) with extra solid cells, doors and perches, under a
/// roof when `roofed` (sky light 14 instead of 15).
#[derive(Default)]
struct Yard {
    solid: Vec<Cell>,
    doors: HashMap<Cell, Door>,
    perches: HashMap<Cell, Perch>,
    roofed: bool,
}

impl Collide for Yard {
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        y < 0
            || self.solid.contains(&[x, y, z])
            || self.doors.get(&[x, y, z]).is_some_and(|d| !d.open)
            || self.perches.contains_key(&[x, y, z])
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
        if self.roofed { 14 } else { 15 }
    }
    fn light(&self, _: i32, _: i32, _: i32) -> u8 {
        if self.roofed { 14 } else { 15 }
    }
    fn door(&self, x: i32, y: i32, z: i32) -> Option<Door> {
        self.doors.get(&[x, y, z]).copied()
    }
    fn perch(&self, x: i32, y: i32, z: i32) -> Perch {
        self.perches.get(&[x, y, z]).copied().unwrap_or(Perch::None)
    }
    fn holdable(&self, x: i32, y: i32, z: i32) -> bool {
        self.solid.contains(&[x, y, z])
    }
}

fn senses<'a>(world: &'a Yard, players: &'a [Target], tick: u64, difficulty: i32) -> Senses<'a> {
    Senses {
        world,
        players,
        mobs: &[],
        items: &NoItems,
        tick,
        difficulty,
        sunny: true,
        raining: false,
        nether: false,
        day: true,
        griefing: true,
    }
}

fn mob(species: Species, pos: [f64; 3]) -> Entity {
    Entity {
        id: 1,
        uuid: 1,
        kind: Kind::Mob(Box::new(Mob::new(species, 0x5eed_1234_abcd))),
        pos,
        vel: [0.0; 3],
        on_ground: true,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

fn m(e: &mut Entity) -> &mut Mob {
    match &mut e.kind {
        Kind::Mob(m) => m,
        _ => unreachable!(),
    }
}

/// A wall across x = 3 with a closed oak door in it at z = 0.
fn walled() -> Yard {
    let mut y = Yard::default();
    for z in -6..=6 {
        for h in 0..3 {
            if z != 0 || h == 2 {
                y.solid.push([3, h, z]);
            }
        }
    }
    let door = Door {
        by_hand: true,
        open: false,
    };
    y.doors.insert([3, 0, 0], door);
    y.doors.insert([3, 1, 0], door);
    y
}

fn bash(difficulty: i32) -> Vec<MobEvent> {
    let world = walled();
    let players = [Target::at(100, 100, [8.5, 0.0, 0.5])];
    let mut e = mob(Species::Zombie, [0.5, 0.0, 0.5]);
    m(&mut e).breaks_doors = true;
    m(&mut e).target = Some(100);
    let mut out = Vec::new();
    for t in 0..400 {
        tick(&mut e, &senses(&world, &players, t, difficulty), &mut out);
        if std::env::var("TRACE").is_ok() {
            let pos = e.pos;
            let n = &m(&mut e).nav;
            eprintln!("{t} {pos:?} {:?} {:?}", n.path, n.index);
        }
    }
    out
}

#[test]
fn a_zombie_breaks_a_wooden_door_on_hard() {
    let out = bash(3);
    let broke = out
        .iter()
        .position(|e| *e == MobEvent::BreakDoor { cell: [3, 1, 0] })
        .expect("the door broke");
    let cracks: Vec<i32> = out[..broke]
        .iter()
        .filter_map(|e| match e {
            MobEvent::DoorCrack { cell, stage } if *cell == [3, 1, 0] => Some(*stage),
            _ => None,
        })
        .collect();
    assert_eq!(cracks, (0..=10).collect::<Vec<_>>());
    assert!(out.iter().any(|e| matches!(e, MobEvent::DoorBash { .. })));
}

#[test]
fn doors_hold_below_hard() {
    let out = bash(2);
    assert!(!out.iter().any(|e| matches!(e, MobEvent::BreakDoor { .. } | MobEvent::DoorCrack { .. })));
}

#[test]
fn only_door_breakers_walk_to_doors() {
    let world = walled();
    let mut p = crate::nav::PathParams::walker(2);
    let around = crate::nav::find_path(&world, [0, 0, 0], [8, 0, 0], &p).unwrap_or_default();
    assert!(!around.contains(&[3, 0, 0]), "{around:?}");
    p.doors = true;
    let path = crate::nav::find_path(&world, [0, 0, 0], [8, 0, 0], &p).unwrap();
    assert!(path.contains(&[3, 0, 0]), "{path:?}");
}

/// A player at `pos` looking at the point `at`.
fn looking(pos: [f64; 3], at: [f64; 3]) -> Target {
    let mut p = Target::at(100, 100, pos);
    let d = [at[0] - pos[0], at[1] - pos[1] - PLAYER_EYE, at[2] - pos[2]];
    let flat = d[0].hypot(d[2]);
    p.yaw = (-d[0]).atan2(d[2]).to_degrees() as f32;
    p.pitch = (-d[1]).atan2(flat).to_degrees() as f32;
    p
}

#[test]
fn an_enderman_turns_on_a_player_who_stares_and_freezes() {
    let world = Yard::default();
    let at = [0.5, 0.0, 0.5];
    let mut e = mob(Species::Enderman, at);
    let eye = [at[0], at[1] + m(&mut e).eye_height(), at[2]];
    let players = [looking([10.5, 0.0, 0.5], eye)];
    let mut out = Vec::new();
    let mut turned = None;
    for t in 0..200 {
        tick(&mut e, &senses(&world, &players, t, 2), &mut out);
        if m(&mut e).target == Some(100) {
            turned = Some(t);
            break;
        }
    }
    assert!(turned.is_some(), "never turned");
    assert!(m(&mut e).stared);
    // Stared at, it stands still (it does not teleport this far off).
    let before = e.pos;
    for t in 0..20 {
        let mut p = players.clone();
        let eye = [e.pos[0], e.pos[1] + m(&mut e).eye_height(), e.pos[2]];
        p[0] = looking(p[0].pos, eye);
        tick(&mut e, &senses(&world, &p, 200 + t, 2), &mut out);
    }
    assert!((e.pos[0] - before[0]).abs() < 0.2, "{:?}", e.pos);
    // Looking away does nothing.
    let mut e2 = mob(Species::Enderman, at);
    let away = [looking([10.5, 0.0, 0.5], [20.0, 1.6, 0.5])];
    for t in 0..200 {
        tick(&mut e2, &senses(&world, &away, t, 2), &mut out);
    }
    assert_eq!(m(&mut e2).target, None);
}

#[test]
fn an_enderman_takes_a_block_it_sees() {
    let mut world = Yard::default();
    world.solid.push([1, 0, 0]);
    let mut e = mob(Species::Enderman, [0.5, 0.0, 0.5]);
    let mut out = Vec::new();
    for t in 0..2000 {
        tick(&mut e, &senses(&world, &[], t, 2), &mut out);
        if out.iter().any(|e| matches!(e, MobEvent::TakeBlock { .. })) {
            break;
        }
        e.pos = [0.5, 0.0, 0.5];
    }
    assert!(out.contains(&MobEvent::TakeBlock { cell: [1, 0, 0] }), "never took it");
}

#[test]
fn a_fox_sleeps_under_cover_by_day() {
    let mut open = Yard::default();
    let mut fox = mob(Species::Fox, [0.5, 0.0, 0.5]);
    let mut out = Vec::new();
    for t in 0..600 {
        tick(&mut fox, &senses(&open, &[], t, 2), &mut out);
        assert!(!m(&mut fox).sitting, "slept in the open");
        fox.pos = [0.5, 0.0, 0.5];
    }
    open.roofed = true;
    let mut slept = false;
    for t in 0..2000 {
        tick(&mut fox, &senses(&open, &[], t, 2), &mut out);
        slept |= m(&mut fox).sitting;
    }
    assert!(slept);
    // A player close by wakes it.
    let near = [Target::at(100, 100, [3.5, 0.0, 0.5])];
    tick(&mut fox, &senses(&open, &near, 3000, 2), &mut out);
    tick(&mut fox, &senses(&open, &near, 3001, 2), &mut out);
    assert!(!m(&mut fox).sitting);
}

#[test]
fn a_tame_cat_sits_on_a_chest() {
    let mut world = Yard::default();
    world.perches.insert([4, 0, 0], Perch::Chest);
    let mut cat = mob(Species::Cat, [0.5, 0.0, 0.5]);
    m(&mut cat).owner = Some(100);
    let mut out = Vec::new();
    let mut sat = false;
    for t in 0..2000 {
        tick(&mut cat, &senses(&world, &[], t, 2), &mut out);
        if m(&mut cat).perched {
            sat = true;
            break;
        }
    }
    assert!(sat, "never sat, at {:?}", cat.pos);
    assert!((cat.pos[0] - 4.5).abs() < 1.0 && cat.pos[1] > 0.5, "{:?}", cat.pos);
}

#[test]
fn a_lead_pulls_and_leads() {
    let world = Yard::default();
    let mut cow = mob(Species::Cow, [0.5, 0.0, 0.5]);
    m(&mut cow).leash = Some([9.5, 0.0, 0.5]);
    let mut out = Vec::new();
    tick(&mut cow, &senses(&world, &[], 0, 2), &mut out);
    assert!(cow.vel[0] > 0.1, "pulled: {:?}", cow.vel);
    for t in 1..200 {
        tick(&mut cow, &senses(&world, &[], t, 2), &mut out);
    }
    let d = 9.5 - cow.pos[0];
    assert!((1.0..3.5).contains(&d), "{:?}", cow.pos);
}

#[test]
fn a_hostile_mob_wakes_a_sleeping_fox() {
    let world = Yard {
        roofed: true,
        ..Yard::default()
    };
    let mut fox = mob(Species::Fox, [0.5, 0.0, 0.5]);
    let mut out = Vec::new();
    for t in 0..2000 {
        tick(&mut fox, &senses(&world, &[], t, 2), &mut out);
        if m(&mut fox).sitting {
            break;
        }
    }
    assert!(m(&mut fox).sitting, "never slept");
    let zombie = [Other {
        id: 9,
        species: Some(Species::Zombie),
        pos: [6.5, 0.0, 0.5],
        width: 0.6,
        height: 1.95,
        baby: false,
        in_love: false,
        owner: None,
        alive: true,
        leashed: false,
        in_caravan: false,
        has_tail: false,
        chain_held: false,
    }];
    let s = Senses {
        mobs: &zombie,
        ..senses(&world, &[], 3000, 2)
    };
    tick(&mut fox, &s, &mut out);
    tick(&mut fox, &s, &mut out);
    assert!(!m(&mut fox).sitting);
}

fn llama(id: i32, pos: [f64; 3]) -> Entity {
    let mut e = mob(Species::Llama, pos);
    e.id = id;
    e
}

fn other_of(e: &Entity) -> Other {
    let Kind::Mob(m) = &e.kind else { panic!() };
    Other {
        id: e.id,
        species: Some(m.species),
        pos: e.pos,
        width: 0.9,
        height: 1.87,
        baby: false,
        in_love: false,
        owner: None,
        alive: true,
        leashed: m.leash.is_some(),
        in_caravan: m.caravan_head.is_some(),
        has_tail: m.caravan_tail,
        chain_held: false,
    }
}

#[test]
fn llamas_fall_in_behind_a_leashed_one() {
    let world = Yard::default();
    let mut head = llama(1, [0.5, 0.0, 0.5]);
    m(&mut head).leash = Some([0.5, 0.0, 0.5]);
    let mut tail = llama(2, [7.5, 0.0, 0.5]);
    let mut out = Vec::new();
    for t in 0..400 {
        let mut o = other_of(&head);
        o.chain_held = true;
        let others = [o];
        let s = Senses {
            mobs: &others,
            ..senses(&world, &[], t, 2)
        };
        tick(&mut tail, &s, &mut out);
        // The head stays on its lead.
        m(&mut head).leash = Some([0.5, 0.0, 0.5]);
    }
    assert!(out.contains(&MobEvent::CaravanJoin { head: 1 }), "never joined");
    assert!(tail.pos[0] < 3.5, "stayed at {:?}", tail.pos);
    // Out of range, it lets go.
    let far = Other {
        pos: [60.0, 0.0, 0.5],
        ..other_of(&head)
    };
    let others = [Other {
        chain_held: true,
        ..far
    }];
    let s = Senses {
        mobs: &others,
        ..senses(&world, &[], 500, 2)
    };
    tick(&mut tail, &s, &mut out);
    tick(&mut tail, &s, &mut out);
    assert!(out.contains(&MobEvent::CaravanLeave { head: 1 }));
}
