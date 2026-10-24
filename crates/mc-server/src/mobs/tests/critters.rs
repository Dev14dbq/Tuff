//! Silverfish, endermites and golems in the world.

use super::*;

fn count_of(ctx: &ServerContext, species: Species) -> usize {
    lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == species && m.dying.is_none()))
        .count()
}

fn state_named(ctx: &ServerContext, name: &str) -> u32 {
    ctx.data.blocks.default_state(name).unwrap()
}

fn normal(ctx: &ServerContext) {
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
}

fn set_rule(ctx: &ServerContext, name: &str, on: bool) {
    lock(&ctx.game_rules).set(name, crate::rules::Value::Bool(on));
}

// ---- silverfish ----

#[test]
fn a_hurt_silverfish_calls_the_ones_in_infested_blocks_out() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let infested = state_named(&ctx, "minecraft:infested_stone");
    let cells: Vec<[i32; 3]> = (3..9).map(|x| [x, -60, 2]).collect();
    for c in &cells {
        set_state(&ctx, *c, infested);
    }
    let id = put(&ctx, Species::Silverfish, [1.5, -60.0, 0.5]);
    run(&ctx, 3);
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(1.0, at));
    // A second later it calls.
    run(&ctx, 30);
    let gone = cells
        .iter()
        .filter(|c| block(&ctx, **c) != "minecraft:infested_stone")
        .count();
    assert!(gone >= 1, "no block was broken");
    // Each one broken let one out, and the caller is still there.
    assert_eq!(count_of(&ctx, Species::Silverfish), 1 + gone);
}

#[test]
fn without_mob_griefing_the_call_turns_infested_blocks_back_into_stone() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    set_rule(&ctx, "minecraft:mob_griefing", false);
    let infested = state_named(&ctx, "minecraft:infested_deepslate");
    set_state(&ctx, [3, -60, 2], infested);
    let id = put(&ctx, Species::Silverfish, [1.5, -60.0, 0.5]);
    run(&ctx, 3);
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.hit(ctx.dim, id, steve(at), blow(1.0, at));
    run(&ctx, 30);
    assert_eq!(block(&ctx, [3, -60, 2]), "minecraft:deepslate");
    assert_eq!(count_of(&ctx, Species::Silverfish), 1);
}

#[test]
fn a_silverfish_hides_in_the_stone_it_walks_by() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    // Far from the player, a floor of stone bricks under it.
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let bricks = state_named(&ctx, "minecraft:stone_bricks");
    let cells: Vec<[i32; 3]> = (14..=44)
        .flat_map(|x| (14..=44).map(move |z| [x, -61, z]))
        .collect();
    for c in &cells {
        set_state(&ctx, *c, bricks);
    }
    let id = put(&ctx, Species::Silverfish, [24.5, -60.0, 24.5]);
    // Far from everyone, but it does not go away on its own.
    if let Some(Kind::Mob(m)) = lock(&ctx.entities)
        .iter_mut()
        .find(|e| e.id == id)
        .map(|e| &mut e.kind)
    {
        m.persistent = true;
    }
    for _ in 0..40 {
        run(&ctx, 50);
        if count_of(&ctx, Species::Silverfish) == 0 {
            break;
        }
    }
    assert_eq!(count_of(&ctx, Species::Silverfish), 0, "never hid");
    let infested = cells
        .iter()
        .filter(|c| block(&ctx, **c) == "minecraft:infested_stone_bricks")
        .count();
    assert_eq!(infested, 1);
}

#[test]
fn breaking_an_infested_block_lets_a_silverfish_out_unless_silk_touch() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let infested = state_named(&ctx, "minecraft:infested_cobblestone");
    let at = mc_protocol::Position::new(1, -60, 0);
    let actor = crate::interact::Actor {
        uuid,
        entity_id: 7000,
        game_mode: 0,
        pose: crate::context::Pose {
            x: 0.5,
            y: -60.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        },
        sneaking: false,
    };
    set_state(&ctx, [1, -60, 0], infested);
    crate::interact::dig(&ctx, actor, 2, at, 1, 1);
    assert_eq!(block(&ctx, [1, -60, 0]), "minecraft:air");
    assert_eq!(count_of(&ctx, Species::Silverfish), 1);
    // With Silk Touch it stays quiet: the block itself is the drop.
    lock(&ctx.entities).clear();
    set_state(&ctx, [1, -60, 0], infested);
    let pick = crate::testkit::stack(&ctx, "diamond_pickaxe", &[("silk_touch", 1)]);
    {
        let mut inv = lock(&ctx.inventories);
        let hands = inv.entry(uuid).or_default();
        let slot = hands.hand_slot(0);
        hands.slots[slot] = Some(pick);
    }
    crate::interact::dig(&ctx, actor, 2, at, 1, 2);
    assert_eq!(block(&ctx, [1, -60, 0]), "minecraft:air");
    assert_eq!(count_of(&ctx, Species::Silverfish), 0);
}

