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
    assert!(with_mob(&ctx, id, |m| m.quirks.air.dup_cd) > 5000);
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

#[test]
fn a_bee_takes_nectar_home_and_the_hive_gives_honey() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [3.5, -60.0, 12.5]);
    set(&ctx, [0, -60, 0], "minecraft:dandelion");
    set(&ctx, [6, -60, 6], "minecraft:bee_nest");
    crate::poi::sync(&ctx);
    let id = put(&ctx, Species::Bee, [2.5, -60.0, 2.5]);
    let hive = mc_protocol::Position::new(6, -60, 6);
    let mut nectar = false;
    let mut entered = false;
    for _ in 0..2500 {
        run(&ctx, 1);
        crate::poi::sync(&ctx);
        nectar |= mob_of(&ctx, id)
            .is_some_and(|e| matches!(&e.kind, Kind::Mob(m) if m.quirks.air.nectar));
        if mob_of(&ctx, id).is_none() {
            entered = true;
            break;
        }
    }
    assert!(nectar, "no nectar");
    assert!(entered, "never went in");
    assert_eq!(crate::mobs::bees::inside(&ctx, hive).len(), 1);
    // After its stay it comes out, and the honey level is up.
    for _ in 0..2500 {
        run(&ctx, 1);
    }
    assert_eq!(count_of(&ctx, Species::Bee), 1, "never came out");
    let level = ctx
        .world
        .block(hive, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.property(s, "honey_level"))
        .and_then(|v| v.parse::<i32>().ok());
    assert!(level.is_some_and(|l| l >= 1), "honey {level:?}");
}

#[test]
fn a_robbed_hive_lets_angry_bees_out_unless_a_campfire_smokes_them() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let uuid = join(&ctx, [3.5, -60.0, 12.5]);
    set(&ctx, [6, -60, 6], "minecraft:beehive");
    let hive = mc_protocol::Position::new(6, -60, 6);
    let bee = put(&ctx, Species::Bee, [6.5, -60.0, 7.5]);
    let mut entities = lock(&ctx.entities);
    let i = entities.iter().position(|e| e.id == bee).unwrap();
    assert!(crate::mobs::bees::enter(&ctx, &mut entities, i, hive));
    drop(entities);
    // A lit campfire five blocks down keeps them in.
    set(&ctx, [6, -64, 6], "minecraft:campfire");
    crate::mobs::bees::disturbed(&ctx, hive, Some(uuid), true);
    assert_eq!(crate::mobs::bees::inside(&ctx, hive).len(), 1);
    set_state(&ctx, [6, -64, 6], ctx.shapes.air());
    crate::mobs::bees::disturbed(&ctx, hive, Some(uuid), true);
    assert!(crate::mobs::bees::inside(&ctx, hive).is_empty());
    run(&ctx, 1);
    let angry = lock(&ctx.entities)
        .iter()
        .any(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Bee && m.anger > 0));
    assert!(angry, "no angry bee");
}

fn set_time(ctx: &ServerContext, t: i64) {
    let mut time = lock(&ctx.time);
    for c in &mut time.clocks {
        c.ticks = t;
    }
    if time.clocks.is_empty() {
        time.age = t;
    }
}

fn heart_state(ctx: &ServerContext, at: [i32; 3]) -> String {
    let p = mc_protocol::Position::new(at[0], at[1], at[2]);
    let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
    ctx.data
        .blocks
        .property(s, "creaking_heart_state")
        .unwrap()
        .to_owned()
}

#[test]
fn a_creaking_heart_wakes_at_night_sends_a_creaking_and_sleeps_by_day() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 20.5]);
    set(&ctx, [10, -59, 10], "minecraft:pale_oak_log");
    set(&ctx, [10, -60, 10], "minecraft:creaking_heart");
    crate::mobs::creaking::placed(&ctx, mc_protocol::Position::new(10, -60, 10));
    set(&ctx, [10, -61, 10], "minecraft:pale_oak_log");
    set_time(&ctx, 6000);
    run(&ctx, 5);
    assert_eq!(heart_state(&ctx, [10, -60, 10]), "dormant");
    set_time(&ctx, 18000);
    for _ in 0..200 {
        run(&ctx, 1);
    }
    assert_eq!(heart_state(&ctx, [10, -60, 10]), "awake");
    assert_eq!(count_of(&ctx, Species::Creaking), 1, "no creaking");
    let id = lock(&ctx.entities)
        .iter()
        .find(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Creaking))
        .map(|e| e.id)
        .unwrap();
    assert_eq!(
        with_mob(&ctx, id, |m| m.quirks.air.home),
        Some([10, -60, 10])
    );
    // A hit at night grows resin on a log by the heart.
    ctx.mobs
        .harm(ctx.dim, id, 5.0, "minecraft:generic", Some(STEVE));
    run(&ctx, 2);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 1.0);
    // Day comes: the heart sleeps and the creaking is gone.
    set_time(&ctx, 1000);
    run(&ctx, 5);
    assert_eq!(heart_state(&ctx, [10, -60, 10]), "dormant");
    assert_eq!(count_of(&ctx, Species::Creaking), 0, "creaking stayed");
}

