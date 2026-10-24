//! Trial spawners and vaults (minecraft.wiki: "Trial Spawner", "Vault").
//!
//! A trial spawner waits for players within its range (14 blocks), then
//! spawns a wave of mobs a few at a time, one every `ticks_between_spawn`
//! while fewer than `simultaneous_mobs` live and fewer than `total_mobs`
//! came; both grow with every player beyond the first. When the wave is
//! spawned and dead it waits 40 ticks, ejects one loot table's items per
//! player (one every 30 ticks) and rests for `target_cooldown_length` ticks
//! (36000). A player with Trial Omen near it turns it ominous (the
//! `ominous_config` is used then, until the rest ends).
//!
//! A vault shows a reward when a player is within 4 blocks and takes the
//! matching key (trial key, ominous trial key for an ominous vault): once
//! per player, it unlocks for 14 ticks and ejects the loot table's items
//! upwards, one every 20 ticks.

use std::collections::{HashMap, HashSet};

use mc_entity::mob::Target;
use mc_entity::nav::{PathParams, standable};
use mc_entity::{Entity, Kind, Species};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use serde_json::Value;

use crate::commands::loot::Env;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::loot::{Drop, LootCtx};
use crate::mobs::{Ground, new_mob};
use crate::tick::lock;

pub const SPAWNER: &str = "minecraft:trial_spawner";
pub const VAULT: &str = "minecraft:vault";

const DEFAULT_RANGE: i32 = 14;
const DEFAULT_COOLDOWN: i64 = 36_000;
const EJECT_WAIT: i64 = 40;
const EJECT_EVERY: i64 = 30;
const UNLOCK_TICKS: i64 = 14;
const VAULT_EJECT_EVERY: i64 = 20;
const VAULT_ACTIVATE: f64 = 4.0;
const VAULT_DEACTIVATE: f64 = 4.5;
const SPAWN_RANGE: i32 = 4;

fn below(n: u32) -> u32 {
    (crate::util::random_u64() >> 33) as u32 % n.max(1)
}

fn int(c: &Compound, key: &str) -> Option<i64> {
    match c.get(key)? {
        Tag::Byte(v) => Some(i64::from(*v)),
        Tag::Short(v) => Some(i64::from(*v)),
        Tag::Int(v) => Some(i64::from(*v)),
        Tag::Long(v) => Some(*v),
        _ => None,
    }
}

fn string<'a>(c: &'a Compound, key: &str) -> Option<&'a str> {
    match c.get(key)? {
        Tag::String(s) => Some(s),
        _ => None,
    }
}

fn centre(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

/// Weighted pick.
fn pick<T>(list: &[(u32, T)]) -> Option<&T> {
    let total: u32 = list.iter().map(|e| e.0).sum();
    if total == 0 {
        return list.first().map(|e| &e.1);
    }
    let mut roll = below(total);
    for (w, t) in list {
        if roll < *w {
            return Some(t);
        }
        roll -= *w;
    }
    None
}

// -- trial spawner config

/// A mob a wave may hold.
#[derive(Clone, Debug)]
pub struct Potential {
    /// `entity` NBT (`id` and more).
    pub entity: Compound,
    /// `equipment.loot_table`, rolled for the mob's gear.
    pub equipment: Option<String>,
    pub drop_chance: f32,
}

/// One of a spawner's two configs (normal and ominous).
#[derive(Clone, Debug)]
pub struct Config {
    pub total: f32,
    pub total_per_player: f32,
    pub simultaneous: f32,
    pub simultaneous_per_player: f32,
    pub ticks_between: i64,
    pub potentials: Vec<(u32, Potential)>,
    pub loot: Vec<(u32, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            total: 6.0,
            total_per_player: 2.0,
            simultaneous: 2.0,
            simultaneous_per_player: 1.0,
            ticks_between: 40,
            potentials: Vec::new(),
            loot: vec![
                (1, "minecraft:spawners/trial_chamber/key".into()),
                (1, "minecraft:spawners/trial_chamber/consumables".into()),
            ],
        }
    }
}

fn json_nbt(v: &Value) -> Compound {
    match crate::loot::json_tag(v) {
        Tag::Compound(c) => c,
        _ => Compound::new(),
    }
}

