use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use crate::context::{Pose, ServerContext};
use crate::interact::{Actor, Hands, use_item_on};
use crate::level::WorldLevel;
use crate::tick::lock;

const AT: Position = Position { x: 0, y: -60, z: 2 };

fn setup() -> Option<(ServerContext, Actor)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let data = mc_data::McData::load(&dir).ok()?;
    let ctx = ServerContext::new(crate::Config::new(dir), data).ok()?;
    let actor = Actor {
        uuid: 7,
        entity_id: 7,
        game_mode: 0,
        pose: Pose {
            x: 0.5,
            y: -60.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        },
        sneaking: false,
    };
    lock(&ctx.inventories).insert(7, Hands::default());
    lock(&ctx.vitals).insert(7, crate::vitals::Vitals::default());
    Some((ctx, actor))
}

fn hold(ctx: &ServerContext, item: &str, count: i32, comps: Option<Compound>) {
    let id = ctx.items.id(item).unwrap();
    let mut inv = lock(&ctx.inventories);
    let hands = inv.get_mut(&7).unwrap();
    hands.slots[36] = Some(ItemStack {
        has_component_changes: comps.is_some(),
        ..ItemStack::new(id, count)
    });
    match comps {
        Some(c) => hands.components.insert(36, Tag::Compound(c)),
        None => hands.components.remove(&36),
    };
}

fn held(ctx: &ServerContext) -> (Option<String>, i32, Option<Tag>) {
    let inv = lock(&ctx.inventories);
    let hands = &inv[&7];
    let stack = hands.slots[36].as_ref();
    (
        stack.and_then(|s| ctx.items.name(s.item).map(str::to_owned)),
        stack.map_or(0, |s| s.count),
        hands.components.get(&36).cloned(),
    )
}

fn put(ctx: &ServerContext, p: Position, block: &str, props: &[(&str, &str)]) {
    let state = ctx.data.blocks.state_id(block, props).unwrap();
    WorldLevel::new(ctx).set_block(p, state);
}

fn block(ctx: &ServerContext, p: Position) -> String {
    let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
    ctx.data
        .blocks
        .describe(s)
        .unwrap()
        .trim_end_matches("[]")
        .to_owned()
}

fn click(ctx: &ServerContext, actor: Actor, p: Position, face: i32) -> crate::interact::Outcome {
    use_item_on(ctx, actor, 0, p, face, [0.5, 0.5, 0.5], 1)
}

