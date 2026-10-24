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
