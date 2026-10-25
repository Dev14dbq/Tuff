//! Zombie sieges (minecraft.wiki, "Zombie siege").

use super::*;

/// A bell somebody holds a ticket of.
fn bell(ctx: &ServerContext, at: [i32; 3]) {
    let bell = ctx.data.blocks.default_state("minecraft:bell").unwrap();
    set_state(ctx, at, bell);
    crate::poi::sync(ctx);
    assert!(crate::poi::claim(ctx, at), "no meeting point at the bell");
}

/// A village wide enough for a point 32 blocks from its middle to be in it.
fn village(ctx: &ServerContext) {
    for cx in -4..=4 {
        for cz in -4..=4 {
            let _ = ctx.world.block(
                mc_protocol::Position::new(cx * 16, 0, cz * 16),
                &ctx.data.blocks,
            );
        }
    }
    for (x, z) in [
        (0, 0),
        (32, 0),
        (-32, 0),
        (0, 32),
        (0, -32),
        (24, 24),
        (-24, 24),
        (24, -24),
        (-24, -24),
    ] {
        bell(ctx, [x, -60, z]);
    }
}

fn zombies(ctx: &ServerContext) -> usize {
    lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Zombie))
        .count()
}

fn setup() -> Option<std::sync::Arc<ServerContext>> {
    let ctx = flat()?;
    ctx.difficulty
        .store(2, std::sync::atomic::Ordering::Relaxed);
    // Only the siege brings zombies.
    lock(&ctx.game_rules).set("minecraft:spawn_monsters", crate::rules::Value::Bool(false));
    let _ = join(&ctx, [0.5, -60.0, 0.5]);
    village(&ctx);
    Some(ctx)
}

#[test]
fn a_siege_night_brings_zombies_into_the_village() {
    let Some(ctx) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_day(&ctx, 18_001);
    crate::mobs::spawn::siege_tonight(&ctx);
    run(&ctx, 70);
    let n = zombies(&ctx);
    assert!((1..=20).contains(&n), "{n} zombies");
    // Each came about 32 blocks from the player (and may have walked a
    // little since).
    for e in lock(&ctx.entities).iter().filter(|e| !e.removed) {
        if matches!(&e.kind, Kind::Mob(m) if m.species == Species::Zombie) {
            let d = (e.pos[0].powi(2) + e.pos[2].powi(2)).sqrt();
            assert!((10.0..=50.0).contains(&d), "zombie {d} blocks away");
        }
    }
    // Twenty tries and no more.
    run(&ctx, 100);
    assert_eq!(zombies(&ctx), n);
}

#[test]
fn no_siege_by_day_or_outside_a_village() {
    let Some(ctx) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    set_day(&ctx, 6000);
    crate::mobs::spawn::siege_tonight(&ctx);
    run(&ctx, 70);
    assert_eq!(zombies(&ctx), 0, "a siege by day");

    let Some(ctx) = setup() else { return };
    lock(&ctx.players).values_mut().for_each(|p| {
        if let Some(q) = &mut p.pose {
            q.x = 300.5;
        }
    });
    set_day(&ctx, 18_001);
    crate::mobs::spawn::siege_tonight(&ctx);
    run(&ctx, 70);
    assert_eq!(zombies(&ctx), 0, "a siege away from the village");
}
