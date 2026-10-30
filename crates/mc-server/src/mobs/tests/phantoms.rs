//! The phantom spawner's clock (`spawn_phantoms`).

use super::*;
use crate::rules::Value;

fn rule(ctx: &ServerContext, name: &str, value: Value) {
    lock(&ctx.game_rules).set(name, value);
}

#[test]
fn the_phantom_clock_waits_a_minute_or_two_and_stands_still_with_the_rule_off() {
    let Some(ctx) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    ctx.difficulty
        .store(3, std::sync::atomic::Ordering::Relaxed);
    lock(&ctx.time).age = 3_000_000;
    set_day(&ctx, 18_000);
    rule(&ctx, "spawn_phantoms", Value::Bool(true));
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    crate::stats::load(&ctx, STEVE);
    crate::stats::custom(&ctx, STEVE, "time_since_rest", 1_000_000_000);
    run(&ctx, 3);
    assert!(!mobs_of(&ctx, Species::Phantom).is_empty(), "first visit");
    let clear = || {
        lock(&ctx.entities)
            .retain(|e| !matches!(&e.kind, Kind::Mob(m) if m.species == Species::Phantom));
    };
    clear();
    // At least 1199 ticks to the next one, however the roll falls.
    run(&ctx, 1100);
    assert!(mobs_of(&ctx, Species::Phantom).is_empty(), "too soon");
    // Time with the rule off does not count: without that the visit would
    // have come by now (at most 2398 ticks after the first).
    rule(&ctx, "spawn_phantoms", Value::Bool(false));
    run(&ctx, 1500);
    rule(&ctx, "spawn_phantoms", Value::Bool(true));
    run(&ctx, 90);
    assert!(
        mobs_of(&ctx, Species::Phantom).is_empty(),
        "the held time counted"
    );
}

#[test]
fn an_insomniac_in_the_nether_gets_no_phantoms() {
    let Some(overworld) = flat() else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = overworld
        .in_dimension("minecraft:the_nether")
        .expect("the Nether");
    ctx.difficulty
        .store(3, std::sync::atomic::Ordering::Relaxed);
    lock(&ctx.time).age = 3_000_000;
    let _ = join(&ctx, [0.5, 70.0, 0.5]);
    if let Some(p) = lock(&ctx.players).get_mut(&STEVE) {
        p.dimension = "minecraft:the_nether".into();
    }
    crate::stats::load(&ctx, STEVE);
    crate::stats::custom(&ctx, STEVE, "time_since_rest", 1_000_000_000);
    run(&ctx, 40);
    assert!(mobs_of(&ctx, Species::Phantom).is_empty());
}
