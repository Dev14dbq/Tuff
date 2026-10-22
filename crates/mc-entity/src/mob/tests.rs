use super::*;
use crate::nav::Liquid;
use crate::physics::Collide;

struct Flat;
impl Collide for Flat {
    fn solid(&self, _: i32, y: i32, _: i32) -> bool {
        y < 0
    }
}
impl Terrain for Flat {
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
}

/// Item 1 is wheat (cow food), 2 a bow.
struct Wheat;
impl Items for Wheat {
    fn tagged(&self, item: i32, tag: &str) -> bool {
        item == 1 && tag == "minecraft:cow_food"
    }
    fn name(&self, item: i32) -> &str {
        match item {
            1 => "minecraft:wheat",
            2 => "minecraft:bow",
            _ => "",
        }
    }
}

fn mob(species: Species, pos: [f64; 3]) -> Entity {
    Entity {
        id: 1,
        uuid: 1,
        kind: Kind::Mob(Box::new(Mob::new(species, 0x1234_5678_9abc))),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

fn senses<'a>(players: &'a [Target], mobs: &'a [Other], tick: u64, sunny: bool) -> Senses<'a> {
    Senses {
        world: &Flat,
        players,
        mobs,
        items: &Wheat,
        tick,
        difficulty: 2,
        sunny,
        raining: false,
        nether: false,
    }
}

fn run(e: &mut Entity, players: &[Target], ticks: u64, sunny: bool) -> Vec<MobEvent> {
    let mut out = Vec::new();
    for t in 0..ticks {
        tick(e, &senses(players, &[], t, sunny), &mut out);
    }
    out
}

fn player(pos: [f64; 3]) -> Target {
    Target::at(100, 100, pos)
}

fn m(e: &mut Entity) -> &mut Mob {
    match &mut e.kind {
        Kind::Mob(m) => m,
        _ => unreachable!(),
    }
}

#[test]
fn mobs_land_and_stay_on_the_ground() {
    let mut e = mob(Species::Cow, [0.5, 5.0, 0.5]);
    run(&mut e, &[], 40, false);
    assert!(e.on_ground);
    assert!((e.pos[1]).abs() < 1e-6, "{:?}", e.pos);
}

#[test]
fn a_zombie_walks_to_a_player_and_bites() {
    let mut e = mob(Species::Zombie, [0.5, 0.0, 0.5]);
    let p = player([8.5, 0.0, 0.5]);
    let out = run(&mut e, std::slice::from_ref(&p), 200, false);
    assert!(e.pos[0] > 5.0, "{:?}", e.pos);
    assert!(
        out.iter()
            .any(|m| matches!(m, MobEvent::Attack { target: 100, .. })),
        "{out:?}"
    );
}

#[test]
fn zombies_burn_in_the_sun_and_creeper_fuse_blows() {
    let mut z = mob(Species::Zombie, [0.5, 0.0, 0.5]);
    let out = run(&mut z, &[], 100, true);
    assert!(out.iter().any(|m| matches!(
        m,
        MobEvent::Hurt {
            kind: "minecraft:on_fire",
            ..
        }
    )));
    let mut c = mob(Species::Creeper, [0.5, 0.0, 0.5]);
    let p = player([2.5, 0.0, 0.5]);
    let out = run(&mut c, &[p], 120, false);
    assert!(
        out.iter()
            .any(|m| matches!(m, MobEvent::Explode { power } if *power == 3.0)),
        "{out:?}"
    );
}

#[test]
fn a_helmet_keeps_the_sun_off() {
    let mut z = mob(Species::Zombie, [0.5, 0.0, 0.5]);
    m(&mut z).gear[slot::HEAD] = Some(Gear {
        item: 9,
        count: 1,
        damage: 0,
    });
    let out = run(&mut z, &[], 100, true);
    assert!(!out.iter().any(|m| matches!(m, MobEvent::Hurt { .. })));
}

#[test]
fn a_hurt_animal_panics_and_runs() {
    let mut e = mob(Species::Cow, [0.5, 0.0, 0.5]);
    run(&mut e, &[], 5, false);
    assert!(m(&mut e).hurt(2.0, Some(100)));
    assert!(!m(&mut e).hurt(1.0, Some(100)));
    let start = e.pos;
    run(&mut e, &[player([0.5, 0.0, -3.5])], 60, false);
    let moved = (e.pos[0] - start[0]).hypot(e.pos[2] - start[2]);
    assert!(moved > 2.0, "{moved}");
}

#[test]
fn hurt_has_an_invulnerability_window() {
    let mut m = Mob::new(Species::Sheep, 7);
    assert!(m.hurt(4.0, None));
    assert!(!m.hurt(3.0, None));
    assert!(m.hurt(6.0, None));
    assert_eq!(m.health, 2.0);
}

#[test]
fn a_cow_follows_wheat() {
    let mut e = mob(Species::Cow, [0.5, 0.0, 0.5]);
    let mut p = player([7.5, 0.0, 0.5]);
    p.held = [1, -1];
    run(&mut e, std::slice::from_ref(&p), 200, false);
    assert!(e.pos[0] > 4.0, "{:?}", e.pos);
}

#[test]
fn two_cows_in_love_breed() {
    let mut a = mob(Species::Cow, [0.5, 0.0, 0.5]);
    let mut b = mob(Species::Cow, [3.5, 0.0, 0.5]);
    b.id = 2;
    m(&mut a).fall_in_love(None);
    m(&mut b).fall_in_love(None);
    let mut bred = false;
    for t in 0..200 {
        let others: Vec<Other> = [&a, &b]
            .iter()
            .map(|e| {
                let Kind::Mob(x) = &e.kind else {
                    unreachable!()
                };
                Other {
                    id: e.id,
                    species: Some(x.species),
                    pos: e.pos,
                    width: 0.9,
                    height: 1.4,
                    baby: x.is_baby(),
                    in_love: x.in_love(),
                    owner: None,
                    alive: true,
                }
            })
            .collect();
        let mut out = Vec::new();
        tick(&mut a, &senses(&[], &others, t, false), &mut out);
        tick(&mut b, &senses(&[], &others, t, false), &mut out);
        if out
            .iter()
            .any(|e| matches!(e, MobEvent::Breed { partner: 1 | 2 }))
        {
            bred = true;
            break;
        }
    }
    assert!(bred);
}

#[test]
fn babies_grow_up() {
    let mut e = mob(Species::Pig, [0.5, 0.0, 0.5]);
    m(&mut e).set_baby(true);
    assert_eq!(m(&mut e).size(), (0.45, 0.45));
    m(&mut e).baby_age = -2;
    run(&mut e, &[], 3, false);
    assert!(!m(&mut e).is_baby());
}

#[test]
fn a_skeleton_with_a_bow_shoots() {
    let mut e = mob(Species::Skeleton, [0.5, 0.0, 0.5]);
    m(&mut e).gear[slot::MAINHAND] = Some(Gear {
        item: 2,
        count: 1,
        damage: 0,
    });
    let p = player([10.5, 0.0, 0.5]);
    let out = run(&mut e, &[p], 200, false);
    assert!(
        out.iter().any(|e| matches!(
            e,
            MobEvent::Shoot {
                missile: Missile::Arrow { .. },
                ..
            }
        )),
        "{out:?}"
    );
}

#[test]
fn slimes_hop() {
    let mut e = mob(Species::Slime, [0.5, 0.0, 0.5]);
    m(&mut e).set_size(2);
    let start = e.pos;
    run(&mut e, &[], 200, false);
    let moved = (e.pos[0] - start[0]).hypot(e.pos[2] - start[2]);
    assert!(moved > 0.5, "{moved}");
}

#[test]
fn higher_priority_goals_preempt_lower_ones() {
    let mut m = Mob::new(Species::Cow, 99);
    let mut out = Vec::new();
    let mut b = Body {
        id: 1,
        pos: [0.5, 0.0, 0.5],
        vel: [0.0; 3],
        on_ground: true,
    };
    let s = senses(&[], &[], 0, false);
    let mut list = vec![
        Slot {
            priority: 6,
            flags: MOVE,
            goal: Goal::Stroll {
                speed: 1.0,
                interval: 1,
                avoid_water: false,
            },
            running: false,
        },
        Slot {
            priority: 1,
            flags: MOVE,
            goal: Goal::Panic { speed: 2.0 },
            running: false,
        },
    ];
    goals::run(&mut list, &mut m, &mut b, &s, &mut out);
    assert!(list[0].running && !list[1].running);
    m.revenge = Some((-1, 100));
    goals::run(&mut list, &mut m, &mut b, &s, &mut out);
    assert!(list[1].running && !list[0].running);
}

#[test]
fn species_round_trip() {
    for &s in Species::ALL {
        assert_eq!(Species::from_id(s.id()), Some(s));
    }
}

#[test]
fn sounds_are_named_like_vanilla() {
    assert_eq!(
        Species::Cow.sound(Sound::Hurt, false).as_deref(),
        Some("entity.cow.hurt")
    );
    assert_eq!(
        Species::Pig.sound(Sound::Ambient, true).as_deref(),
        Some("entity.baby_pig.ambient")
    );
    assert_eq!(Species::Creeper.sound(Sound::Ambient, false), None);
    let mut s = Mob::new(Species::Slime, 1);
    s.set_size(1);
    assert_eq!(
        s.sound_name(Sound::Hurt).as_deref(),
        Some("entity.slime.hurt_small")
    );
}
