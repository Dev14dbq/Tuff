//! The aquatic mobs (Claude): fish, squids, dolphins, turtles, axolotls,
//! frogs, tadpoles, guardians and nautiluses.

use super::*;
use crate::nav::{Feature, Liquid};
use crate::physics::Collide;

/// A pool: solid below y = 0, water from 0 up to `depth` within 30 blocks
/// of the middle, air elsewhere; sand floors when `sand`.
#[derive(Default)]
struct Pool {
    depth: i32,
    sand: bool,
    raining: bool,
}

impl Pool {
    fn deep() -> Self {
        Self {
            depth: 12,
            ..Self::default()
        }
    }
}

impl Collide for Pool {
    fn solid(&self, _: i32, y: i32, _: i32) -> bool {
        y < 0
    }
}

impl Terrain for Pool {
    fn liquid(&self, x: i32, y: i32, z: i32) -> Liquid {
        if (0..self.depth).contains(&y) && x.abs() <= 30 && z.abs() <= 30 {
            Liquid::Water
        } else {
            Liquid::None
        }
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
    fn feature(&self, _: i32, y: i32, _: i32, kind: Feature) -> bool {
        kind == Feature::Sand && self.sand && y < 0
    }
}

fn senses<'a>(world: &'a Pool, players: &'a [Target], mobs: &'a [Other], tick: u64) -> Senses<'a> {
    Senses {
        world,
        players,
        mobs,
        items: &NoItems,
        tick,
        difficulty: 2,
        sunny: false,
        raining: world.raining,
        nether: false,
        day: false,
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
        on_ground: false,
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

fn run(
    e: &mut Entity,
    world: &Pool,
    players: &[Target],
    mobs: &[Other],
    ticks: u64,
) -> Vec<MobEvent> {
    let mut out = Vec::new();
    for t in 0..ticks {
        tick(e, &senses(world, players, mobs, t), &mut out);
    }
    out
}

fn other(id: i32, species: Species, pos: [f64; 3]) -> Other {
    let (width, height) = species.size();
    Other {
        id,
        species: Some(species),
        pos,
        width,
        height,
        baby: false,
        in_love: false,
        owner: None,
        alive: true,
        leashed: false,
        in_caravan: false,
        has_tail: false,
        chain_held: false,
    }
}

fn wet(world: &Pool, pos: [f64; 3]) -> bool {
    world.liquid(
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ) == Liquid::Water
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    dist_sq(a, b).sqrt()
}

#[test]
fn the_table_has_the_numbers_of_the_wiki() {
    use Species::*;
    for (s, health, w, h) in [
        (Cod, 3.0, 0.5, 0.3),
        (Salmon, 3.0, 0.7, 0.4),
        (TropicalFish, 3.0, 0.5, 0.4),
        (Squid, 10.0, 0.8, 0.8),
        (GlowSquid, 10.0, 0.8, 0.8),
        (Dolphin, 10.0, 0.9, 0.6),
        (Turtle, 30.0, 1.2, 0.4),
        (Axolotl, 14.0, 0.75, 0.42),
        (Frog, 10.0, 0.5, 0.5),
        (Tadpole, 6.0, 0.4, 0.3),
        (Guardian, 30.0, 0.85, 0.85),
        (ElderGuardian, 80.0, 1.9975, 1.9975),
        (Nautilus, 15.0, 0.875, 0.95),
        (ZombieNautilus, 15.0, 0.875, 0.95),
        (Pufferfish, 3.0, 0.35, 0.35),
    ] {
        assert_eq!(s.max_health(), health, "{s:?}");
        assert_eq!(s.size(), (w, h), "{s:?}");
    }
    assert!(ZombieNautilus.undead());
    assert_eq!(Squid.category(), Category::WaterCreature);
    assert_eq!(Cod.category(), Category::WaterAmbient);
    assert_eq!(GlowSquid.category(), Category::UndergroundWater);
    assert_eq!(Axolotl.category(), Category::Axolotls);
    assert_eq!(Guardian.category(), Category::Monster);
    assert_eq!(Species::from_id("minecraft:glow_squid"), Some(GlowSquid));
}

#[test]
fn a_pufferfish_swells_in_three_boxes() {
    let mut e = mob(Species::Pufferfish, [0.5, 5.0, 0.5]);
    assert_eq!(e.size(), (0.35, 0.35));
    m(&mut e).quirks.aqua.puff = 1;
    assert_eq!(e.size(), (0.49, 0.49));
    m(&mut e).quirks.aqua.puff = 2;
    assert_eq!(e.size(), (0.7, 0.7));
}

#[test]
fn a_salmon_comes_in_three_sizes() {
    let mut seen = [0; 3];
    for seed in 0..600_u64 {
        let s = Mob::new(Species::Salmon, seed * 7919 + 3);
        seen[s.variant as usize] += 1;
    }
    // 31.6, 52.6 and 15.8 per cent.
    assert!(seen[0] > 120 && seen[0] < 260, "{seen:?}");
    assert!(seen[1] > 250 && seen[1] < 380, "{seen:?}");
    assert!(seen[2] > 50 && seen[2] < 150, "{seen:?}");
    let mut e = mob(Species::Salmon, [0.5, 5.0, 0.5]);
    m(&mut e).variant = 0;
    assert_eq!(e.size(), (0.35, 0.2));
    m(&mut e).variant = 2;
    assert_eq!(e.size(), (1.05, 0.6));
}

#[test]
fn tropical_fish_are_mostly_the_common_kinds() {
    let mut common = 0;
    for seed in 0..500_u64 {
        let f = Mob::new(Species::TropicalFish, seed * 104_729 + 5);
        if aqua::TROPICAL_KINDS.contains(&f.variant) {
            common += 1;
        }
    }
    assert!(common > 400, "{common}");
}

#[test]
fn a_cod_swims_about_and_stays_in_the_water() {
    let world = Pool::deep();
    let mut e = mob(Species::Cod, [0.5, 5.0, 0.5]);
    let start = e.pos;
    let mut far = 0.0_f64;
    for t in 0..1500 {
        tick(&mut e, &senses(&world, &[], &[], t), &mut Vec::new());
        assert!(wet(&world, e.pos), "left the water at {t}: {:?}", e.pos);
        far = far.max(dist(start, e.pos));
    }
    assert!(far > 3.0, "{far}");
    assert!(!m(&mut e).is_dead());
}

#[test]
fn fish_keep_away_from_a_player() {
    let world = Pool::deep();
    let mut e = mob(Species::Cod, [0.5, 5.0, 0.5]);
    let player = Target::at(100, 100, [4.5, 5.0, 0.5]);
    run(&mut e, &world, std::slice::from_ref(&player), &[], 60);
    assert!(dist(e.pos, player.pos) > 6.0, "{:?}", e.pos);
}

#[test]
fn a_hurt_fish_bolts() {
    let world = Pool::deep();
    let mut e = mob(Species::Salmon, [0.5, 5.0, 0.5]);
    m(&mut e).hurt(1.0, Some(7));
    let from = e.pos;
    run(&mut e, &world, &[], &[], 20);
    assert!(dist(from, e.pos) > 1.0, "{:?}", e.pos);
}

#[test]
fn fish_follow_the_leader_of_the_school() {
    let world = Pool::deep();
    let mut e = mob(Species::Cod, [10.5, 5.0, 0.5]);
    m(&mut e)
        .brain
        .goals
        .retain(|s| matches!(s.goal, Goal::Aqua(aqua::Act::School { .. })));
    // Another cod with a lower id swims over there.
    e.id = 5;
    let leader = {
        let mut o = other(2, Species::Cod, [0.5, 5.0, 0.5]);
        o.baby = false;
        o
    };
    run(&mut e, &world, &[], std::slice::from_ref(&leader), 120);
    assert!(dist(e.pos, leader.pos) < 5.0, "{:?}", e.pos);
}

#[test]
fn a_stranded_fish_flops_and_then_chokes() {
    let world = Pool::default();
    let mut e = mob(Species::Cod, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    let out = run(&mut e, &world, &[], &[], 100);
    assert!(
        out.iter().any(|ev| matches!(ev,
            MobEvent::Sound { name, .. } if name == "entity.cod.flop")),
        "{out:?}"
    );
    let out = run(&mut e, &world, &[], &[], 400);
    assert!(
        out.iter().any(|ev| matches!(ev,
            MobEvent::Hurt { kind: "minecraft:drown", amount } if *amount == 2.0)),
        "{out:?}"
    );
}

#[test]
fn a_pufferfish_swells_near_a_player_and_eases_off() {
    let world = Pool::deep();
    let mut e = mob(Species::Pufferfish, [0.5, 5.0, 0.5]);
    m(&mut e).brain.goals.clear();
    let near = Target::at(100, 100, [1.5, 5.0, 0.5]);
    run(&mut e, &world, std::slice::from_ref(&near), &[], 5);
    assert_eq!(m(&mut e).quirks.aqua.puff, 1);
    run(&mut e, &world, &[near], &[], 60);
    assert_eq!(m(&mut e).quirks.aqua.puff, 2);
    // Alone again: a little over a second to the middle size, five
    // seconds more to none.
    run(&mut e, &world, &[], &[], 70);
    assert_eq!(m(&mut e).quirks.aqua.puff, 1);
    run(&mut e, &world, &[], &[], 110);
    assert_eq!(m(&mut e).quirks.aqua.puff, 0);
}

#[test]
fn a_swollen_pufferfish_stings_and_poisons() {
    let world = Pool::deep();
    let mut e = mob(Species::Pufferfish, [0.5, 5.0, 0.5]);
    m(&mut e).brain.goals.clear();
    let touching = Target::at(100, 100, [0.7, 5.0, 0.5]);
    let out = run(&mut e, &world, std::slice::from_ref(&touching), &[], 70);
    let stings: Vec<_> = out
        .iter()
        .filter_map(|ev| match ev {
            MobEvent::Sting {
                target: 100,
                damage,
                poison,
            } => Some((*damage, *poison)),
            _ => None,
        })
        .collect();
    assert!(stings.contains(&(2.0, 60)), "{stings:?}");
    assert!(stings.contains(&(3.0, 120)), "{stings:?}");
}

#[test]
fn a_pufferfish_ignores_the_fish_that_do_not_frighten_it() {
    let world = Pool::deep();
    let mut e = mob(Species::Pufferfish, [0.5, 5.0, 0.5]);
    m(&mut e).brain.goals.clear();
    let cod = other(9, Species::Cod, [1.0, 5.0, 0.5]);
    run(&mut e, &world, &[], &[cod], 30);
    assert_eq!(m(&mut e).quirks.aqua.puff, 0);
    let zombie = other(10, Species::Zombie, [1.0, 5.0, 0.5]);
    run(&mut e, &world, &[], &[zombie], 5);
    assert_eq!(m(&mut e).quirks.aqua.puff, 1);
}

#[test]
fn a_hurt_squid_squirts_ink_and_bolts() {
    let world = Pool::deep();
    let mut e = mob(Species::Squid, [0.5, 5.0, 0.5]);
    let attacker = Target::at(100, 100, [2.5, 5.0, 0.5]);
    m(&mut e).hurt(1.0, Some(100));
    let out = run(&mut e, &world, std::slice::from_ref(&attacker), &[], 20);
    assert!(out.contains(&MobEvent::Ink), "{out:?}");
    // Away from the one who hit it.
    assert!(
        dist(attacker.pos, e.pos) > 4.0 && e.pos[0] < 0.5,
        "{:?}",
        e.pos
    );
}

#[test]
fn a_squid_drifts_in_all_three_dimensions() {
    let world = Pool::deep();
    let mut e = mob(Species::Squid, [0.5, 5.0, 0.5]);
    let (mut low, mut high) = (e.pos[1], e.pos[1]);
    let start = e.pos;
    let mut far = 0.0_f64;
    for t in 0..3000 {
        tick(&mut e, &senses(&world, &[], &[], t), &mut Vec::new());
        low = low.min(e.pos[1]);
        high = high.max(e.pos[1]);
        far = far.max(dist(start, e.pos));
    }
    assert!(far > 5.0, "{far}");
    assert!(high - low > 0.5, "{low} {high}");
    assert!(wet(&world, e.pos), "{:?}", e.pos);
}

#[test]
fn a_hurt_glow_squid_goes_dark() {
    let world = Pool::deep();
    let mut e = mob(Species::GlowSquid, [0.5, 5.0, 0.5]);
    m(&mut e).hurt(1.0, Some(100));
    run(&mut e, &world, &[], &[], 1);
    assert!(m(&mut e).quirks.aqua.dark > 90);
    run(&mut e, &world, &[], &[], 120);
    assert_eq!(m(&mut e).quirks.aqua.dark, 0);
}

#[test]
fn a_stranded_squid_chokes_but_does_not_flop() {
    let world = Pool::default();
    let mut e = mob(Species::Squid, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    let out = run(&mut e, &world, &[], &[], 330);
    assert!(
        out.iter().any(|ev| matches!(
            ev,
            MobEvent::Hurt {
                kind: "minecraft:drown",
                ..
            }
        )),
        "{out:?}"
    );
    assert!(e.pos[1] < 0.1);
}

#[test]
fn a_dolphin_gives_grace_to_a_swimmer() {
    let world = Pool::deep();
    let mut e = mob(Species::Dolphin, [0.5, 5.0, 0.5]);
    let mut swimmer = Target::at(100, 100, [6.5, 5.0, 0.5]);
    swimmer.swimming = true;
    let out = run(&mut e, &world, std::slice::from_ref(&swimmer), &[], 120);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Grace { player: 100 })),
        "{out:?}"
    );
    assert!(dist(e.pos, swimmer.pos) < 5.0, "{:?}", e.pos);
}

