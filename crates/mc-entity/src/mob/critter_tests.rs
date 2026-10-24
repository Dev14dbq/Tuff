//! Silverfish, endermites and golems.

use super::*;
use crate::nav::Liquid;
use crate::physics::Collide;

/// A flat floor (y < 0) whose stone can be hidden in when `stone` is set,
/// hot when `hot`, in the day and the rain when those are set.
#[derive(Default)]
struct Den {
    stone: bool,
    hot: bool,
    day: bool,
    rain: bool,
}

impl Collide for Den {
    fn solid(&self, _: i32, y: i32, _: i32) -> bool {
        y < 0
    }
}

impl Terrain for Den {
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
    fn infestable(&self, _: i32, y: i32, _: i32) -> bool {
        self.stone && y < 0
    }
    fn snow_golem_melts(&self, _: i32, _: i32, _: i32) -> bool {
        self.hot
    }
}

fn senses<'a>(world: &'a Den, players: &'a [Target], tick: u64, griefing: bool) -> Senses<'a> {
    Senses {
        world,
        players,
        mobs: &[],
        items: &NoItems,
        tick,
        difficulty: 2,
        sunny: world.day && !world.rain,
        raining: world.rain,
        nether: false,
        day: world.day,
        griefing,
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

fn run(
    e: &mut Entity,
    world: &Den,
    players: &[Target],
    ticks: u64,
    griefing: bool,
) -> Vec<MobEvent> {
    run_among(e, world, players, &[], ticks, griefing)
}

/// [`run`] with other mobs about.
fn run_among(
    e: &mut Entity,
    world: &Den,
    players: &[Target],
    mobs: &[Other],
    ticks: u64,
    griefing: bool,
) -> Vec<MobEvent> {
    let mut out = Vec::new();
    for t in 0..ticks {
        let s = Senses {
            mobs,
            ..senses(world, players, t, griefing)
        };
        tick(e, &s, &mut out);
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

#[test]
fn a_silverfish_goes_for_a_player_and_bites() {
    let mut e = mob(Species::Silverfish, [0.5, 0.0, 0.5]);
    let p = Target::at(100, 100, [6.5, 0.0, 0.5]);
    let out = run(&mut e, &Den::default(), &[p], 150, true);
    assert!(
        out.iter().any(|ev| matches!(
            ev,
            MobEvent::Attack { target: 100, damage } if (*damage - 1.0).abs() < 1e-6
        )),
        "{out:?}"
    );
    assert_eq!(Species::Silverfish.max_health(), 8.0);
}

#[test]
fn a_silverfish_at_peace_hides_in_stone_next_to_it() {
    let mut e = mob(Species::Silverfish, [0.5, 0.0, 0.5]);
    let out = run(
        &mut e,
        &Den {
            stone: true,
            ..Den::default()
        },
        &[],
        600,
        true,
    );
    let hid = out.iter().find_map(|ev| match ev {
        MobEvent::Hide { cell } => Some(*cell),
        _ => None,
    });
    assert!(hid.is_some(), "never hid");
    assert!(hid.unwrap()[1] < 0, "{hid:?}");
}

#[test]
fn a_silverfish_does_not_hide_without_mob_griefing_or_with_a_target() {
    let mut e = mob(Species::Silverfish, [0.5, 0.0, 0.5]);
    let out = run(
        &mut e,
        &Den {
            stone: true,
            ..Den::default()
        },
        &[],
        600,
        false,
    );
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Hide { .. })));
    // With a player in sight it fights instead.
    let mut e = mob(Species::Silverfish, [0.5, 0.0, 0.5]);
    let p = Target::at(100, 100, [3.5, 0.0, 0.5]);
    let out = run(
        &mut e,
        &Den {
            stone: true,
            ..Den::default()
        },
        &[p],
        200,
        true,
    );
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Hide { .. })));
}

#[test]
fn a_hurt_silverfish_calls_its_friends_after_a_second() {
    let mut e = mob(Species::Silverfish, [0.5, 0.0, 0.5]);
    m(&mut e).quirks.call_friends = 20;
    let out = run(&mut e, &Den::default(), &[], 19, true);
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::WakeFriends)));
    let out = run(&mut e, &Den::default(), &[], 5, true);
    assert_eq!(
        out.iter()
            .filter(|ev| matches!(ev, MobEvent::WakeFriends))
            .count(),
        1
    );
    assert_eq!(m(&mut e).quirks.call_friends, 0);
}

