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
use crate::tick::lock;

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
            ctx.mobs.bell(ctx.dim, clicked);
            crate::stats::custom(ctx, actor.uuid, "bell_ring", 1);
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
        "minecraft:vault" => match &held {
            Some(h) if crate::trials::insert_key(ctx, clicked, actor.uuid, h) => {
                if actor.game_mode != CREATIVE {
                    consume_held(ctx, actor, hand);
                }
                true
            }
            _ => false,
        },
        "minecraft:respawn_anchor" => anchor(ctx, outcome, actor, hand, clicked, state, held_name),
        "minecraft:command_block"
        | "minecraft:chain_command_block"
        | "minecraft:repeating_command_block" => {
            match crate::command_blocks::open(ctx, actor.uuid, actor.game_mode, clicked) {
                Some(packet) => {
                    outcome.events.push(Event::Direct {
                        uuid: actor.uuid,
                        packet: crate::context::Direct::BlockEntityData(packet),
                    });
                    true
                }
                None => false,
            }
        }
        id @ ("minecraft:structure_block" | "minecraft:jigsaw") => {
            match crate::structure_blocks::open(ctx, actor.uuid, actor.game_mode, clicked, id) {
                Some(packet) => {
                    outcome.events.push(Event::Direct {
                        uuid: actor.uuid,
                        packet: crate::context::Direct::BlockEntityData(packet),
                    });
                    true
                }
                None => false,
            }
        }
        id @ ("minecraft:test_block" | "minecraft:test_instance_block") => {
            match crate::test_blocks::open(ctx, actor.uuid, actor.game_mode, clicked, id) {
                Some(packet) => {
                    outcome.events.push(Event::Direct {
                        uuid: actor.uuid,
                        packet: crate::context::Direct::BlockEntityData(packet),
                    });
                    true
                }
                None => false,
            }
        }
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
        // The music stops before the disc is out.
        crate::jukeboxes::stop(ctx, p);
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
    // Event 1010 plays the song, named by its place in the jukebox_song
    // registry as clients were told; the signal starts with the block.
    crate::jukeboxes::start(ctx, p);
    if let Some(new) = set_record(true) {
        set(ctx, outcome, p, new);
    }
    if actor.game_mode != CREATIVE {
        consume_held(ctx, actor, hand);
    }
    true
}