#[test]
fn breaking_a_heart_kills_its_creaking() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 20.5]);
    set(&ctx, [10, -59, 10], "minecraft:pale_oak_log");
    set(&ctx, [10, -60, 10], "minecraft:creaking_heart");
    crate::mobs::creaking::placed(&ctx, mc_protocol::Position::new(10, -60, 10));
    set(&ctx, [10, -61, 10], "minecraft:pale_oak_log");
    set_time(&ctx, 18000);
    for _ in 0..200 {
        run(&ctx, 1);
    }
    assert_eq!(count_of(&ctx, Species::Creaking), 1);
    let p = mc_protocol::Position::new(10, -60, 10);
    let state = ctx.world.block(p, &ctx.data.blocks).unwrap();
    crate::mobs::creaking::broken(&ctx, p, state, Some(STEVE));
    set_state(&ctx, [10, -60, 10], ctx.shapes.air());
    run(&ctx, 30);
    assert_eq!(count_of(&ctx, Species::Creaking), 0);
}

fn chest_slots(
    ctx: &ServerContext,
    at: [i32; 3],
) -> Vec<Option<mc_protocol::packets::play::ItemStack>> {
    let p = mc_protocol::Position::new(at[0], at[1], at[2]);
    let name = ctx
        .world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        .unwrap()
        .to_owned();
    crate::containers::slots(ctx, p, crate::containers::kind_of(&name).unwrap())
}

fn place_chest(ctx: &ServerContext, at: [i32; 3], name: &str) {
    set(ctx, at, name);
    let p = mc_protocol::Position::new(at[0], at[1], at[2]);
    let state = ctx.world.block(p, &ctx.data.blocks).unwrap();
    crate::containers::placed(ctx, p, state);
}

#[test]
fn a_carved_pumpkin_on_copper_makes_a_golem_and_a_copper_chest() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [20.5, -60.0, 10.5]);
    set(&ctx, [20, -60, 0], "minecraft:exposed_copper");
    set(&ctx, [20, -59, 0], "minecraft:carved_pumpkin");
    let p = mc_protocol::Position::new(20, -59, 0);
    let state = ctx.world.block(p, &ctx.data.blocks).unwrap();
    crate::mobs::golems::placed(&ctx, p, state);
    assert_eq!(block(&ctx, [20, -60, 0]), "minecraft:exposed_copper_chest");
    assert_eq!(block(&ctx, [20, -59, 0]), "minecraft:air");
    let golem = lock(&ctx.entities)
        .iter()
        .find(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::CopperGolem))
        .map(|e| e.id)
        .expect("no golem");
    assert_eq!(with_mob(&ctx, golem, |m| m.quirks.air.weather), 1);
}

