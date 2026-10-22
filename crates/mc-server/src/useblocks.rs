//! Blocks that do something when a player uses them, besides opening a
//! window: composters, jukeboxes, bells, beds and respawn anchors
//! (minecraft.wiki pages of each).

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::text;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::interact::{Actor, Outcome};
use crate::level::{WorldLevel, to_pos};

const CREATIVE: i32 = 1;

fn set(ctx: &ServerContext, outcome: &mut Outcome, p: Position, state: u32) {
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    mc_tick::set_block(&mut level, logic, to_pos(p), state, mc_tick::Flags::ALL);
    let changes = level.into_changes();
    outcome.updates.extend(changes.iter().copied());
    outcome.events.push(Event::BlocksChanged { by: 0, changes });
}

fn centre(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

fn message(outcome: &mut Outcome, actor: Actor, key: &str) {
    outcome.events.push(Event::Message {
        uuid: actor.uuid,
        line: text::translate(key, Vec::new()),
        overlay: true,
    });
}

/// The chance (percent) that an item fills a composter a level.
pub fn compost_chance(name: &str) -> u32 {
    let n = name.strip_prefix("minecraft:").unwrap_or(name);
    if n.ends_with("_leaves") || n.ends_with("_sapling") {
        return 30;
    }
    match n {
        "kelp" | "dried_kelp" | "beetroot_seeds" | "melon_seeds" | "pumpkin_seeds"
        | "wheat_seeds" | "torchflower_seeds" | "pitcher_pod" | "short_grass" | "seagrass"
        | "sweet_berries" | "glow_berries" | "moss_carpet" | "pale_moss_carpet"
        | "hanging_roots" | "small_dripleaf" | "mangrove_propagule" | "cherry_sapling"
        | "short_dry_grass" | "tall_dry_grass" | "bush" | "firefly_bush" => 30,
        "cactus"
        | "dried_kelp_block"
        | "flowering_azalea_leaves"
        | "glow_lichen"
        | "melon_slice"
        | "nether_sprouts"
        | "sugar_cane"
        | "tall_grass"
        | "twisting_vines"
        | "vine"
        | "weeping_vines"
        | "sunflower_unused"
        | "leaf_litter"
        | "wildflowers" => 50,
        "apple"
        | "azalea"
        | "beetroot"
        | "big_dripleaf"
        | "carrot"
        | "cocoa_beans"
        | "fern"
        | "large_fern"
        | "lily_pad"
        | "melon"
        | "moss_block"
        | "pale_moss_block"
        | "brown_mushroom"
        | "red_mushroom"
        | "nether_wart"
        | "potato"
        | "pumpkin"
        | "carved_pumpkin"
        | "crimson_roots"
        | "warped_roots"
        | "sea_pickle"
        | "shroomlight"
        | "spore_blossom"
        | "wheat"
        | "crimson_fungus"
        | "warped_fungus"
        | "dandelion"
        | "poppy"
        | "blue_orchid"
        | "allium"
        | "azure_bluet"
        | "red_tulip"
        | "orange_tulip"
        | "white_tulip"
        | "pink_tulip"
        | "oxeye_daisy"
        | "cornflower"
        | "lily_of_the_valley"
        | "wither_rose"
        | "torchflower"
        | "open_eyeblossom"
        | "closed_eyeblossom"
        | "sunflower"
        | "lilac"
        | "rose_bush"
        | "peony"
        | "pitcher_plant"
        | "cherry_leaves_unused"
        | "pale_hanging_moss" => 65,
        "baked_potato"
        | "bread"
        | "cookie"
        | "flowering_azalea"
        | "hay_block"
        | "red_mushroom_block"
        | "brown_mushroom_block"
        | "mushroom_stem"
        | "nether_wart_block"
        | "warped_wart_block" => 85,
        "cake" | "pumpkin_pie" => 100,
        _ => 0,
    }
}

/// Tries to use the block at `clicked`; `true` if it reacted (the caller
/// returns `outcome`).
pub fn try_use(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    clicked: Position,
    face: usize,
) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(clicked, blocks) else {
        return false;
    };
    let Some(name) = blocks.block_name(state) else {
        return false;
    };
    let held = {
        let inventories = ctx.inventories.lock().expect("inventories lock");
        inventories
            .get(&actor.uuid)
            .and_then(|h| h.held(hand).cloned())
    };
    let held_name = held.as_ref().and_then(|s| ctx.items.name(s.item));
    let reacted = match name {
        "minecraft:composter" => composter(ctx, outcome, actor, hand, clicked, state, held_name),
        "minecraft:jukebox" => jukebox(ctx, outcome, actor, hand, clicked, state, held_name),
        "minecraft:bell" => {
            fx::sound(
                ctx,
                "block.bell.use",
                fx::category::BLOCKS,
                centre(clicked),
                2.0,
                1.0,
            );
            outcome.events.push(Event::BlockAction {
                position: clicked,
                action: 1,
                param: face as u8,
                block: ctx.data.builtin.id("minecraft:block", name).unwrap_or(0),
            });
            true
        }
        n if n.ends_with("_bed") => bed(ctx, outcome, actor, clicked, state),
        "minecraft:respawn_anchor" => anchor(ctx, outcome, actor, hand, clicked, state, held_name),
        _ => false,
    };
    if reacted {
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
    }
    reacted
}

