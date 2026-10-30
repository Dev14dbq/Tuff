//! Game rules that steer mobs: `spawner_blocks_work`, `max_entity_cramming`
//! and `spawn_phantoms`.

use super::*;
use crate::rules::Value;

fn rule(ctx: &ServerContext, name: &str, value: Value) {
    lock(&ctx.game_rules).set(name, value);
}

fn silverfish_spawner(ctx: &ServerContext, at: [i32; 3]) {
    set_state(
        ctx,
        at,
        ctx.data.blocks.default_state("minecraft:spawner").unwrap(),
    );
    let mut entity = mc_nbt::Compound::new();
    entity.insert("id", mc_nbt::Tag::String("minecraft:silverfish".into()));
    let mut data = mc_nbt::Compound::new();
    data.insert("entity", mc_nbt::Tag::Compound(entity));
    let mut be = mc_nbt::Compound::new();
    be.insert("SpawnData", mc_nbt::Tag::Compound(data));
    be.insert("Delay", mc_nbt::Tag::Short(1));
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
}

#[test]
fn spawners_stop_and_refuse_eggs_while_spawner_blocks_work_is_off() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let at = [3, -60, 3];
    silverfish_spawner(&ctx, at);
    rule(&ctx, "spawner_blocks_work", Value::Bool(false));
    run(&ctx, 10);
    assert!(mobs_of(&ctx, Species::Silverfish).is_empty());

    // An egg on it: refused with vanilla's message, the egg kept. (Natural
    // spawning may have put a cow in the flat world already; only the egg is
    // being judged.)
    let cows = mobs_of(&ctx, Species::Cow).len();
    hold(&ctx, "minecraft:cow_spawn_egg", 2);
    let out = crate::interact::use_item_on(
        &ctx,
        actor_at([0.5, -60.0, 0.5]),
        0,
        mc_protocol::Position::new(at[0], at[1], at[2]),
        1,
        [0.5, 1.0, 0.5],
        1,
    );
    let told = out.events.iter().any(|e| {
        matches!(e, Event::Message { line, overlay: false, .. }
            if line == &mc_protocol::text::translate("advMode.notEnabled.spawner", Vec::new()))
    });
    assert!(told, "no message");
    assert_eq!(mobs_of(&ctx, Species::Cow).len(), cows);
    let left = lock(&ctx.inventories)[&STEVE].main_hand().map(|s| s.count);
    assert_eq!(left, Some(2));
    let be = ctx
        .world
        .block_entity(
            mc_protocol::Position::new(at[0], at[1], at[2]),
            &ctx.data.blocks,
        )
        .unwrap();
    let kind = be
        .data
        .get("SpawnData")
        .and_then(|t| match t {
            mc_nbt::Tag::Compound(c) => c.get("entity").cloned(),
            _ => None,
        })
        .and_then(|t| match t {
            mc_nbt::Tag::Compound(c) => c.get("id").cloned(),
            _ => None,
        });
    assert_eq!(
        kind,
        Some(mc_nbt::Tag::String("minecraft:silverfish".into()))
    );

    rule(&ctx, "spawner_blocks_work", Value::Bool(true));
    run(&ctx, 10);
    assert!(!mobs_of(&ctx, Species::Silverfish).is_empty());
}

/// How many of `n` pigs penned in one block get hurt, with the rule at `limit`.
fn crushed_at(limit: i32, n: usize) -> usize {
    let ctx = flat().unwrap();
    rule(&ctx, "max_entity_cramming", Value::Int(limit));
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        for y in -60..=-59 {
            set(&ctx, [dx, y, dz], "minecraft:stone");
        }
    }
    for _ in 0..n {
        put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    }
    run(&ctx, 60);
    mobs_of(&ctx, Species::Pig)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.health < m.max_health))
        .count()
}

#[test]
fn max_entity_cramming_sets_the_crowd_and_zero_turns_it_off() {
    if flat().is_none() {
        eprintln!("skipped: no game data");
        return;
    }
    // At 4: three others are fine, four crush.
    assert_eq!(crushed_at(4, 4), 0);
    assert!(crushed_at(4, 6) > 0);
    // At 0 nothing is ever crushed.
    assert_eq!(crushed_at(0, 30), 0);
}

#[test]
fn a_player_in_the_crowd_counts() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    // Two pigs and Steve in one spot: with the rule at 2 each pig has two
    // others in its space.
    rule(&ctx, "max_entity_cramming", Value::Int(2));
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        for y in -60..=-59 {
            set(&ctx, [dx, y, dz], "minecraft:stone");
        }
    }
    put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    run(&ctx, 60);
    let hurt = mobs_of(&ctx, Species::Pig)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.health < m.max_health))
        .count();
    assert!(hurt > 0);
}

fn phantoms_after(on: bool, game_mode: i32) -> usize {
    let ctx = flat().unwrap();
    // Hard, a long-lived world: the regional difficulty always passes.
    ctx.difficulty
        .store(3, std::sync::atomic::Ordering::Relaxed);
    lock(&ctx.time).age = 3_000_000;
    set_day(&ctx, 18_000);
    rule(&ctx, "spawn_phantoms", Value::Bool(on));
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    if let Some(p) = lock(&ctx.players).get_mut(&STEVE) {
        p.game_mode = game_mode;
    }
    // Statistics only count for players whose file was loaded at join.
    crate::stats::load(&ctx, STEVE);
    crate::stats::custom(&ctx, STEVE, "time_since_rest", 1_000_000_000);
    run(&ctx, 3);
    mobs_of(&ctx, Species::Phantom).len()
}

#[test]
fn phantoms_come_only_with_spawn_phantoms_on() {
    if flat().is_none() {
        eprintln!("skipped: no game data");
        return;
    }
    assert!(phantoms_after(true, 0) > 0, "no phantoms");
    assert_eq!(phantoms_after(false, 0), 0);
    // Vanilla passes over spectators only: creative players draw them too.
    assert!(phantoms_after(true, 1) > 0, "none for a creative player");
}
