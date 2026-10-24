//! Flying mobs: bats, bees, allays, breezes, happy ghasts, bogged, parched,
//! giants, shulkers, creakings, copper golems and sulfur cubes.

use super::*;

fn set_mob<R>(ctx: &ServerContext, id: i32, f: impl FnOnce(&mut Mob) -> R) -> R {
    let mut all = lock(&ctx.entities);
    let Kind::Mob(m) = &mut all.iter_mut().find(|e| e.id == id).unwrap().kind else {
        panic!()
    };
    f(m)
}

fn count_of(ctx: &ServerContext, species: Species) -> usize {
    lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == species && m.dying.is_none()))
        .count()
}

#[test]
fn flying_mobs_have_their_wiki_stats() {
    for (s, hp, size) in [
        (Species::Bat, 6.0, (0.5, 0.9)),
        (Species::Bee, 10.0, (0.55, 0.5)),
        (Species::Allay, 20.0, (0.35, 0.6)),
        (Species::Breeze, 30.0, (0.6, 1.77)),
        (Species::HappyGhast, 20.0, (4.0, 4.0)),
        (Species::Bogged, 16.0, (0.6, 1.99)),
        (Species::Parched, 16.0, (0.6, 1.99)),
        (Species::Shulker, 30.0, (1.0, 1.0)),
        (Species::Creaking, 1.0, (0.9, 2.7)),
        (Species::CopperGolem, 12.0, (0.49, 0.98)),
        (Species::SulfurCube, 8.0, (0.98, 0.98)),
    ] {
        assert_eq!(s.max_health(), hp, "{s:?}");
        assert_eq!(s.size(), size, "{s:?}");
        assert_eq!(Species::from_id(s.id()), Some(s));
    }
    assert!(Species::Bat.flies() && Species::Bee.flies() && Species::Allay.flies());
    assert!(!Species::Parched.burns_in_sun() && Species::Bogged.burns_in_sun());
    assert_eq!(Species::Bat.category(), mc_entity::mob::Category::Ambient);
}

#[test]
fn the_flyers_state_is_saved_and_shown() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut bee = Mob::new(Species::Bee, 3);
    bee.quirks.air.nectar = true;
    bee.quirks.air.hive = Some([4, 70, -2]);
    bee.quirks.air.crops = 3;
    let mut c = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, &bee, &mut c);
    assert_eq!(
        c.get("hive_pos"),
        Some(&mc_nbt::Tag::IntArray(vec![4, 70, -2]))
    );
    let mut back = Mob::new(Species::Bee, 9);
    crate::mobs::nbt::apply(&ctx, &mut back, &c);
    assert!(back.quirks.air.nectar);
    assert_eq!(back.quirks.air.hive, Some([4, 70, -2]));
    assert_eq!(back.quirks.air.crops, 3);
    // Flag 8 is nectar, 4 the sting, 2 anger.
    let meta = crate::mobs::meta::metadata_of(&ctx, &bee, 0);
    assert!(meta.contains(&(18, mc_protocol::packets::play::MetadataValue::Byte(8))));
    let mut shulker = Mob::new(Species::Shulker, 3);
    shulker.quirks.air.color = 5;
    let meta = crate::mobs::meta::metadata_of(&ctx, &shulker, 0);
    assert!(meta.contains(&(18, mc_protocol::packets::play::MetadataValue::Byte(5))));
}

#[test]
fn a_closed_shulker_takes_little_and_turns_arrows_away() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 20.5]);
    let id = put(&ctx, Species::Shulker, [0.5, -60.0, 0.5]);
    run(&ctx, 1);
    ctx.mobs.harm(ctx.dim, id, 10.0, "minecraft:arrow", None);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 30.0);
    // 20 armor points leave 40% of a blow.
    ctx.mobs.harm(ctx.dim, id, 10.0, "minecraft:generic", None);
    run(&ctx, 1);
    let left = with_mob(&ctx, id, |m| m.health);
    assert!(left < 30.0 && left > 24.0, "health {left}");
}

#[test]
fn a_shulker_opens_for_a_player_and_shoots_a_bullet() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let uuid = join(&ctx, [0.5, -60.0, 8.5]);
    let id = put(&ctx, Species::Shulker, [0.5, -60.0, 0.5]);
    let mut shot = false;
    for _ in 0..200 {
        run(&ctx, 1);
        shot |= lock(&ctx.entities).iter().any(|e| {
            matches!(&e.kind, Kind::Projectile(p)
                if matches!(p.shot, mc_entity::extra::Shot::ShulkerBullet { .. }))
        });
    }
    assert!(with_mob(&ctx, id, |m| m.quirks.air.peek) > 0, "shell shut");
    assert!(shot, "no bullet");
    let _ = uuid;
}

#[test]
fn a_breeze_fires_wind_charges() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [0.5, -60.0, 8.5]);
    let id = put(&ctx, Species::Breeze, [0.5, -60.0, 0.5]);
    let mut shot = false;
    for _ in 0..200 {
        run(&ctx, 1);
        shot |= lock(&ctx.entities).iter().any(|e| {
            matches!(&e.kind, Kind::Projectile(p)
                if matches!(p.shot, mc_entity::extra::Shot::WindCharge { breeze: true }))
        });
    }
    assert!(shot, "no wind charge");
    // It takes no fall damage.
    ctx.mobs.harm(ctx.dim, id, 9.0, "minecraft:fall", None);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 30.0);
}

