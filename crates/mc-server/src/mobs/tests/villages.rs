//! Villages: gossip, children, bells and golems (minecraft.wiki,
//! "Villager", "Bell", "Iron Golem").

use mc_entity::gossip::{self, Kind as Gossip};
use mc_entity::trade::Cost;

use super::critters::{normal, set_mob};
use super::*;

const PLAYER: u128 = 0x5eed;

fn tell(ctx: &ServerContext, id: i32, f: impl FnOnce(&mut mc_entity::trade::Merchant)) {
    let mut all = lock(&ctx.entities);
    let e = all.iter_mut().find(|e| e.id == id).expect("villager");
    let Kind::Merchant(m) = &mut e.kind else {
        panic!("no villager")
    };
    f(m);
}

fn state(ctx: &ServerContext, name: &str, set: &[(&str, &str)]) -> u32 {
    let b = &ctx.data.blocks;
    let mut s = b.default_state(name).unwrap();
    for (k, v) in set {
        s = b.with_property(s, k, v).unwrap();
    }
    s
}

/// A bed whose head is at `head`, the foot to its west.
fn bed(ctx: &ServerContext, head: [i32; 3]) {
    let foot = state(
        ctx,
        "minecraft:red_bed",
        &[("facing", "east"), ("part", "foot")],
    );
    let top = state(
        ctx,
        "minecraft:red_bed",
        &[("facing", "east"), ("part", "head")],
    );
    set_state(ctx, [head[0] - 1, head[1], head[2]], foot);
    set_state(ctx, head, top);
}

fn food(ctx: &ServerContext, name: &str, count: i32) -> Cost {
    Cost {
        item: ctx.items.id(name).unwrap(),
        count,
    }
}

fn villagers(ctx: &ServerContext) -> Vec<i32> {
    lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Merchant(m) if !m.wandering))
        .map(|e| e.id)
        .collect()
}

// ---- gossip ----

#[test]
fn villagers_tell_each_other_what_they_heard_less_what_it_costs_to_share() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    let a = villager_at(&ctx, [4.5, -60.0, 4.5]);
    let b = villager_at(&ctx, [5.5, -60.0, 4.5]);
    let far = villager_at(&ctx, [40.5, -60.0, 4.5]);
    tell(&ctx, a, |m| {
        m.gossip.happen(PLAYER, Gossip::MajorNegative);
        m.gossip.happen(PLAYER, Gossip::MajorPositive);
    });
    run_all(&ctx, 25);
    let (a_gossip, b_gossip) = (merchant_of(&ctx, a).gossip, merchant_of(&ctx, b).gossip);
    // 25 less the 10 it costs; curing is never shared.
    assert_eq!(b_gossip.value(PLAYER, Gossip::MajorNegative), 15);
    assert_eq!(b_gossip.value(PLAYER, Gossip::MajorPositive), 0);
    assert_eq!(a_gossip.value(PLAYER, Gossip::MajorNegative), 25);
    // Out of earshot nothing is heard.
    assert!(merchant_of(&ctx, far).gossip.is_empty());
}

#[test]
fn gossip_fades_every_twenty_minutes() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    let id = villager_at(&ctx, [4.5, -60.0, 4.5]);
    tell(&ctx, id, |m| {
        m.gossip.happen(PLAYER, Gossip::MinorNegative);
        m.gossip.happen(PLAYER, Gossip::MajorPositive);
    });
    lock(&ctx.time).age = 23_998;
    run(&ctx, 1);
    let m = merchant_of(&ctx, id);
    assert_eq!(m.gossip.value(PLAYER, Gossip::MinorNegative), 25);
    run(&ctx, 2);
    let m = merchant_of(&ctx, id);
    assert_eq!(m.gossip.value(PLAYER, Gossip::MinorNegative), 5);
    assert_eq!(m.gossip.value(PLAYER, Gossip::MajorPositive), 20);
}

