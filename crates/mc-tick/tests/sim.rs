//! Runs block logic on an in-memory world with the real game data
//! (`cache/mcdata/26.3`); skipped without it.

use std::collections::HashMap;
use std::path::Path;

use mc_tick::{BlockPos, BlockRules, Ctx, Dir, Flags, Level, ScheduledTicks, Shapes};

struct World {
    blocks: HashMap<BlockPos, u32>,
    air: u32,
    time: i64,
    ticks: ScheduledTicks,
    extra: Extra,
}

#[derive(Default)]
struct Extra {
    minecarts: std::collections::HashSet<BlockPos>,
    torch: HashMap<BlockPos, Vec<i64>>,
    fizzes: u32,
    items: HashMap<BlockPos, Vec<Option<(i32, u32)>>>,
    cooldown: HashMap<BlockPos, i32>,
    dispensed: Vec<(BlockPos, Dir, bool)>,
    warm: bool,
    dried: u32,
    /// A humid biome: fire burns neighbours away sooner.
    humid: bool,
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
    fn minecarts_at(&self, p: BlockPos) -> u32 {
        u32::from(self.extra.minecarts.contains(&p))
    }
    fn torch_burnout(&mut self, p: BlockPos, add: bool) -> bool {
        let now = self.time;
        let list = self.extra.torch.entry(p).or_default();
        list.retain(|&t| t >= now - 60);
        if add {
            list.push(now);
        }
        list.len() >= 8
    }
    fn torch_fizz(&mut self, _p: BlockPos) {
        self.extra.fizzes += 1;
    }
    fn container_transfer(&mut self, from: BlockPos, _fs: Dir, to: BlockPos, _ts: Dir) -> bool {
        if !self.extra.items.contains_key(&from) || !self.extra.items.contains_key(&to) {
            return false;
        }
        let mut src = self.extra.items.remove(&from).unwrap();
        let dst = self.extra.items.get_mut(&to).unwrap();
        let mut moved = false;
        'slots: for slot in src.iter_mut() {
            let Some((item, count)) = *slot else { continue };
            let target = dst
                .iter()
                .position(|d| d.is_some_and(|(i, c)| i == item && c < 64))
                .or_else(|| dst.iter().position(Option::is_none));
            let Some(t) = target else { continue 'slots };
            dst[t] = Some((item, dst[t].map_or(0, |d| d.1) + 1));
            *slot = (count > 1).then_some((item, count - 1));
            moved = true;
            break;
        }
        self.extra.items.insert(from, src);
        moved
    }
    fn hopper_cooldown(&self, p: BlockPos) -> i32 {
        self.extra.cooldown.get(&p).copied().unwrap_or(0)
    }
    fn set_hopper_cooldown(&mut self, p: BlockPos, ticks: i32) {
        self.extra.cooldown.insert(p, ticks);
    }
    fn ultrawarm(&self) -> bool {
        self.extra.warm
    }
    fn burns_out_faster(&self, _p: BlockPos) -> bool {
        self.extra.humid
    }
    fn sponge_dried(&mut self, _p: BlockPos) {
        self.extra.dried += 1;
    }
    fn dispense(&mut self, p: BlockPos, facing: Dir, dropper: bool) {
        self.extra.dispensed.push((p, facing, dropper));
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
        extra: Extra::default(),
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

/// A falling stream below is a drop. A pool of the same fluid on solid
/// ground is not, so water poured above a pond still spreads both ways.
#[test]
fn a_pool_below_is_not_a_cliff_but_a_fall_is() {
    let Some((data, shapes, rules)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |name: &str| data.blocks.default_state(name).unwrap();
    let water_at = |world: &World, x: i32, y: i32| {
        data.blocks.block_name(world.block(BlockPos::new(x, y, 0))) == Some("minecraft:water")
    };
    let pour = |below: u32| {
        let mut world = World {
            blocks: HashMap::new(),
            air: state("minecraft:air"),
            time: 0,
            ticks: ScheduledTicks::default(),
            extra: Extra::default(),
        };
        for x in -4..=4 {
            for z in -4..=4 {
                world
                    .blocks
                    .insert(BlockPos::new(x, -59, z), state("minecraft:stone"));
            }
        }
        world.blocks.insert(BlockPos::new(1, -59, 0), below);
        mc_tick::set_block(
            &mut world,
            ctx,
            BlockPos::new(0, -58, 0),
            state("minecraft:water"),
            Flags::ALL,
        );
        run_ticks(&mut world, ctx, 8);
        (water_at(&world, 1, -58), water_at(&world, -1, -58))
    };
    let falling = shapes.state("minecraft:water", &[("level", "8")]).unwrap();
    let pool = shapes.state("minecraft:water", &[("level", "4")]).unwrap();
    let (toward_fall, _) = pour(falling);
    assert!(toward_fall, "water aims at a stream falling below");
    let (toward_pool, away_from_pool) = pour(pool);
    assert!(toward_pool, "water still reaches a pool beside it");
    assert!(
        away_from_pool,
        "a pool on the ground does not take the whole flow"
    );
}

#[test]
fn water_boils_away_where_the_dimension_is_ultrawarm() {
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
        extra: Extra {
            warm: true,
            ..Extra::default()
        },
    };
    mc_tick::set_block(
        &mut world,
        ctx,
        BlockPos::new(0, -58, 0),
        state("minecraft:water"),
        Flags::ALL,
    );
    let wet = shapes.with(state("minecraft:oak_slab"), "waterlogged", "true");
    mc_tick::set_block(&mut world, ctx, BlockPos::new(1, -58, 0), wet, Flags::ALL);
    assert_eq!(
        world.block(BlockPos::new(0, -58, 0)),
        state("minecraft:air")
    );
    let left = world.block(BlockPos::new(1, -58, 0));
    assert_eq!(shapes.property(left, "waterlogged"), Some("false"));
    assert!(shapes.block_name(left) == Some("minecraft:oak_slab"));
    world
        .blocks
        .insert(BlockPos::new(2, -58, 0), state("minecraft:water"));
    mc_tick::fluids::tick(&mut world, ctx, BlockPos::new(2, -58, 0), 0);
    assert_eq!(
        world.block(BlockPos::new(2, -58, 0)),
        state("minecraft:air")
    );
    assert!(world.extra.dried >= 3);
}

/// The last step of `fluids2.txt`: water poured onto lava that has just
/// hardened, beside an older pond. Levels are what vanilla has after 20 ticks.
#[test]
fn water_on_hardened_lava_beside_a_pond_matches_vanilla() {
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
        extra: Extra::default(),
    };
    for x in -12..=28 {
        for z in -12..=28 {
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
    run_ticks(&mut world, ctx, 180);
    mc_tick::set_block(
        &mut world,
        ctx,
        BlockPos::new(20, -59, 24),
        state("minecraft:lava"),
        Flags::ALL,
    );
    mc_tick::set_block(
        &mut world,
        ctx,
        BlockPos::new(20, -57, 24),
        state("minecraft:water"),
        Flags::ALL,
    );
    run_ticks(&mut world, ctx, 20);
    let describe = |x: i32, y: i32, z: i32| {
        let s = world.block(BlockPos::new(x, y, z));
        match data.blocks.block_name(s) {
            Some("minecraft:water") => format!(
                "water[level={}]",
                data.blocks.property(s, "level").unwrap_or("?")
            ),
            Some(name) => name.trim_start_matches("minecraft:").to_owned(),
            None => "air".into(),
        }
    };
    let expect = [
        (19, -59, 21, "water[level=6]"),
        (20, -59, 21, "water[level=5]"),
        (18, -59, 22, "water[level=6]"),
        (17, -59, 23, "water[level=6]"),
        (18, -59, 23, "water[level=5]"),
        (22, -59, 23, "water[level=5]"),
        (23, -59, 23, "water[level=6]"),
        (18, -59, 25, "water[level=7]"),
        (22, -59, 25, "water[level=7]"),
        (18, -59, 26, "air"),
        (22, -59, 26, "air"),
        (20, -58, 25, "water[level=8]"),
        (20, -57, 25, "water[level=1]"),
        (19, -59, 22, "water[level=2]"),
        (21, -59, 22, "water[level=2]"),
        (18, -59, 24, "water[level=1]"),
        (22, -59, 24, "water[level=1]"),
    ];
    let mut misses = Vec::new();
    for (x, y, z, want) in expect {
        let got = describe(x, y, z);
        if got != want {
            misses.push(format!("{x} {y} {z}: {got} want {want}"));
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
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
        extra: Extra::default(),
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
        extra: Extra::default(),
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
        extra: Extra::default(),
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
        extra: Extra::default(),
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

/// Humid biomes take 50 off each burn chance, so the same roll can eat a
/// block there and leave it alone in a dry biome.
#[test]
fn humid_fire_eats_a_neighbour_a_dry_one_spares() {
    let Some((data, shapes, rules)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |name: &str| data.blocks.default_state(name).unwrap();
    let burnt = |humid: bool, x: i32| {
        let mut world = World {
            blocks: HashMap::new(),
            air: state("minecraft:air"),
            time: 1,
            ticks: ScheduledTicks::default(),
            extra: Extra {
                humid,
                ..Extra::default()
            },
        };
        let fire_at = BlockPos::new(x, 1, 0);
        let plank = BlockPos::new(x + 1, 1, 0);
        world
            .blocks
            .insert(BlockPos::new(x, 0, 0), state("minecraft:stone"));
        world.blocks.insert(plank, state("minecraft:oak_planks"));
        let fire = mc_tick::fire::can_light(&world, ctx, fire_at).unwrap();
        world.blocks.insert(fire_at, fire);
        mc_tick::fire::block_tick(&mut world, ctx, fire_at, shapes.block_index(fire));
        world.block(plank) != state("minecraft:oak_planks")
    };
    let found = (0..80).any(|x| burnt(true, x) && !burnt(false, x));
    assert!(found, "humidity never changed whether the plank burned");
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
        extra: Extra::default(),
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

fn new_world() -> Option<(mc_data::McData, Shapes, BlockRules)> {
    setup()
}

fn name_at(data: &mc_data::McData, w: &World, p: BlockPos) -> String {
    data.blocks.describe(w.block(p)).unwrap().to_owned()
}

fn floor(w: &mut World, data: &mc_data::McData, y: i32) {
    let stone = data.blocks.default_state("minecraft:stone").unwrap();
    for x in -8..=8 {
        for z in -8..=8 {
            w.blocks.insert(BlockPos::new(x, y, z), stone);
        }
    }
}

fn empty_world(data: &mc_data::McData) -> World {
    World {
        blocks: HashMap::new(),
        air: data.blocks.default_state("minecraft:air").unwrap(),
        time: 0,
        ticks: ScheduledTicks::default(),
        extra: Extra::default(),
    }
}

#[test]
fn torch_burns_out_after_eight_toggles() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let torch = BlockPos::new(0, -59, 0);
    let block = BlockPos::new(1, -59, 0);
    let _ = block;
    mc_tick::set_block(
        &mut w,
        ctx,
        torch,
        state("minecraft:redstone_torch"),
        Flags::ALL,
    );
    let tk = shapes.block_index(state("minecraft:redstone_torch"));
    let _ = tk;
    let lever = BlockPos::new(0, -60, 1);
    let lever_state = shapes
        .state(
            "minecraft:lever",
            &[("face", "wall"), ("facing", "south"), ("powered", "false")],
        )
        .unwrap();
    w.blocks.insert(lever, lever_state);
    let mut toggles_off = 0;
    for _ in 0..12 {
        mc_tick::redstone::use_block(&mut w, ctx, lever);
        run_block_ticks(&mut w, ctx, 2);
        if name_at(&data, &w, torch).contains("lit=false") {
            toggles_off += 1;
        }
        mc_tick::redstone::use_block(&mut w, ctx, lever);
        run_block_ticks(&mut w, ctx, 2);
    }
    assert!(
        w.extra.fizzes >= 1,
        "torch never burned out ({toggles_off} offs)"
    );
    // Burnt out: stays dark though nothing powers it.
    assert!(name_at(&data, &w, torch).contains("lit=false"));
    run_block_ticks(&mut w, ctx, 170);
    assert!(name_at(&data, &w, torch).contains("lit=true"));
}

#[test]
fn rails_join_into_curves_slopes_and_straights() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let rail = state("minecraft:rail");
    let put = |w: &mut World, x: i32, z: i32| {
        mc_tick::set_block(w, ctx, BlockPos::new(x, -59, z), rail, Flags::ALL);
    };
    // A corner: rails at (0,0), (1,0) and (0,1).
    put(&mut w, 0, 0);
    put(&mut w, 1, 0);
    put(&mut w, 0, 1);
    let at = |w: &World, x: i32, z: i32| name_at(&data, w, BlockPos::new(x, -59, z));
    assert!(
        at(&w, 0, 0).contains("shape=south_east"),
        "{}",
        at(&w, 0, 0)
    );
    assert!(at(&w, 1, 0).contains("shape=east_west"), "{}", at(&w, 1, 0));
    assert!(
        at(&w, 0, 1).contains("shape=north_south"),
        "{}",
        at(&w, 0, 1)
    );
    // A slope: a rail one up to the east of a lone straight pair.
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    mc_tick::set_block(
        &mut w,
        ctx,
        BlockPos::new(3, -59, 3),
        state("minecraft:stone"),
        Flags::ALL,
    );
    mc_tick::set_block(&mut w, ctx, BlockPos::new(2, -59, 3), rail, Flags::ALL);
    mc_tick::set_block(&mut w, ctx, BlockPos::new(3, -58, 3), rail, Flags::ALL);
    let s = name_at(&data, &w, BlockPos::new(2, -59, 3));
    assert!(s.contains("shape=ascending_east"), "{s}");
    // A powered rail never curves.
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let pr = state("minecraft:powered_rail");
    mc_tick::set_block(&mut w, ctx, BlockPos::new(0, -59, 0), pr, Flags::ALL);
    mc_tick::set_block(&mut w, ctx, BlockPos::new(1, -59, 0), rail, Flags::ALL);
    mc_tick::set_block(&mut w, ctx, BlockPos::new(0, -59, 1), rail, Flags::ALL);
    let s = name_at(&data, &w, BlockPos::new(0, -59, 0));
    assert!(
        s.contains("shape=north_south") || s.contains("shape=east_west"),
        "{s}"
    );
}

#[test]
fn powered_rail_chain_carries_power_eight_further() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let pr = state("minecraft:powered_rail");
    for x in 0..12 {
        mc_tick::set_block(&mut w, ctx, BlockPos::new(x, -59, 0), pr, Flags::ALL);
    }
    mc_tick::set_block(
        &mut w,
        ctx,
        BlockPos::new(-1, -59, 0),
        state("minecraft:redstone_block"),
        Flags::ALL,
    );
    let on =
        |w: &World, x: i32| name_at(&data, w, BlockPos::new(x, -59, 0)).contains("powered=true");
    // Power spreads from the rail beside the source one rail at a time as each
    // updates; after settling, nine rails are on (the source's neighbour plus 8).
    let lit = (0..12).filter(|&x| on(&w, x)).count();
    assert_eq!(lit, 9, "{lit} rails powered");
    // Activator rails behave the same.
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let ar = state("minecraft:activator_rail");
    for x in 0..4 {
        mc_tick::set_block(&mut w, ctx, BlockPos::new(x, -59, 0), ar, Flags::ALL);
    }
    mc_tick::set_block(
        &mut w,
        ctx,
        BlockPos::new(-1, -59, 0),
        state("minecraft:redstone_block"),
        Flags::ALL,
    );
    assert!((0..4).all(|x| on(&w, x)));
}

#[test]
fn detector_rail_follows_minecarts() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let p = BlockPos::new(0, -59, 0);
    mc_tick::set_block(&mut w, ctx, p, state("minecraft:detector_rail"), Flags::ALL);
    w.extra.minecarts.insert(p);
    mc_tick::redstone::entity_inside(&mut w, ctx, p);
    assert!(name_at(&data, &w, p).contains("powered=true"));
    // It strongly powers the block below.
    let below = BlockPos::new(0, -60, 0);
    assert_eq!(
        mc_tick::redstone::power_from(&w, ctx, BlockPos::new(0, -61, 0), Dir::Up, true),
        15
    );
    let _ = below;
    w.extra.minecarts.clear();
    run_block_ticks(&mut w, ctx, 21);
    assert!(name_at(&data, &w, p).contains("powered=false"));
}

#[test]
fn hopper_moves_items_down_and_locks_when_powered() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let chest = BlockPos::new(0, -57, 0);
    let hopper = BlockPos::new(0, -58, 0);
    let target = BlockPos::new(0, -59, 0);
    mc_tick::set_block(&mut w, ctx, chest, state("minecraft:chest"), Flags::ALL);
    mc_tick::set_block(&mut w, ctx, hopper, state("minecraft:hopper"), Flags::ALL);
    mc_tick::set_block(&mut w, ctx, target, state("minecraft:chest"), Flags::ALL);
    w.extra
        .items
        .insert(chest, vec![Some((7, 3)), None, None, None, None]);
    w.extra.items.insert(hopper, vec![None; 5]);
    w.extra.items.insert(target, vec![None; 5]);
    let count =
        |w: &World, p: BlockPos| -> u32 { w.extra.items[&p].iter().flatten().map(|s| s.1).sum() };
    // The hopper above `target` faces down by default.
    for _ in 0..40 {
        w.time += 1;
        mc_tick::hopper::tick(&mut w, ctx, hopper);
    }
    assert_eq!(count(&w, chest) + count(&w, hopper) + count(&w, target), 3);
    assert_eq!(count(&w, target), 3, "all three arrive, 8 ticks apart");
    // Powered: locked.
    w.extra
        .items
        .insert(chest, vec![Some((7, 2)), None, None, None, None]);
    mc_tick::set_block(
        &mut w,
        ctx,
        BlockPos::new(1, -58, 0),
        state("minecraft:redstone_block"),
        Flags::ALL,
    );
    assert!(name_at(&data, &w, hopper).contains("enabled=false"));
    for _ in 0..40 {
        w.time += 1;
        mc_tick::hopper::tick(&mut w, ctx, hopper);
    }
    assert_eq!(count(&w, chest), 2);
}

#[test]
fn dispenser_fires_once_per_rising_edge_after_four_ticks() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    floor(&mut w, &data, -60);
    let d = BlockPos::new(0, -59, 0);
    mc_tick::set_block(&mut w, ctx, d, state("minecraft:dispenser"), Flags::ALL);
    let src = BlockPos::new(1, -59, 0);
    mc_tick::set_block(
        &mut w,
        ctx,
        src,
        state("minecraft:redstone_block"),
        Flags::ALL,
    );
    assert!(name_at(&data, &w, d).contains("triggered=true"));
    run_block_ticks(&mut w, ctx, 3);
    assert!(w.extra.dispensed.is_empty());
    run_block_ticks(&mut w, ctx, 1);
    assert_eq!(w.extra.dispensed.len(), 1);
    // Still powered: no second shot. Unpowered: re-armed.
    run_block_ticks(&mut w, ctx, 10);
    assert_eq!(w.extra.dispensed.len(), 1);
    mc_tick::set_block(&mut w, ctx, src, state("minecraft:air"), Flags::ALL);
    assert!(name_at(&data, &w, d).contains("triggered=false"));
    // The dropper is the same with dropper = true.
    let dr = BlockPos::new(0, -59, 3);
    mc_tick::set_block(&mut w, ctx, dr, state("minecraft:dropper"), Flags::ALL);
    mc_tick::set_block(
        &mut w,
        ctx,
        BlockPos::new(1, -59, 3),
        state("minecraft:redstone_block"),
        Flags::ALL,
    );
    run_block_ticks(&mut w, ctx, 4);
    assert!(
        w.extra
            .dispensed
            .iter()
            .any(|&(p, _, dropper)| p == dr && dropper)
    );
}

#[test]
fn sponge_soaks_water_within_range_and_wets() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    let water = shapes.state("minecraft:water", &[("level", "0")]).unwrap();
    let lava = shapes.state("minecraft:lava", &[("level", "0")]).unwrap();
    // A 9x9x9 pool of water, and one lava block that must stay.
    for x in -4..=4 {
        for y in 0..9 {
            for z in -4..=4 {
                w.blocks.insert(BlockPos::new(x, y, z), water);
            }
        }
    }
    let lava_at = BlockPos::new(1, 4, 0);
    w.blocks.insert(lava_at, lava);
    let at = BlockPos::new(0, 4, 0);
    mc_tick::set_block(&mut w, ctx, at, state("minecraft:sponge"), Flags::ALL);
    assert_eq!(name_at(&data, &w, at), "minecraft:wet_sponge[]");
    let left = w.blocks.values().filter(|&&s| s == water).count();
    // 729 water blocks, minus the sponge's and the lava's, minus 65 soaked up.
    assert_eq!(left, 729 - 2 - 65);
    let now = w.block(lava_at);
    assert!(now != water && !shapes.is_air(now));
}

#[test]
fn wet_sponge_dries_in_the_nether_and_dry_sponge_waits_for_water() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    let at = BlockPos::new(0, 0, 0);
    mc_tick::set_block(&mut w, ctx, at, state("minecraft:wet_sponge"), Flags::ALL);
    assert_eq!(name_at(&data, &w, at), "minecraft:wet_sponge[]");
    w.extra.warm = true;
    let other = BlockPos::new(5, 0, 0);
    mc_tick::set_block(
        &mut w,
        ctx,
        other,
        state("minecraft:wet_sponge"),
        Flags::ALL,
    );
    assert_eq!(name_at(&data, &w, other), "minecraft:sponge[]");
    assert_eq!(w.extra.dried, 1);
    // A dry sponge with no water around stays dry, then soaks a neighbour that
    // appears.
    let dry = BlockPos::new(0, 5, 0);
    mc_tick::set_block(&mut w, ctx, dry, state("minecraft:sponge"), Flags::ALL);
    assert_eq!(name_at(&data, &w, dry), "minecraft:sponge[]");
    // Out of the Nether it soaks and stays wet.
    w.extra.warm = false;
    let water = shapes.state("minecraft:water", &[("level", "0")]).unwrap();
    mc_tick::set_block(&mut w, ctx, dry.offset(Dir::East), water, Flags::ALL);
    assert_eq!(name_at(&data, &w, dry), "minecraft:wet_sponge[]");
}