#[test]
fn an_endermite_bites_for_two_and_is_gone_after_two_minutes() {
    let mut e = mob(Species::Endermite, [0.5, 0.0, 0.5]);
    let p = Target::at(100, 100, [5.5, 0.0, 0.5]);
    let out = run(&mut e, &Den::default(), &[p], 120, true);
    assert!(
        out.iter().any(|ev| matches!(
            ev,
            MobEvent::Attack { target: 100, damage } if (*damage - 2.0).abs() < 1e-6
        )),
        "{out:?}"
    );
    let mut e = mob(Species::Endermite, [0.5, 0.0, 0.5]);
    let out = run(
        &mut e,
        &Den::default(),
        &[],
        ENDERMITE_LIFE as u64 - 1,
        true,
    );
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Vanish)));
    let out = run(&mut e, &Den::default(), &[], 1, true);
    assert!(out.iter().any(|ev| matches!(ev, MobEvent::Vanish)));
}

#[test]
fn a_named_or_kept_endermite_does_not_go() {
    for named in [true, false] {
        let mut e = mob(Species::Endermite, [0.5, 0.0, 0.5]);
        if named {
            m(&mut e).custom_name = Some("Mite".into());
        } else {
            m(&mut e).persistent = true;
        }
        let out = run(&mut e, &Den::default(), &[], 2500, true);
        assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Vanish)));
        assert_eq!(m(&mut e).quirks.lifetime, 0);
    }
}

#[test]
fn endermen_go_for_endermites_in_sight() {
    let mut e = mob(Species::Enderman, [0.5, 0.0, 0.5]);
    let mite = other(9, Species::Endermite, [30.5, 0.0, 0.5]);
    run_among(&mut e, &Den::default(), &[], &[mite], 100, true);
    assert_eq!(m(&mut e).target, Some(9));
    assert!(e.pos[0] > 3.0, "{:?}", e.pos);
}

#[test]
fn an_endermite_turns_on_whoever_hurt_it() {
    let mut e = mob(Species::Endermite, [0.5, 0.0, 0.5]);
    let foe = other(9, Species::Enderman, [4.5, 0.0, 0.5]);
    assert!(m(&mut e).hurt(1.0, Some(9)));
    let out = run_among(&mut e, &Den::default(), &[], &[foe], 20, true);
    assert_eq!(m(&mut e).target, Some(9));
    assert!(
        out.iter()
            .any(|ev| matches!(ev, MobEvent::Alert { target: 9 }))
    );
}

fn villager(id: i32, pos: [f64; 3]) -> Other {
    Other {
        species: None,
        ..other(id, Species::Zombie, pos)
    }
}

fn damages(out: &[MobEvent], target: i32) -> Vec<f32> {
    out.iter()
        .filter_map(|ev| match ev {
            MobEvent::Attack { target: t, damage } if *t == target => Some(*damage),
            _ => None,
        })
        .collect()
}

#[test]
fn an_iron_golem_hits_monsters_for_seven_and_a_half_to_twenty_one_and_a_half() {
    let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
    let zombie = other(9, Species::Zombie, [3.5, 0.0, 0.5]);
    let out = run_among(&mut e, &Den::default(), &[], &[zombie], 600, true);
    let hits = damages(&out, 9);
    assert!(hits.len() > 10, "{} hits", hits.len());
    assert!(hits.iter().all(|d| (7.5..=21.5).contains(d)), "{hits:?}");
    assert!(hits.iter().any(|d| *d > 14.5) && hits.iter().any(|d| *d < 14.5));
    assert!(out.iter().any(|ev| matches!(ev, MobEvent::Status(4))));
    assert_eq!(Species::IronGolem.max_health(), 100.0);
    assert_eq!(Species::IronGolem.knockback_resistance(), 1.0);
    assert_eq!(Species::IronGolem.size(), (1.4, 2.7));
}

#[test]
fn an_iron_golem_leaves_creepers_and_other_golems_alone() {
    let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
    let others = [
        other(8, Species::Creeper, [3.5, 0.0, 0.5]),
        other(9, Species::IronGolem, [0.5, 0.0, 3.5]),
        other(10, Species::Cow, [-3.5, 0.0, 0.5]),
    ];
    let out = run_among(&mut e, &Den::default(), &[], &others, 300, true);
    assert_eq!(m(&mut e).target, None);
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Attack { .. })));
    // A golem that hurt it is not a target either.
    assert!(m(&mut e).hurt(5.0, Some(9)));
    run_among(&mut e, &Den::default(), &[], &others, 20, true);
    assert_eq!(m(&mut e).target, None);
}