fn level_of(ctx: &ServerContext, state: u32, key: &str) -> i32 {
    ctx.data
        .blocks
        .property(state, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn composter(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    p: Position,
    state: u32,
    held: Option<&str>,
) -> bool {
    let level = level_of(ctx, state, "level");
    if level == 8 {
        let mut world = crate::level::WorldLevel::new(ctx);
        let logic = world.logic();
        mc_tick::composter::empty(&mut world, logic, crate::level::to_pos(p), state);
        let changes = world.into_changes();
        outcome.updates.extend(changes.iter().copied());
        outcome.events.push(Event::BlocksChanged { by: 0, changes });
        if let Some(bone_meal) = ctx.items.id("minecraft:bone_meal") {
            crate::tick::drop_item(
                ctx,
                p,
                crate::loot::Drop {
                    item: bone_meal,
                    count: 1,
                    components: Compound::new(),
                },
            );
        }
        return true;
    }
    let chance = held.map_or(0, compost_chance);
    if level >= 7 || chance == 0 {
        return false;
    }
    let success = crate::util::random_u64() % 100 < u64::from(chance);
    let mut world = crate::level::WorldLevel::new(ctx);
    let logic = world.logic();
    mc_tick::composter::add(&mut world, logic, crate::level::to_pos(p), state, success);
    let changes = world.into_changes();
    outcome.updates.extend(changes.iter().copied());
    outcome.events.push(Event::BlocksChanged { by: 0, changes });
    if actor.game_mode != CREATIVE {
        consume_held(ctx, actor, hand);
    }
    true
}

fn consume_held(ctx: &ServerContext, actor: Actor, hand: i32) {
    let slot = {
        let inventories = ctx.inventories.lock().expect("inventories lock");
        inventories.get(&actor.uuid).map(|h| h.hand_slot(hand))
    };
    if let Some(slot) = slot {
        crate::held::consume(ctx, actor.uuid, slot, 1);
    }
}

fn jukebox(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    p: Position,
    state: u32,
    held: Option<&str>,
) -> bool {
    let blocks = &ctx.data.blocks;
    let has = blocks.property(state, "has_record") == Some("true");
    let set_record = |value: bool| {
        blocks.state_id(
            "minecraft:jukebox",
            &[("has_record", if value { "true" } else { "false" })],
        )
    };
    if has {
        let record =
            ctx.world
                .block_entity(p, blocks)
                .and_then(|be| match be.data.get("RecordItem") {
                    Some(Tag::Compound(c)) => match c.get("id") {
                        Some(Tag::String(id)) => Some(id.clone()),
                        _ => None,
                    },
                    _ => None,
                });
        if let Some(id) = record.and_then(|id| ctx.items.id(&id)) {
            crate::tick::drop_item(
                ctx,
                p,
                crate::loot::Drop {
                    item: id,
                    count: 1,
                    components: Compound::new(),
                },
            );
        }
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: "minecraft:jukebox".into(),
                data: Compound::new(),
            },
            blocks,
        );
        if let Some(new) = set_record(false) {
            set(ctx, outcome, p, new);
        }
        fx::level_event(ctx, 1011, centre(p), 0);
        return true;
    }
    let Some(name) = held.filter(|n| n.starts_with("minecraft:music_disc_")) else {
        return false;
    };
    let mut record = Compound::new();
    record.insert("id", Tag::String(name.to_owned()));
    record.insert("count", Tag::Int(1));
    let mut data = Compound::new();
    data.insert("RecordItem", Tag::Compound(record));
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: "minecraft:jukebox".into(),
            data,
        },
        blocks,
    );
    if let Some(new) = set_record(true) {
        set(ctx, outcome, p, new);
    }
    // Event 1010 plays the song, named by its place in the jukebox_song
    // registry as clients were told.
    let song = ctx
        .items
        .get(ctx.items.id(name).unwrap_or(-1))
        .and_then(|i| i.jukebox_song.clone())
        .and_then(|song| {
            ctx.data
                .data_pack()
                .registry_entries("minecraft:jukebox_song")
                .ok()?
                .iter()
                .position(|e| *e == song)
        });
    if let Some(song) = song {
        fx::level_event(ctx, 1010, centre(p), song as i32);
    }
    if actor.game_mode != CREATIVE {
        consume_held(ctx, actor, hand);
    }
    true
}

