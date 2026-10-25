//! Statistics (minecraft.wiki, "Statistics"): per-player counters kept in
//! `stats/<uuid>.json` as vanilla writes them
//! (`{"stats":{"minecraft:mined":{"minecraft:stone":3}},"DataVersion":N}`),
//! loaded when the player joins and saved with their player file. The
//! client's statistics screen asks for them (`Award Statistics`), and
//! scoreboard objectives with a stat criterion count along.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use mc_protocol::packets::play::AwardStats;

use crate::context::{Pose, ServerContext};
use crate::tick::lock;

/// What the stat types count, in vanilla's names.
pub const MINED: &str = "minecraft:mined";
pub const CRAFTED: &str = "minecraft:crafted";
pub const USED: &str = "minecraft:used";
pub const BROKEN: &str = "minecraft:broken";
pub const PICKED_UP: &str = "minecraft:picked_up";
pub const DROPPED: &str = "minecraft:dropped";
pub const KILLED: &str = "minecraft:killed";
pub const KILLED_BY: &str = "minecraft:killed_by";
pub const CUSTOM: &str = "minecraft:custom";

/// The registry a stat type's ids come from.
fn registry_of(kind: &str) -> &'static str {
    match kind {
        MINED => "minecraft:block",
        CRAFTED | USED | BROKEN | PICKED_UP | DROPPED => "minecraft:item",
        KILLED | KILLED_BY => "minecraft:entity_type",
        _ => "minecraft:custom_stat",
    }
}

/// `name` with the `minecraft:` namespace.
fn full(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// One player's counters: stat type → stat → value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Counts(BTreeMap<String, BTreeMap<String, i32>>);

impl Counts {
    pub fn get(&self, kind: &str, id: &str) -> i32 {
        self.0
            .get(&full(kind))
            .and_then(|m| m.get(&full(id)))
            .copied()
            .unwrap_or(0)
    }

    /// Adds `n` (it saturates, as vanilla's ints do); the new value.
    pub fn add(&mut self, kind: &str, id: &str, n: i32) -> i32 {
        let v = self
            .0
            .entry(full(kind))
            .or_default()
            .entry(full(id))
            .or_insert(0);
        *v = v.saturating_add(n);
        *v
    }

    pub fn set(&mut self, kind: &str, id: &str, value: i32) {
        if value == 0 {
            if let Some(m) = self.0.get_mut(&full(kind)) {
                m.remove(&full(id));
            }
        } else {
            self.0
                .entry(full(kind))
                .or_default()
                .insert(full(id), value);
        }
    }

    /// Every non-zero counter: (type, stat, value).
    pub fn all(&self) -> impl Iterator<Item = (&str, &str, i32)> {
        self.0.iter().flat_map(|(k, m)| {
            m.iter()
                .filter(|(_, v)| **v != 0)
                .map(move |(id, v)| (k.as_str(), id.as_str(), *v))
        })
    }

    /// The file's contents.
    pub fn to_json(&self, data_version: i32) -> serde_json::Value {
        let mut stats = serde_json::Map::new();
        for (kind, m) in &self.0 {
            let m: serde_json::Map<String, serde_json::Value> = m
                .iter()
                .filter(|(_, v)| **v != 0)
                .map(|(id, v)| (id.clone(), serde_json::Value::from(*v)))
                .collect();
            if !m.is_empty() {
                stats.insert(kind.clone(), serde_json::Value::Object(m));
            }
        }
        serde_json::json!({ "stats": stats, "DataVersion": data_version })
    }

    pub fn from_json(v: &serde_json::Value) -> Self {
        let mut out = Self::default();
        for (kind, m) in v["stats"].as_object().into_iter().flatten() {
            for (id, value) in m.as_object().into_iter().flatten() {
                if let Some(n) = value.as_i64() {
                    out.set(
                        kind,
                        id,
                        n.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                    );
                }
            }
        }
        out
    }
}

/// What is watched of a player between ticks (not saved).
#[derive(Debug, Default)]
struct Watch {
    sneaking: bool,
    sprinting: bool,
    /// In a bed in the last tick.
    asleep: bool,
    /// Where they were in the last tick.
    last: Option<Pose>,
    /// Vanilla's `moveDist` and the next whole step, for the step game
    /// events.
    walked: f64,
    next_step: f64,
}

/// The counters of everyone online.
#[derive(Debug, Default)]
pub struct All {
    counts: HashMap<u128, Counts>,
    watch: HashMap<u128, Watch>,
}

fn file(ctx: &ServerContext, uuid: u128) -> Option<PathBuf> {
    let world = ctx.config.world.as_ref()?;
    Some(
        world
            .join("stats")
            .join(format!("{}.json", crate::util::uuid_string(uuid))),
    )
}

/// Reads a joining player's file (a new player starts at zero).
pub fn load(ctx: &ServerContext, uuid: u128) {
    let counts = file(ctx, uuid).map(|f| read(&f)).unwrap_or_default();
    let mut all = lock(&ctx.stats);
    all.counts.insert(uuid, counts);
    all.watch.insert(uuid, Watch::default());
}

/// Writes an online player's file.
pub fn save(ctx: &ServerContext, uuid: u128) {
    let Some(file) = file(ctx, uuid) else {
        return;
    };
    let Some(counts) = lock(&ctx.stats).counts.get(&uuid).cloned() else {
        return;
    };
    if let Err(e) = write(&file, &counts, ctx.data.version.world_version) {
        tracing::warn!("saving statistics: {e:#}");
    }
}

/// The counters in a file (none for a missing or broken one).
fn read(file: &std::path::Path) -> Counts {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|v| Counts::from_json(&v))
        .unwrap_or_default()
}

