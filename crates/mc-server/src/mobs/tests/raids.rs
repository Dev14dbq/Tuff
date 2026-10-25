//! Raids and patrols (minecraft.wiki, "Raid", "Patrol").

use super::*;
use crate::effects::{self, Instance};
use crate::raids::{self, Status};

/// A bell somebody holds a ticket of: a village around `at`.
fn village(ctx: &ServerContext, at: [i32; 3]) {
    let bell = ctx.data.blocks.default_state("minecraft:bell").unwrap();
    set_state(ctx, at, bell);
    crate::poi::sync(ctx);
    assert!(crate::poi::claim(ctx, at), "no meeting point at the bell");
}

/// Loads the chunks within `r` of the origin.
fn load(ctx: &ServerContext, r: i32) {
    for cx in -r..=r {
        for cz in -r..=r {
            let _ = ctx.world.block(
                mc_protocol::Position::new(cx * 16, 0, cz * 16),
                &ctx.data.blocks,
            );
        }
    }
}

fn raiders(ctx: &ServerContext, raid: i32) -> Vec<i32> {
    lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter(|e| {
            matches!(&e.kind, Kind::Mob(m)
                if m.dying.is_none() && m.quirks.illager.raid == Some(raid))
        })
        .map(|e| e.id)
        .collect()
}

fn kill(ctx: &ServerContext, ids: &[i32]) {
    for e in lock(&ctx.entities).iter_mut() {
        if ids.contains(&e.id) {
            e.removed = true;
        }
    }
}

fn status(ctx: &ServerContext, id: i32) -> Option<Status> {
    lock(&ctx.raids)
        .raids
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.status)
}

fn has(ctx: &ServerContext, effect: &str) -> Option<i32> {
    lock(&ctx.vitals)
        .get(&STEVE)
        .and_then(|v| v.effects.active.get(effect))
        .map(|i| i.amplifier)
}

fn raid_ticks(ctx: &ServerContext, n: usize) {
    for _ in 0..n {
        lock(&ctx.time).age += 1;
        raids::tick(ctx);
    }
}

fn set_difficulty(ctx: &ServerContext, d: i32) {
    ctx.difficulty
        .store(d, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn bad_omen_in_a_village_becomes_raid_omen_and_then_a_raid() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_difficulty(&ctx, 2);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    load(&ctx, 4);
    village(&ctx, [2, -60, 2]);
    let bad = effects::kind_of("bad_omen").unwrap();
    assert!(effects::give(
        &ctx,
        STEVE,
        bad,
        Instance::new(1, 120_000, false)
    ));
    raid_ticks(&ctx, 1);
    assert_eq!(has(&ctx, "bad_omen"), None);
    assert_eq!(has(&ctx, "raid_omen"), Some(1));
    assert!(
        lock(&ctx.raids).raids.is_empty(),
        "the raid waits for the omen"
    );
    // Raid Omen lasts 30 seconds; then the raid of level II starts.
    for _ in 0..600 {
        let now = {
            let mut t = lock(&ctx.time);
            t.age += 1;
            t.age
        };
        effects::tick(&ctx, now);
        raids::tick(&ctx);
    }
    let (id, level, waves) = {
        let state = lock(&ctx.raids);
        let r = state.raids.first().expect("no raid started");
        assert!(r.viewers.contains(&STEVE), "Steve sees no bar");
        (r.id, r.level, r.waves(2))
    };
    assert_eq!(level, 2);
    // Normal: five waves and the bonus one for a level above I.
    assert_eq!(waves, 6);
    assert_eq!(has(&ctx, "raid_omen"), None);
    // Fifteen seconds of quiet, then the first wave with its captain.
    raid_ticks(&ctx, 302);
    let wave = raiders(&ctx, id);
    assert!(wave.len() >= 4, "{} raiders", wave.len());
    let banner = ctx.items.id("minecraft:white_banner").unwrap();
    let captains = wave
        .iter()
        .filter(|&&r| {
            with_mob(&ctx, r, |m| {
                m.quirks.illager.patrol_leader
                    && m.persistent
                    && m.gear[mc_entity::mob::slot::HEAD]
                        .as_ref()
                        .is_some_and(|g| g.item == banner)
            })
        })
        .count();
    assert_eq!(captains, 1);
}

#[test]
fn killing_every_wave_wins_and_makes_heroes() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_difficulty(&ctx, 1);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    load(&ctx, 4);
    village(&ctx, [2, -60, 2]);
    let id = raids::start(&ctx, [2, -60, 2], 1, STEVE).expect("no raid");
    let mut waves = 0;
    for _ in 0..2000 {
        raid_ticks(&ctx, 1);
        let alive = raiders(&ctx, id);
        if !alive.is_empty() {
            waves += 1;
            kill(&ctx, &alive);
        }
        if status(&ctx, id) != Some(Status::Ongoing) {
            break;
        }
    }
    // Easy: three waves, no bonus at level I.
    assert_eq!(waves, 3);
    assert_eq!(status(&ctx, id), Some(Status::Victory));
    assert_eq!(has(&ctx, "hero_of_the_village"), Some(0));
    // It lingers half a minute, then ends.
    raid_ticks(&ctx, 600);
    assert_eq!(status(&ctx, id), None);
}