impl Config {
    pub fn from_json(v: &Value) -> Self {
        let mut c = Self::default();
        let f = |k: &str, d: f32| v.get(k).and_then(Value::as_f64).map_or(d, |x| x as f32);
        c.total = f("total_mobs", c.total);
        c.total_per_player = f("total_mobs_added_per_player", c.total_per_player);
        c.simultaneous = f("simultaneous_mobs", c.simultaneous);
        c.simultaneous_per_player = f(
            "simultaneous_mobs_added_per_player",
            c.simultaneous_per_player,
        );
        c.ticks_between = v
            .get("ticks_between_spawn")
            .and_then(Value::as_i64)
            .unwrap_or(c.ticks_between);
        if let Some(list) = v.get("spawn_potentials").and_then(Value::as_array) {
            for e in list {
                let weight = e.get("weight").and_then(Value::as_u64).unwrap_or(1) as u32;
                let data = &e["data"];
                let entity = json_nbt(&data["entity"]);
                if entity.is_empty() {
                    continue;
                }
                let equipment = data["equipment"]["loot_table"]
                    .as_str()
                    .map(mc_command::args::full_id);
                let drop_chance = data["equipment"]["slot_drop_chances"]
                    .as_f64()
                    .map_or(0.085, |d| d as f32);
                c.potentials.push((
                    weight,
                    Potential {
                        entity,
                        equipment,
                        drop_chance,
                    },
                ));
            }
        }
        if let Some(list) = v.get("loot_tables_to_eject").and_then(Value::as_array) {
            c.loot = list
                .iter()
                .filter_map(|e| {
                    Some((
                        e.get("weight").and_then(Value::as_u64).unwrap_or(1) as u32,
                        mc_command::args::full_id(e.get("data")?.as_str()?),
                    ))
                })
                .collect();
        }
        c
    }

    pub fn target_total(&self, players: usize) -> i32 {
        let extra = players.saturating_sub(1) as f32;
        (self.total + self.total_per_player * extra).floor() as i32
    }

    pub fn target_simultaneous(&self, players: usize) -> i32 {
        let extra = players.saturating_sub(1) as f32;
        (self.simultaneous + self.simultaneous_per_player * extra).floor() as i32
    }
}

/// The config a block entity names under `key` (`normal_config`,
/// `ominous_config`): a data pack entry or an inline object.
fn config_of(ctx: &ServerContext, data: &Compound, key: &str) -> Config {
    match data.get(key) {
        Some(Tag::String(name)) => {
            let name = name.trim_start_matches("minecraft:");
            let cache = &ctx.mobs.trial_configs;
            if let Some(c) = lock(cache).get(name) {
                return c.clone();
            }
            let pack = ctx.data.data_pack();
            let found = pack
                .read_entry("minecraft:trial_spawner", name)
                .map(|v| Config::from_json(&v))
                .unwrap_or_default();
            lock(cache).insert(name.to_owned(), found.clone());
            found
        }
        Some(Tag::Compound(c)) => {
            let mut cfg = Config::default();
            let f = |k: &str, d: f32| match c.get(k) {
                Some(Tag::Float(v)) => *v,
                Some(Tag::Double(v)) => *v as f32,
                Some(Tag::Int(v)) => *v as f32,
                _ => d,
            };
            cfg.total = f("total_mobs", cfg.total);
            cfg.total_per_player = f("total_mobs_added_per_player", cfg.total_per_player);
            cfg.simultaneous = f("simultaneous_mobs", cfg.simultaneous);
            cfg.simultaneous_per_player = f(
                "simultaneous_mobs_added_per_player",
                cfg.simultaneous_per_player,
            );
            cfg.ticks_between = int(c, "ticks_between_spawn").unwrap_or(cfg.ticks_between);
            if let Some(Tag::List(list)) = c.get("spawn_potentials") {
                for e in list {
                    let Tag::Compound(e) = e else { continue };
                    let Some(Tag::Compound(d)) = e.get("data") else {
                        continue;
                    };
                    let Some(Tag::Compound(entity)) = d.get("entity") else {
                        continue;
                    };
                    cfg.potentials.push((
                        int(e, "weight").unwrap_or(1).max(0) as u32,
                        Potential {
                            entity: entity.clone(),
                            equipment: None,
                            drop_chance: 0.085,
                        },
                    ));
                }
            }
            cfg
        }
        _ => Config::default(),
    }
}

// -- trial spawner state

/// What a trial spawner remembers while loaded.
#[derive(Clone, Debug, Default)]
pub struct Trial {
    players: HashSet<u128>,
    mobs: HashSet<u128>,
    spawned: i32,
    next_spawn: i64,
    /// When the next state step is due (reward ejection).
    due: i64,
    queue: Vec<Drop>,
}

