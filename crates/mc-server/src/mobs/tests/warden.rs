//! Sculk sensors, shriekers and the warden (minecraft.wiki, "Sculk Sensor",
//! "Sculk Shrieker", "Warden").

use super::*;
use crate::vibrations::{self, Source};
use mc_entity::mob::warden::Doing;

fn state(ctx: &ServerContext, name: &str, set: &[(&str, &str)]) -> u32 {
    let b = &ctx.data.blocks;
    let mut s = b.default_state(name).unwrap();
    for (k, v) in set {
        s = b.with_property(s, k, v).unwrap();
    }
    s
}

fn block(ctx: &ServerContext, at: [i32; 3]) -> u32 {
    ctx.world
        .block(
            mc_protocol::Position::new(at[0], at[1], at[2]),
            &ctx.data.blocks,
        )
        .unwrap()
}

fn prop(ctx: &ServerContext, at: [i32; 3], k: &str) -> String {
    ctx.data
        .blocks
        .property(block(ctx, at), k)
        .unwrap_or_default()
        .to_owned()
}

/// Vibrations and the block ticks they schedule, `n` times.
fn hear(ctx: &ServerContext, n: usize) {
    for _ in 0..n {
        lock(&ctx.time).age += 1;
        vibrations::tick(ctx);
        crate::tick::run_scheduled_ticks(ctx);
    }
}

fn at(p: [i32; 3]) -> mc_protocol::Position {
    mc_protocol::Position::new(p[0], p[1], p[2])
}

/// A flat world with no mobs spawning: one nearby could be heard instead of
/// what a test does (whether one spawns hangs on the random numbers other
/// tests draw).
fn quiet() -> Option<std::sync::Arc<ServerContext>> {
    let ctx = flat()?;
    lock(&ctx.game_rules).set("spawn_mobs", crate::rules::Value::Bool(false));
    Some(ctx)
}

fn placed(ctx: &ServerContext, p: [i32; 3], who: Source) {
    let stone = ctx.data.blocks.default_state("minecraft:stone").unwrap();
    vibrations::emit_at(ctx, "block_place", at(p), who, Some(stone));
}

#[test]
fn a_sensor_hears_a_block_placed_and_powers_by_distance() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let sensor = [0, -60, 0];
    set_state(&ctx, sensor, state(&ctx, "minecraft:sculk_sensor", &[]));
    hear(&ctx, 1);
    // Four blocks away: arrives in four ticks, power 15 - 7 = 8.
    placed(&ctx, [4, -60, 0], Source::default());
    hear(&ctx, 3);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
    hear(&ctx, 2);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "active");
    assert_eq!(prop(&ctx, sensor, "power"), "8");
    // A comparator reads the frequency: 13 for a block placed.
    assert_eq!(vibrations::comparator(&ctx, at(sensor)), Some(13));
    // 30 ticks on, 10 of cooldown, then it listens again.
    hear(&ctx, 30);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "cooldown");
    assert_eq!(prop(&ctx, sensor, "power"), "0");
    hear(&ctx, 10);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
    // Too far for a plain sensor.
    placed(&ctx, [9, -60, 0], Source::default());
    hear(&ctx, 12);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
}

#[test]
fn wool_stops_vibrations_and_wool_makes_none() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let sensor = [0, -60, 0];
    set_state(&ctx, sensor, state(&ctx, "minecraft:sculk_sensor", &[]));
    let wool = state(&ctx, "minecraft:white_wool", &[]);
    set_state(&ctx, [2, -60, 0], wool);
    hear(&ctx, 1);
    placed(&ctx, [4, -60, 0], Source::default());
    hear(&ctx, 8);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
    // Wool placed is not heard either.
    vibrations::emit_at(
        &ctx,
        "block_place",
        at([0, -60, 3]),
        Source::default(),
        Some(wool),
    );
    hear(&ctx, 8);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
    // A sneaking step is not; a normal one is.
    let sneak = Source {
        sneaking: true,
        ..Source::default()
    };
    let stone = ctx.data.blocks.default_state("minecraft:stone");
    vibrations::emit(&ctx, "step", [0.5, -60.0, 3.5], sneak, stone);
    hear(&ctx, 8);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
    vibrations::emit(&ctx, "step", [0.5, -60.0, 3.5], Source::default(), stone);
    hear(&ctx, 8);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "active");
}

#[test]
fn a_calibrated_sensor_hears_only_its_frequency_and_farther() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let sensor = [0, -60, 0];
    set_state(
        &ctx,
        sensor,
        state(
            &ctx,
            "minecraft:calibrated_sculk_sensor",
            &[("facing", "north")],
        ),
    );
    // A redstone block behind it (north) gives 15: only explosions and
    // deaths are heard.
    set_state(
        &ctx,
        [0, -60, -1],
        state(&ctx, "minecraft:redstone_block", &[]),
    );
    hear(&ctx, 1);
    placed(&ctx, [12, -60, 0], Source::default());
    hear(&ctx, 15);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "inactive");
    vibrations::emit(&ctx, "explode", [12.5, -60.0, 0.5], Source::default(), None);
    hear(&ctx, 15);
    assert_eq!(prop(&ctx, sensor, "sculk_sensor_phase"), "active");
}