#[test]
fn a_golem_a_player_made_never_goes_for_players_but_a_village_golem_does_when_hit() {
    let p = Target::at(100, 100, [3.5, 0.0, 0.5]);
    for made in [true, false] {
        let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
        m(&mut e).quirks.player_made = made;
        // Calm players are left alone by both.
        let out = run(&mut e, &Den::default(), std::slice::from_ref(&p), 100, true);
        assert!(damages(&out, 100).is_empty());
        // Hit by the player.
        assert!(m(&mut e).hurt(1.0, Some(100)));
        let out = run(&mut e, &Den::default(), std::slice::from_ref(&p), 100, true);
        assert_eq!(!damages(&out, 100).is_empty(), !made, "player made: {made}");
    }
}

#[test]
fn a_village_golem_goes_for_a_player_with_a_bad_name_and_a_made_one_does_not() {
    let bad = Target::at(100, 100, [5.5, 0.0, 0.5]);
    for made in [false, true] {
        let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
        m(&mut e).quirks.player_made = made;
        m(&mut e).quirks.scorned = vec![100];
        let out = run(
            &mut e,
            &Den::default(),
            std::slice::from_ref(&bad),
            200,
            true,
        );
        assert_eq!(!damages(&out, 100).is_empty(), !made, "player made: {made}");
    }
}

#[test]
fn a_golem_walks_back_to_the_villagers_and_offers_them_a_poppy() {
    // Forty blocks off the village it heads there.
    let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
    let v = villager(7, [40.5, 0.0, 0.5]);
    run_among(&mut e, &Den::default(), &[], &[v], 400, true);
    assert!(e.pos[0] > 10.0, "{:?}", e.pos);
    // By day, now and then, with a villager beside it.
    let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
    let v = villager(7, [2.5, 0.0, 0.5]);
    let day = Den {
        day: true,
        ..Den::default()
    };
    let (mut offered, mut stopped) = (None, None);
    for t in 0..60_000u64 {
        let mut out = Vec::new();
        let s = Senses {
            mobs: std::slice::from_ref(&v),
            ..senses(&day, &[], t, true)
        };
        tick(&mut e, &s, &mut out);
        for ev in out {
            match ev {
                MobEvent::Status(11) => offered = offered.or(Some(t)),
                MobEvent::Status(34) => stopped = stopped.or(Some(t)),
                _ => {}
            }
        }
        if stopped.is_some() {
            break;
        }
    }
    let (from, to) = (
        offered.expect("never offered"),
        stopped.expect("never stopped"),
    );
    // It holds it out at most 400 ticks (a stroll back to the village
    // may end it sooner).
    assert!(to - from <= 401, "{from} {to}");
}

#[test]
fn a_golem_holds_a_poppy_out_to_a_child_that_comes_and_takes_it() {
    let day = Den {
        day: true,
        ..Den::default()
    };
    // Whether the offer is taken, by a child or a grown villager beside it.
    let offer = |baby: bool| {
        let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
        m(&mut e).speed_attr = 0.0;
        let mut v = villager(7, [2.5, 0.0, 0.5]);
        v.baby = baby;
        let (mut offered, mut taken, mut stopped) = (false, None, None);
        for t in 0..60_000u64 {
            let mut out = Vec::new();
            let s = Senses {
                mobs: std::slice::from_ref(&v),
                ..senses(&day, &[], t, true)
            };
            tick(&mut e, &s, &mut out);
            for ev in out {
                match ev {
                    MobEvent::Status(11) => offered = true,
                    MobEvent::FlowerTaken(id) => taken = taken.or(Some((id, t))),
                    MobEvent::Status(34) => stopped = stopped.or(Some(t)),
                    _ => {}
                }
            }
            if stopped.is_some() {
                break;
            }
        }
        (offered, taken)
    };
    let (offered, taken) = offer(true);
    assert!(offered);
    assert_eq!(taken.map(|t| t.0), Some(7));
    // A grown one is offered it too, and never takes it.
    let (offered, taken) = offer(false);
    assert!(offered && taken.is_none());
}