fn write(file: &std::path::Path, counts: &Counts, data_version: i32) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&counts.to_json(data_version)).unwrap_or_default();
    std::fs::write(file, text)
}

/// A player is gone: their counters leave the memory.
pub fn forget(ctx: &ServerContext, uuid: u128) {
    let mut all = lock(&ctx.stats);
    all.counts.remove(&uuid);
    all.watch.remove(&uuid);
}

/// A player's value of a stat.
pub fn get(ctx: &ServerContext, uuid: u128, kind: &str, id: &str) -> i32 {
    lock(&ctx.stats)
        .counts
        .get(&uuid)
        .map_or(0, |c| c.get(kind, id))
}

/// Counts `n` more of a stat for an online player and lets the scoreboard
/// know.
pub fn add(ctx: &ServerContext, uuid: u128, kind: &str, id: &str, n: i32) {
    if n <= 0 {
        return;
    }
    {
        let mut all = lock(&ctx.stats);
        let Some(c) = all.counts.get_mut(&uuid) else {
            return;
        };
        c.add(kind, id, n);
    }
    crate::commands::stat_scores(ctx, uuid, &full(kind), &full(id), n);
}

/// `add` for a custom stat (`jump`, `minecraft:deaths`).
pub fn custom(ctx: &ServerContext, uuid: u128, id: &str, n: i32) {
    add(ctx, uuid, CUSTOM, id, n);
}

/// `add` for a stat of items, by item id.
pub fn item(ctx: &ServerContext, uuid: u128, kind: &str, item: i32, n: i32) {
    if let Some(name) = ctx.items.name(item) {
        add(ctx, uuid, kind, name, n);
    }
}

/// `add` for a stat of blocks, by state.
pub fn block(ctx: &ServerContext, uuid: u128, kind: &str, state: u32, n: i32) {
    if let Some(name) = ctx.data.blocks.block_name(state) {
        add(ctx, uuid, kind, name, n);
    }
}

/// Sets a stat back to zero.
pub fn reset(ctx: &ServerContext, uuid: u128, kind: &str, id: &str) {
    if let Some(c) = lock(&ctx.stats).counts.get_mut(&uuid) {
        c.set(kind, id, 0);
    }
}

