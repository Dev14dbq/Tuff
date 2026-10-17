//! Runs block logic on an in-memory world with the real game data
//! (`cache/mcdata/26.3`); skipped without it.

use std::collections::HashMap;
use std::path::Path;

use mc_tick::{BlockPos, BlockRules, Ctx, Flags, Level, ScheduledTicks, Shapes};

struct World {
    blocks: HashMap<BlockPos, u32>,
    air: u32,
    time: i64,
    ticks: ScheduledTicks,
}

impl Level for World {
    fn block(&self, p: BlockPos) -> u32 {
        self.blocks.get(&p).copied().unwrap_or(self.air)
    }
    fn set(&mut self, p: BlockPos, state: u32) -> bool {
        self.blocks.insert(p, state);
        true
    }
    fn game_time(&self) -> i64 {
        self.time
    }
    fn schedule_fluid_tick(&mut self, p: BlockPos, fluid: u32, delay: i64) {
        self.ticks.fluids.schedule(p, fluid, self.time + delay, 0);
    }
    fn schedule_block_tick(&mut self, p: BlockPos, block: u32, delay: i64, priority: i32) {
        self.ticks
            .blocks
            .schedule(p, block, self.time + delay, priority);
    }
}

fn setup() -> Option<(mc_data::McData, Shapes, BlockRules)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let data = mc_data::McData::load(&dir).ok()?;
    let attach = std::fs::read_to_string(dir.join("observed/fence_attach.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v["states"].clone()).ok());
    let shapes = Shapes::new(&data, attach);
    let rules = BlockRules::new(&data).ok()?;
    Some((data, shapes, rules))
}

fn run_ticks(world: &mut World, ctx: Ctx, n: i64) {
    for _ in 0..n {
        world.time += 1;
        for (p, f) in world.ticks.fluids.take_due(world.time) {
            mc_tick::fluids::tick(world, ctx, p, f);
        }
    }
}

#[test]
fn falling_lava_on_water_spreads_after_making_stone() {
    let Some((data, shapes, rules)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |name: &str| data.blocks.default_state(name).unwrap();
    let mut world = World {
        blocks: HashMap::new(),
        air: state("minecraft:air"),
        time: 0,
        ticks: ScheduledTicks::default(),
    };
    for x in -12..=12 {
        for z in -12..=12 {
            world
                .blocks
                .insert(BlockPos::new(x, -60, z), state("minecraft:stone"));
        }
    }
    let water = BlockPos::new(0, -59, 0);
    mc_tick::set_block(&mut world, ctx, water, state("minecraft:water"), Flags::ALL);
    run_ticks(&mut world, ctx, 70);
    let lava = BlockPos::new(2, -57, 0);
    mc_tick::set_block(&mut world, ctx, lava, state("minecraft:lava"), Flags::ALL);
    let name = |w: &World, p: BlockPos| data.blocks.describe(w.block(p)).unwrap().to_owned();
    for step in 0..6 {
        run_ticks(&mut world, ctx, 30);
        eprintln!(
            "t+{}: above {} | falling {} | side {} | under {}",
            (step + 1) * 30,
            name(&world, BlockPos::new(2, -57, 0)),
            name(&world, BlockPos::new(2, -58, 0)),
            name(&world, BlockPos::new(1, -58, 0)),
            name(&world, BlockPos::new(2, -59, 0)),
        );
    }
    assert!(name(&world, BlockPos::new(1, -58, 0)).starts_with("minecraft:lava"));
}

#[test]
fn lava_and_water_placed_together() {
    let Some((data, shapes, rules)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |name: &str| data.blocks.default_state(name).unwrap();
    let mut world = World {
        blocks: HashMap::new(),
        air: state("minecraft:air"),
        time: 0,
        ticks: ScheduledTicks::default(),
    };
    for x in 10..=30 {
        for z in 10..=30 {
            world
                .blocks
                .insert(BlockPos::new(x, -60, z), state("minecraft:stone"));
        }
    }
    mc_tick::set_block(
        &mut world,
        ctx,
        BlockPos::new(20, -59, 20),
        state("minecraft:water"),
        Flags::ALL,
    );
    mc_tick::set_block(
        &mut world,
        ctx,
        BlockPos::new(22, -57, 20),
        state("minecraft:lava"),
        Flags::ALL,
    );
    let watch = [
        BlockPos::new(22, -58, 20),
        BlockPos::new(22, -59, 20),
        BlockPos::new(21, -57, 20),
        BlockPos::new(21, -58, 20),
        BlockPos::new(21, -59, 20),
        BlockPos::new(20, -58, 20),
    ];
    let mut seen: Vec<u32> = watch.iter().map(|&p| world.block(p)).collect();
    for _ in 0..180 {
        run_ticks(&mut world, ctx, 1);
        for (i, &p) in watch.iter().enumerate() {
            let now = world.block(p);
            if now != seen[i] {
                seen[i] = now;
                eprintln!(
                    "t={} {:?} -> {}",
                    world.time,
                    (p.x, p.y, p.z),
                    data.blocks.describe(now).unwrap()
                );
            }
        }
    }
    for y in [-59, -58, -57] {
        eprintln!("y={y}");
        for z in 17..=23 {
            let row: Vec<String> = (18..=26)
                .map(|x| {
                    let d = data
                        .blocks
                        .describe(world.block(BlockPos::new(x, y, z)))
                        .unwrap();
                    let short = d.trim_start_matches("minecraft:");
                    match short {
                        "air[]" => ".".into(),
                        "stone[]" => "S".into(),
                        "cobblestone[]" => "C".into(),
                        "obsidian[]" => "O".into(),
                        s if s.starts_with("water") => format!("w{}", &s[12..s.len() - 1]),
                        s if s.starts_with("lava") => format!("l{}", &s[11..s.len() - 1]),
                        s => s.into(),
                    }
                })
                .collect();
            eprintln!(
                "  z={z}: {}",
                row.iter().map(|c| format!("{c:>3}")).collect::<String>()
            );
        }
    }
}