#[test]
fn hitting_a_villager_is_remembered_by_it_and_killing_one_by_those_who_saw() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    let victim = villager_at(&ctx, [2.5, -60.0, 0.5]);
    let near = villager_at(&ctx, [6.5, -60.0, 0.5]);
    let far = villager_at(&ctx, [30.5, -60.0, 0.5]);
    // Between it and the one who sees nothing: a wall.
    let behind = villager_at(&ctx, [2.5, -60.0, 8.5]);
    for y in -60..-56 {
        for x in -2..6 {
            set_state(&ctx, [x, y, 4], state(&ctx, "minecraft:stone", &[]));
        }
    }
    ctx.mobs.hit(
        ctx.dim,
        victim,
        steve([0.5, -60.0, 0.5]),
        blow(1.0, [0.5, -60.0, 0.5]),
    );
    run_all(&ctx, 1);
    assert_eq!(
        merchant_of(&ctx, victim)
            .gossip
            .value(uuid, Gossip::MinorNegative),
        25
    );
    assert!(merchant_of(&ctx, near).gossip.is_empty());
    // A killing blow.
    ctx.mobs.hit(
        ctx.dim,
        victim,
        steve([0.5, -60.0, 0.5]),
        blow(40.0, [0.5, -60.0, 0.5]),
    );
    run_all(&ctx, 1);
    assert!(mob_of(&ctx, victim).is_none());
    assert_eq!(
        merchant_of(&ctx, near)
            .gossip
            .value(uuid, Gossip::MajorNegative),
        25
    );
    assert!(merchant_of(&ctx, far).gossip.is_empty());
    assert!(merchant_of(&ctx, behind).gossip.is_empty());
}

#[test]
fn a_zombie_villager_cured_thanks_the_villager_it_becomes_for_good() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::ZombieVillager, [5.5, -60.0, 5.5]);
    let mut e = mob_of(&ctx, id).unwrap();
    if let Kind::Mob(m) = &mut e.kind {
        m.curer = Some(PLAYER);
    }
    hurt::cure(&ctx, &mut e);
    let cured = villagers(&ctx);
    assert_eq!(cured.len(), 1);
    let g = merchant_of(&ctx, cured[0]).gossip;
    assert_eq!(g.value(PLAYER, Gossip::MajorPositive), 20);
    assert_eq!(g.value(PLAYER, Gossip::MinorPositive), 25);
    assert_eq!(g.reputation(PLAYER), 125);
}

#[test]
fn golems_the_village_has_go_for_a_player_a_villager_near_them_thinks_ill_of() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let golem = put(&ctx, Species::IronGolem, [5.5, -60.0, 0.5]);
    let made = put(&ctx, Species::IronGolem, [-5.5, -60.0, 0.5]);
    set_mob(&ctx, made, |m| m.quirks.player_made = true);
    let near = villager_at(&ctx, [8.5, -60.0, 0.5]);
    run(&ctx, 100);
    assert_eq!(lock(&ctx.vitals)[&uuid].health, 20.0);
    // Four kills seen: 100 of major negative, -500 in all.
    tell(&ctx, near, |m| {
        for _ in 0..4 {
            m.gossip.happen(uuid, Gossip::MajorNegative);
        }
        assert!(m.gossip.reputation(uuid) <= gossip::HOSTILE);
    });
    run(&ctx, 100);
    assert!(lock(&ctx.vitals)[&uuid].health < 20.0);
    assert_eq!(
        with_mob(&ctx, golem, |m| m.quirks.scorned.clone()),
        vec![uuid]
    );
    assert_eq!(with_mob(&ctx, made, |m| m.target), None);
    // Forgiven (its gossip gone), the golem calms down.
    tell(&ctx, near, |m| m.gossip = Default::default());
    run(&ctx, 20);
    assert!(with_mob(&ctx, golem, |m| m.quirks.scorned.is_empty()));
}

#[test]
fn only_a_villager_within_ten_blocks_of_a_golem_counts() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let golem = put(&ctx, Species::IronGolem, [5.5, -60.0, 0.5]);
    let far = villager_at(&ctx, [30.5, -60.0, 0.5]);
    tell(&ctx, far, |m| {
        for _ in 0..4 {
            m.gossip.happen(uuid, Gossip::MajorNegative);
        }
    });
    run(&ctx, 40);
    assert!(with_mob(&ctx, golem, |m| m.quirks.scorned.is_empty()));
    assert_eq!(lock(&ctx.vitals)[&uuid].health, 20.0);
}

// ---- children ----

fn ready_parents(ctx: &ServerContext) -> (i32, i32) {
    let a = villager_at(ctx, [4.5, -60.0, 10.5]);
    let b = villager_at(ctx, [5.5, -60.0, 10.5]);
    // 3 bread is 12 points; the other has 4 carrots, 4 potatoes and 4
    // beetroots.
    tell(ctx, a, |m| m.pantry = vec![food(ctx, "minecraft:bread", 3)]);
    tell(ctx, b, |m| {
        m.pantry = vec![
            food(ctx, "minecraft:carrot", 4),
            food(ctx, "minecraft:potato", 4),
            food(ctx, "minecraft:beetroot", 4),
        ];
    });
    (a, b)
}