#[test]
fn buckets_and_bottles_in_a_cauldron() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:cauldron", &[]);
    hold(&ctx, "minecraft:water_bucket", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:water_cauldron[level=3]");
    assert_eq!(held(&ctx).0.as_deref(), Some("minecraft:bucket"));
    // A bottle takes a third.
    hold(&ctx, "minecraft:glass_bottle", 2, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:water_cauldron[level=2]");
    let potion = lock(&ctx.inventories)[&7]
        .slots
        .iter()
        .flatten()
        .any(|s| ctx.items.name(s.item) == Some("minecraft:potion"));
    assert!(potion);
    // An empty bucket only takes a full cauldron.
    hold(&ctx, "minecraft:bucket", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:water_cauldron[level=2]");
    // Water back in from a bottle, then the bucket takes it all.
    let mut water = Compound::new();
    let mut contents = Compound::new();
    contents.insert("potion", Tag::String("minecraft:water".into()));
    water.insert("minecraft:potion_contents", Tag::Compound(contents));
    hold(&ctx, "minecraft:potion", 1, Some(water));
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:water_cauldron[level=3]");
    assert_eq!(held(&ctx).0.as_deref(), Some("minecraft:glass_bottle"));
    hold(&ctx, "minecraft:bucket", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:cauldron");
    assert_eq!(held(&ctx).0.as_deref(), Some("minecraft:water_bucket"));
    // Lava.
    hold(&ctx, "minecraft:lava_bucket", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:lava_cauldron");
}

#[test]
fn water_washes_dyed_leather() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:water_cauldron", &[("level", "1")]);
    let mut dyed = Compound::new();
    dyed.insert("minecraft:dyed_color", Tag::Int(0xff0000));
    hold(&ctx, "minecraft:leather_helmet", 1, Some(dyed));
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:cauldron");
    assert_eq!(held(&ctx).2, None);
}

#[test]
fn cake_slices_and_candles() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:cake", &[]);
    hold(&ctx, "minecraft:red_candle", 2, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:red_candle_cake[lit=false]");
    assert_eq!(held(&ctx).1, 1);
    // Full players do not eat.
    lock(&ctx.inventories).get_mut(&7).unwrap().slots[36] = None;
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:red_candle_cake[lit=false]");
    lock(&ctx.vitals).get_mut(&7).unwrap().food = 10;
    let o = click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:cake[bites=1]");
    assert_eq!(lock(&ctx.vitals)[&7].food, 12);
    assert_eq!(o.drops.len(), 1, "the candle drops");
}

#[test]
fn flower_pots_take_and_give_plants() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:flower_pot", &[]);
    hold(&ctx, "minecraft:poppy", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:potted_poppy");
    assert_eq!(held(&ctx).0, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:flower_pot");
    assert_eq!(held(&ctx).0.as_deref(), Some("minecraft:poppy"));
}

#[test]
fn tools_strip_scrape_wax_till_and_flatten() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:oak_log", &[("axis", "x")]);
    hold(&ctx, "minecraft:iron_axe", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:stripped_oak_log[axis=x]");
    assert_eq!(
        lock(&ctx.inventories)[&7].slots[36]
            .as_ref()
            .unwrap()
            .damage,
        1
    );

    put(&ctx, AT, "minecraft:copper_block", &[]);
    hold(&ctx, "minecraft:honeycomb", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:waxed_copper_block");
    hold(&ctx, "minecraft:iron_axe", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:copper_block");
    put(
        &ctx,
        AT,
        "minecraft:weathered_cut_copper_stairs",
        &[("facing", "east")],
    );
    click(&ctx, actor, AT, 1);
    assert!(block(&ctx, AT).starts_with("minecraft:exposed_cut_copper_stairs[facing=east"));

    put(&ctx, AT, "minecraft:dirt", &[]);
    hold(&ctx, "minecraft:wooden_hoe", 1, None);
    click(&ctx, actor, AT, 1);
    assert!(block(&ctx, AT).starts_with("minecraft:farmland"));
    put(&ctx, AT, "minecraft:grass_block", &[]);
    hold(&ctx, "minecraft:wooden_shovel", 1, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:dirt_path");
}

#[test]
fn bone_meal_grows_grass_tall() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:short_grass", &[]);
    hold(&ctx, "minecraft:bone_meal", 2, None);
    click(&ctx, actor, AT, 1);
    assert_eq!(block(&ctx, AT), "minecraft:tall_grass[half=lower]");
    let up = Position::new(AT.x, AT.y + 1, AT.z);
    assert_eq!(block(&ctx, up), "minecraft:tall_grass[half=upper]");
    assert_eq!(held(&ctx).1, 1);
}

#[test]
fn candles_stack_and_signs_take_dye() {
    let Some((ctx, actor)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:candle", &[]);
    hold(&ctx, "minecraft:candle", 3, None);
    click(&ctx, actor, AT, 1);
    assert!(block(&ctx, AT).starts_with("minecraft:candle[candles=2"));

    put(&ctx, AT, "minecraft:oak_sign", &[("rotation", "8")]);
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: AT.x,
            y: AT.y,
            z: AT.z,
            id: "minecraft:sign".into(),
            data: Compound::new(),
        },
        &ctx.data.blocks,
    );
    hold(&ctx, "minecraft:red_dye", 1, None);
    click(&ctx, actor, AT, 1);
    let be = ctx.world.block_entity(AT, &ctx.data.blocks).unwrap();
    // The player stands north of a sign facing north: its front.
    let Some(Tag::Compound(front)) = be.data.get("front_text") else {
        panic!("no front text: {:?}", be.data);
    };
    assert_eq!(front.get("color"), Some(&Tag::String("red".into())));
    assert_eq!(held(&ctx).0, None);
}

#[test]
fn rain_and_drips_fill_cauldrons() {
    let Some((ctx, _)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:cauldron", &[]);
    let mut level = WorldLevel::new(&ctx);
    let logic = level.logic();
    let mut rng = mc_tick::rng::Rng::new(3);
    let p = crate::level::to_pos(AT);
    for _ in 0..200 {
        mc_tick::cauldron::precipitation(&mut level, logic, p, true, &mut rng);
    }
    drop(level);
    assert_eq!(block(&ctx, AT), "minecraft:water_cauldron[level=3]");

    // Lava over a dripstone block, a stalactite of two, a cauldron below.
    let x = 5;
    put(&ctx, Position::new(x, -60, 0), "minecraft:cauldron", &[]);
    put(
        &ctx,
        Position::new(x, -56, 0),
        "minecraft:pointed_dripstone",
        &[("vertical_direction", "down"), ("thickness", "tip")],
    );
    put(
        &ctx,
        Position::new(x, -55, 0),
        "minecraft:pointed_dripstone",
        &[("vertical_direction", "down"), ("thickness", "base")],
    );
    put(
        &ctx,
        Position::new(x, -54, 0),
        "minecraft:dripstone_block",
        &[],
    );
    put(&ctx, Position::new(x, -53, 0), "minecraft:lava", &[]);
    let mut level = WorldLevel::new(&ctx);
    let top = mc_tick::BlockPos::new(x, -55, 0);
    for _ in 0..400 {
        mc_tick::cauldron::dripstone_tick(&mut level, logic, top, &mut rng);
    }
    let cauldron = mc_tick::BlockPos::new(x, -60, 0);
    let kind = ctx
        .shapes
        .block_index(mc_tick::Level::block(&level, cauldron));
    assert!(mc_tick::Level::is_block_tick_scheduled(
        &level, cauldron, kind
    ));
    mc_tick::cauldron::block_tick(&mut level, logic, cauldron, kind);
    drop(level);
    assert_eq!(
        block(&ctx, Position::new(x, -60, 0)),
        "minecraft:lava_cauldron"
    );
}

#[test]
fn daylight_detectors_see_the_noon_sun() {
    let Some((ctx, _)) = setup() else {
        eprintln!("skipped: no game data");
        return;
    };
    put(&ctx, AT, "minecraft:daylight_detector", &[]);
    if let Some(c) = lock(&ctx.time).clocks.first_mut() {
        c.ticks = 6000;
    }
    crate::daylight::tick(&ctx, 0);
    assert!(block(&ctx, AT).contains("power=15"), "{}", block(&ctx, AT));
}