/// Everything non-zero, for the statistics screen.
pub fn packet(ctx: &ServerContext, uuid: u128) -> AwardStats {
    let all = lock(&ctx.stats);
    let mut stats = Vec::new();
    if let Some(c) = all.counts.get(&uuid) {
        for (kind, id, value) in c.all() {
            let category = ctx.data.builtin.id("minecraft:stat_type", kind);
            let stat = ctx.data.builtin.id(registry_of(kind), id);
            if let (Some(category), Some(stat)) = (category, stat) {
                stats.push((category, stat, value));
            }
        }
    }
    AwardStats { stats }
}

/// A player opened the window of a block (or used the block): the
/// `open_*`, `interact_with_*` and `inspect_*` stats.
pub fn opened(ctx: &ServerContext, uuid: u128, block: Option<&str>) {
    let Some(block) = block else { return };
    let name = block.strip_prefix("minecraft:").unwrap_or(block);
    let stat = match name {
        "chest" => "open_chest",
        "trapped_chest" => {
            custom(ctx, uuid, "trigger_trapped_chest", 1);
            "open_chest"
        }
        "barrel" => "open_barrel",
        "ender_chest" => "open_enderchest",
        n if n.ends_with("shulker_box") => "open_shulker_box",
        "crafting_table" => "interact_with_crafting_table",
        "furnace" => "interact_with_furnace",
        "blast_furnace" => "interact_with_blast_furnace",
        "smoker" => "interact_with_smoker",
        "brewing_stand" => "interact_with_brewingstand",
        "beacon" => "interact_with_beacon",
        "anvil" | "chipped_anvil" | "damaged_anvil" => "interact_with_anvil",
        "grindstone" => "interact_with_grindstone",
        "smithing_table" => "interact_with_smithing_table",
        "loom" => "interact_with_loom",
        "stonecutter" => "interact_with_stonecutter",
        "cartography_table" => "interact_with_cartography_table",
        "lectern" => "interact_with_lectern",
        "campfire" | "soul_campfire" => "interact_with_campfire",
        "dispenser" => "inspect_dispenser",
        "dropper" => "inspect_dropper",
        "hopper" => "inspect_hopper",
        _ => return,
    };
    custom(ctx, uuid, stat, 1);
}

/// Whether the player sneaks now.
pub fn sneaking(ctx: &ServerContext, uuid: u128) -> bool {
    lock(&ctx.stats)
        .watch
        .get(&uuid)
        .is_some_and(|w| w.sneaking)
}

/// The player sneaks or stops (Player Input).
pub fn set_sneaking(ctx: &ServerContext, uuid: u128, on: bool) {
    if let Some(w) = lock(&ctx.stats).watch.get_mut(&uuid) {
        w.sneaking = on;
    }
}

/// The player sprints or stops (Player Command).
pub fn set_sprinting(ctx: &ServerContext, uuid: u128, on: bool) {
    if let Some(w) = lock(&ctx.stats).watch.get_mut(&uuid) {
        w.sprinting = on;
    }
}

pub fn is_sprinting(ctx: &ServerContext, uuid: u128) -> bool {
    lock(&ctx.stats)
        .watch
        .get(&uuid)
        .is_some_and(|w| w.sprinting)
}

/// Whether the player sits in a boat.
pub fn in_boat(ctx: &ServerContext, entity_id: i32) -> bool {
    vehicle_of(ctx, entity_id).is_some_and(|v| v.ends_with("boat") || v.ends_with("raft"))
}

/// `registry:` name of the entity type of `e`, for `killed` and `killed_by`.
pub fn entity_type(e: &mc_entity::Entity) -> Option<String> {
    use mc_entity::Kind;
    match &e.kind {
        Kind::Mob(m) => Some(m.species.id().to_owned()),
        Kind::Merchant(m) => Some(
            if m.wandering {
                "minecraft:wandering_trader"
            } else {
                "minecraft:villager"
            }
            .to_owned(),
        ),
        other => crate::fx::type_name(other).map(|n| format!("minecraft:{n}")),
    }
}