fn bed(ctx: &ServerContext, outcome: &mut Outcome, actor: Actor, p: Position, state: u32) -> bool {
    let rule = crate::environment::bed_rule(ctx, [p.x, p.y, p.z]);
    if rule.explodes {
        // Where its rule says so (the Nether, the End) both halves go and
        // the bed explodes (minecraft.wiki, "Bed").
        let blocks = &ctx.data.blocks;
        let head = blocks.property(state, "part") == Some("head");
        let facing = blocks
            .property(state, "facing")
            .and_then(mc_tick::Dir::from_name);
        if let Some(f) = facing {
            let (dx, dy, dz) = f.offset();
            let sign = if head { -1 } else { 1 };
            let other = Position::new(p.x + sign * dx, p.y + sign * dy, p.z + sign * dz);
            if ctx
                .world
                .block(other, blocks)
                .and_then(|s| blocks.block_name(s))
                == blocks.block_name(state)
            {
                set(ctx, outcome, other, ctx.shapes.air());
            }
        }
        set(ctx, outcome, p, ctx.shapes.air());
        lock(&ctx.pending_blasts).push(crate::explosion::Blast::respawn_point(p, false));
        return true;
    }
    // The head is where a sleeper lies; vanilla measures from both halves.
    let blocks = &ctx.data.blocks;
    let facing = blocks
        .property(state, "facing")
        .and_then(mc_tick::Dir::from_name);
    let to_head = |p: Position, sign: i32| match facing {
        Some(f) => {
            let (dx, _, dz) = f.offset();
            Position::new(p.x + sign * dx, p.y, p.z + sign * dz)
        }
        None => p,
    };
    let (head, foot) = if blocks.property(state, "part") == Some("head") {
        (p, to_head(p, -1))
    } else {
        (to_head(p, 1), p)
    };
    let near = |b: Position| {
        (actor.pose.x - (f64::from(b.x) + 0.5)).abs() <= 3.0
            && (actor.pose.y - f64::from(b.y)).abs() <= 2.0
            && (actor.pose.z - (f64::from(b.z) + 0.5)).abs() <= 3.0
    };
    if !near(head) && !near(foot) {
        message(outcome, actor, "block.minecraft.bed.too_far_away");
        return true;
    }
    let respawn = crate::players::Respawn {
        dimension: ctx.world.dimension.clone(),
        pos: (p.x, p.y, p.z),
        yaw: actor.pose.yaw,
        pitch: 0.0,
        forced: false,
    };
    let set_spawn = rule.can_set_spawn != "never";
    if set_spawn
        && let Some(pl) = ctx
            .players
            .lock()
            .expect("players lock")
            .get_mut(&actor.uuid)
    {
        pl.extra.respawn = Some(respawn);
    }
    // Sleep comes when the sky is dark enough (minecraft.wiki, "Bed"):
    // at night, a little longer in the rain, at any time in a thunderstorm.
    let dark = {
        let t = ctx
            .time
            .lock()
            .expect("time lock")
            .clocks
            .iter()
            .find(|c| c.name == "minecraft:overworld")
            .map_or(0, |c| c.ticks)
            .rem_euclid(24000);
        let weather = lock(&ctx.weather);
        (weather.raining && weather.thundering)
            || (12542..=23459).contains(&t)
            || (weather.raining && (12010..=23991).contains(&t))
    };
    let can_sleep = match rule.can_sleep.as_str() {
        "always" => true,
        "never" => false,
        _ => dark,
    };
    if set_spawn {
        message(outcome, actor, "block.minecraft.set_spawn");
    }
    if !can_sleep {
        message(outcome, actor, "block.minecraft.bed.no_sleep");
        return true;
    }
    if actor.game_mode != 1 && monsters_near(ctx, head, actor.entity_id) {
        message(outcome, actor, "block.minecraft.bed.not_safe");
        return true;
    }
    let age = ctx.time.lock().expect("time lock").age;
    {
        let mut sleepers = lock(&ctx.sleepers);
        if sleepers
            .iter()
            .any(|(u, s)| *u != actor.uuid && s.bed == head)
        {
            drop(sleepers);
            message(outcome, actor, "block.minecraft.bed.occupied");
            return true;
        }
        sleepers.insert(
            actor.uuid,
            Sleeper {
                since: age,
                bed: head,
                entity_id: actor.entity_id,
            },
        );
    }
    // Lying down: the pose and the bed tell every client (the sleeper's
    // own too) to lay the player in it.
    let _ = ctx.events.send(Event::EntityData {
        uuid: actor.uuid,
        packet: crate::mobs::blocks::sleep_data(actor.entity_id, Some([head.x, head.y, head.z])),
    });
    true
}

/// Monsters that keep a player awake: within 8 blocks of the bed's head
/// across and 5 up or down (minecraft.wiki, "Bed"), most hostile mobs; a
/// zombified piglin only when after this player.
fn monsters_near(ctx: &ServerContext, head: Position, player: i32) -> bool {
    use mc_entity::mob::Species::*;
    let (x, y, z) = (
        f64::from(head.x) + 0.5,
        f64::from(head.y),
        f64::from(head.z) + 0.5,
    );
    lock(&ctx.entities).iter().any(|e| {
        let mc_entity::Kind::Mob(m) = &e.kind else {
            return false;
        };
        let keeps_awake = match m.species {
            ZombifiedPiglin => m.target == Some(player),
            Slime | MagmaCube | Ghast | Hoglin | EnderDragon | CamelHusk | ZombieNautilus => false,
            s => s.category() == mc_entity::mob::Category::Monster,
        };
        keeps_awake
            && !m.is_dead()
            && (e.pos[0] - x).abs() <= 8.0
            && (e.pos[1] - y).abs() <= 5.0
            && (e.pos[2] - z).abs() <= 8.0
    })
}