#[test]
fn a_copper_golem_carries_items_from_the_copper_chest_to_a_chest_that_holds_them() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [3.5, -60.0, 12.5]);
    place_chest(&ctx, [0, -60, 0], "minecraft:copper_chest");
    place_chest(&ctx, [8, -60, 0], "minecraft:chest");
    place_chest(&ctx, [8, -60, 4], "minecraft:chest");
    let cobble = ctx.items.id("minecraft:cobblestone").unwrap();
    let dirt = ctx.items.id("minecraft:dirt").unwrap();
    let copper = mc_protocol::Position::new(0, -60, 0);
    crate::containers::put(
        &ctx,
        copper,
        crate::containers::kind_of("minecraft:copper_chest").unwrap(),
        &[(
            0,
            Some(mc_protocol::packets::play::ItemStack::new(cobble, 40)),
        )],
    );
    // One chest holds dirt (no good), the other is empty.
    crate::containers::put(
        &ctx,
        mc_protocol::Position::new(8, -60, 4),
        crate::containers::kind_of("minecraft:chest").unwrap(),
        &[(0, Some(mc_protocol::packets::play::ItemStack::new(dirt, 5)))],
    );
    let id = put(&ctx, Species::CopperGolem, [2.5, -60.0, 0.5]);
    for _ in 0..1500 {
        run(&ctx, 1);
    }
    let in_copper: i32 = chest_slots(&ctx, [0, -60, 0])
        .iter()
        .flatten()
        .map(|s| s.count)
        .sum();
    let in_far: i32 = chest_slots(&ctx, [8, -60, 0])
        .iter()
        .flatten()
        .filter(|s| s.item == cobble)
        .map(|s| s.count)
        .sum();
    let in_dirt: i32 = chest_slots(&ctx, [8, -60, 4])
        .iter()
        .flatten()
        .filter(|s| s.item == cobble)
        .map(|s| s.count)
        .sum();
    assert_eq!(in_dirt, 0, "put cobblestone in the dirt chest");
    assert!(in_far >= 16, "moved only {in_far}");
    assert_eq!(
        in_copper + in_far + with_mob(&ctx, id, |m| m.quirks.air.carried.map_or(0, |c| c.1)),
        40
    );
}

#[test]
fn a_copper_golem_weathers_and_wax_stops_it() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [3.5, -60.0, 12.5]);
    let id = put(&ctx, Species::CopperGolem, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    run(&ctx, 2);
    let now = lock(&ctx.time).age;
    set_mob(&ctx, id, |m| m.quirks.air.next_weather = now + 3);
    run(&ctx, 10);
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.weather), 1);
    // Honeycomb waxes it.
    let _ = steve_at(&ctx, 0.5, false);
    hold(&ctx, "minecraft:honeycomb", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 3.5]), 0);
    run(&ctx, 2);
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.next_weather), -2);
    run(&ctx, 5);
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.weather), 1);
}

#[test]
fn a_happy_ghast_takes_a_harness_carries_four_and_gives_it_back_to_shears() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let uuid = join(&ctx, [0.5, -60.0, 8.5]);
    let _ = uuid;
    let id = put(&ctx, Species::HappyGhast, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.no_ai = true);
    // No harness, no ride.
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    assert!(with_mob(&ctx, id, |m| m.riders.is_empty()));
    hold(&ctx, "minecraft:white_harness", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    assert!(with_mob(&ctx, id, |m| m.gear[mc_entity::mob::slot::BODY].is_some()));
    hold(&ctx, "minecraft:air", 0);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    assert_eq!(with_mob(&ctx, id, |m| m.riders.len()), 1);
    // Shears take the harness off.
    hold(&ctx, "minecraft:shears", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    assert!(with_mob(&ctx, id, |m| m.gear[mc_entity::mob::slot::BODY].is_none()));
    assert_eq!(items_of(&ctx, "minecraft:white_harness"), 1);
}

#[test]
fn a_snowball_shortens_a_ghastlings_growing_up() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 8.5]);
    let id = put(&ctx, Species::HappyGhast, [0.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| {
        m.no_ai = true;
        m.set_baby(true);
    });
    assert_eq!(with_mob(&ctx, id, |m| m.size()), (0.95, 0.95));
    ctx.mobs.harm(ctx.dim, id, 0.0, "minecraft:thrown", None);
    run(&ctx, 2);
    let left = with_mob(&ctx, id, |m| m.baby_age);
    assert!((-21600..=-21500).contains(&left), "baby_age {left}");
}

#[test]
fn a_bat_hangs_from_the_ceiling_until_a_player_comes_near() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [30.5, -60.0, 30.5]);
    // A low room, so the bat flits about under its ceiling.
    for x in -6..=6 {
        for z in -6..=6 {
            set(&ctx, [x, -59, z], "minecraft:stone");
            if x.abs() == 6 || z.abs() == 6 {
                set(&ctx, [x, -60, z], "minecraft:stone");
            }
        }
    }
    let id = put(&ctx, Species::Bat, [0.5, -60.0, 0.5]);
    let mut hung = false;
    for _ in 0..1200 {
        run(&ctx, 1);
        if with_mob(&ctx, id, |m| m.quirks.air.hanging) {
            hung = true;
            break;
        }
    }
    assert!(hung, "never hung");
    let meta = crate::mobs::metadata_packet(&ctx, &mob_of(&ctx, id).unwrap()).unwrap();
    assert!(
        meta.entries
            .contains(&(16, mc_protocol::packets::play::MetadataValue::Byte(1)))
    );
    // It hangs right under the ceiling until a player three blocks off
    // wakes it.
    let at = mob_of(&ctx, id).unwrap().pos;
    assert!((at[1] - (-59.0 - 0.9)).abs() < 1e-6, "hangs at {at:?}");
    run(&ctx, 20);
    assert_eq!(mob_of(&ctx, id).unwrap().pos, at, "moved while hanging");
    move_steve(&ctx, [at[0], -60.0, at[2] + 3.0]);
    run(&ctx, 5);
    assert!(
        !with_mob(&ctx, id, |m| m.quirks.air.hanging),
        "still hanging"
    );
}