#[test]
fn shriekers_warn_four_times_then_a_warden_comes() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [3.5, -60.0, 3.5]);
    set_state(
        &ctx,
        [0, -60, 0],
        state(&ctx, "minecraft:sculk_sensor", &[]),
    );
    set_state(
        &ctx,
        [0, -60, 4],
        state(&ctx, "minecraft:sculk_shrieker", &[("can_summon", "true")]),
    );
    hear(&ctx, 1);
    for level in 1..=4 {
        placed(&ctx, [2, -60, 0], Source::player(&ctx, STEVE));
        hear(&ctx, 12);
        assert_eq!(
            prop(&ctx, [0, -60, 4], "shrieking"),
            "true",
            "warning {level}"
        );
        assert_eq!(lock(&ctx.vibrations).warnings[&STEVE].level, level);
        let dark = lock(&ctx.vitals)[&STEVE]
            .effects
            .active
            .contains_key("darkness");
        assert!(dark, "no darkness at warning {level}");
        // The shriek ends and the warning's 10 seconds pass.
        hear(&ctx, 200);
    }
    let wardens: Vec<i32> = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Warden))
        .map(|e| e.id)
        .collect();
    assert_eq!(wardens.len(), 1, "no warden came");
    with_mob(&ctx, wardens[0], |m| {
        assert_eq!(m.health, 500.0);
        assert!(matches!(
            m.quirks.warden.doing,
            Doing::Emerging | Doing::Nothing
        ));
    });
}

#[test]
fn a_warden_emerges_hears_gets_angry_roars_and_fights() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    let _ = join(&ctx, [30.5, -60.0, 30.5]);
    let mut e = new_mob(&ctx, Species::Warden, [0.5, -60.0, 0.5]);
    crate::mobs::warden::emerge(&mut e);
    let warden = e.id;
    lock(&ctx.entities).push(e);
    let cow = put(&ctx, Species::Cow, [6.5, -60.0, 0.5]);
    // While it comes out, nothing hurts it.
    let mut all = lock(&ctx.entities);
    let i = all.iter().position(|e| e.id == warden).unwrap();
    let hit = crate::mobs::hurt::Harm::new(20.0, "minecraft:generic");
    assert!(!crate::mobs::hurt::harm(&ctx, &mut all[i], &hit));
    drop(all);
    let both = |ctx: &ServerContext, n: usize| {
        for _ in 0..n {
            run_all(ctx, 1);
            vibrations::tick(ctx);
        }
    };
    both(&ctx, 135);
    with_mob(&ctx, warden, |m| {
        assert_ne!(m.quirks.warden.doing, Doing::Emerging)
    });
    // Things the cow does, two seconds apart, anger it to a roar; after the
    // roar it goes for the cow and kills it.
    let mut roared = false;
    let mut chased = false;
    for t in 0..600 {
        if t % 45 == 0 && t < 180 {
            vibrations::emit(&ctx, "eat", [6.5, -60.0, 0.5], Source::entity(cow), None);
        }
        both(&ctx, 1);
        let Some((doing, target)) = mob_of(&ctx, warden).and_then(|e| match &e.kind {
            Kind::Mob(m) => Some((m.quirks.warden.doing, m.target)),
            _ => None,
        }) else {
            break;
        };
        roared |= doing == Doing::Roaring;
        chased |= roared && doing == Doing::Nothing && target == Some(cow);
        let alive = mob_of(&ctx, cow).is_some_and(|e| match &e.kind {
            Kind::Mob(m) => m.dying.is_none() && !e.removed,
            _ => false,
        });
        if !alive {
            break;
        }
    }
    assert!(roared, "it never roared");
    assert!(chased, "it never went for the cow");
    let dead = mob_of(&ctx, cow).is_none_or(|e| match &e.kind {
        Kind::Mob(m) => m.dying.is_some() || e.removed,
        _ => true,
    });
    assert!(dead, "the cow lives");
}

#[test]
fn a_warden_darkens_players_near_it() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 0.5]);
    put(&ctx, Species::Warden, [0.5, -60.0, 0.5]);
    lock(&ctx.time).age = 119;
    run_all(&ctx, 1);
    let dark = lock(&ctx.vitals)[&STEVE]
        .effects
        .active
        .get("darkness")
        .map(|i| i.duration);
    assert!(dark.is_some_and(|d| d > 200), "{dark:?}");
}

#[test]
fn a_catalyst_takes_the_experience_of_a_kill_and_spreads_sculk() {
    let Some(ctx) = quiet() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [10.5, -60.0, 0.5]);
    let catalyst = [2, -60, 0];
    set_state(&ctx, catalyst, state(&ctx, "minecraft:sculk_catalyst", &[]));
    hear(&ctx, 1);
    let mut e = new_mob(&ctx, Species::Zombie, [4.5, -60.0, 0.5]);
    if let Kind::Mob(m) = &mut e.kind {
        m.hurt_by_player = Some((STEVE, 100));
    }
    crate::mobs::hurt::die(&ctx, &mut e, Some(("minecraft:player", None)), 0);
    let orbs = lock(&ctx.entities)
        .iter()
        .filter(|e| matches!(e.kind, Kind::ExperienceOrb { .. }))
        .count();
    assert_eq!(orbs, 0, "the experience dropped");
    assert_eq!(prop(&ctx, catalyst, "bloom"), "true");
    for _ in 0..20 {
        lock(&ctx.time).age += 1;
        crate::catalysts::tick(&ctx);
    }
    assert_eq!(prop(&ctx, catalyst, "bloom"), "false");
    let sculk = ctx.data.blocks.default_state("minecraft:sculk").unwrap();
    let mut n = 0;
    for x in -4..=12 {
        for z in -8..=8 {
            for y in -63..=-60 {
                n += i32::from(block(&ctx, [x, y, z]) == sculk);
            }
        }
    }
    assert!(n >= 3, "{n} sculk");
}