pub type Trials = HashMap<(usize, i32, i32, i32), Trial>;

const STATES: [&str; 6] = [
    "inactive",
    "waiting_for_players",
    "active",
    "waiting_for_reward_ejection",
    "ejecting_reward",
    "cooldown",
];

fn set_property(ctx: &ServerContext, p: Position, key: &str, value: &str) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return false;
    };
    let Some(new) = blocks.with_property(state, key, value) else {
        return false;
    };
    if new == state {
        return false;
    }
    let mut level = crate::level::WorldLevel::new(ctx);
    level.set_block(p, new);
    let changes = level.into_changes();
    let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    true
}

fn mob_alive(entities: &[Entity], uuid: u128) -> bool {
    entities
        .iter()
        .any(|e| e.uuid == uuid && !e.removed && matches!(&e.kind, Kind::Mob(m) if !m.is_dead()))
}

fn roll_table(ctx: &ServerContext, name: &str, origin: [f64; 3]) -> Vec<Drop> {
    let Some(table) = ctx.loot.table(name) else {
        return Vec::new();
    };
    let cx = LootCtx {
        origin: Some(origin),
        ..Default::default()
    };
    ctx.loot.roll(&Env(ctx), table, &cx)
}

fn equip(ctx: &ServerContext, pot: &Potential, origin: [f64; 3]) -> Compound {
    let mut out = pot.entity.clone();
    let Some(table) = &pot.equipment else {
        return out;
    };
    let mut eq = Compound::new();
    for drop in roll_table(ctx, table, origin) {
        let stack = drop.stack();
        let Some(name) = ctx.items.name(stack.item) else {
            continue;
        };
        let slot = if name.ends_with("_helmet") {
            "head"
        } else if name.ends_with("_chestplate") {
            "chest"
        } else if name.ends_with("_leggings") {
            "legs"
        } else if name.ends_with("_boots") {
            "feet"
        } else {
            "mainhand"
        };
        let mut item = Compound::new();
        item.insert("id", name);
        item.insert("count", Tag::Int(stack.count));
        item.insert("components", Tag::Compound(drop.components.clone()));
        eq.insert(slot, Tag::Compound(item));
    }
    if !eq.is_empty() {
        out.insert("equipment", Tag::Compound(eq));
        let mut dc = Compound::new();
        for s in ["mainhand", "offhand", "feet", "legs", "chest", "head"] {
            dc.insert(s, Tag::Float(pot.drop_chance));
        }
        out.insert("drop_chances", Tag::Compound(dc));
    }
    out
}

fn target_range(data: &Compound) -> f64 {
    int(data, "required_player_range").unwrap_or(i64::from(DEFAULT_RANGE)) as f64
}