#[test]
fn a_spawner_with_silverfish_makes_them() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let at = [3, -60, 3];
    set_state(&ctx, at, state_named(&ctx, "minecraft:spawner"));
    let mut entity = mc_nbt::Compound::new();
    entity.insert("id", mc_nbt::Tag::String("minecraft:silverfish".into()));
    let mut data = mc_nbt::Compound::new();
    data.insert("entity", mc_nbt::Tag::Compound(entity));
    let mut be = mc_nbt::Compound::new();
    be.insert("SpawnData", mc_nbt::Tag::Compound(data));
    be.insert("Delay", mc_nbt::Tag::Short(1));
    be.insert("MinSpawnDelay", mc_nbt::Tag::Short(200));
    be.insert("MaxSpawnDelay", mc_nbt::Tag::Short(800));
    be.insert("SpawnCount", mc_nbt::Tag::Short(16));
    be.insert("MaxNearbyEntities", mc_nbt::Tag::Short(6));
    be.insert("RequiredPlayerRange", mc_nbt::Tag::Short(16));
    be.insert("SpawnRange", mc_nbt::Tag::Short(4));
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: at[0],
            y: at[1],
            z: at[2],
            id: "minecraft:mob_spawner".into(),
            data: be,
        },
        &ctx.data.blocks,
    );
    run(&ctx, 10);
    assert!(count_of(&ctx, Species::Silverfish) >= 1, "none made");
}

// ---- golems ----

fn set_mob<R>(ctx: &ServerContext, id: i32, f: impl FnOnce(&mut Mob) -> R) -> R {
    let mut all = lock(&ctx.entities);
    let Kind::Mob(m) = &mut all.iter_mut().find(|e| e.id == id).unwrap().kind else {
        panic!()
    };
    f(m)
}

#[test]
fn iron_ingots_mend_an_iron_golem_by_a_quarter() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::IronGolem, [2.5, -60.0, 0.5]);
    set_mob(&ctx, id, |m| m.health = 30.0);
    hold(&ctx, "minecraft:iron_ingot", 4);
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.use_on(ctx.dim, id, steve(at), 0);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 55.0);
    ctx.mobs.use_on(ctx.dim, id, steve(at), 0);
    ctx.mobs.use_on(ctx.dim, id, steve(at), 0);
    run(&ctx, 1);
    assert_eq!(with_mob(&ctx, id, |m| m.health), 100.0);
    // At full health the ingot is kept.
    ctx.mobs.use_on(ctx.dim, id, steve(at), 0);
    run(&ctx, 1);
    let left = lock(&ctx.inventories)[&STEVE].main_hand().map(|s| s.count);
    assert_eq!(left, Some(1));
}

#[test]
fn shears_take_a_snow_golems_pumpkin_off_once() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let id = put(&ctx, Species::SnowGolem, [2.5, -60.0, 0.5]);
    hold(&ctx, "minecraft:shears", 1);
    let at = [0.5, -60.0, 0.5];
    ctx.mobs.use_on(ctx.dim, id, steve(at), 0);
    run(&ctx, 1);
    assert!(!with_mob(&ctx, id, |m| m.quirks.pumpkin));
    assert_eq!(items_of(&ctx, "minecraft:carved_pumpkin"), 1);
    ctx.mobs.use_on(ctx.dim, id, steve(at), 0);
    run(&ctx, 1);
    assert_eq!(items_of(&ctx, "minecraft:carved_pumpkin"), 1);
    // Clients are told: bit 16 of field 16.
    let e = mob_of(&ctx, id).unwrap();
    let Kind::Mob(m) = &e.kind else { panic!() };
    let data = metadata_of(&ctx, m, 0);
    assert!(data.contains(&(16, mc_protocol::packets::play::MetadataValue::Byte(0))));
}

#[test]
fn a_snow_golem_leaves_a_trail_of_snow() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let _ = put(&ctx, Species::SnowGolem, [10.5, -60.0, 10.5]);
    run(&ctx, 60);
    let snow = (-5..30)
        .flat_map(|x| (-5..30).map(move |z| [x, -60, z]))
        .filter(|c| block(&ctx, *c) == "minecraft:snow")
        .count();
    assert!(snow >= 1, "no snow");
    // Not with mob griefing off.
    set_rule(&ctx, "minecraft:mob_griefing", false);
    for x in -5..30 {
        for z in -5..30 {
            set_state(&ctx, [x, -60, z], ctx.shapes.air());
        }
    }
    run(&ctx, 60);
    let snow = (-5..30)
        .flat_map(|x| (-5..30).map(move |z| [x, -60, z]))
        .filter(|c| block(&ctx, *c) == "minecraft:snow")
        .count();
    assert_eq!(snow, 0);
}