/// Distances, to the centimetre as vanilla rounds them.
fn cm(blocks: f64) -> i32 {
    (blocks * 100.0).round() as i32
}

/// What taking the result of a crafting grid made: the item and how many
/// of it, from how far the ingredients shrank (`slots[0]` is the result,
/// the rest the grid, before and after the click).
pub fn crafts(
    before: &[Option<mc_protocol::packets::play::ItemStack>],
    after: &[Option<mc_protocol::packets::play::ItemStack>],
) -> Option<(i32, i32)> {
    let result = before.first()?.as_ref()?;
    let times = before
        .iter()
        .zip(after)
        .skip(1)
        .map(|(b, a)| {
            let count = |s: &Option<mc_protocol::packets::play::ItemStack>| {
                s.as_ref().map_or(0, |s| s.count)
            };
            count(b) - count(a)
        })
        .max()?;
    (times > 0).then(|| (result.item, result.count * times))
}

/// A player threw a stack out of their hands.
pub fn threw(ctx: &ServerContext, uuid: u128, stack: &mc_protocol::packets::play::ItemStack) {
    item(ctx, uuid, DROPPED, stack.item, stack.count);
    custom(ctx, uuid, "drop", 1);
}

/// A player killed `e`.
pub fn killed(ctx: &ServerContext, uuid: u128, e: &mc_entity::Entity) {
    let Some(kind) = entity_type(e) else { return };
    add(ctx, uuid, KILLED, &kind, 1);
    custom(ctx, uuid, "mob_kills", 1);
}

/// A player died, hurt by `attacker` (the name death messages show).
pub fn died(ctx: &ServerContext, uuid: u128, attacker: Option<&str>) {
    custom(ctx, uuid, "deaths", 1);
    reset(ctx, uuid, CUSTOM, "time_since_death");
    let Some(name) = attacker else { return };
    let player = lock(&ctx.players)
        .values()
        .find(|p| p.profile.name == name)
        .map(|p| p.profile.uuid);
    if let Some(killer) = player {
        add(ctx, uuid, KILLED_BY, "minecraft:player", 1);
        if killer != uuid {
            add(ctx, killer, KILLED, "minecraft:player", 1);
            custom(ctx, killer, "player_kills", 1);
        }
        return;
    }
    let species = mc_entity::mob::Species::ALL
        .iter()
        .find(|s| s.display_name() == name);
    if let Some(s) = species {
        add(ctx, uuid, KILLED_BY, s.id(), 1);
    }
}

/// A player landed after falling `distance` blocks.
pub fn fell(ctx: &ServerContext, uuid: u128, distance: f64) {
    if distance >= 2.0 {
        custom(ctx, uuid, "fall_one_cm", cm(distance));
    }
    // The players lock is let go before the vibration, which takes it again.
    let pose = lock(&ctx.players).get(&uuid).and_then(|p| p.pose);
    if distance > 0.0
        && let Some(pose) = pose
    {
        let below = ground(ctx, pose);
        crate::vibrations::emit(
            ctx,
            "hit_ground",
            [pose.x, pose.y, pose.z],
            crate::vibrations::Source::player(ctx, uuid),
            below,
        );
    }
}

/// The block a player stands on.
fn ground(ctx: &ServerContext, pose: Pose) -> Option<u32> {
    ctx.world.block_if_loaded(mc_protocol::Position::new(
        pose.x.floor() as i32,
        (pose.y - 0.2).floor() as i32,
        pose.z.floor() as i32,
    ))
}