#[test]
fn a_flying_bat_flits_about_instead_of_hanging_in_the_air() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [12.5, -60.0, 0.5]);
    let start = [0.5, -52.0, 0.5];
    let id = put(&ctx, Species::Bat, start);
    let (mut far, mut path, mut cells) = (0.0f64, 0.0f64, std::collections::HashSet::new());
    let mut last = start;
    for _ in 0..200 {
        run(&ctx, 1);
        let pos = mob_of(&ctx, id).expect("bat still there").pos;
        let step = |a: [f64; 3], b: [f64; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        far = far.max(step(pos, start));
        path += step(pos, last);
        last = pos;
        cells.insert([pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32]);
    }
    assert!(!with_mob(&ctx, id, |m| m.quirks.air.hanging));
    // Ten seconds of flight covers ground, wanders off and keeps turning.
    assert!(path > 20.0, "flew only {path:.2} blocks");
    assert!(far > 3.0, "never got more than {far:.2} blocks away");
    assert!(cells.len() >= 10, "visited only {} cells", cells.len());
}

#[test]
fn bats_spawn_in_the_dark_as_ambient_mobs() {
    assert_eq!(category(Species::Bat), crate::fx::category::AMBIENT);
    assert!(Species::Bat.flies());
    assert_eq!(Species::Bat.xp_reward(), (0, 0));
}

#[test]
fn a_sulfur_cube_swallows_a_block_flies_off_a_blow_and_gives_it_back_to_shears() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 6.5]);
    let id = put(&ctx, Species::SulfurCube, [0.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:dirt", 2);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    let regular = mc_entity::mob::cube::index_of("regular").unwrap();
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.archetype), regular);
    // It takes a blow from the south without a scratch and rolls north.
    ctx.mobs.hit(
        ctx.dim,
        id,
        steve([0.5, -60.0, 3.5]),
        blow(6.0, [0.5, -60.0, 3.5]),
    );
    run(&ctx, 6);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 8.0);
    let z = lock(&ctx.entities)
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.pos[2])
        .unwrap();
    assert!(z < 0.0, "z {z}");
    // Shears take the block out again.
    hold(&ctx, "minecraft:shears", 1);
    run(&ctx, 60);
    let at = lock(&ctx.entities)
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.pos)
        .unwrap();
    move_steve(&ctx, [at[0], at[1], at[2] + 2.0]);
    ctx.mobs
        .use_on(ctx.dim, id, steve([at[0], at[1], at[2] + 2.0]), 0);
    run(&ctx, 2);
    assert_eq!(with_mob(&ctx, id, |m| m.quirks.air.archetype), 0);
    assert_eq!(items_of(&ctx, "minecraft:dirt"), 1);
    // A small one takes none.
    let small = put(&ctx, Species::SulfurCube, [2.5, -60.0, 0.5]);
    set_mob(&ctx, small, |m| m.set_baby(true));
    hold(&ctx, "minecraft:dirt", 1);
    ctx.mobs.use_on(ctx.dim, small, steve([2.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    assert_eq!(with_mob(&ctx, small, |m| m.quirks.air.archetype), 0);
}

#[test]
fn a_cube_with_tnt_goes_off_after_it_is_lit() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 12.5]);
    let id = put(&ctx, Species::SulfurCube, [0.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:tnt", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    hold(&ctx, "minecraft:flint_and_steel", 1);
    ctx.mobs.use_on(ctx.dim, id, steve([0.5, -60.0, 4.5]), 0);
    run(&ctx, 2);
    assert!(with_mob(&ctx, id, |m| m.quirks.air.fuse) >= 0, "not lit");
    run(&ctx, 130);
    assert!(
        mob_of(&ctx, id).is_none() || with_mob(&ctx, id, |m| m.is_dead()),
        "still there"
    );
}