/// Runs until there is a child or the ticks are up.
fn until_a_child(ctx: &ServerContext, ticks: usize) -> Option<i32> {
    for _ in 0..ticks {
        run_all(ctx, 1);
        let child = villagers(ctx)
            .into_iter()
            .find(|&v| merchant_of(ctx, v).is_baby());
        if child.is_some() {
            return child;
        }
    }
    None
}

#[test]
fn two_willing_villagers_with_a_spare_bed_have_a_child_that_takes_it_and_grows_up() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    // Three beds: one each and one to spare.
    for x in [8, 10, 12] {
        bed(&ctx, [x, -60, 12]);
    }
    let (a, b) = ready_parents(&ctx);
    // The beds are theirs first.
    run_all(&ctx, 1);
    let child = until_a_child(&ctx, 400).expect("no child was born");
    let c = merchant_of(&ctx, child);
    assert!((-24_000..0).contains(&c.age), "{}", c.age);
    assert!(c.profession.is_none() && c.offers.is_empty());
    // It has the bed nobody had; nobody else may take it.
    let bed = c.home.expect("the child has no bed");
    assert_eq!(lock(&ctx.poi).get([bed.0, bed.1, bed.2]).unwrap().free, 0);
    for p in [a, b] {
        let m = merchant_of(&ctx, p);
        assert_ne!(m.home, Some(bed));
        // 12 points spent, the rest kept; five minutes before the next.
        assert_eq!(crate::mobs::village::points(&ctx, &m), 0, "{:?}", m.pantry);
        assert!(m.age > 5000);
    }
    // A child is small, sells nothing, and grows up after 20 minutes.
    let e = mob_of(&ctx, child).unwrap();
    assert_eq!(e.size().1, 0.975);
    tell(&ctx, child, |m| m.age = -3);
    run_all(&ctx, 4);
    let c = merchant_of(&ctx, child);
    assert!(!c.is_baby());
    assert_eq!(mob_of(&ctx, child).unwrap().size().1, 1.95);
}

#[test]
fn villagers_without_food_enough_or_a_bed_to_spare_have_no_child() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    // Only their own beds.
    for x in [8, 10] {
        bed(&ctx, [x, -60, 12]);
    }
    let (a, b) = ready_parents(&ctx);
    // Each has taken a bed before it comes to breeding.
    let (food_a, food_b) = (merchant_of(&ctx, a).pantry, merchant_of(&ctx, b).pantry);
    tell(&ctx, a, |m| m.pantry.clear());
    tell(&ctx, b, |m| m.pantry.clear());
    run_all(&ctx, 100);
    assert!(merchant_of(&ctx, a).home.is_some() && merchant_of(&ctx, b).home.is_some());
    // ... and they meet again, with food.
    for (id, x) in [(a, 4.5), (b, 5.5)] {
        let mut all = lock(&ctx.entities);
        let e = all.iter_mut().find(|e| e.id == id).unwrap();
        e.pos = [x, -60.0, 10.5];
    }
    tell(&ctx, a, |m| m.pantry = food_a);
    tell(&ctx, b, |m| m.pantry = food_b);
    assert!(until_a_child(&ctx, 400).is_none());
    // They ate, in vain.
    for p in [a, b] {
        let m = merchant_of(&ctx, p);
        assert_eq!(crate::mobs::village::points(&ctx, &m), 0);
        assert_eq!(m.age, 0);
    }
    // Hungry ones never try.
    bed(&ctx, [12, -60, 12]);
    for p in [a, b] {
        tell(&ctx, p, |m| {
            m.pantry = vec![food(&ctx, "minecraft:bread", 2)]
        });
    }
    assert!(until_a_child(&ctx, 200).is_none());
    assert_eq!(
        merchant_of(&ctx, a).pantry,
        vec![food(&ctx, "minecraft:bread", 2)]
    );
}

#[test]
fn villagers_pick_up_the_food_thrown_to_them() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    let id = villager_at(&ctx, [4.5, -60.0, 4.5]);
    let bread = ctx.items.id("minecraft:bread").unwrap();
    let stone = ctx.items.id("minecraft:stone").unwrap();
    crate::tick::spawn_item_at(&ctx, [6.5, -60.0, 4.5], bread, 2);
    crate::tick::spawn_item_at(&ctx, [4.5, -60.0, 6.5], stone, 1);
    run_all(&ctx, 200);
    assert_eq!(
        merchant_of(&ctx, id).pantry,
        vec![food(&ctx, "minecraft:bread", 2)]
    );
    assert_eq!(items_of(&ctx, "minecraft:bread"), 0);
    assert_eq!(items_of(&ctx, "minecraft:stone"), 1);
}