/// Every whole step on the ground is a step game event (`moveDist` grows
/// by six tenths of the way walked). Standing on a shrieker sets it off.
fn steps(ctx: &ServerContext, uuid: u128, (before, after): (Pose, Pose), game_mode: i32) {
    if game_mode == 3 || !after.on_ground {
        return;
    }
    let below = ground(ctx, after);
    let blocks = &ctx.data.blocks;
    let name = below.and_then(|s| blocks.block_name(s));
    if name == Some(crate::vibrations::SHRIEKER) {
        crate::vibrations::stepped_on(
            ctx,
            mc_protocol::Position::new(
                after.x.floor() as i32,
                (after.y - 0.2).floor() as i32,
                after.z.floor() as i32,
            ),
            uuid,
        );
    }
    let flat = (after.x - before.x).hypot(after.z - before.z);
    if flat == 0.0 || flat > 16.0 {
        return;
    }
    let step = {
        let mut all = lock(&ctx.stats);
        let Some(w) = all.watch.get_mut(&uuid) else {
            return;
        };
        w.walked += flat * 0.6;
        if w.walked > w.next_step {
            w.next_step = w.walked.floor() + 1.0;
            true
        } else {
            false
        }
    };
    if step {
        let mut source = crate::vibrations::Source::player(ctx, uuid);
        // A sensor stepped on hears even a sneaking player.
        if matches!(
            name,
            Some(crate::vibrations::SENSOR | crate::vibrations::CALIBRATED)
        ) {
            source.sneaking = false;
        }
        crate::vibrations::emit(ctx, "step", [after.x, after.y, after.z], source, below);
    }
}

/// One tick for every online player: the clocks, and how far they went.
pub fn tick(ctx: &ServerContext) {
    let players: Vec<(u128, i32, Pose, i32)> = lock(&ctx.players)
        .values()
        .filter_map(|p| {
            p.pose
                .map(|pose| (p.profile.uuid, p.game_mode, pose, p.entity_id))
        })
        .collect();
    for (uuid, game_mode, pose, entity_id) in players {
        let here = ctx.of_player(uuid);
        let ctx = &*here;
        let Some(watch) = lock(&ctx.stats).watch.get_mut(&uuid).map(|w| {
            let before = w.last.replace(pose);
            (before, w.sneaking, w.sprinting, w.asleep)
        }) else {
            continue;
        };
        let (before, sneaking, sprinting, was_asleep) = watch;
        let (alive, gliding) = lock(&ctx.vitals)
            .get(&uuid)
            .map_or((true, false), |v| (!v.dead, v.gliding));
        let asleep = lock(&ctx.sleepers).contains_key(&uuid);
        custom(ctx, uuid, "play_time", 1);
        custom(ctx, uuid, "total_world_time", 1);
        if alive {
            custom(ctx, uuid, "time_since_death", 1);
            if sneaking {
                custom(ctx, uuid, "sneak_time", 1);
            }
            if !asleep {
                custom(ctx, uuid, "time_since_rest", 1);
            }
        }
        if asleep && !was_asleep {
            custom(ctx, uuid, "sleep_in_bed", 1);
            reset(ctx, uuid, CUSTOM, "time_since_rest");
            crate::criteria::plain(ctx, uuid, "minecraft:slept_in_bed");
        }
        // Where the player is, once a second.
        if alive && get(ctx, uuid, CUSTOM, "minecraft:play_time") % 20 == 0 {
            crate::criteria::plain(ctx, uuid, "minecraft:location");
        }
        if let Some(w) = lock(&ctx.stats).watch.get_mut(&uuid) {
            w.asleep = asleep;
        }
        if let Some(before) = before {
            steps(ctx, uuid, (before, pose), game_mode);
            travelled(
                ctx,
                uuid,
                entity_id,
                (before, pose),
                (sneaking, sprinting, gliding, game_mode),
            );
        }
    }
}