fn bed(ctx: &ServerContext, outcome: &mut Outcome, actor: Actor, p: Position, _state: u32) -> bool {
    if ctx.world.dimension != "minecraft:overworld" {
        return true;
    }
    let respawn = crate::players::Respawn {
        dimension: ctx.world.dimension.clone(),
        pos: (p.x, p.y, p.z),
        yaw: actor.pose.yaw,
        pitch: 0.0,
        forced: false,
    };
    if let Some(pl) = ctx
        .players
        .lock()
        .expect("players lock")
        .get_mut(&actor.uuid)
    {
        pl.extra.respawn = Some(respawn);
    }
    let night = {
        let time = ctx.time.lock().expect("time lock");
        let t = time
            .clocks
            .iter()
            .find(|c| c.name == "minecraft:overworld")
            .map_or(0, |c| c.ticks)
            .rem_euclid(24000);
        (12542..=23459).contains(&t)
    };
    if night {
        message(outcome, actor, "block.minecraft.set_spawn");
        ctx.sleepers
            .lock()
            .expect("sleepers lock")
            .insert(actor.uuid, ctx.time.lock().expect("time lock").age);
    } else {
        message(outcome, actor, "block.minecraft.bed.no_sleep");
        message(outcome, actor, "block.minecraft.set_spawn");
    }
    true
}

fn anchor(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    p: Position,
    state: u32,
    held: Option<&str>,
) -> bool {
    let charges = level_of(ctx, state, "charges");
    let with = |c: i32| {
        ctx.data
            .blocks
            .state_id("minecraft:respawn_anchor", &[("charges", &c.to_string())])
    };
    if held == Some("minecraft:glowstone") && charges < 4 {
        if let Some(new) = with(charges + 1) {
            set(ctx, outcome, p, new);
        }
        fx::sound(
            ctx,
            "block.respawn_anchor.charge",
            fx::category::BLOCKS,
            centre(p),
            1.0,
            1.0,
        );
        if actor.game_mode != CREATIVE {
            consume_held(ctx, actor, hand);
        }
        return true;
    }
    if charges == 0 {
        return false;
    }
    if !ctx.world.dimension.contains("nether") {
        return true;
    }
    let respawn = crate::players::Respawn {
        dimension: ctx.world.dimension.clone(),
        pos: (p.x, p.y, p.z),
        yaw: actor.pose.yaw,
        pitch: 0.0,
        forced: false,
    };
    if let Some(pl) = ctx
        .players
        .lock()
        .expect("players lock")
        .get_mut(&actor.uuid)
    {
        pl.extra.respawn = Some(respawn);
    }
    fx::sound(
        ctx,
        "block.respawn_anchor.set_spawn",
        fx::category::BLOCKS,
        centre(p),
        1.0,
        1.0,
    );
    message(outcome, actor, "block.minecraft.set_spawn");
    true
}

/// Players who lay down in a bed at night, and when (world age): when all
/// of the players have, the night passes.
pub type Sleepers = std::collections::HashMap<u128, i64>;

/// One game tick: if every player is in bed, morning comes after 100 ticks.
pub fn tick(ctx: &ServerContext) {
    let age = ctx.time.lock().expect("time lock").age;
    let mut sleepers = ctx.sleepers.lock().expect("sleepers lock");
    sleepers.retain(|_, since| age - *since < 101);
    if sleepers.is_empty() {
        return;
    }
    let online: Vec<u128> = ctx
        .players
        .lock()
        .expect("players lock")
        .values()
        .filter(|p| p.game_mode != 3)
        .map(|p| p.profile.uuid)
        .collect();
    if online.is_empty()
        || !online.iter().all(|u| sleepers.contains_key(u))
        || sleepers.values().any(|&since| age - since < 100)
    {
        return;
    }
    sleepers.clear();
    drop(sleepers);
    let packet = {
        let mut time = ctx.time.lock().expect("time lock");
        if let Some(clock) = time
            .clocks
            .iter_mut()
            .find(|c| c.name == "minecraft:overworld")
        {
            let today = clock.ticks.div_euclid(24000);
            clock.ticks = (today + 1) * 24000;
        }
        time.packet(true)
    };
    let _ = ctx.events.send(Event::Time(packet));
}