// ---- bells ----

#[test]
fn a_bell_sends_the_villagers_to_their_beds_and_wakes_those_asleep() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    bed(&ctx, [20, -60, 3]);
    set_state(&ctx, [10, -60, 3], state(&ctx, "minecraft:bell", &[]));
    let near = villager_at(&ctx, [14.5, -60.0, 3.5]);
    let far = villager_at(&ctx, [14.5, -60.0, 60.5]);
    run_all(&ctx, 80);
    assert_eq!(merchant_of(&ctx, near).home, Some((20, -60, 3)));
    assert!(merchant_of(&ctx, near).walk.hiding.is_none());
    ctx.mobs
        .bell(ctx.dim, mc_protocol::Position::new(10, -60, 3));
    run_all(&ctx, 2);
    let hiding = merchant_of(&ctx, near).walk.hiding.expect("not hiding");
    assert_eq!(hiding.1, [20, -60, 3]);
    assert!(merchant_of(&ctx, far).walk.hiding.is_none());
    // It heads there, and comes out when the time is up.
    run_all(&ctx, 150);
    let e = mob_of(&ctx, near).unwrap();
    assert!((e.pos[0] - 20.5).abs() < 3.0, "{:?}", e.pos);
    run_all(&ctx, 200);
    assert!(merchant_of(&ctx, near).walk.hiding.is_none());
}

#[test]
fn a_bell_wakes_a_sleeping_villager() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    bed(&ctx, [7, -60, 3]);
    let id = villager_at(&ctx, [5.5, -60.0, 3.5]);
    set_day(&ctx, 13000);
    for _ in 0..600 {
        run_all(&ctx, 1);
        if merchant_of(&ctx, id).walk.asleep {
            break;
        }
    }
    assert!(merchant_of(&ctx, id).walk.asleep);
    ctx.mobs
        .bell(ctx.dim, mc_protocol::Position::new(2, -60, 3));
    run_all(&ctx, 3);
    assert!(!merchant_of(&ctx, id).walk.asleep);
}

#[test]
fn a_bell_makes_raiders_within_48_blocks_glow_when_one_is_within_32() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_day(&ctx, 1000);
    let near = put(&ctx, Species::Witch, [10.5, -60.0, 0.5]);
    let mid = put(&ctx, Species::Witch, [40.5, -60.0, 0.5]);
    let far = put(&ctx, Species::Witch, [60.5, -60.0, 0.5]);
    let pig = put(&ctx, Species::Pig, [12.5, -60.0, 0.5]);
    for id in [near, mid, far, pig] {
        set_mob(&ctx, id, |m| {
            m.persistent = true;
            m.speed_attr = 0.0;
        });
    }
    let glowing = |id| with_mob(&ctx, id, |m| m.effects.active.contains_key("glowing"));
    run_all(&ctx, 2);
    assert!(!glowing(near));
    ctx.mobs
        .bell(ctx.dim, mc_protocol::Position::new(0, -60, 3));
    run_all(&ctx, 2);
    assert!(glowing(near) && glowing(mid));
    assert!(!glowing(far) && !glowing(pig));
    // It wears off in three seconds.
    run_all(&ctx, 70);
    assert!(!glowing(near));
    // With none within 32 blocks, nothing glows.
    let _ = mid;
    set_mob(&ctx, near, |m| m.health = 0.0);
    run_all(&ctx, 40);
    ctx.mobs
        .bell(ctx.dim, mc_protocol::Position::new(0, -60, 3));
    run_all(&ctx, 2);
    assert!(!glowing(mid));
}

#[test]
fn villagers_gather_at_their_bell_in_the_afternoon() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_state(&ctx, [24, -60, 3], state(&ctx, "minecraft:bell", &[]));
    set_day(&ctx, 9500);
    let id = villager_at(&ctx, [4.5, -60.0, 3.5]);
    run_all(&ctx, 60);
    assert_eq!(merchant_of(&ctx, id).meeting, Some((24, -60, 3)));
    run_all(&ctx, 500);
    let e = mob_of(&ctx, id).unwrap();
    assert!((e.pos[0] - 24.5).abs() < 6.0, "{:?}", e.pos);
}