#[test]
fn a_dolphin_follows_a_boat_and_gives_no_grace() {
    let world = Pool::deep();
    let mut e = mob(Species::Dolphin, [0.5, 5.0, 0.5]);
    let mut rower = Target::at(100, 100, [10.5, 11.5, 0.5]);
    rower.boating = true;
    let out = run(&mut e, &world, std::slice::from_ref(&rower), &[], 160);
    assert!(
        !out.iter().any(|ev| matches!(ev, MobEvent::Grace { .. })),
        "{out:?}"
    );
    assert!(dist(e.pos, rower.pos) < 7.0, "{:?}", e.pos);
}

#[test]
fn a_dolphin_leads_the_way_to_a_treasure() {
    let world = Pool::deep();
    let mut e = mob(Species::Dolphin, [0.5, 5.0, 0.5]);
    m(&mut e).quirks.aqua.got_fish = true;
    m(&mut e).quirks.aqua.treasure = Some([20, 5, 0]);
    let mut arrived = None;
    for t in 0..400 {
        tick(&mut e, &senses(&world, &[], &[], t), &mut Vec::new());
        if m(&mut e).quirks.aqua.treasure.is_none() {
            arrived = Some(t);
            break;
        }
    }
    let t = arrived.expect("got there");
    assert!(dist(e.pos, [20.5, 5.5, 0.5]) < 5.0, "{:?}", e.pos);
    assert!(t > 20, "{t}");
    assert!(!m(&mut e).quirks.aqua.got_fish);
}