/// The distance a player covered since the last tick, by how they got
/// around.
fn travelled(
    ctx: &ServerContext,
    uuid: u128,
    entity_id: i32,
    (before, after): (Pose, Pose),
    (sneaking, sprinting, gliding, game_mode): (bool, bool, bool, i32),
) {
    let (dx, dy, dz) = (after.x - before.x, after.y - before.y, after.z - before.z);
    let flat = dx.hypot(dz);
    let all = (flat * flat + dy * dy).sqrt();
    // A teleport is not a walk.
    if all > 16.0 || all == 0.0 {
        return;
    }
    let blocks = &ctx.data.blocks;
    let name_at = |y: f64| {
        ctx.world
            .block(
                mc_protocol::Position::new(
                    after.x.floor() as i32,
                    y.floor() as i32,
                    after.z.floor() as i32,
                ),
                blocks,
            )
            .and_then(|s| blocks.block_name(s))
            .map(str::to_owned)
    };
    // On a mount or in a vehicle.
    if let Some(vehicle) = vehicle_of(ctx, entity_id) {
        let stat = match vehicle.as_str() {
            "minecraft:pig" => Some("pig_one_cm"),
            "minecraft:strider" => Some("strider_one_cm"),
            "minecraft:horse"
            | "minecraft:donkey"
            | "minecraft:mule"
            | "minecraft:skeleton_horse"
            | "minecraft:zombie_horse"
            | "minecraft:camel"
            | "minecraft:llama" => Some("horse_one_cm"),
            "minecraft:happy_ghast" => Some("happy_ghast_one_cm"),
            v if v.ends_with("_boat") || v.ends_with("_raft") || v.ends_with("_chest_boat") => {
                Some("boat_one_cm")
            }
            v if v.ends_with("minecart") => Some("minecart_one_cm"),
            _ => None,
        };
        if let Some(stat) = stat {
            custom(ctx, uuid, stat, cm(flat));
        }
        return;
    }
    // A jump: off the ground, upwards.
    if before.on_ground && !after.on_ground && dy > 0.0 {
        custom(ctx, uuid, "jump", 1);
    }
    let feet = name_at(after.y);
    let eyes = name_at(after.y + 1.62);
    let in_water = feet.as_deref() == Some("minecraft:water");
    let under_water = eyes.as_deref() == Some("minecraft:water");
    crate::commands::in_water(ctx, uuid, in_water);
    let climbing = feet.as_deref().is_some_and(|n| {
        matches!(n, "minecraft:ladder" | "minecraft:scaffolding")
            || n.ends_with("vine")
            || n.ends_with("vines")
            || n.ends_with("vines_plant")
    });
    if in_water && sprinting && under_water {
        custom(ctx, uuid, "swim_one_cm", cm(all));
    } else if under_water {
        custom(ctx, uuid, "walk_under_water_one_cm", cm(all));
    } else if in_water {
        custom(ctx, uuid, "walk_on_water_one_cm", cm(flat));
    } else if climbing {
        if dy > 0.0 {
            custom(ctx, uuid, "climb_one_cm", cm(dy));
        }
    } else if after.on_ground {
        let stat = if sprinting {
            "sprint_one_cm"
        } else if sneaking {
            "crouch_one_cm"
        } else {
            "walk_one_cm"
        };
        custom(ctx, uuid, stat, cm(flat));
    } else if gliding {
        custom(ctx, uuid, "aviate_one_cm", cm(all));
    } else if game_mode != 3 && cm(flat) > 25 {
        custom(ctx, uuid, "fly_one_cm", cm(flat));
    }
}