/// Runs the trial spawners; returns the mobs they made.
pub fn tick(
    ctx: &ServerContext,
    ground: &Ground,
    entities: &[Entity],
    targets: &[Target],
    difficulty: i32,
    now: i64,
) -> Vec<Entity> {
    let mut out = Vec::new();
    if !ctx.game_rule_bool("spawner_blocks_work") {
        return out;
    }
    let spawners = ctx.world.block_entities_of(SPAWNER);
    let mut all = lock(&ctx.mobs.trials);
    all.retain(|&(dim, x, y, z), _| {
        dim != ctx.dim || spawners.iter().any(|b| (b.x, b.y, b.z) == (x, y, z))
    });
    for be in spawners {
        let p = Position::new(be.x, be.y, be.z);
        let blocks = &ctx.data.blocks;
        let Some(state) = ctx.world.block(p, blocks) else {
            continue;
        };
        let phase = STATES
            .iter()
            .position(|s| blocks.property(state, "trial_spawner_state") == Some(*s))
            .unwrap_or(0);
        let ominous = blocks.property(state, "ominous") == Some("true");
        let key = (ctx.dim, be.x, be.y, be.z);
        let trial = all.entry(key).or_default();
        let mid = centre(p);
        let range = target_range(&be.data);
        let near: Vec<&Target> = targets
            .iter()
            .filter(|t| t.targetable && dist2(t.pos, mid) <= range * range)
            .collect();
        let cfg = config_of(
            ctx,
            &be.data,
            if ominous {
                "ominous_config"
            } else {
                "normal_config"
            },
        );
        match phase {
            0 => {
                if !cfg.potentials.is_empty() {
                    set_property(ctx, p, "trial_spawner_state", "waiting_for_players");
                }
            }
            1 => {
                *trial = Trial::default();
                if difficulty != 0 && !near.is_empty() {
                    trial.players = near.iter().map(|t| t.uuid).collect();
                    fx::sound(
                        ctx,
                        "block.trial_spawner.detect_player",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    fx::level_event(ctx, 3012, mid, near.len() as i32);
                    trial.next_spawn = now + cfg.ticks_between.min(20);
                    set_property(ctx, p, "trial_spawner_state", "active");
                }
            }
            2 => {
                if near.is_empty() {
                    // Everyone left: the wave starts again.
                    *trial = Trial::default();
                    set_property(ctx, p, "trial_spawner_state", "waiting_for_players");
                    continue;
                }
                for t in &near {
                    trial.players.insert(t.uuid);
                }
                if !ominous && near.iter().any(|t| has_omen(ctx, t.uuid)) {
                    become_ominous(ctx, p, &be.data, mid, trial);
                    continue;
                }
                trial.mobs.retain(|u| mob_alive(entities, *u));
                let want_total = cfg.target_total(trial.players.len());
                let want_now = cfg.target_simultaneous(trial.players.len());
                if trial.spawned >= want_total {
                    if trial.mobs.is_empty() {
                        trial.queue = reward(ctx, &cfg, trial.players.len(), mid);
                        trial.due = now + EJECT_WAIT;
                        set_property(ctx, p, "trial_spawner_state", "waiting_for_reward_ejection");
                    }
                    continue;
                }
                if now < trial.next_spawn || trial.mobs.len() as i32 >= want_now {
                    continue;
                }
                if difficulty == 0 {
                    continue;
                }
                let Some(pot) = pick(&cfg.potentials) else {
                    continue;
                };
                let nbt = equip(ctx, pot, mid);
                if let Some(e) = spawn_one(ctx, ground, &nbt, p, mid) {
                    trial.mobs.insert(e.uuid);
                    trial.spawned += 1;
                    trial.next_spawn = now + cfg.ticks_between;
                    out.push(e);
                }
            }
            3 => {
                if now >= trial.due {
                    trial.due = now;
                    fx::sound(
                        ctx,
                        "block.trial_spawner.open_shutter",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    set_property(ctx, p, "trial_spawner_state", "ejecting_reward");
                }
            }
            4 => {
                if now < trial.due {
                    continue;
                }
                if trial.queue.is_empty() {
                    fx::sound(
                        ctx,
                        "block.trial_spawner.close_shutter",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    let cooldown =
                        int(&be.data, "target_cooldown_length").unwrap_or(DEFAULT_COOLDOWN);
                    let mut data = be.data.clone();
                    data.insert("cooldown_ends_at", Tag::Long(now + cooldown));
                    ctx.world
                        .set_block_entity(mc_world::BlockEntity { data, ..be.clone() }, blocks);
                    set_property(ctx, p, "trial_spawner_state", "cooldown");
                    continue;
                }
                let item = trial.queue.remove(0);
                let stack = item.stack();
                crate::tick::spawn_stack_at(ctx, [mid[0], mid[1] + 1.0, mid[2]], &stack);
                fx::sound(
                    ctx,
                    "block.trial_spawner.eject_item",
                    fx::category::BLOCKS,
                    mid,
                    1.0,
                    1.0,
                );
                trial.due = now + EJECT_EVERY;
            }
            _ => {
                let ends = int(&be.data, "cooldown_ends_at").unwrap_or(0);
                if now >= ends {
                    *trial = Trial::default();
                    if ominous {
                        set_property(ctx, p, "ominous", "false");
                    }
                    set_property(ctx, p, "trial_spawner_state", "waiting_for_players");
                }
            }
        }
    }
    out
}

fn has_omen(ctx: &ServerContext, uuid: u128) -> bool {
    lock(&ctx.vitals)
        .get(&uuid)
        .is_some_and(|v| v.effects.has("trial_omen"))
}

/// The spawner turns ominous: the omen's cost is paid, the items it drops
/// when it does appear, and the wave begins again.
fn become_ominous(
    ctx: &ServerContext,
    p: Position,
    _data: &Compound,
    mid: [f64; 3],
    trial: &mut Trial,
) {
    set_property(ctx, p, "ominous", "true");
    fx::sound(
        ctx,
        "block.trial_spawner.ominous_activate",
        fx::category::BLOCKS,
        mid,
        1.0,
        1.0,
    );
    fx::level_event(ctx, 3020, mid, 1);
    for drop in roll_table(
        ctx,
        "minecraft:spawners/trial_chamber/items_to_drop_when_ominous",
        mid,
    ) {
        crate::tick::spawn_stack_at(ctx, [mid[0], mid[1] + 1.0, mid[2]], &drop.stack());
    }
    trial.mobs.clear();
    trial.spawned = 0;
}

/// One loot table's items per player.
fn reward(ctx: &ServerContext, cfg: &Config, players: usize, mid: [f64; 3]) -> Vec<Drop> {
    let mut out = Vec::new();
    for _ in 0..players {
        if let Some(table) = pick(&cfg.loot) {
            out.extend(roll_table(ctx, table, mid));
        }
    }
    out
}

/// Puts one mob of `nbt` within reach of the spawner, if there is room.
fn spawn_one(
    ctx: &ServerContext,
    ground: &Ground,
    nbt: &Compound,
    at: Position,
    mid: [f64; 3],
) -> Option<Entity> {
    let id = mc_command::args::full_id(string(nbt, "id")?);
    let species = Species::from_id(&id)?;
    let (_, h) = species.size();
    let params = PathParams::walker(h.ceil() as i32);
    for _ in 0..10 {
        let x = at.x + below(2 * SPAWN_RANGE as u32 + 1) as i32 - SPAWN_RANGE;
        let y = at.y + below(3) as i32 - 1;
        let z = at.z + below(2 * SPAWN_RANGE as u32 + 1) as i32 - SPAWN_RANGE;
        if !(species.flies() || standable(ground, [x, y, z], &params)) {
            continue;
        }
        let pos = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
        let mut e = new_mob(ctx, species, pos);
        if let Kind::Mob(m) = &mut e.kind {
            m.yaw = below(360) as f32;
            crate::mobs::nbt::apply(ctx, m, nbt);
            m.persistent = true;
        }
        fx::level_event(ctx, 3011, pos, 0);
        fx::sound(
            ctx,
            "block.trial_spawner.spawn_mob",
            fx::category::BLOCKS,
            mid,
            1.0,
            1.0,
        );
        return Some(e);
    }
    None
}

// -- vaults

fn nested<'a>(c: &'a Compound, key: &str) -> Option<&'a Compound> {
    match c.get(key)? {
        Tag::Compound(v) => Some(v),
        _ => None,
    }
}

fn vault_table(data: &Compound, ominous: bool) -> String {
    let named = nested(data, "config").and_then(|c| string(c, "loot_table"));
    match named {
        Some(n) => mc_command::args::full_id(n),
        None if ominous => "minecraft:chests/trial_chambers/reward_ominous".into(),
        None => "minecraft:chests/trial_chambers/reward".into(),
    }
}

fn vault_key(ctx: &ServerContext, data: &Compound, ominous: bool) -> Option<i32> {
    let named = nested(data, "config")
        .and_then(|c| nested(c, "key_item"))
        .and_then(|k| string(k, "id"));
    ctx.items.id(&match named {
        Some(n) => mc_command::args::full_id(n),
        None if ominous => "minecraft:ominous_trial_key".into(),
        None => "minecraft:trial_key".into(),
    })
}

fn uuid_tag(u: u128) -> Tag {
    Tag::IntArray((0..4).map(|i| (u >> (96 - 32 * i)) as u32 as i32).collect())
}

fn rewarded(data: &Compound) -> Vec<Tag> {
    match nested(data, "server_data").and_then(|s| s.get("rewarded_players")) {
        Some(Tag::List(l)) => l.clone(),
        _ => Vec::new(),
    }
}

fn save_vault(ctx: &ServerContext, be: &mc_world::BlockEntity, data: Compound) {
    ctx.world.set_block_entity(
        mc_world::BlockEntity { data, ..be.clone() },
        &ctx.data.blocks,
    );
}

fn server_data(data: &mut Compound) -> &mut Compound {
    if !matches!(data.get("server_data"), Some(Tag::Compound(_))) {
        data.insert("server_data", Tag::Compound(Compound::new()));
    }
    match data.get_mut("server_data") {
        Some(Tag::Compound(c)) => c,
        _ => unreachable!(),
    }
}

fn queued(ctx: &ServerContext, data: &Compound) -> Vec<Option<ItemStack>> {
    let empty = Compound::new();
    let s = nested(data, "server_data").unwrap_or(&empty);
    let n = int(s, "total_ejections_needed").unwrap_or(0).max(0) as usize;
    let mut holder = Compound::new();
    if let Some(items) = s.get("items_to_eject") {
        holder.insert("Items", items.clone());
    }
    let mut slots = crate::containers::read_items(ctx, &holder, n.max(64));
    slots.retain(Option::is_some);
    slots
}

/// A player puts a key into the vault at `p`. Returns whether it took it.
pub fn insert_key(ctx: &ServerContext, p: Position, player: u128, held: &ItemStack) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return false;
    };
    if blocks.property(state, "vault_state") != Some("active") {
        return false;
    }
    let ominous = blocks.property(state, "ominous") == Some("true");
    let Some(be) = ctx.world.block_entity(p, blocks) else {
        return false;
    };
    if vault_key(ctx, &be.data, ominous) != Some(held.item) {
        return false;
    }
    let mid = centre(p);
    let who = uuid_tag(player);
    let mut data = be.data.clone();
    if rewarded(&data).contains(&who) {
        fx::sound(
            ctx,
            "block.vault.reject_rewarded_player",
            fx::category::BLOCKS,
            mid,
            1.0,
            1.0,
        );
        return false;
    }
    let drops = roll_table(ctx, &vault_table(&data, ominous), mid);
    let stacks: Vec<Option<ItemStack>> = drops.iter().map(|d| Some(d.stack())).collect();
    let now = lock(&ctx.time).age;
    let s = server_data(&mut data);
    let mut list = rewarded_list(s);
    list.push(who);
    s.insert("rewarded_players", Tag::List(list));
    s.insert(
        "items_to_eject",
        crate::containers::write_items(ctx, &stacks),
    );
    s.insert("total_ejections_needed", Tag::Int(stacks.len() as i32));
    s.insert("state_updating_resumes_at", Tag::Long(now + UNLOCK_TICKS));
    save_vault(ctx, &be, data);
    fx::sound(
        ctx,
        "block.vault.insert_item",
        fx::category::BLOCKS,
        mid,
        1.0,
        1.0,
    );
    fx::level_event(ctx, 3015, mid, 0);
    set_property(ctx, p, "vault_state", "unlocking");
    true
}