#[test]
fn a_dolphin_dries_out_in_a_few_minutes() {
    let world = Pool::default();
    let mut e = mob(Species::Dolphin, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    let out = run(&mut e, &world, &[], &[], 2300);
    assert!(!out.iter().any(|ev| matches!(
        ev,
        MobEvent::Hurt {
            kind: "minecraft:dry_out",
            ..
        }
    )));
    let out = run(&mut e, &world, &[], &[], 200);
    assert!(
        out.iter().any(|ev| matches!(
            ev,
            MobEvent::Hurt {
                kind: "minecraft:dry_out",
                ..
            }
        )),
        "{out:?}"
    );
    // Rain keeps it moist.
    let mut e = mob(Species::Dolphin, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    let rain = Pool {
        raining: true,
        ..Pool::default()
    };
    let out = run(&mut e, &rain, &[], &[], 3000);
    assert!(!out.iter().any(|ev| matches!(
        ev,
        MobEvent::Hurt {
            kind: "minecraft:dry_out",
            ..
        }
    )));
}

#[test]
fn a_dolphin_goes_up_for_air() {
    let world = Pool::deep();
    let mut e = mob(Species::Dolphin, [0.5, 1.0, 0.5]);
    m(&mut e).quirks.aqua.breath = 250;
    run(&mut e, &world, &[], &[], 120);
    assert!(e.pos[1] > 8.0, "{:?}", e.pos);
}

#[test]
fn a_turtle_with_an_egg_lays_it_on_the_sand_at_home() {
    let world = Pool {
        sand: true,
        ..Pool::default()
    };
    let mut e = mob(Species::Turtle, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    m(&mut e).quirks.aqua.home = Some([0, 0, 0]);
    m(&mut e).quirks.aqua.has_egg = true;
    let out = run(&mut e, &world, &[], &[], 400);
    let laid: Vec<_> = out
        .iter()
        .filter_map(|ev| match ev {
            MobEvent::LayEgg { cell, count } => Some((*cell, *count)),
            _ => None,
        })
        .collect();
    assert_eq!(laid.len(), 1, "{out:?}");
    assert!((1..=4).contains(&laid[0].1));
    assert!(!m(&mut e).quirks.aqua.has_egg);
}

#[test]
fn a_turtle_with_an_egg_walks_home() {
    let world = Pool {
        sand: true,
        ..Pool::default()
    };
    let mut e = mob(Species::Turtle, [20.5, 0.0, 0.5]);
    e.on_ground = true;
    m(&mut e).quirks.aqua.home = Some([0, 0, 0]);
    m(&mut e).quirks.aqua.has_egg = true;
    let before = dist(e.pos, [0.5, 0.0, 0.5]);
    run(&mut e, &world, &[], &[], 600);
    assert!(dist(e.pos, [0.5, 0.0, 0.5]) < before - 5.0, "{:?}", e.pos);
}

#[test]
fn a_baby_turtle_that_grows_up_drops_a_scute() {
    let world = Pool::default();
    let mut e = mob(Species::Turtle, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    m(&mut e).set_baby(true);
    m(&mut e).baby_age = -10;
    let out = run(&mut e, &world, &[], &[], 20);
    assert!(
        out.contains(&MobEvent::Lay {
            item: "minecraft:turtle_scute"
        }),
        "{out:?}"
    );
}

#[test]
fn a_turtle_walks_slowly_and_swims_fast() {
    let dry = Pool::default();
    let mut e = mob(Species::Turtle, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    m(&mut e).brain.goals.clear();
    m(&mut e).goto = Some(([30.5, 0.0, 0.5], 1.0));
    let mut out = Vec::new();
    for t in 0..40 {
        m(&mut e).goto = Some(([30.5, 0.0, 0.5], 1.0));
        tick(&mut e, &senses(&dry, &[], &[], t), &mut out);
    }
    let walked = e.pos[0] - 0.5;
    let pool = Pool::deep();
    let mut f = mob(Species::Turtle, [0.5, 5.0, 0.5]);
    m(&mut f).brain.goals.clear();
    for t in 0..40 {
        m(&mut f).goto = Some(([30.5, 5.0, 0.5], 1.0));
        tick(&mut f, &senses(&pool, &[], &[], t), &mut out);
    }
    let swam = f.pos[0] - 0.5;
    assert!(walked > 0.5 && swam > walked * 1.3, "{walked} {swam}");
}

#[test]
fn an_axolotl_plays_dead_when_it_is_hurt_in_the_water() {
    let world = Pool::deep();
    let mut e = mob(Species::Axolotl, [0.5, 5.0, 0.5]);
    run(&mut e, &world, &[], &[], 2);
    // Below half health it always does.
    m(&mut e).health = 6.0;
    m(&mut e).hurt(1.0, Some(100));
    assert_eq!(m(&mut e).quirks.aqua.dead_play, aqua::PLAY_DEAD);
    let still = e.pos;
    run(&mut e, &world, &[], &[], 100);
    assert!(dist(still, e.pos) < 0.5);
    run(&mut e, &world, &[], &[], 110);
    assert_eq!(m(&mut e).quirks.aqua.dead_play, 0);
}

#[test]
fn an_axolotl_out_of_the_water_does_not_play_dead() {
    let world = Pool::default();
    let mut e = mob(Species::Axolotl, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    run(&mut e, &world, &[], &[], 2);
    m(&mut e).health = 4.0;
    m(&mut e).hurt(1.0, Some(100));
    assert_eq!(m(&mut e).quirks.aqua.dead_play, 0);
}

#[test]
fn an_axolotl_hunts_fish_and_then_rests() {
    let world = Pool::deep();
    let mut e = mob(Species::Axolotl, [0.5, 5.0, 0.5]);
    let cod = other(7, Species::Cod, [3.5, 5.0, 0.5]);
    let out = run(&mut e, &world, &[], &[cod], 150);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Attack { target: 7, damage } if *damage == 2.0)),
        "{out:?}"
    );
    // Something that means harm is hunted whatever the wait.
    m(&mut e).quirks.aqua.hunt_wait = 2000;
    m(&mut e).target = None;
    let tadpole = other(8, Species::Tadpole, [3.5, 5.0, 0.5]);
    let out = run(&mut e, &world, &[], &[tadpole], 100);
    assert!(
        !out.iter()
            .any(|ev| matches!(ev, MobEvent::Attack { target: 8, .. })),
        "{out:?}"
    );
    let drowned = other(9, Species::Drowned, [3.5, 5.0, 0.5]);
    let out = run(&mut e, &world, &[], &[drowned], 150);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Attack { target: 9, .. })),
        "{out:?}"
    );
}

