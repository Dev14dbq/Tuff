//! Random ticks of the chunks near players (minecraft.wiki, "Tick" →
//! "Random tick"). Picking is one pass over those chunks under one lock,
//! skipping by palette every section with nothing that random-ticks; only
//! the picked blocks that care go through block logic.

use std::sync::atomic::{AtomicI32, Ordering};

use crate::context::{Event, ServerContext};
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

/// Vanilla's level-wide random tick generator state.
static LCG: AtomicI32 = AtomicI32::new(0x2A5F_1C3B);

/// The chunks that receive random ticks: entity-ticking chunks, forceload,
/// and one chunk further when simulation distance exceeds the view.
pub(crate) fn ticking_chunks(ctx: &ServerContext) -> Vec<(i32, i32)> {
    crate::tickets::rebuild(ctx);
    crate::tickets::random_chunks(ctx)
}

/// One game tick of random ticks.
pub fn tick(ctx: &ServerContext) {
    let chunks = ticking_chunks(ctx);
    if !chunks.is_empty() {
        tick_chunks(ctx, &chunks);
    }
}

/// Random ticks of `chunks`; returns how many blocks were ticked.
fn tick_chunks(ctx: &ServerContext, chunks: &[(i32, i32)]) -> usize {
    let speed = lock(&ctx.game_rules).int("minecraft:random_tick_speed");
    if speed <= 0 {
        return 0;
    }
    let shapes = &ctx.shapes;
    let mut lcg = LCG.load(Ordering::Relaxed);
    let picks = ctx
        .world
        .random_tick_picks(chunks, speed as u32, &mut lcg, |s| shapes.random_ticks(s));
    LCG.store(lcg, Ordering::Relaxed);
    if picks.is_empty() {
        return 0;
    }
    let count = picks.len();
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    let time = lock(&ctx.time).age;
    let mut rng =
        mc_tick::rng::Rng::new((time as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ lcg as u64);
    for (p, state) in picks {
        let at = to_pos(p);
        // An earlier pick this tick may have changed it.
        if mc_tick::Level::block(&level, at) != state {
            continue;
        }
        mc_tick::random::tick(&mut level, logic, at, state, &mut rng);
        if shapes.random_behaviour(state) == mc_tick::random::Behaviour::NetherPortal {
            portal_piglin(ctx, at, rng.unit());
        }
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
    count
}

/// A nether portal block ticked where the dimension lets portals spawn
/// piglins (the Overworld): a zombified piglin, one time in 2000 per level
/// of difficulty, in the portal's bottom block (minecraft.wiki, "Zombified
/// Piglin"). `roll` is in [0, 1).
fn portal_piglin(ctx: &ServerContext, at: mc_tick::BlockPos, roll: f64) {
    let difficulty = ctx.difficulty();
    if difficulty == 0
        || roll >= f64::from(difficulty) / 2000.0
        || !ctx.game_rule_bool("minecraft:spawn_mobs")
        || !crate::environment::flag(
            ctx,
            Some([at.x, at.y, at.z]),
            crate::environment::NETHER_PORTAL_SPAWNS_PIGLIN,
            false,
        )
    {
        return;
    }
    let blocks = &ctx.data.blocks;
    let is_portal = |y: i32| {
        ctx.world
            .block(mc_protocol::Position::new(at.x, y, at.z), blocks)
            .and_then(|s| blocks.block_name(s))
            == Some("minecraft:nether_portal")
    };
    let mut y = at.y;
    while y > ctx.world.min_y && is_portal(y - 1) {
        y -= 1;
    }
    // It stands on the frame.
    if !crate::hits::stops(ctx, [at.x, y - 1, at.z]) {
        return;
    }
    let pos = [f64::from(at.x) + 0.5, f64::from(y), f64::from(at.z) + 0.5];
    let e = crate::mobs::new_mob(ctx, mc_entity::Species::ZombifiedPiglin, pos);
    let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
    lock(&ctx.entities).push(e);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The simulation distance the cost tests tick (the default).
    const SIMULATION_DISTANCE: i32 = 10;

    #[test]
    fn random_ticks_reach_as_far_as_the_simulation_distance() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Ok(data) = mc_data::McData::load(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut config = crate::Config::new(dir);
        config.simulation_distance = 4;
        config.view_distance = 10;
        let ctx = ServerContext::new(config, data).expect("context");
        crate::testkit::join(&ctx, 1, 1, [8.0, -60.0, 8.0], 1);
        let chunks = ticking_chunks(&ctx);
        assert_eq!(chunks.len(), 9 * 9);
        assert!(chunks.contains(&(4, -4)) && !chunks.contains(&(5, 0)));
    }

    #[test]
    fn an_overworld_portal_now_and_then_lets_a_zombified_piglin_out() {
        let Some(ctx) = crate::projectiles::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let blocks = &ctx.data.blocks;
        let set = |y: i32, name: &str| {
            ctx.world.set_block(
                mc_protocol::Position::new(0, y, 0),
                blocks.default_state(name).unwrap(),
                blocks,
                &ctx.rules,
            );
        };
        set(-60, "minecraft:obsidian");
        for y in -59..=-57 {
            set(y, "minecraft:nether_portal");
        }
        let top = mc_tick::BlockPos::new(0, -57, 0);
        // Normal difficulty: one tick in a thousand.
        portal_piglin(&ctx, top, 0.0011);
        assert!(lock(&ctx.entities).is_empty());
        portal_piglin(&ctx, top, 0.0009);
        let all = lock(&ctx.entities);
        assert_eq!(all.len(), 1);
        assert!(matches!(&all[0].kind,
            mc_entity::Kind::Mob(m) if m.species == mc_entity::Species::ZombifiedPiglin));
        assert_eq!(all[0].pos, [0.5, -59.0, 0.5]);
    }

    fn world(name: &str) -> Option<(std::sync::Arc<ServerContext>, std::path::PathBuf)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let folder = std::env::temp_dir().join(format!("mc-{name}-{}", std::process::id()));
        let mut config = crate::Config::new(dir);
        config.world = Some(folder.clone());
        config.seed = Some(12345);
        let ctx = ServerContext::new(config, data).expect("context");
        let r = SIMULATION_DISTANCE + 1;
        for cx in -r..=r {
            for cz in -r..=r {
                let _ = ctx.world.block(
                    mc_protocol::Position::new(cx * 16, 64, cz * 16),
                    &ctx.data.blocks,
                );
            }
        }
        Some((ctx, folder))
    }

    /// Cost of flowing water and lava: sources dropped from the sky all over
    /// a generated world, then 600 ticks of scheduled ticks.
    /// `cargo test -p mc-server --release fluid_tick_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn fluid_tick_cost() {
        let Some((ctx, folder)) = world("fluids") else {
            eprintln!("skipped: no game data");
            return;
        };
        let water = ctx.data.blocks.default_state("minecraft:water").unwrap();
        let lava = ctx.data.blocks.default_state("minecraft:lava").unwrap();
        {
            let mut level = WorldLevel::new(&ctx);
            for i in 0..100 {
                let (x, z) = ((i % 10) * 14 - 70, (i / 10) * 14 - 70);
                let fluid = if i % 4 == 0 { lava } else { water };
                level.set_block(mc_protocol::Position::new(x, 140, z), fluid);
            }
            let _ = level.into_changes();
        }
        let mut worst = std::time::Duration::ZERO;
        let started = std::time::Instant::now();
        let ticks = std::env::var("FLUID_TICKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        if std::env::var("FLUID_TICKS").is_ok() {
            eprintln!("ticking");
        }
        for _ in 0..ticks {
            lock(&ctx.time).age += 1;
            let t = std::time::Instant::now();
            crate::tick::run_scheduled_ticks(&ctx);
            let spent = t.elapsed();
            worst = worst.max(spent);
        }
        let took = started.elapsed();
        eprintln!(
            "{ticks} ticks of 100 falling fluids: {:.3} ms a tick on average, worst {:.3} ms, {} ticks waiting",
            took.as_secs_f64() * 1000.0 / f64::from(ticks),
            worst.as_secs_f64() * 1000.0,
            lock(&ctx.ticks).fluids.len()
        );
        let _ = std::fs::remove_dir_all(folder);
    }

    /// Cost of random ticks over simulation distance 10 (441 chunks) of a
    /// generated world. `cargo test -p mc-server --release random_tick_cost
    /// -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn random_tick_cost() {
        let Some((ctx, folder)) = world("random") else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut chunks = Vec::new();
        for cx in -SIMULATION_DISTANCE..=SIMULATION_DISTANCE {
            for cz in -SIMULATION_DISTANCE..=SIMULATION_DISTANCE {
                chunks.push((cx, cz));
            }
        }
        let ticks = 200;
        let started = std::time::Instant::now();
        let mut ticked = 0;
        for _ in 0..ticks {
            ticked += tick_chunks(&ctx, &chunks);
        }
        let took = started.elapsed();
        eprintln!(
            "{ticks} ticks over {} chunks: {:.3} ms a tick, {} blocks ticked a tick",
            chunks.len(),
            took.as_secs_f64() * 1000.0 / f64::from(ticks),
            ticked / ticks as usize
        );
        let _ = std::fs::remove_dir_all(folder);
    }
}