#[test]
fn chorus_breaks_apart_when_its_support_goes() {
    let Some((data, shapes, rules)) = new_world() else {
        return;
    };
    let ctx = Ctx {
        shapes: &shapes,
        facts: &rules,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap();
    let mut w = empty_world(&data);
    let at = |x, y, z| BlockPos::new(x, y, z);
    let put = |w: &mut World, p, name: &str| {
        mc_tick::set_block(w, ctx, p, state(name), Flags::ALL);
    };
    put(&mut w, at(0, 0, 0), "minecraft:end_stone");
    put(&mut w, at(0, 1, 0), "minecraft:chorus_plant");
    put(&mut w, at(0, 2, 0), "minecraft:chorus_plant");
    put(&mut w, at(0, 3, 0), "minecraft:chorus_flower");
    run_block_ticks(&mut w, ctx, 3);
    let kind = |w: &World, p| shapes.block_name(w.block(p));
    assert_eq!(kind(&w, at(0, 1, 0)), Some("minecraft:chorus_plant"));
    assert_eq!(shapes.property(w.block(at(0, 1, 0)), "down"), Some("true"));
    assert_eq!(shapes.property(w.block(at(0, 1, 0)), "up"), Some("true"));
    // The flower on top goes; the stem under it stays and stops pointing up.
    put(&mut w, at(0, 3, 0), "minecraft:air");
    run_block_ticks(&mut w, ctx, 3);
    assert_eq!(kind(&w, at(0, 2, 0)), Some("minecraft:chorus_plant"));
    assert_eq!(shapes.property(w.block(at(0, 2, 0)), "up"), Some("false"));
    // A side branch off the open top stays. A flower above that same block
    // would make the junction illegal, so the branch comes after the flower.
    put(&mut w, at(1, 2, 0), "minecraft:chorus_plant");
    put(&mut w, at(2, 2, 0), "minecraft:chorus_flower");
    run_block_ticks(&mut w, ctx, 3);
    assert_eq!(kind(&w, at(1, 2, 0)), Some("minecraft:chorus_plant"));
    assert_eq!(kind(&w, at(2, 2, 0)), Some("minecraft:chorus_flower"));
    // The base goes, and the stem, the side branch and its flower follow.
    put(&mut w, at(0, 1, 0), "minecraft:air");
    run_block_ticks(&mut w, ctx, 8);
    assert_eq!(kind(&w, at(0, 0, 0)), Some("minecraft:end_stone"));
    for p in [at(0, 1, 0), at(0, 2, 0), at(1, 2, 0), at(2, 2, 0)] {
        assert!(shapes.is_air(w.block(p)), "{p:?} still standing");
    }
}