#[test]
fn a_frog_eats_a_small_slime_and_leaves_a_big_one() {
    let world = Pool::default();
    let mut e = mob(Species::Frog, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    let mut big = other(6, Species::Slime, [2.5, 0.0, 0.5]);
    big.width = 1.04;
    let out = run(&mut e, &world, &[], &[big], 120);
    assert!(
        !out.iter().any(|ev| matches!(ev, MobEvent::Eat { .. })),
        "{out:?}"
    );
    let small = other(5, Species::MagmaCube, [3.5, 0.0, 0.5]);
    let out = run(&mut e, &world, &[], &[small], 160);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Eat { target: 5 })),
        "{out:?}"
    );
}

#[test]
fn a_frog_with_frogspawn_to_lay_goes_to_the_water() {
    let world = Pool {
        depth: 1,
        ..Pool::default()
    };
    let mut e = mob(Species::Frog, [0.5, 1.0, 0.5]);
    m(&mut e).quirks.aqua.pregnant = true;
    let out = run(&mut e, &world, &[], &[], 200);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::LaySpawn { cell } if cell[1] == 1)),
        "{out:?}"
    );
    assert!(!m(&mut e).quirks.aqua.pregnant);
}

#[test]
fn a_tadpole_grows_into_a_frog_after_twenty_minutes() {
    let world = Pool::deep();
    let mut e = mob(Species::Tadpole, [0.5, 5.0, 0.5]);
    m(&mut e).quirks.aqua.growth = aqua::TADPOLE_GROWTH - 5;
    let out = run(&mut e, &world, &[], &[], 10);
    assert!(out.contains(&MobEvent::Convert(Species::Frog)), "{out:?}");
}

