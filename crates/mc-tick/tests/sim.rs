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
    fn in_world(&self, p: BlockPos) -> bool {
        (-64..320).contains(&p.y)
    }
    fn spawn_falling_block(&mut self, _p: BlockPos, _state: u32) {}
    fn is_block_tick_scheduled(&self, p: BlockPos, block: u32) -> bool {
        self.ticks.blocks.is_scheduled(p, block)
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
    let conductor = std::fs::read_to_string(dir.join("observed/conductor.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v["states"].clone()).ok());
    let support = std::fs::read_to_string(dir.join("observed/support.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v["states"].clone()).ok());
    let shapes = Shapes::new(&data, attach, conductor).with_support(support);
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

#[test]
fn dust_line_connects_and_carries_power() {
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
    for x in -2..=20 {
        world
            .blocks
            .insert(BlockPos::new(x, -60, 0), state("minecraft:stone"));
    }
    for x in 1..=5 {
        let p = BlockPos::new(x, -59, 0);
        let s = mc_tick::placement_state(&world, ctx, p, state("minecraft:redstone_wire"));
        mc_tick::set_block(&mut world, ctx, p, s, Flags::ALL);
        eprintln!(
            "after {x}: wire1 {} shape-of-1 {}",
            data.blocks
                .describe(world.block(BlockPos::new(1, -59, 0)))
                .unwrap(),
            data.blocks
                .describe(mc_tick::redstone::wire_shape(
                    &world,
                    ctx,
                    BlockPos::new(1, -59, 0),
                    world.block(BlockPos::new(1, -59, 0))
                ))
                .unwrap()
        );
    }
    let p0 = BlockPos::new(0, -59, 0);
    mc_tick::set_block(
        &mut world,
        ctx,
        p0,
        state("minecraft:redstone_block"),
        Flags::ALL,
    );
    for x in 1..=5 {
        eprintln!(
            "{x}: {}",
            data.blocks
                .describe(world.block(BlockPos::new(x, -59, 0)))
                .unwrap()
        );
    }
    eprintln!(
        "default wire {}",
        data.blocks
            .describe(state("minecraft:redstone_wire"))
            .unwrap()
    );
}

#[test]
fn lamp_set_beside_powered_dust_stays_off() {
    let Some((data, shapes, rules)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |text: &str| -> u32 {
        let (name, props) = text.split_once('[').unwrap_or((text, ""));
        let props: Vec<(&str, &str)> = props
            .trim_end_matches(']')
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .collect();
        data.blocks.state_id(name, &props).unwrap()
    };
    let mut world = World {
        blocks: HashMap::new(),
        air: state("minecraft:air"),
        time: 0,
        ticks: ScheduledTicks::default(),
    };
    for x in 0..=8 {
        world
            .blocks
            .insert(BlockPos::new(x, -60, 6), state("minecraft:stone"));
    }
    for (x, s) in [
        (2, "minecraft:stone"),
        (3, "minecraft:redstone_wall_torch[facing=east]"),
        (4, "minecraft:redstone_wire"),
        (5, "minecraft:redstone_lamp"),
    ] {
        let p = BlockPos::new(x, -59, 6);
        let placed = mc_tick::placement_state(&world, ctx, p, state(s));
        mc_tick::set_block(&mut world, ctx, p, placed, Flags::ALL);
        for x in 3..=5 {
            eprint!(
                "{}  ",
                data.blocks
                    .describe(world.block(BlockPos::new(x, -59, 6)))
                    .unwrap()
            );
        }
        eprintln!();
    }
}

fn run_block_ticks(world: &mut World, ctx: Ctx, n: i64) {
    for _ in 0..n {
        world.time += 1;
        for (p, kind) in world.ticks.blocks.take_due(world.time) {
            mc_tick::block_tick(world, ctx, p, kind);
        }
    }
}

#[test]
fn fire_burns_planks_and_dies_on_stone() {
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
    for x in -3..=3 {
        for z in -3..=3 {
            world
                .blocks
                .insert(BlockPos::new(x, 0, z), state("minecraft:oak_planks"));
        }
    }
    world
        .blocks
        .insert(BlockPos::new(20, 0, 0), state("minecraft:stone"));
    let fire = mc_tick::fire::can_light(&world, ctx, BlockPos::new(0, 1, 0)).unwrap();
    mc_tick::set_block(&mut world, ctx, BlockPos::new(0, 1, 0), fire, Flags::ALL);
    let lone = mc_tick::fire::can_light(&world, ctx, BlockPos::new(20, 1, 0)).unwrap();
    mc_tick::set_block(&mut world, ctx, BlockPos::new(20, 1, 0), lone, Flags::ALL);
    // In the air beside nothing that burns: no fire.
    assert!(mc_tick::fire::can_light(&world, ctx, BlockPos::new(40, 5, 0)).is_none());
    run_block_ticks(&mut world, ctx, 6000);
    let planks = state("minecraft:oak_planks");
    let left = world.blocks.values().filter(|&&s| s == planks).count();
    assert!(left < 49, "fire burnt no planks");
    assert_eq!(
        world.block(BlockPos::new(20, 1, 0)),
        world.air,
        "fire on stone went out"
    );
}

#[test]
fn random_ticks_grow_spread_and_decay() {
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
    let mut rng = mc_tick::rng::Rng::new(7);
    // Wheat on farmland ripens.
    let farmland = shapes.with(state("minecraft:farmland"), "moisture", "7");
    world.blocks.insert(BlockPos::new(0, 0, 0), farmland);
    let wheat = BlockPos::new(0, 1, 0);
    world.blocks.insert(wheat, state("minecraft:wheat"));
    for _ in 0..2000 {
        let s = world.block(wheat);
        mc_tick::random::tick(&mut world, ctx, wheat, s, &mut rng);
    }
    assert_eq!(shapes.property(world.block(wheat), "age"), Some("7"));
    // Grass spreads onto dirt beside it.
    world
        .blocks
        .insert(BlockPos::new(10, 0, 0), state("minecraft:grass_block"));
    world
        .blocks
        .insert(BlockPos::new(11, 0, 0), state("minecraft:dirt"));
    for _ in 0..500 {
        let p = BlockPos::new(10, 0, 0);
        let s = world.block(p);
        mc_tick::random::tick(&mut world, ctx, p, s, &mut rng);
    }
    assert_eq!(
        shapes.block_name(world.block(BlockPos::new(11, 0, 0))),
        Some("minecraft:grass_block")
    );
    // Leaves cut off from their log decay.
    let log = BlockPos::new(20, 0, 0);
    let leaf = BlockPos::new(20, 1, 0);
    world.blocks.insert(log, state("minecraft:oak_log"));
    let leaves = shapes.with(state("minecraft:oak_leaves"), "distance", "1");
    world.blocks.insert(leaf, leaves);
    let air = world.air;
    mc_tick::set_block(&mut world, ctx, log, air, Flags::ALL);
    run_block_ticks(&mut world, ctx, 5);
    assert_eq!(shapes.property(world.block(leaf), "distance"), Some("7"));
    let s = world.block(leaf);
    mc_tick::random::tick(&mut world, ctx, leaf, s, &mut rng);
    assert_eq!(world.block(leaf), world.air);
}