#[test]
fn a_creaking_with_a_heart_cannot_be_hurt() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 20.5]);
    let id = put(&ctx, Species::Creaking, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.quirks.air.home = Some([5, -60, 5]));
    ctx.mobs.harm(ctx.dim, id, 5.0, "minecraft:generic", None);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 1.0);
    ctx.mobs
        .harm(ctx.dim, id, 5.0, "minecraft:generic_kill", None);
    run(&ctx, 1);
    assert!(with_mob(&ctx, id, |m| m.is_dead()));
}

#[test]
fn a_creaking_stands_still_while_looked_at() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 12.5]);
    // The player faces north (yaw 180) towards the creaking.
    {
        let mut players = lock(&ctx.players);
        if let Some(p) = players.get_mut(&STEVE)
            && let Some(pose) = &mut p.pose
        {
            pose.yaw = 180.0;
            pose.pitch = 0.0;
        }
    }
    let id = put(&ctx, Species::Creaking, [0.5, -60.0, 0.5]);
    run(&ctx, 40);
    let still = lock(&ctx.entities)
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.pos)
        .unwrap();
    assert!(with_mob(&ctx, id, |m| m.quirks.air.frozen), "not frozen");
    assert!(
        (still[0] - 0.5).abs() < 0.2 && (still[2] - 0.5).abs() < 0.2,
        "{still:?}"
    );
    // Looking away, it comes.
    {
        let mut players = lock(&ctx.players);
        if let Some(p) = players.get_mut(&STEVE)
            && let Some(pose) = &mut p.pose
        {
            pose.yaw = 0.0;
        }
    }
    run(&ctx, 60);
    let moved = lock(&ctx.entities)
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.pos)
        .unwrap();
    assert!(moved[2] > 1.0, "{moved:?}");
}

#[test]
fn a_big_sulfur_cube_splits_in_two_when_it_dies() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 8.5]);
    let id = put(&ctx, Species::SulfurCube, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    ctx.mobs.harm(ctx.dim, id, 50.0, "minecraft:generic", None);
    run(&ctx, 30);
    let small: Vec<bool> = lock(&ctx.entities)
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if m.species == Species::SulfurCube && m.dying.is_none() => {
                Some(m.is_baby())
            }
            _ => None,
        })
        .collect();
    assert_eq!(small, vec![true, true]);
}

#[test]
fn a_bogged_gives_two_mushrooms_to_shears_once() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 3.5]);
    let id = put(&ctx, Species::Bogged, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    hold(&ctx, "minecraft:shears", 1);
    for _ in 0..2 {
        ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 3.5]), 0);
        run(&ctx, 2);
    }
    let n = items_of(&ctx, "minecraft:brown_mushroom") + items_of(&ctx, "minecraft:red_mushroom");
    assert_eq!(n, 2, "mushrooms");
    assert!(with_mob(&ctx, id, |m| m.quirks.air.sheared));
}

#[test]
fn an_allay_takes_a_gift_and_brings_back_matching_items() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 6.5]);
    let id = put(&ctx, Species::Allay, [0.5, -59.0, 3.5]);
    hold(&ctx, "minecraft:cobblestone", 3);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 6.5]), 0);
    run(&ctx, 2);
    assert!(with_mob(&ctx, id, |m| m
        .gear_item(mc_entity::mob::slot::MAINHAND)
        .is_some()));
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.liked), Some(STEVE));
    // Cobblestone lying 10 blocks away is fetched and brought to the player.
    let cobble = ctx.items.id("minecraft:cobblestone").unwrap();
    crate::tick::spawn_item_at(&ctx, [10.5, -59.5, 3.5], cobble, 5);
    let mut carried = false;
    for _ in 0..600 {
        run(&ctx, 1);
        carried |= with_mob(&ctx, id, |m| m.quirks.air.carried.is_some());
    }
    assert!(carried, "never picked the stack up");
    assert!(items_of(&ctx, "minecraft:cobblestone") >= 5);
}

#[test]
fn a_dancing_allay_splits_with_an_amethyst_shard() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 6.5]);
    let id = put(&ctx, Species::Allay, [0.5, -59.0, 3.5]);
    set_mob(&ctx, id, |m| m.quirks.air.dancing = true);
    hold(&ctx, "minecraft:amethyst_shard", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 6.5]), 0);
    run(&ctx, 2);
    assert_eq!(count_of(&ctx, Species::Allay), 2);
    // Both wait five minutes before they can split again.
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.dup_cd) > 5000, true);
}

#[test]
fn a_bee_stings_poisons_and_dies() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let uuid = join(&ctx, [0.5, -60.0, 1.5]);
    let id = put(&ctx, Species::Bee, [0.5, -60.0, 0.5]);
    // Hit lightly: it turns angry.
    ctx.mobs.hit(
        ctx.dim,
        id,
        steve([0.5, -60.0, 1.5]),
        blow(0.5, [0.5, -60.0, 1.5]),
    );
    for _ in 0..200 {
        run(&ctx, 1);
        if with_mob(&ctx, id, |m| m.quirks.air.stung) {
            break;
        }
    }
    assert!(with_mob(&ctx, id, |m| m.quirks.air.stung), "never stung");
    assert!(lock(&ctx.vitals)[&uuid].health < 20.0);
    let poisoned = crate::effects::kind_of("poison")
        .and_then(|k| lock(&ctx.vitals).get(&uuid)?.effects.get(k).cloned());
    assert!(poisoned.is_some(), "no poison");
    // It dies a minute later.
    run(&ctx, 1300);
    assert!(mob_of(&ctx, id).is_none_or(|e| matches!(&e.kind, Kind::Mob(m) if m.is_dead())));
}