#[test]
fn a_guardian_charges_its_beam_and_fires() {
    let world = Pool::deep();
    let mut e = mob(Species::Guardian, [0.5, 5.0, 0.5]);
    let target = Target::at(100, 100, [5.5, 5.0, 0.5]);
    let mut out = Vec::new();
    let mut aimed = false;
    let mut hit_at = None;
    for t in 0..300 {
        tick(
            &mut e,
            &senses(&world, std::slice::from_ref(&target), &[], t),
            &mut out,
        );
        aimed |= m(&mut e).quirks.aqua.beam == 100;
        if hit_at.is_none() && out.iter().any(|ev| matches!(ev, MobEvent::Attack { .. })) {
            hit_at = Some(t);
        }
    }
    assert!(aimed);
    let at = hit_at.expect("fired");
    assert!(at > 80, "fired at {at}");
    assert!(out.iter().any(|ev| matches!(ev,
        MobEvent::Attack { target: 100, damage } if *damage == 6.0)));
    assert!(out
        .iter()
        .any(|ev| matches!(ev, MobEvent::MagicAttack { target: 100, .. })));
}

/// The tick of the first beam, and its damage.
fn first_beam(species: Species) -> (u64, f32) {
    let world = Pool::deep();
    let mut e = mob(species, [0.5, 5.0, 0.5]);
    let target = Target::at(100, 100, [5.5, 5.0, 0.5]);
    for t in 0..400 {
        let mut out = Vec::new();
        tick(
            &mut e,
            &senses(&world, std::slice::from_ref(&target), &[], t),
            &mut out,
        );
        for ev in out {
            if let MobEvent::Attack { damage, .. } = ev {
                return (t, damage);
            }
        }
    }
    panic!("no beam");
}

