//! Silverfish, endermites and golems.

use super::*;
use crate::nav::Liquid;
use crate::physics::Collide;

/// A flat floor (y < 0) whose stone can be hidden in when `stone` is set.
#[derive(Default)]
struct Den {
    stone: bool,
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
}

fn senses<'a>(world: &'a Den, players: &'a [Target], tick: u64, griefing: bool) -> Senses<'a> {
    Senses {
        world,
        players,
        mobs: &[],
        items: &NoItems,
        tick,
        difficulty: 2,
        sunny: false,
        raining: false,
        nether: false,
        day: false,
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
    let mut out = Vec::new();
    for t in 0..ticks {
        tick(e, &senses(world, players, t, griefing), &mut out);
    }
    out
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
    let out = run(&mut e, &Den { stone: true }, &[], 600, true);
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
    let out = run(&mut e, &Den { stone: true }, &[], 600, false);
    assert!(!out.iter().any(|ev| matches!(ev, MobEvent::Hide { .. })));
    // With a player in sight it fights instead.
    let mut e = mob(Species::Silverfish, [0.5, 0.0, 0.5]);
    let p = Target::at(100, 100, [3.5, 0.0, 0.5]);
    let out = run(&mut e, &Den { stone: true }, &[p], 200, true);
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
