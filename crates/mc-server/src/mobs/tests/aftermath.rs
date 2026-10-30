//! Cramming damage to players and the effects that act on a kill or a hurt:
//! oozing, weaving, wind charged and infested.

use super::*;
use crate::rules::Value;

fn rule(ctx: &ServerContext, name: &str, value: Value) {
    lock(&ctx.game_rules).set(name, value);
}

/// Gives the mob an effect and kills it outright.
fn kill_with(ctx: &ServerContext, id: i32, effect: &'static str) {
    let uuid = mob_of(ctx, id).unwrap().uuid;
    crate::mob_effects::with_mob(ctx, uuid, |e| {
        if let Kind::Mob(m) = &mut e.kind {
            m.effects
                .active
                .insert(effect, mc_entity::effect::Instance::new(0, 1000, true));
        }
        hurt::harm(ctx, e, &hurt::Harm::new(1000.0, "minecraft:generic"));
    });
}

#[allow(clippy::let_and_return)] // the guard must go before the world does
fn crushed_player_health(limit: i32) -> f32 {
    let ctx = flat().expect("game data");
    rule(&ctx, "max_entity_cramming", Value::Int(limit));
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    for _ in 0..3 {
        put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    }
    run(&ctx, 200);
    let health = lock(&ctx.vitals).get(&STEVE).unwrap().health;
    health
}

#[test]
fn a_player_in_a_crowd_past_the_cap_is_crushed() {
    // Three pigs share the player's space: that is more than cap - 1
    // others when the cap is 3, not when it is 4. A cap of 0 turns it off.
    assert!(crushed_player_health(3) < 20.0, "not crushed at 3");
    assert_eq!(crushed_player_health(4), 20.0);
    assert_eq!(crushed_player_health(0), 20.0);
}

#[test]
fn oozing_leaves_two_slimes_unless_the_crowd_cap_is_reached() {
    let ctx = flat().expect("game data");
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let pig = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    kill_with(&ctx, pig, "oozing");
    run(&ctx, 2);
    let slimes = mobs_of(&ctx, Species::Slime);
    assert_eq!(slimes.len(), 2);
    assert!(
        slimes
            .iter()
            .all(|e| matches!(&e.kind, Kind::Mob(m) if m.size == 2))
    );

    // With the cap at 3 and two slimes close by, only one more fits.
    rule(&ctx, "max_entity_cramming", Value::Int(3));
    let pig = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    kill_with(&ctx, pig, "oozing");
    run(&ctx, 2);
    assert_eq!(mobs_of(&ctx, Species::Slime).len(), 3);

    // Full already: none.
    let pig = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    kill_with(&ctx, pig, "oozing");
    run(&ctx, 2);
    assert_eq!(mobs_of(&ctx, Species::Slime).len(), 3);
}

#[test]
fn weaving_leaves_cobwebs_only_when_mobs_may_grief() {
    let ctx = flat().expect("game data");
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let webs = |ctx: &ServerContext| {
        let mut n = 0;
        for x in -2..=3 {
            for y in -62..=-58 {
                for z in -2..=3 {
                    n += usize::from(crate::testkit::block(ctx, [x, y, z]) == "minecraft:cobweb");
                }
            }
        }
        n
    };
    rule(&ctx, "mob_griefing", Value::Bool(false));
    let pig = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    kill_with(&ctx, pig, "weaving");
    run(&ctx, 2);
    assert_eq!(webs(&ctx), 0);

    rule(&ctx, "mob_griefing", Value::Bool(true));
    let pig = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    kill_with(&ctx, pig, "weaving");
    run(&ctx, 2);
    let n = webs(&ctx);
    assert!((2..=3).contains(&n), "{n} cobwebs");
}

#[test]
fn wind_charged_bursts_into_a_gust_that_pushes_the_neighbours() {
    let ctx = flat().expect("game data");
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let victim = put(&ctx, Species::Pig, [0.5, -60.0, 0.5]);
    let near = put(&ctx, Species::Cow, [1.5, -60.0, 0.5]);
    let calm = put(&ctx, Species::Cow, [-9.5, -60.0, 0.5]);
    kill_with(&ctx, victim, "wind_charged");
    run(&ctx, 3);
    let x = |id| mob_of(&ctx, id).unwrap().pos[0];
    assert!(x(near) > 1.6, "pushed to {}", x(near));
    assert_eq!(x(calm), -9.5);
}

#[test]
fn infested_lets_silverfish_out_of_the_hurt_now_and_then() {
    let ctx = flat().expect("game data");
    let _ = join(&ctx, [20.5, -60.0, 20.5]);
    let plain = put(&ctx, Species::Cow, [0.5, -60.0, 0.5]);
    let carrier = put(&ctx, Species::Cow, [5.5, -60.0, 5.5]);
    let hit = |id: i32| {
        let uuid = mob_of(&ctx, id).unwrap().uuid;
        crate::mob_effects::with_mob(&ctx, uuid, |e| {
            if let Kind::Mob(m) = &mut e.kind {
                m.invulnerable = 0;
            }
            hurt::harm(&ctx, e, &hurt::Harm::new(0.001, "minecraft:generic"))
        })
    };
    for _ in 0..40 {
        hit(plain);
    }
    assert!(mobs_of(&ctx, Species::Silverfish).is_empty());
    let uuid = mob_of(&ctx, carrier).unwrap().uuid;
    crate::mob_effects::with_mob(&ctx, uuid, |e| {
        if let Kind::Mob(m) = &mut e.kind {
            m.health = 1.0e6;
            m.max_health = 1.0e6;
            m.effects.active.insert(
                "infested",
                mc_entity::effect::Instance::new(0, 100_000, true),
            );
        }
    });
    let mut hits = 0;
    for _ in 0..400 {
        hits += usize::from(hit(carrier) == Some(true));
    }
    // One hit in ten lets one or two out; none at all in 400 is a one in
    // 10^18 chance.
    let fish = mobs_of(&ctx, Species::Silverfish).len();
    assert!(
        hits > 0 && fish > 0 && fish < 2 * hits,
        "{fish} from {hits} hits"
    );
}