/// A sleeping player gets up (the leave bed button, a hit, the morning):
/// the clients stand them up again.
pub fn wake(ctx: &ServerContext, uuid: u128) {
    let Some(sleeper) = lock(&ctx.sleepers).remove(&uuid) else {
        return;
    };
    let _ = ctx.events.send(Event::EntityData {
        uuid,
        packet: crate::mobs::blocks::sleep_data(sleeper.entity_id, None),
    });
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
    if !crate::environment::flag(
        ctx,
        Some([p.x, p.y, p.z]),
        crate::environment::RESPAWN_ANCHOR_WORKS,
        false,
    ) {
        // A charged anchor where it does not work (outside the Nether)
        // blows up; water beside it spares the blocks around.
        let blocks = &ctx.data.blocks;
        let wet = |q: Position| {
            ctx.world.block(q, blocks).is_some_and(|s| {
                blocks.block_name(s) == Some("minecraft:water")
                    || blocks.property(s, "waterlogged") == Some("true")
            })
        };
        let shielded = [(0, 1, 0), (1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)]
            .iter()
            .any(|&(dx, dy, dz)| wet(Position::new(p.x + dx, p.y + dy, p.z + dz)));
        set(ctx, outcome, p, ctx.shapes.air());
        lock(&ctx.pending_blasts).push(crate::explosion::Blast::respawn_point(p, shielded));
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

/// A player asleep in a bed.
#[derive(Clone, Copy, Debug)]
pub struct Sleeper {
    /// World age when they lay down.
    pub since: i64,
    /// The bed's head.
    pub bed: Position,
    pub entity_id: i32,
}

/// Players asleep in beds: when enough of them have slept long enough, the
/// night passes.
pub type Sleepers = std::collections::HashMap<u128, Sleeper>;

/// How long a player must have slept before the night may pass.
const SLEEP_TICKS: i64 = 100;

/// One game tick: once enough players have slept 100 ticks, morning comes
/// and they all wake (with `advance_time` off they only wake).
pub fn tick(ctx: &ServerContext) {
    let age = ctx.time.lock().expect("time lock").age;
    let online: Vec<u128> = ctx
        .players
        .lock()
        .expect("players lock")
        .values()
        .filter(|p| p.game_mode != 3)
        .map(|p| p.profile.uuid)
        .collect();
    let slept = {
        let mut sleepers = lock(&ctx.sleepers);
        // Those who left the game are no longer in bed.
        sleepers.retain(|u, _| online.contains(u));
        if sleepers.is_empty() {
            return;
        }
        sleepers
            .values()
            .filter(|s| age - s.since >= SLEEP_TICKS)
            .count()
    };
    // `players_sleeping_percentage` of the players (at least one) must
    // have slept.
    let percentage = ctx
        .game_rules
        .lock()
        .expect("rules lock")
        .int("players_sleeping_percentage");
    let required = ((online.len() as f64 * f64::from(percentage) / 100.0).ceil() as usize).max(1);
    if slept < required {
        return;
    }
    let asleep: Vec<u128> = lock(&ctx.sleepers).keys().copied().collect();
    for uuid in asleep {
        wake(ctx, uuid);
    }
    if !ctx.game_rule_bool("advance_time") {
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Pose;

    #[test]
    fn a_charged_anchor_outside_the_nether_explodes() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Some(ctx) = mc_data::McData::load(&dir)
            .ok()
            .and_then(|data| ServerContext::new(crate::Config::new(dir), data).ok())
        else {
            eprintln!("skipped: no game data");
            return;
        };
        let actor = Actor {
            uuid: 9,
            entity_id: 9,
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
        let p = Position::new(5, -60, 5);
        let state = ctx
            .data
            .blocks
            .state_id("minecraft:respawn_anchor", &[("charges", "2")])
            .unwrap();
        WorldLevel::new(&ctx).set_block(p, state);
        let mut outcome = Outcome::default();
        assert!(anchor(&ctx, &mut outcome, actor, 0, p, state, None));
        // The anchor is gone and a blast with fire waits for the tick.
        let now = ctx.world.block(p, &ctx.data.blocks).unwrap();
        assert!(ctx.data.blocks.is_air(now));
        let blasts = lock(&ctx.pending_blasts);
        assert_eq!(blasts.len(), 1);
        assert!(blasts[0].fire && blasts[0].respawn == Some(false));
        assert_eq!(blasts[0].power, 5.0);
        drop(blasts);
        // An uncharged one is left alone.
        let empty = ctx
            .data
            .blocks
            .state_id("minecraft:respawn_anchor", &[("charges", "0")])
            .unwrap();
        WorldLevel::new(&ctx).set_block(p, empty);
        assert!(!anchor(&ctx, &mut outcome, actor, 0, p, empty, None));
        assert_eq!(lock(&ctx.pending_blasts).len(), 1);
    }

    fn sleeper(ctx: &ServerContext) -> (Actor, Position, u32) {
        crate::testkit::join(ctx, 9, 9, [0.5, -60.0, 0.5], 0);
        let actor = Actor {
            uuid: 9,
            entity_id: 9,
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
        // Head at z=2, foot at z=1.
        let blocks = &ctx.data.blocks;
        let mut level = WorldLevel::new(ctx);
        for (part, z) in [("foot", 1), ("head", 2)] {
            let state = blocks
                .state_id("minecraft:red_bed", &[("part", part), ("facing", "south")])
                .unwrap();
            level.set_block(Position::new(0, -60, z), state);
        }
        let foot = Position::new(0, -60, 1);
        (actor, foot, ctx.world.block(foot, blocks).unwrap())
    }

    fn set_clock(ctx: &ServerContext, ticks: i64) {
        let mut time = lock(&ctx.time);
        let clock = time
            .clocks
            .iter_mut()
            .find(|c| c.name == "minecraft:overworld")
            .unwrap();
        clock.ticks = ticks;
    }

    fn clock(ctx: &ServerContext) -> i64 {
        lock(&ctx.time)
            .clocks
            .iter()
            .find(|c| c.name == "minecraft:overworld")
            .unwrap()
            .ticks
    }

    #[test]
    fn a_player_lies_down_at_night_and_morning_comes() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let (actor, foot, state) = sleeper(&ctx);
        let mut events = ctx.events.subscribe();
        // By day they only get their spawn set.
        set_clock(&ctx, 1000);
        let mut outcome = Outcome::default();
        assert!(bed(&ctx, &mut outcome, actor, foot, state));
        assert!(lock(&ctx.sleepers).is_empty());
        set_clock(&ctx, 18000);
        assert!(bed(&ctx, &mut outcome, actor, foot, state));
        // Laid in the bed's head, for everyone to see.
        let laid = std::iter::from_fn(|| events.try_changed()).any(|e| {
            matches!(e, Event::EntityData { uuid: 9, packet }
                if packet.entries.iter().any(|(k, v)| *k == mc_protocol::packets::play::metadata::POSE
                    && matches!(v, mc_protocol::packets::play::MetadataValue::Pose(2))))
        });
        assert!(laid);
        assert_eq!(lock(&ctx.sleepers)[&9].bed, Position::new(0, -60, 2));
        for _ in 0..SLEEP_TICKS {
            lock(&ctx.time).age += 1;
            tick(&ctx);
        }
        assert_eq!(clock(&ctx), 24000);
        assert!(lock(&ctx.sleepers).is_empty());
    }

    #[test]
    fn leaving_the_bed_keeps_the_night() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let (actor, foot, state) = sleeper(&ctx);
        set_clock(&ctx, 18000);
        assert!(bed(&ctx, &mut Outcome::default(), actor, foot, state));
        lock(&ctx.time).age += 50;
        wake(&ctx, 9);
        for _ in 0..SLEEP_TICKS {
            lock(&ctx.time).age += 1;
            tick(&ctx);
        }
        assert_eq!(clock(&ctx), 18000);
    }

    #[test]
    fn a_zombie_by_the_bed_keeps_the_player_up() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let (actor, foot, state) = sleeper(&ctx);
        set_clock(&ctx, 18000);
        crate::commands::execute(
            &ctx,
            &mut crate::tick::TickControl::default(),
            &crate::commands::Source::Console,
            "summon minecraft:zombie 4 -60 4",
        );
        assert!(!lock(&ctx.entities).is_empty());
        let mut outcome = Outcome::default();
        assert!(bed(&ctx, &mut outcome, actor, foot, state));
        assert!(lock(&ctx.sleepers).is_empty());
        let told = outcome.events.iter().any(|e| {
            matches!(e, Event::Message { line, .. } if format!("{line:?}").contains("not_safe"))
        });
        assert!(told);
    }
}