/// The entity type of what a player rides, if they ride.
fn vehicle_of(ctx: &ServerContext, entity_id: i32) -> Option<String> {
    use mc_entity::Kind;
    let entities = ctx.entities.try_lock().ok()?;
    entities.iter().find_map(|e| {
        let riders: &[i32] = match &e.kind {
            Kind::Vehicle(v) => &v.riders,
            Kind::Mob(m) => &m.riders,
            _ => return None,
        };
        riders
            .contains(&entity_id)
            .then(|| entity_type(e))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    const ME: u128 = 0x57a7;

    /// A flat world with a player standing in it, their statistics loaded.
    fn setup() -> Option<std::sync::Arc<ServerContext>> {
        let ctx = testkit::flat()?;
        testkit::join(&ctx, ME, 41, [0.5, -60.0, 0.5], 0);
        load(&ctx, ME);
        Some(ctx)
    }

    fn count(ctx: &ServerContext, id: &str) -> i32 {
        get(ctx, ME, CUSTOM, id)
    }

    #[test]
    fn a_file_is_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!("mc-stats-{}", std::process::id()));
        let file = dir.join("stats/x.json");
        let mut c = Counts::default();
        c.add(MINED, "stone", 9);
        c.add(CUSTOM, "jump", 2);
        write(&file, &c, 4321).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("\"minecraft:mined\"") && text.contains("\"DataVersion\": 4321"));
        assert_eq!(read(&file), c);
        assert_eq!(read(&dir.join("missing.json")), Counts::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_clocks_run_while_a_player_is_in_the_world() {
        let Some(ctx) = setup() else { return };
        for _ in 0..5 {
            tick(&ctx);
        }
        assert_eq!(count(&ctx, "play_time"), 5);
        assert_eq!(count(&ctx, "total_world_time"), 5);
        assert_eq!(count(&ctx, "time_since_death"), 5);
        assert_eq!(count(&ctx, "time_since_rest"), 5);
        assert_eq!(count(&ctx, "sneak_time"), 0);
        set_sneaking(&ctx, ME, true);
        tick(&ctx);
        tick(&ctx);
        assert_eq!(count(&ctx, "sneak_time"), 2);
    }

    #[test]
    fn walking_sprinting_and_sneaking_count_their_own_distances() {
        let Some(ctx) = setup() else { return };
        tick(&ctx);
        testkit::place(&ctx, ME, [1.5, -60.0, 0.5], true);
        tick(&ctx);
        assert_eq!(count(&ctx, "walk_one_cm"), 100);
        set_sprinting(&ctx, ME, true);
        testkit::place(&ctx, ME, [3.5, -60.0, 0.5], true);
        tick(&ctx);
        assert_eq!(count(&ctx, "sprint_one_cm"), 200);
        set_sprinting(&ctx, ME, false);
        set_sneaking(&ctx, ME, true);
        testkit::place(&ctx, ME, [3.5, -60.0, 0.8], true);
        tick(&ctx);
        assert_eq!(count(&ctx, "crouch_one_cm"), 30);
        assert_eq!(count(&ctx, "walk_one_cm"), 100);
        // A teleport is not a walk.
        testkit::place(&ctx, ME, [300.5, -60.0, 0.8], true);
        tick(&ctx);
        assert_eq!(count(&ctx, "walk_one_cm"), 100);
    }

    #[test]
    fn leaving_the_ground_upwards_is_a_jump_and_a_long_fall_is_counted() {
        let Some(ctx) = setup() else { return };
        tick(&ctx);
        testkit::place(&ctx, ME, [0.5, -59.6, 0.5], false);
        tick(&ctx);
        assert_eq!(count(&ctx, "jump"), 1);
        fell(&ctx, ME, 1.0);
        assert_eq!(count(&ctx, "fall_one_cm"), 0);
        fell(&ctx, ME, 5.0);
        assert_eq!(count(&ctx, "fall_one_cm"), 500);
    }

    #[test]
    fn damage_and_death_are_counted_in_tenths() {
        let Some(ctx) = setup() else { return };
        crate::vitals::hurt_by(
            &ctx,
            ME,
            3.0,
            "minecraft:generic",
            "death.attack.generic",
            None,
        );
        assert_eq!(count(&ctx, "damage_taken"), 30);
        assert_eq!(count(&ctx, "deaths"), 0);
        tick(&ctx);
        assert_eq!(count(&ctx, "time_since_death"), 1);
        crate::vitals::hurt_by(
            &ctx,
            ME,
            40.0,
            "minecraft:out_of_world",
            "death.attack.outOfWorld",
            Some("Zombie"),
        );
        assert_eq!(count(&ctx, "deaths"), 1);
        assert_eq!(count(&ctx, "time_since_death"), 0);
        assert_eq!(get(&ctx, ME, KILLED_BY, "minecraft:zombie"), 1);
    }

    #[test]
    fn the_statistics_screen_gets_every_non_zero_counter_by_registry_id() {
        let Some(ctx) = setup() else { return };
        custom(&ctx, ME, "jump", 3);
        add(&ctx, ME, MINED, "minecraft:stone", 2);
        item(
            &ctx,
            ME,
            CRAFTED,
            ctx.items.id("minecraft:stick").unwrap(),
            4,
        );
        add(&ctx, ME, KILLED, "minecraft:zombie", 1);
        let stats = packet(&ctx, ME).stats;
        let b = &ctx.data.builtin;
        let id = |r: &str, n: &str| b.id(r, n).unwrap();
        assert_eq!(stats.len(), 4);
        for want in [
            (
                id("minecraft:stat_type", CUSTOM),
                id("minecraft:custom_stat", "minecraft:jump"),
                3,
            ),
            (
                id("minecraft:stat_type", MINED),
                id("minecraft:block", "minecraft:stone"),
                2,
            ),
            (
                id("minecraft:stat_type", CRAFTED),
                id("minecraft:item", "minecraft:stick"),
                4,
            ),
            (
                id("minecraft:stat_type", KILLED),
                id("minecraft:entity_type", "minecraft:zombie"),
                1,
            ),
        ] {
            assert!(stats.contains(&want), "{want:?} in {stats:?}");
        }
        assert!(packet(&ctx, ME ^ 1).stats.is_empty());
    }

    #[test]
    fn scoreboard_objectives_with_a_stat_criterion_count_along() {
        let Some(ctx) = setup() else { return };
        let objective = |name: &str, criteria: &str| crate::scoreboard::Objective {
            name: name.into(),
            criteria: criteria.into(),
            display: mc_nbt::Tag::from(name),
            hearts: false,
            auto_update: false,
            format: None,
        };
        {
            let mut board = lock(&ctx.scoreboard);
            board.add_objective(objective("jumps", "minecraft.custom:minecraft.jump"));
            board.add_objective(objective("stone", "minecraft.mined:minecraft.stone"));
            board.add_objective(objective("dead", "deathCount"));
        }
        custom(&ctx, ME, "jump", 2);
        custom(&ctx, ME, "jump", 1);
        add(&ctx, ME, MINED, "minecraft:dirt", 1);
        died(&ctx, ME, None);
        let board = lock(&ctx.scoreboard);
        let score = |o: &str| board.score("P41", o).map_or(0, |s| s.value);
        assert_eq!((score("jumps"), score("stone"), score("dead")), (3, 0, 1));
    }

    #[test]
    fn counts_round_trip_through_the_vanilla_file_format() {
        let mut c = Counts::default();
        c.add("minecraft:mined", "minecraft:stone", 3);
        c.add("mined", "stone", 2);
        c.add("custom", "minecraft:jump", 7);
        c.set("custom", "walk_one_cm", 0);
        assert_eq!(c.get("minecraft:mined", "minecraft:stone"), 5);
        let json = c.to_json(4000);
        assert_eq!(json["stats"]["minecraft:mined"]["minecraft:stone"], 5);
        assert_eq!(json["stats"]["minecraft:custom"]["minecraft:jump"], 7);
        assert_eq!(json["DataVersion"], 4000);
        assert_eq!(Counts::from_json(&json), c);
        // What vanilla wrote is read as it is.
        let text = r#"{"stats":{"minecraft:crafted":{"minecraft:stick":4}},"DataVersion":1}"#;
        let read = Counts::from_json(&serde_json::from_str(text).unwrap());
        assert_eq!(read.get("crafted", "stick"), 4);
        assert_eq!(read.all().count(), 1);
    }

    #[test]
    fn counters_saturate_and_zero_ones_are_not_written() {
        let mut c = Counts::default();
        c.add("custom", "jump", i32::MAX);
        c.add("custom", "jump", 5);
        assert_eq!(c.get("custom", "jump"), i32::MAX);
        c.set("custom", "jump", 0);
        assert!(c.to_json(1)["stats"].as_object().unwrap().is_empty());
    }
}