#[test]
fn an_elder_guardian_is_quicker_and_hits_harder() {
    let (guardian, small) = first_beam(Species::Guardian);
    let (elder, big) = first_beam(Species::ElderGuardian);
    assert!(elder + 15 < guardian, "{elder} {guardian}");
    assert_eq!((small, big), (6.0, 8.0));
}

#[test]
fn a_beam_needs_a_clear_line() {
    struct Wall(Pool);
    impl Collide for Wall {
        fn solid(&self, x: i32, y: i32, _: i32) -> bool {
            y < 0 || x == 3
        }
    }
    impl Terrain for Wall {
        fn liquid(&self, x: i32, y: i32, z: i32) -> Liquid {
            self.0.liquid(x, y, z)
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
    let world = Wall(Pool::deep());
    let mut e = mob(Species::Guardian, [0.5, 5.0, 0.5]);
    m(&mut e)
        .brain
        .goals
        .retain(|s| matches!(s.goal, Goal::Aqua(aqua::Act::Beam { .. })));
    m(&mut e).brain.goals[0].goal = Goal::Aqua(aqua::Act::Beam {
        duration: 80,
        hold: 6.0,
    });
    let target = Target::at(100, 100, [6.5, 5.0, 0.5]);
    let mut out = Vec::new();
    for t in 0..200 {
        let s = Senses {
            world: &world,
            players: std::slice::from_ref(&target),
            mobs: &[],
            items: &NoItems,
            tick: t,
            difficulty: 2,
            sunny: false,
            raining: false,
            nether: false,
            day: false,
            griefing: true,
        };
        tick(&mut e, &s, &mut out);
    }
    assert!(
        !out.iter().any(|ev| matches!(ev, MobEvent::Attack { .. })),
        "{out:?}"
    );
}

#[test]
fn guardian_spikes_come_in_when_it_swims() {
    let world = Pool::deep();
    let mut e = mob(Species::Guardian, [0.5, 5.0, 0.5]);
    m(&mut e).brain.goals.clear();
    run(&mut e, &world, &[], &[], 40);
    assert!(!m(&mut e).quirks.aqua.retract);
    e.vel = [0.2, 0.0, 0.0];
    m(&mut e).goto = Some(([20.5, 5.0, 0.5], 1.0));
    let mut out = Vec::new();
    tick(&mut e, &senses(&world, &[], &[], 50), &mut out);
    tick(&mut e, &senses(&world, &[], &[], 51), &mut out);
    assert!(m(&mut e).quirks.aqua.retract);
}

#[test]
fn a_stranded_guardian_flops_without_choking() {
    let world = Pool::default();
    let mut e = mob(Species::Guardian, [0.5, 0.0, 0.5]);
    e.on_ground = true;
    let out = run(&mut e, &world, &[], &[], 800);
    assert!(
        out.iter().any(|ev| matches!(ev,
            MobEvent::Sound { name, .. } if name == "entity.guardian.flop")),
        "{out:?}"
    );
    assert!(
        !out.iter().any(|ev| matches!(ev, MobEvent::Hurt { .. })),
        "{out:?}"
    );
}

#[test]
fn an_elder_guardian_curses_every_minute() {
    let world = Pool::deep();
    let mut e = mob(Species::ElderGuardian, [0.5, 5.0, 0.5]);
    m(&mut e).brain.goals.clear();
    let out = run(&mut e, &world, &[], &[], 2500);
    let curses = out.iter().filter(|ev| **ev == MobEvent::Curse).count();
    assert_eq!(curses, 2, "{out:?}");
}

#[test]
fn a_nautilus_dashes_at_whoever_hurt_it() {
    let world = Pool::deep();
    let mut e = mob(Species::Nautilus, [0.5, 5.0, 0.5]);
    let attacker = Target::at(100, 100, [4.5, 5.0, 0.5]);
    m(&mut e).hurt(1.0, Some(100));
    let out = run(&mut e, &world, &[attacker], &[], 120);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Attack { target: 100, damage } if *damage == 3.0)),
        "{out:?}"
    );
}

#[test]
fn a_nautilus_hunts_pufferfish() {
    let world = Pool::deep();
    let mut e = mob(Species::Nautilus, [0.5, 5.0, 0.5]);
    let puffer = other(9, Species::Pufferfish, [4.5, 5.0, 0.5]);
    let out = run(&mut e, &world, &[], &[puffer], 200);
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Attack { target: 9, .. })),
        "{out:?}"
    );
}

#[test]
fn zombie_nautilus_variants_and_voices() {
    assert_eq!(
        Species::Cod.sound(Sound::Hurt, false).as_deref(),
        Some("entity.cod.hurt")
    );
    assert_eq!(
        Species::Pufferfish.sound(Sound::Death, false).as_deref(),
        Some("entity.puffer_fish.death")
    );
    assert_eq!(
        Species::Turtle.sound(Sound::Hurt, true).as_deref(),
        Some("entity.turtle.hurt_baby")
    );
    assert_eq!(
        Species::ZombieNautilus
            .sound(Sound::Ambient, false)
            .as_deref(),
        Some("entity.zombie_nautilus.ambient")
    );
}