#[test]
fn a_golem_offers_its_poppy_to_a_child_before_a_grown_villager() {
    let day = Den {
        day: true,
        ..Den::default()
    };
    let mut e = mob(Species::IronGolem, [0.5, 0.0, 0.5]);
    m(&mut e).speed_attr = 0.0;
    let grown = villager(7, [1.5, 0.0, 0.5]);
    let mut child = villager(8, [4.5, 0.0, 0.5]);
    child.baby = true;
    let mobs = [grown, child];
    for t in 0..60_000u64 {
        let mut out = Vec::new();
        let s = Senses {
            mobs: &mobs,
            ..senses(&day, &[], t, true)
        };
        tick(&mut e, &s, &mut out);
        if out.iter().any(|ev| matches!(ev, MobEvent::Status(11))) {
            let looking = m(&mut e)
                .brain
                .goals
                .iter()
                .any(|s| matches!(s.goal, Goal::OfferFlower { to: Some(8), .. }));
            assert!(looking);
            return;
        }
    }
    panic!("never offered");
}

#[test]
fn zombies_and_skeletons_go_for_iron_golems() {
    for species in [Species::Zombie, Species::Skeleton, Species::Spider] {
        let mut e = mob(species, [0.5, 0.0, 0.5]);
        let golem = other(9, Species::IronGolem, [6.5, 0.0, 0.5]);
        run_among(&mut e, &Den::default(), &[], &[golem], 100, true);
        assert_eq!(m(&mut e).target, Some(9), "{species:?}");
    }
}

#[test]
fn a_snow_golem_throws_snowballs_at_monsters_but_not_players() {
    let mut e = mob(Species::SnowGolem, [0.5, 0.0, 0.5]);
    assert_eq!(Species::SnowGolem.max_health(), 4.0);
    assert!(m(&mut e).quirks.pumpkin);
    let zombie = other(9, Species::Creeper, [8.5, 0.0, 0.5]);
    let p = Target::at(100, 100, [-3.5, 0.0, 0.5]);
    let out = run_among(
        &mut e,
        &Den::default(),
        std::slice::from_ref(&p),
        &[zombie],
        200,
        true,
    );
    let shots = out
        .iter()
        .filter(|ev| {
            matches!(
                ev,
                MobEvent::Shoot {
                    target: 9,
                    missile: Missile::Snowball,
                    ..
                }
            )
        })
        .count();
    assert!(shots >= 3, "{shots} snowballs");
    assert!(
        !out.iter()
            .any(|ev| matches!(ev, MobEvent::Shoot { target: 100, .. }))
    );
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Attack { .. })));
}

fn count_of(out: &[MobEvent], f: impl Fn(&MobEvent) -> bool) -> usize {
    out.iter().filter(|ev| f(ev)).count()
}

#[test]
fn a_snow_golem_leaves_snow_in_the_cold_and_melts_in_the_heat_the_rain_and_water() {
    let cold = Den::default();
    let mut e = mob(Species::SnowGolem, [0.5, 0.0, 0.5]);
    let out = run(&mut e, &cold, &[], 40, true);
    assert!(count_of(&out, |ev| matches!(ev, MobEvent::Trail)) >= 30);
    assert_eq!(count_of(&out, |ev| matches!(ev, MobEvent::Hurt { .. })), 0);
    // No trail with mob griefing off.
    let mut e = mob(Species::SnowGolem, [0.5, 0.0, 0.5]);
    let out = run(&mut e, &cold, &[], 40, false);
    assert_eq!(count_of(&out, |ev| matches!(ev, MobEvent::Trail)), 0);
    // Hot: it burns and leaves nothing.
    let hot = Den {
        hot: true,
        ..Den::default()
    };
    let mut e = mob(Species::SnowGolem, [0.5, 0.0, 0.5]);
    let out = run(&mut e, &hot, &[], 40, true);
    assert_eq!(count_of(&out, |ev| matches!(ev, MobEvent::Trail)), 0);
    assert!(
        count_of(&out, |ev| matches!(
            ev,
            MobEvent::Hurt {
                kind: "minecraft:on_fire",
                ..
            }
        )) >= 3
    );
    // Rain.
    let rain = Den {
        rain: true,
        day: true,
        ..Den::default()
    };
    let mut e = mob(Species::SnowGolem, [0.5, 0.0, 0.5]);
    let out = run(&mut e, &rain, &[], 40, true);
    assert!(
        count_of(&out, |ev| matches!(
            ev,
            MobEvent::Hurt {
                kind: "minecraft:drown",
                ..
            }
        )) >= 3
    );
}