#[test]
fn a_raid_is_lost_when_nobody_is_left_in_the_village() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_difficulty(&ctx, 2);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    load(&ctx, 4);
    village(&ctx, [2, -60, 2]);
    let id = raids::start(&ctx, [2, -60, 2], 1, STEVE).expect("no raid");
    raid_ticks(&ctx, 302);
    let wave = raiders(&ctx, id);
    assert!(!wave.is_empty());
    crate::poi::release(&ctx, [2, -60, 2]);
    raid_ticks(&ctx, 20);
    assert_eq!(status(&ctx, id), Some(Status::Loss));
    assert!(with_mob(&ctx, wave[0], |m| m.quirks.illager.celebrating));
    assert_eq!(has(&ctx, "hero_of_the_village"), None);
    raid_ticks(&ctx, 600);
    assert_eq!(status(&ctx, id), None);
    assert!(with_mob(&ctx, wave[0], |m| {
        m.quirks.illager.raid.is_none() && !m.quirks.illager.celebrating
    }));
}

#[test]
fn raiders_march_on_the_village() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_difficulty(&ctx, 2);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    // Creative, so nobody fights and they only march.
    lock(&ctx.players).get_mut(&STEVE).unwrap().game_mode = 1;
    load(&ctx, 4);
    village(&ctx, [2, -60, 2]);
    let id = raids::start(&ctx, [2, -60, 2], 1, STEVE).expect("no raid");
    raid_ticks(&ctx, 302);
    let wave = raiders(&ctx, id);
    let far = |ctx: &ServerContext, r: i32| {
        let e = mob_of(ctx, r).unwrap();
        ((e.pos[0] - 2.5).powi(2) + (e.pos[2] - 2.5).powi(2)).sqrt()
    };
    let before = far(&ctx, wave[0]);
    assert!(before > 16.0, "spawned {before} blocks away");
    run_all(&ctx, 300);
    let after = far(&ctx, wave[0]);
    assert!(after < before - 8.0, "{before} -> {after}");
}

#[test]
fn a_patrol_has_a_captain_and_keeps_together() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_difficulty(&ctx, 2);
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    load(&ctx, 4);
    let n = raids::try_patrol(&ctx, [0.5, -60.0, 0.5]);
    assert!(n >= 2, "{n} pillagers");
    let patrol: Vec<(i32, bool, Option<[f64; 3]>)> = lock(&ctx.entities)
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Mob(m) if m.species == Species::Pillager => Some((
                e.id,
                m.quirks.illager.patrol_leader,
                m.quirks.illager.patrol_target,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(patrol.iter().filter(|p| p.1).count(), 1);
    let target = patrol
        .iter()
        .find(|p| p.1)
        .unwrap()
        .2
        .expect("the captain goes nowhere");
    lock(&ctx.time).age = 19;
    raid_ticks(&ctx, 1);
    for (id, _, _) in &patrol {
        assert_eq!(
            with_mob(&ctx, *id, |m| m.quirks.illager.patrol_target),
            Some(target)
        );
    }
    // Near a village there is no patrol.
    village(&ctx, [2, -60, 2]);
    assert_eq!(raids::try_patrol(&ctx, [0.5, -60.0, 0.5]), 0);
}

#[test]
fn a_patrol_captain_leaves_an_ominous_bottle() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    let mut e = new_mob(&ctx, Species::Pillager, [3.5, -60.0, 0.5]);
    if let Kind::Mob(m) = &mut e.kind {
        m.quirks.illager.patrol_leader = true;
        m.quirks.illager.patrolling = true;
        m.gear[mc_entity::mob::slot::HEAD] = raids::ominous_banner(&ctx);
        m.drop_chance[mc_entity::mob::slot::HEAD] = 2.0;
        m.hurt_by_player = Some((STEVE, 100));
    }
    crate::mobs::hurt::die(&ctx, &mut e, Some(("minecraft:player", None)), 0);
    let bottle = ctx.items.id("minecraft:ominous_bottle").unwrap();
    let banner = ctx.items.id("minecraft:white_banner").unwrap();
    let dropped: Vec<i32> = lock(&ctx.entities)
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::Item { item, .. } => Some(*item),
            _ => None,
        })
        .collect();
    assert!(dropped.contains(&bottle), "{dropped:?}");
    assert!(dropped.contains(&banner), "{dropped:?}");
}