#[test]
fn an_iron_golem_kills_a_zombie_and_is_not_thrown_back_by_blows() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let golem = put(&ctx, Species::IronGolem, [10.5, -60.0, 0.5]);
    let zombie = put(&ctx, Species::Zombie, [14.5, -60.0, 0.5]);
    run(&ctx, 200);
    assert!(
        mob_of(&ctx, zombie).is_none_or(|e| matches!(&e.kind, Kind::Mob(m) if m.is_dead())),
        "zombie lives"
    );
    // A player's blow pushes a zombie but not the golem.
    let still = put(&ctx, Species::IronGolem, [-10.5, -60.0, 0.5]);
    let control = put(&ctx, Species::Zombie, [-10.5, -60.0, 5.5]);
    for id in [still, control] {
        set_mob(&ctx, id, |m| m.no_ai = true);
    }
    run(&ctx, 3);
    let at = [-10.5, -58.0, 2.5];
    for id in [still, control] {
        ctx.mobs.hit(ctx.dim, id, steve(at), blow(1.0, at));
    }
    run(&ctx, 1);
    let push = |id| mob_of(&ctx, id).unwrap().vel[2].abs();
    assert!(push(control) > 0.1, "{}", push(control));
    assert!(push(still) < 1e-6, "{}", push(still));
    let _ = golem;
}

#[test]
fn iron_golems_that_players_did_not_make_go_for_players_the_villagers_scorn() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let uuid = join(&ctx, [0.5, -60.0, 0.5]);
    let golem = put(&ctx, Species::IronGolem, [5.5, -60.0, 0.5]);
    let made = put(&ctx, Species::IronGolem, [-5.5, -60.0, 0.5]);
    set_mob(&ctx, made, |m| m.quirks.player_made = true);
    run(&ctx, 100);
    assert_eq!(lock(&ctx.vitals)[&uuid].health, 20.0);
    ctx.mobs.offend(uuid, 125);
    assert!(ctx.mobs.notorious(uuid));
    run(&ctx, 100);
    assert!(lock(&ctx.vitals)[&uuid].health < 20.0);
    let _ = golem;
    assert_eq!(with_mob(&ctx, made, |m| m.target), None);
    // Time heals the name: 125 down to 85 in two days.
    ctx.mobs.forgive();
    assert!(ctx.mobs.notorious(uuid));
    ctx.mobs.forgive();
    assert!(!ctx.mobs.notorious(uuid));
}

#[test]
fn golems_stay_in_peaceful_and_far_from_players_and_are_saved() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(0, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let iron = put(&ctx, Species::IronGolem, [60.5, -60.0, 0.5]);
    let snow = put(&ctx, Species::SnowGolem, [-60.5, -60.0, 0.5]);
    set_mob(&ctx, iron, |m| m.idle = 5000);
    for _ in 0..30 {
        run(&ctx, 100);
    }
    assert!(mob_of(&ctx, iron).is_some() && mob_of(&ctx, snow).is_some());
    let e = mob_of(&ctx, iron).unwrap();
    let Kind::Mob(m) = &e.kind else { panic!() };
    let mut c = mc_nbt::Compound::new();
    set_mob(&ctx, iron, |m| m.quirks.player_made = true);
    let e = mob_of(&ctx, iron).unwrap();
    let Kind::Mob(m2) = &e.kind else { panic!() };
    crate::mobs::nbt::write(&ctx, m2, &mut c);
    assert_eq!(c.get("PlayerCreated"), Some(&mc_nbt::Tag::Byte(1)));
    let mut again = Mob::new(Species::IronGolem, 1);
    crate::mobs::nbt::apply(&ctx, &mut again, &c);
    assert!(again.quirks.player_made);
    assert!(!m.quirks.player_made);
    // Its loot table: poppies and iron.
    set_mob(&ctx, iron, |m| m.health = 1.0);
    ctx.mobs.hit(
        ctx.dim,
        iron,
        steve([60.5, -60.0, 0.5]),
        blow(1000.0, [60.5, -60.0, 0.5]),
    );
    ctx.mobs
        .harm(ctx.dim, iron, 1000.0, "minecraft:generic", Some(STEVE));
    run(&ctx, 2);
    assert!(items_of(&ctx, "minecraft:iron_ingot") >= 3);
}

// ---- endermites ----

#[test]
fn an_endermite_vanishes_at_the_end_of_its_life_unless_named() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    normal(&ctx);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let old = put(&ctx, Species::Endermite, [20.5, -60.0, 0.5]);
    let named = put(&ctx, Species::Endermite, [21.5, -60.0, 0.5]);
    for e in lock(&ctx.entities).iter_mut() {
        if let Kind::Mob(m) = &mut e.kind {
            m.quirks.lifetime = 2395;
            if e.id == named {
                m.custom_name = Some("Pearl".into());
                m.persistent = true;
            }
        }
    }
    run(&ctx, 10);
    assert!(mob_of(&ctx, old).is_none(), "still there");
    assert!(mob_of(&ctx, named).is_some());
}

#[test]
fn an_endermite_lifetime_is_saved_as_vanilla_does() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let mut e = crate::mobs::new_mob(&ctx, Species::Endermite, [0.5, -60.0, 0.5]);
    let Kind::Mob(m) = &mut e.kind else { panic!() };
    m.quirks.lifetime = 77;
    let mut c = mc_nbt::Compound::new();
    crate::mobs::nbt::write(&ctx, m, &mut c);
    assert_eq!(c.get("Lifetime"), Some(&mc_nbt::Tag::Int(77)));
    let mut again = Mob::new(Species::Endermite, 3);
    crate::mobs::nbt::apply(&ctx, &mut again, &c);
    assert_eq!(again.quirks.lifetime, 77);
}