fn rewarded_list(s: &Compound) -> Vec<Tag> {
    match s.get("rewarded_players") {
        Some(Tag::List(l)) => l.clone(),
        _ => Vec::new(),
    }
}

/// Runs the vaults near players.
pub fn tick_vaults(ctx: &ServerContext, targets: &[Target], now: i64) {
    for be in ctx.world.block_entities_of(VAULT) {
        let p = Position::new(be.x, be.y, be.z);
        let blocks = &ctx.data.blocks;
        let Some(state) = ctx.world.block(p, blocks) else {
            continue;
        };
        let mid = centre(p);
        let phase = blocks.property(state, "vault_state").unwrap_or("inactive");
        let ominous = blocks.property(state, "ominous") == Some("true");
        let within = |r: f64| {
            targets
                .iter()
                .any(|t| t.present && dist2(t.pos, mid) <= r * r)
        };
        let resumes = nested(&be.data, "server_data")
            .and_then(|s| int(s, "state_updating_resumes_at"))
            .unwrap_or(0);
        match phase {
            "inactive" => {
                if within(VAULT_ACTIVATE) {
                    let mut data = be.data.clone();
                    // The item the vault shows: one of the table's.
                    let shown = roll_table(ctx, &vault_table(&data, ominous), mid)
                        .into_iter()
                        .next();
                    if let Some(drop) = shown {
                        let mut shared = nested(&data, "shared_data").cloned().unwrap_or_default();
                        let mut item = Compound::new();
                        item.insert("id", ctx.items.name(drop.item).unwrap_or("minecraft:air"));
                        item.insert("count", Tag::Int(1));
                        shared.insert("display_item", Tag::Compound(item));
                        data.insert("shared_data", Tag::Compound(shared));
                        save_vault(ctx, &be, data);
                    }
                    fx::sound(
                        ctx,
                        "block.vault.activate",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    set_property(ctx, p, "vault_state", "active");
                }
            }
            "active" => {
                if !within(VAULT_DEACTIVATE) {
                    fx::sound(
                        ctx,
                        "block.vault.deactivate",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    set_property(ctx, p, "vault_state", "inactive");
                }
            }
            "unlocking" => {
                if now >= resumes {
                    fx::sound(
                        ctx,
                        "block.vault.open_shutter",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    let mut data = be.data.clone();
                    server_data(&mut data).insert(
                        "state_updating_resumes_at",
                        Tag::Long(now + VAULT_EJECT_EVERY),
                    );
                    save_vault(ctx, &be, data);
                    set_property(ctx, p, "vault_state", "ejecting");
                }
            }
            "ejecting" => {
                if now < resumes {
                    continue;
                }
                let mut items = queued(ctx, &be.data);
                let mut data = be.data.clone();
                if items.is_empty() {
                    fx::sound(
                        ctx,
                        "block.vault.close_shutter",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    let s = server_data(&mut data);
                    s.remove("items_to_eject");
                    s.insert("total_ejections_needed", Tag::Int(0));
                    save_vault(ctx, &be, data);
                    set_property(
                        ctx,
                        p,
                        "vault_state",
                        if within(VAULT_DEACTIVATE) {
                            "active"
                        } else {
                            "inactive"
                        },
                    );
                    continue;
                }
                if let Some(stack) = items.remove(0) {
                    crate::tick::spawn_stack_at(ctx, [mid[0], mid[1] + 0.7, mid[2]], &stack);
                    fx::sound(
                        ctx,
                        "block.vault.eject_item",
                        fx::category::BLOCKS,
                        mid,
                        1.0,
                        1.0,
                    );
                    fx::level_event(ctx, 3017, mid, 0);
                }
                let s = server_data(&mut data);
                s.insert(
                    "items_to_eject",
                    crate::containers::write_items(ctx, &items),
                );
                s.insert(
                    "state_updating_resumes_at",
                    Tag::Long(now + VAULT_EJECT_EVERY),
                );
                save_vault(ctx, &be, data);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::WorldLevel;

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    fn target(uuid: u128, pos: [f64; 3]) -> Target {
        Target::at(1, uuid, pos)
    }

    #[test]
    fn counts_grow_with_players() {
        let c = Config::default();
        assert_eq!(c.target_total(1), 6);
        assert_eq!(c.target_total(3), 10);
        assert_eq!(c.target_simultaneous(1), 2);
        assert_eq!(c.target_simultaneous(4), 5);
    }

    #[test]
    fn vault_takes_a_key_once_and_ejects() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let p = Position::new(0, -60, 0);
        let vault = blocks.default_state("minecraft:vault").unwrap();
        WorldLevel::new(&ctx).set_block(p, vault);
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: VAULT.into(),
                data: Compound::new(),
            },
            blocks,
        );
        let key = ItemStack::new(ctx.items.id("minecraft:trial_key").unwrap(), 1);
        let near = [target(7, [0.5, -59.0, 0.5])];
        let state = |ctx: &ServerContext| {
            let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
            ctx.data
                .blocks
                .property(s, "vault_state")
                .unwrap()
                .to_owned()
        };
        // Nobody near: it does not take the key.
        assert!(!insert_key(&ctx, p, 7, &key));
        tick_vaults(&ctx, &near, 0);
        assert_eq!(state(&ctx), "active");
        let dirt = ItemStack::new(ctx.items.id("minecraft:dirt").unwrap(), 1);
        assert!(!insert_key(&ctx, p, 7, &dirt));
        assert!(insert_key(&ctx, p, 7, &key));
        assert_eq!(state(&ctx), "unlocking");
        // The same player is turned away.
        assert!(!insert_key(&ctx, p, 7, &key));
        for t in 1..=14 {
            tick_vaults(&ctx, &near, t);
        }
        assert_eq!(state(&ctx), "ejecting");
        let before = lock(&ctx.entities).len();
        for t in 15..400 {
            tick_vaults(&ctx, &near, t);
        }
        assert!(lock(&ctx.entities).len() > before, "items came out");
        assert_eq!(state(&ctx), "active");
        // Another player may use it.
        assert!(insert_key(&ctx, p, 8, &key));
    }

    #[test]
    fn trial_spawner_runs_a_wave_and_pays_out() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let p = Position::new(0, -60, 0);
        let spawner = blocks.default_state("minecraft:trial_spawner").unwrap();
        WorldLevel::new(&ctx).set_block(p, spawner);
        let mut data = Compound::new();
        data.insert(
            "normal_config",
            Tag::String("minecraft:trial_chamber/melee/zombie/normal".into()),
        );
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: SPAWNER.into(),
                data,
            },
            blocks,
        );
        let cfg = config_of(
            &ctx,
            &ctx.world.block_entity(p, blocks).unwrap().data,
            "normal_config",
        );
        assert_eq!(cfg.potentials.len(), 1);
        assert_eq!(cfg.simultaneous, 3.0);
        let phase = |ctx: &ServerContext| {
            let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
            ctx.data
                .blocks
                .property(s, "trial_spawner_state")
                .unwrap()
                .to_owned()
        };
        let ground = Ground::new(&ctx);
        let near = [target(7, [0.5, -59.0, 3.5])];
        let mut entities: Vec<Entity> = Vec::new();
        let mut now = 0;
        let step = |entities: &mut Vec<Entity>, near: &[Target], now: i64| {
            let made = tick(&ctx, &ground, entities, near, 2, now);
            entities.extend(made);
        };
        step(&mut entities, &near, now);
        assert_eq!(phase(&ctx), "waiting_for_players");
        step(&mut entities, &near, now);
        assert_eq!(phase(&ctx), "active");
        // Nobody about: it starts over.
        step(&mut entities, &[], now);
        assert_eq!(phase(&ctx), "waiting_for_players");
        step(&mut entities, &near, now);
        assert_eq!(phase(&ctx), "active");
        // Kill each mob it makes at once until the six are through.
        for _ in 0..2000 {
            now += 1;
            step(&mut entities, &near, now);
            for e in entities.iter_mut() {
                e.removed = true;
            }
            if phase(&ctx) != "active" {
                break;
            }
        }
        assert_eq!(phase(&ctx), "waiting_for_reward_ejection");
        let mut seen = false;
        for _ in 0..400 {
            now += 1;
            step(&mut entities, &near, now);
            seen |= phase(&ctx) == "ejecting_reward";
            if phase(&ctx) == "cooldown" {
                break;
            }
        }
        assert!(seen);
        assert_eq!(phase(&ctx), "cooldown");
        let be = ctx.world.block_entity(p, blocks).unwrap();
        assert!(int(&be.data, "cooldown_ends_at").unwrap() >= now + DEFAULT_COOLDOWN - 1);
        now += DEFAULT_COOLDOWN + 1;
        step(&mut entities, &near, now);
        assert_eq!(phase(&ctx), "waiting_for_players");
    }

    #[test]
    fn omen_turns_a_spawner_ominous_and_the_rest_turns_it_back() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let p = Position::new(40, -60, 0);
        WorldLevel::new(&ctx).set_block(
            p,
            blocks
                .state_id(
                    "minecraft:trial_spawner",
                    &[("trial_spawner_state", "active"), ("ominous", "false")],
                )
                .unwrap(),
        );
        let mut data = Compound::new();
        data.insert(
            "normal_config",
            Tag::String("minecraft:trial_chamber/melee/zombie/normal".into()),
        );
        data.insert(
            "ominous_config",
            Tag::String("minecraft:trial_chamber/melee/zombie/ominous".into()),
        );
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: SPAWNER.into(),
                data,
            },
            blocks,
        );
        let mut effect = Compound::new();
        effect.insert("id", "minecraft:trial_omen");
        effect.insert("duration", Tag::Int(1000));
        let mut v = crate::vitals::Vitals::default();
        v.effects =
            crate::effects::Effects::from_nbt(Some(&Tag::List(vec![Tag::Compound(effect)])));
        lock(&ctx.vitals).insert(9, v);
        let near = [target(9, [40.5, -59.0, 3.5])];
        let ground = Ground::new(&ctx);
        tick(&ctx, &ground, &[], &near, 2, 0);
        let s = ctx.world.block(p, blocks).unwrap();
        assert_eq!(blocks.property(s, "ominous"), Some("true"));
        // Its rest over, it is plain again.
        WorldLevel::new(&ctx).set_block(
            p,
            blocks
                .state_id(
                    "minecraft:trial_spawner",
                    &[("trial_spawner_state", "cooldown"), ("ominous", "true")],
                )
                .unwrap(),
        );
        tick(&ctx, &ground, &[], &near, 2, 5);
        let s = ctx.world.block(p, blocks).unwrap();
        assert_eq!(blocks.property(s, "ominous"), Some("false"));
        assert_eq!(
            blocks.property(s, "trial_spawner_state"),
            Some("waiting_for_players")
        );
    }
}
