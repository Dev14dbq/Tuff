//! Villagers and wandering traders: workstations that give a villager its
//! profession, the trades of each profession and level (from the data pack's
//! `villager_trade` and `trade_set` entries), experience and levels, restocks,
//! prices, and the wandering trader's visits.
//!
//! Villagers have no schedule or pathfinding here: an unemployed villager
//! takes a free workstation near it, and works there for good.
//!
//! Wandering traders follow the wiki ("Wandering Trader", Spawning). The
//! counter and the chance live in `data/minecraft/wandering_trader.dat`
//! (`spawn_delay`, `spawn_chance`), where 26.1 moved them out of level.dat.
//! `WanderingTraderId` was removed in that change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use mc_data::{McData, TagValue};
use mc_entity::trade::{Cost, Merchant, Offer};
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::{self, ItemStack};
use serde_json::Value;

use crate::context::{Direct, Event, ServerContext};
use crate::items::Items;
use crate::util::random_u64;

/// Workstation blocks and the professions they give (the points of interest
/// villagers can take a job at).
const WORKSTATIONS: &[(&str, &str)] = &[
    ("minecraft:blast_furnace", "armorer"),
    ("minecraft:smoker", "butcher"),
    ("minecraft:cartography_table", "cartographer"),
    ("minecraft:brewing_stand", "cleric"),
    ("minecraft:composter", "farmer"),
    ("minecraft:barrel", "fisherman"),
    ("minecraft:fletching_table", "fletcher"),
    ("minecraft:cauldron", "leatherworker"),
    ("minecraft:water_cauldron", "leatherworker"),
    ("minecraft:lava_cauldron", "leatherworker"),
    ("minecraft:powder_snow_cauldron", "leatherworker"),
    ("minecraft:lectern", "librarian"),
    ("minecraft:stonecutter", "mason"),
    ("minecraft:loom", "shepherd"),
    ("minecraft:smithing_table", "toolsmith"),
    ("minecraft:grindstone", "weaponsmith"),
];

/// The professions workstations give.
pub const PROFESSIONS: &[&str] = &[
    "armorer",
    "butcher",
    "cartographer",
    "cleric",
    "farmer",
    "fisherman",
    "fletcher",
    "leatherworker",
    "librarian",
    "mason",
    "shepherd",
    "toolsmith",
    "weaponsmith",
];

/// The profession a workstation block gives.
pub fn profession_for(block: &str) -> Option<&'static str> {
    WORKSTATIONS
        .iter()
        .find(|(b, _)| *b == block)
        .map(|&(_, p)| p)
}

/// How far from itself a villager looks for a workstation.
const SEARCH_RADIUS: i32 = 16;
const SEARCH_HEIGHT: i32 = 6;
/// Ticks between a trade that earned a level and the level.
const LEVEL_UP_DELAY: i32 = 40;
/// A wandering trader stays this long.
pub const TRADER_STAY: u32 = 48000;

/// One trade of the data pack.
#[derive(Clone, Debug)]
struct Def {
    wants: Cost,
    extra: Option<Cost>,
    gives: Cost,
    max_uses: i32,
    xp: i32,
    multiplier: f32,
    /// Villager types it is for (all if empty).
    variants: Vec<String>,
}

#[derive(Clone, Debug)]
struct TradeSet {
    amount: usize,
    trades: Vec<Def>,
}

/// Ticks between chances to spawn a wandering trader (one Minecraft day).
const TRADER_DELAY: i64 = 24000;
/// How often the counter moves, while `spawn_wandering_traders` is on.
const TRADER_PERIOD: i64 = 1200;
const CHANCE_START: i32 = 25;
const CHANCE_STEP: i32 = 25;
const CHANCE_MAX: i32 = 75;
/// Horizontal search around the player or the nearest bell, `-48..=47`.
const TRADER_RANGE: i32 = 48;
/// Same search for each llama, `-4..=3`.
const LLAMA_RANGE: i32 = 4;
const TRADER_FILE: &str = "data/minecraft/wandering_trader.dat";

/// When the wandering trader visits next (`spawn_delay` and `spawn_chance`).
#[derive(Debug)]
pub struct Spawner {
    pub delay: i64,
    pub chance: i32,
    file: Option<PathBuf>,
    data_version: i32,
}

fn nbt_int(c: &Compound, key: &str) -> Option<i32> {
    match c.get(key) {
        Some(Tag::Int(v)) => Some(*v),
        Some(Tag::Byte(v)) => Some(i32::from(*v)),
        Some(Tag::Long(v)) => i32::try_from(*v).ok(),
        _ => None,
    }
}

impl Spawner {
    /// `spawn_delay` 24000 and `spawn_chance` 25 when the world has no file yet.
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        let file = world.map(|w| w.join(TRADER_FILE));
        let mut s = Self {
            delay: TRADER_DELAY,
            chance: CHANCE_START,
            file,
            data_version,
        };
        if let Some(f) = &s.file
            && let Ok(root) = crate::world::read_nbt_gz_compound(f)
            && let Some(Tag::Compound(data)) = root.get("data")
        {
            if let Some(n) = nbt_int(data, "spawn_delay") {
                s.delay = i64::from(n);
            }
            if let Some(n) = nbt_int(data, "spawn_chance") {
                s.chance = n;
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut data = Compound::new();
        data.insert("spawn_delay", Tag::Int(self.delay as i32));
        data.insert("spawn_chance", Tag::Int(self.chance));
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving the wandering trader: {e:#}");
        }
    }
}

/// What one minute of the spawn counter decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visit {
    /// The day is not over yet.
    Waiting,
    /// `spawn_mobs` is off, so this day is skipped.
    MobsOff,
    /// The 25/50/75 roll failed.
    Missed,
    /// Nobody to spawn near; the chance goes back to 25%.
    NoPlayer,
    /// The extra 1-in-10 roll failed.
    Gated,
    /// Look for a spot and spawn.
    Spawn,
}

/// One minute of the counter. `roll` is `0..100` (success when it is below
/// the chance before this minute) and `gate` is `0..10` (success on 0).
fn minute(s: &mut Spawner, spawn_mobs: bool, roll: i32, gate: i32, has_player: bool) -> Visit {
    s.delay -= TRADER_PERIOD;
    if s.delay > 0 {
        return Visit::Waiting;
    }
    s.delay = TRADER_DELAY;
    if !spawn_mobs {
        return Visit::MobsOff;
    }
    let old = s.chance;
    s.chance = (s.chance + CHANCE_STEP).clamp(CHANCE_START, CHANCE_MAX);
    if roll >= old {
        return Visit::Missed;
    }
    if !has_player {
        s.chance = CHANCE_START;
        return Visit::NoPlayer;
    }
    if gate != 0 {
        return Visit::Gated;
    }
    Visit::Spawn
}

/// All trade sets, by name without namespace (`farmer/level_1`).
#[derive(Debug)]
pub struct Trades {
    sets: HashMap<String, TradeSet>,
    pub spawner: Mutex<Spawner>,
}

fn cost_of(value: &Value, items: &Items) -> Option<Cost> {
    // A payment that must carry components is not supported.
    if value.get("components").is_some() {
        return None;
    }
    let item = items.id(value.get("id")?.as_str()?)?;
    let count = value.get("count").and_then(Value::as_i64).unwrap_or(1);
    Some(Cost {
        item,
        count: i32::try_from(count).ok()?,
    })
}

/// The villager types a trade is limited to; `None` if it has a condition
/// that is not understood.
fn variants_of(json: &Value) -> Option<Vec<String>> {
    let Some(predicate) = json.get("merchant_predicate") else {
        return Some(Vec::new());
    };
    match predicate.pointer("/predicate/minecraft:predicates/minecraft:villager~1variant")? {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(list) => list.iter().map(|v| v.as_str().map(str::to_owned)).collect(),
        _ => None,
    }
}

fn parse_def(json: &Value, items: &Items) -> Option<Def> {
    // Enchanted, dyed, mapped and potion results need item components.
    if json.get("given_item_modifier").is_some() {
        return None;
    }
    let wants = cost_of(json.get("wants")?, items)?;
    let extra = match json.get("additional_wants") {
        Some(v) => Some(cost_of(v, items)?),
        None => None,
    };
    let gives = cost_of(json.get("gives")?, items)?;
    let number = |key: &str, default: i64| json.get(key).and_then(Value::as_i64).unwrap_or(default);
    Some(Def {
        wants,
        extra,
        gives,
        max_uses: number("max_uses", 4) as i32,
        xp: number("xp", 1) as i32,
        multiplier: json
            .get("reputation_discount")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32,
        variants: variants_of(json)?,
    })
}

/// Entries of a tag, with the tags inside it.
fn flatten(
    tags: &std::collections::BTreeMap<String, Vec<TagValue>>,
    name: &str,
    out: &mut Vec<String>,
    depth: u32,
) {
    if depth > 8 {
        return;
    }
    for value in tags.get(name).into_iter().flatten() {
        match value {
            TagValue::Entry { id, .. } => out.push(id.clone()),
            TagValue::Tag { id, .. } => flatten(tags, id, out, depth + 1),
        }
    }
}

fn below(n: usize) -> usize {
    (random_u64() % n.max(1) as u64) as usize
}

impl Trades {
    pub fn load(data: &McData, items: &Items, world: Option<&Path>, data_version: i32) -> Self {
        let pack = data.data_pack();
        let tags = pack.tags("minecraft:villager_trade").unwrap_or_default();
        let mut sets = HashMap::new();
        for name in pack
            .registry_entries("minecraft:trade_set")
            .unwrap_or_default()
        {
            let Ok(json) = pack.read_entry("minecraft:trade_set", &name) else {
                continue;
            };
            let amount = json.get("amount").and_then(Value::as_u64).unwrap_or(1) as usize;
            let Some(tag) = json.get("trades").and_then(Value::as_str) else {
                continue;
            };
            let mut ids = Vec::new();
            match tag.strip_prefix('#') {
                Some(tag) => flatten(&tags, tag, &mut ids, 0),
                None => ids.push(tag.to_owned()),
            }
            let trades = ids
                .iter()
                .filter_map(|id| pack.read_entry("minecraft:villager_trade", id).ok())
                .filter_map(|json| parse_def(&json, items))
                .collect();
            let key = name.strip_prefix("minecraft:").unwrap_or(&name).to_owned();
            sets.insert(key, TradeSet { amount, trades });
        }
        Self {
            sets,
            spawner: Mutex::new(Spawner::load(world, data_version)),
        }
    }

    /// Picks the trades of the set `name` that suit a villager of `variant`.
    pub fn pick(&self, name: &str, variant: &str) -> Vec<Offer> {
        let Some(set) = self.sets.get(name) else {
            return Vec::new();
        };
        let mut suitable: Vec<&Def> = set
            .trades
            .iter()
            .filter(|d| d.variants.is_empty() || d.variants.iter().any(|v| v == variant))
            .collect();
        let mut picked = Vec::new();
        while picked.len() < set.amount && !suitable.is_empty() {
            let def = suitable.swap_remove(below(suitable.len()));
            picked.push(Offer {
                cost_a: def.wants,
                cost_b: def.extra,
                result: def.gives,
                uses: 0,
                max_uses: def.max_uses,
                xp: def.xp,
                price_multiplier: def.multiplier,
                demand: 0,
                special_price: 0,
                reward_xp: true,
            });
        }
        picked
    }

    /// The trades of a profession at a level.
    pub fn level_offers(&self, profession: &str, level: i32, variant: &str) -> Vec<Offer> {
        self.pick(&format!("{profession}/level_{level}"), variant)
    }

    /// What a wandering trader sells.
    pub fn trader_offers(&self) -> Vec<Offer> {
        ["buying", "common", "uncommon"]
            .iter()
            .flat_map(|set| self.pick(&format!("wandering_trader/{set}"), ""))
            .collect()
    }
}

/// A new villager or wandering trader (`kind` is `minecraft:villager` or
/// `minecraft:wandering_trader`).
pub fn new_merchant(ctx: &ServerContext, kind: &str, pos: [f64; 3]) -> Option<Entity> {
    let merchant = match kind {
        "minecraft:villager" => Merchant::villager(),
        "minecraft:wandering_trader" => {
            let mut m = Merchant::wandering_trader();
            m.offers = ctx.trades.trader_offers();
            m
        }
        _ => return None,
    };
    Some(Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Merchant(Box::new(merchant)),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    })
}

/// Puts the entity in the world and tells everyone.
pub fn spawn(ctx: &ServerContext, entity: Entity) {
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    crate::tick::lock(&ctx.entities).push(entity);
}

/// The metadata that shows a villager's profession and level.
pub fn villager_data(ctx: &ServerContext, entity_id: i32, m: &Merchant) -> play::SetEntityData {
    let id = |registry: &str, name: &str| ctx.data.builtin.id(registry, name).unwrap_or(0);
    let profession = match &m.profession {
        Some(p) => id("minecraft:villager_profession", &format!("minecraft:{p}")),
        None => id("minecraft:villager_profession", "minecraft:none"),
    };
    play::SetEntityData {
        entity_id,
        entries: vec![(
            play::metadata::VILLAGER_DATA,
            play::MetadataValue::VillagerData(
                id("minecraft:villager_type", &m.variant),
                profession,
                m.level,
            ),
        )],
    }
}

/// Hero of the Village makes villagers' prices drop.
fn hero_level(ctx: &ServerContext, uuid: u128) -> Option<i32> {
    let vitals = crate::tick::lock(&ctx.vitals);
    let kind = crate::effects::kind_of("minecraft:hero_of_the_village")?;
    vitals.get(&uuid)?.effects.get(kind).map(|i| i.amplifier)
}

/// Sets the discounts a player gets from this merchant.
fn refresh_prices(ctx: &ServerContext, uuid: u128, m: &mut Merchant) {
    let hero = if m.wandering {
        None
    } else {
        hero_level(ctx, uuid)
    };
    for offer in &mut m.offers {
        offer.special_price = match hero {
            Some(amplifier) => {
                let rate = 0.3 + 0.0625 * amplifier as f32;
                -((rate * offer.cost_a.count as f32).floor() as i32).max(1)
            }
            None => 0,
        };
    }
}

/// The state of a merchant window.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub title: String,
    pub offers: Vec<Offer>,
    pub level: i32,
    pub xp: i32,
    pub wandering: bool,
}

fn title_of(m: &Merchant) -> String {
    if m.wandering {
        return "Wandering Trader".to_owned();
    }
    match &m.profession {
        Some(p) => {
            let mut name = p.clone();
            name[..1].make_ascii_uppercase();
            name
        }
        None => "Villager".to_owned(),
    }
}

fn snapshot_of(m: &Merchant) -> Snapshot {
    Snapshot {
        title: title_of(m),
        offers: m.offers.clone(),
        level: m.level,
        xp: m.xp,
        wandering: m.wandering,
    }
}

/// Runs `f` on the merchant with this entity id.
fn with_merchant<R>(
    ctx: &ServerContext,
    entity_id: i32,
    f: impl FnOnce(&mut Merchant) -> R,
) -> Option<R> {
    let mut entities = crate::tick::lock(&ctx.entities);
    let e = entities
        .iter_mut()
        .find(|e| e.id == entity_id && !e.removed)?;
    match &mut e.kind {
        Kind::Merchant(m) => Some(f(m)),
        _ => None,
    }
}

/// Whether the entity is a villager or a wandering trader.
pub fn is_merchant(ctx: &ServerContext, entity_id: i32) -> bool {
    crate::tick::lock(&ctx.entities)
        .iter()
        .any(|e| e.id == entity_id && matches!(e.kind, Kind::Merchant(_)))
}

/// A player right-clicks a merchant: its window, if there is something to
/// trade.
pub fn open(ctx: &ServerContext, uuid: u128, entity_id: i32) -> Option<Snapshot> {
    with_merchant(ctx, entity_id, |m| {
        if m.offers.is_empty() {
            return None;
        }
        refresh_prices(ctx, uuid, m);
        if !m.trading.contains(&uuid) {
            m.trading.push(uuid);
        }
        Some(snapshot_of(m))
    })?
}

/// The window closed.
pub fn close(ctx: &ServerContext, uuid: u128, entity_id: i32) {
    with_merchant(ctx, entity_id, |m| m.trading.retain(|u| *u != uuid));
}

/// The window's current state.
pub fn snapshot(ctx: &ServerContext, uuid: u128, entity_id: i32) -> Option<Snapshot> {
    with_merchant(ctx, entity_id, |m| {
        refresh_prices(ctx, uuid, m);
        snapshot_of(m)
    })
}

/// What a stack in a payment slot is worth to a trade.
pub fn cost_in(stack: Option<&ItemStack>) -> Option<Cost> {
    stack.filter(|s| !s.has_changes()).map(|s| Cost {
        item: s.item,
        count: s.count,
    })
}

/// The offer that takes these payments, the selected one first.
pub fn find_offer(
    ctx: &ServerContext,
    offers: &[Offer],
    selected: Option<usize>,
    a: Option<&ItemStack>,
    b: Option<&ItemStack>,
) -> Option<usize> {
    let (a, b) = (cost_in(a), cost_in(b));
    let fits = |o: &Offer| o.accepts(a, b, ctx.items.max_stack(o.cost_a.item));
    selected
        .filter(|&i| offers.get(i).is_some_and(&fits))
        .or_else(|| offers.iter().position(&fits))
}

/// Takes the payment for up to `requested` results out of the payment
/// slots; how many were paid. `i32::MAX` asks for as many as the client's
/// view of the slots (`reported`) says it paid for.
pub fn pay(
    ctx: &ServerContext,
    offer: &Offer,
    grid: &mut [Option<ItemStack>],
    reported: &[Option<ItemStack>],
    requested: i32,
) -> i32 {
    let max = ctx.items.max_stack(offer.cost_a.item);
    let price = offer.price_a(max);
    let requested = if requested == i32::MAX {
        let a = usize::from(grid[0].as_ref().is_none_or(|s| s.item != offer.cost_a.item));
        let had = grid[a].as_ref().map_or(0, |s| s.count);
        let now = reported[a].as_ref().map_or(0, |s| s.count);
        ((had - now) / price).max(1)
    } else {
        requested
    };
    let take = |slot: &mut Option<ItemStack>, count: i32| {
        if let Some(stack) = slot {
            stack.count -= count;
            if stack.count <= 0 {
                *slot = None;
            }
        }
    };
    let mut done = 0;
    while done < requested && offer.uses + done < offer.max_uses {
        let (a, b) = (cost_in(grid[0].as_ref()), cost_in(grid[1].as_ref()));
        if !offer.accepts(a, b, max) {
            break;
        }
        let paid_a = a.is_some_and(|c| c.item == offer.cost_a.item && c.count >= price);
        let (pa, pb) = if paid_a { (0, 1) } else { (1, 0) };
        take(&mut grid[pa], price);
        if let Some(cost_b) = offer.cost_b {
            take(&mut grid[pb], cost_b.count);
        }
        done += 1;
    }
    done
}

/// A player took `times` results of trade `index`: the uses, the villager's
/// experience (and its level), the player's experience.
pub fn complete(
    ctx: &ServerContext,
    uuid: u128,
    entity_id: i32,
    index: usize,
    times: i32,
) -> Option<Snapshot> {
    let gained = with_merchant(ctx, entity_id, |m| {
        let offer = m.offers.get_mut(index)?;
        let times = times.clamp(1, (offer.max_uses - offer.uses).max(1));
        offer.uses += times;
        let (xp, reward) = (offer.xp * times, offer.reward_xp);
        if !m.wandering {
            m.xp += xp;
            if m.should_level_up() && m.level_up_in.is_none() {
                m.level_up_in = Some(LEVEL_UP_DELAY);
            }
        }
        Some((times, reward))
    })??;
    if gained.1 {
        let points: i32 = (0..gained.0).map(|_| 3 + below(4) as i32).sum();
        give_xp(ctx, uuid, points);
    }
    snapshot(ctx, uuid, entity_id)
}

fn give_xp(ctx: &ServerContext, uuid: u128, points: i32) {
    let packet = {
        let mut vitals = crate::tick::lock(&ctx.vitals);
        let Some(v) = vitals.get_mut(&uuid) else {
            return;
        };
        v.add_xp_points(points);
        v.experience_packet()
    };
    let _ = ctx.events.send(Event::Direct {
        uuid,
        packet: Direct::Experience(packet),
    });
}

/// The packet that shows the trades.
pub fn offers_packet(window: i32, s: &Snapshot) -> play::MerchantOffers {
    let cost = |c: &Cost| play::TradeCost {
        item: c.item,
        count: c.count,
    };
    play::MerchantOffers {
        window,
        offers: s
            .offers
            .iter()
            .map(|o| play::TradeOffer {
                cost_a: cost(&o.cost_a),
                cost_b: o.cost_b.as_ref().map(cost),
                result: ItemStack::new(o.result.item, o.result.count),
                out_of_stock: o.out_of_stock(),
                uses: o.uses,
                max_uses: o.max_uses,
                xp: o.xp,
                special_price: o.special_price,
                price_multiplier: o.price_multiplier,
                demand: o.demand,
            })
            .collect(),
        level: s.level,
        xp: s.xp,
        show_progress: !s.wandering,
        can_restock: !s.wandering,
    }
}

/// One tick of a merchant; the metadata to send if its look changed.
pub fn tick(ctx: &ServerContext, entity: &mut Entity, now: i64) -> Option<play::SetEntityData> {
    let (id, age) = (entity.id, entity.age);
    let origin = entity.block_pos();
    let Kind::Merchant(m) = &mut entity.kind else {
        return None;
    };
    if m.wandering {
        if age >= TRADER_STAY && m.trading.is_empty() {
            entity.removed = true;
        }
        return None;
    }
    let mut changed = false;
    if let Some(left) = m.level_up_in {
        if left > 1 {
            m.level_up_in = Some(left - 1);
        } else {
            m.level_up_in = None;
            if m.should_level_up()
                && let Some(profession) = m.profession.clone()
            {
                m.level += 1;
                let more = ctx.trades.level_offers(&profession, m.level, &m.variant);
                m.offers.extend(more);
                changed = true;
            }
        }
    }
    // The rest is slow, and spread over the villagers.
    if (age + id.unsigned_abs()) % 20 == 0 {
        // Players who left without closing the window.
        if !m.trading.is_empty() {
            let online = crate::tick::lock(&ctx.players);
            m.trading.retain(|u| online.contains_key(u));
        }
        match m.profession.clone() {
            Some(profession) => {
                let still = m
                    .job_site
                    .and_then(|(x, y, z)| crate::tick::lock(&ctx.poi).get([x, y, z]))
                    .and_then(|r| crate::poi::profession_of(r.kind))
                    .is_some_and(|p| p == profession);
                if !still && m.xp == 0 && m.level == 1 {
                    // Never traded: the job goes with the workstation.
                    if let Some((x, y, z)) = m.job_site.take() {
                        crate::poi::release(ctx, [x, y, z]);
                    }
                    m.profession = None;
                    m.offers.clear();
                    changed = true;
                } else if still && m.needs_restock() && m.may_restock(now) {
                    let day_time = now.rem_euclid(24000);
                    // They work in the morning and the afternoon.
                    if (2000..11000).contains(&day_time) {
                        m.restock(now);
                    }
                }
            }
            None if (age + id.unsigned_abs()) % 100 == 0 => {
                if let Some((at, profession)) = find_job(ctx, origin) {
                    m.job_site = Some(at);
                    m.profession = Some(profession.to_owned());
                    m.offers = ctx.trades.level_offers(profession, m.level, &m.variant);
                    changed = true;
                }
            }
            None => {}
        }
    }
    changed.then(|| villager_data(ctx, id, m))
}

/// The nearest free workstation around `origin`, its ticket taken.
fn find_job(
    ctx: &ServerContext,
    origin: (i32, i32, i32),
) -> Option<((i32, i32, i32), &'static str)> {
    let (at, kind) = crate::tick::lock(&ctx.poi).take_nearest(
        [origin.0, origin.1, origin.2],
        SEARCH_RADIUS,
        SEARCH_HEIGHT,
        |k| crate::poi::profession_of(k).is_some(),
    )?;
    Some(((at[0], at[1], at[2]), crate::poi::profession_of(kind)?))
}

/// The wandering trader's visits (minecraft.wiki, "Wandering Trader",
/// Spawning): every 1200 ticks, while `spawn_wandering_traders` is on, the
/// delay drops by 1200. At 0 it resets to 24000. Unless `spawn_mobs` is off
/// there is a 25% chance, then 50%, then 75%, and after that a 1-in-10 chance
/// to spawn near a random player (or the nearest bell within 48 blocks).
pub fn tick_spawner(ctx: &ServerContext, now: i64) {
    if ctx.world.dimension != "minecraft:overworld" {
        return;
    }
    if now % TRADER_PERIOD != 0
        || !crate::tick::lock(&ctx.game_rules).bool("spawn_wandering_traders")
    {
        return;
    }
    let players = overworld_players(ctx);
    let visit = {
        let mut s = ctx.trades.spawner.lock().expect("spawner lock");
        let visit = minute(
            &mut s,
            ctx.game_rule_bool("spawn_mobs"),
            below(100) as i32,
            below(10) as i32,
            !players.is_empty(),
        );
        s.save();
        visit
    };
    if visit != Visit::Spawn {
        return;
    }
    let near = players[below(players.len())];
    if try_spawn(ctx, near) {
        let mut s = ctx.trades.spawner.lock().expect("spawner lock");
        s.chance = CHANCE_START;
        s.save();
    }
}

fn overworld_players(ctx: &ServerContext) -> Vec<crate::context::Pose> {
    crate::tick::lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| p.pose)
        .collect()
}

/// `-range` to `range - 1` on each horizontal axis, as `nextInt(range * 2) - range`.
fn horizontal(range: i32) -> (i32, i32) {
    let span = (range * 2).max(1) as usize;
    (below(span) as i32 - range, below(span) as i32 - range)
}

/// The nearest bell within 48 blocks, in chunks that are already loaded.
fn nearest_bell(ctx: &ServerContext, origin: Position) -> Option<Position> {
    let info = ctx.data.blocks.get("minecraft:bell")?;
    let (lo, hi) = (info.first_state, info.last_state);
    let air = ctx.rules.air;
    let blocks = &ctx.data.blocks;
    let y_lo = (origin.y - TRADER_RANGE).max(ctx.world.min_y);
    let y_hi = (origin.y + TRADER_RANGE).min(ctx.world.min_y + ctx.world.height as i32 - 1);
    let limit = i64::from(TRADER_RANGE) * i64::from(TRADER_RANGE);
    let mut best: Option<(i64, Position)> = None;
    let mut buf = vec![air; 16 * 16 * 16];
    let cx0 = (origin.x - TRADER_RANGE) >> 4;
    let cx1 = (origin.x + TRADER_RANGE) >> 4;
    let cz0 = (origin.z - TRADER_RANGE) >> 4;
    let cz1 = (origin.z + TRADER_RANGE) >> 4;
    for cx in cx0..=cx1 {
        for cz in cz0..=cz1 {
            if !ctx.world.chunk_loaded(cx, cz) {
                continue;
            }
            let mut y = y_lo;
            while y <= y_hi {
                ctx.world
                    .read_cube([cx << 4, y, cz << 4], 16, air, blocks, &mut buf);
                for dy in 0..16 {
                    let by = y + dy;
                    if !(y_lo..=y_hi).contains(&by) {
                        continue;
                    }
                    for dx in 0..16 {
                        for dz in 0..16 {
                            let state = buf[((dx * 16 + dy) * 16 + dz) as usize];
                            if !(lo..=hi).contains(&state) {
                                continue;
                            }
                            let bell = Position::new((cx << 4) + dx, by, (cz << 4) + dz);
                            let dist = {
                                let d = |a: i32, b: i32| i64::from(a - b);
                                let (x, yy, z) = (
                                    d(bell.x, origin.x),
                                    d(bell.y, origin.y),
                                    d(bell.z, origin.z),
                                );
                                x * x + yy * yy + z * z
                            };
                            if dist <= limit && best.is_none_or(|(d, _)| dist < d) {
                                best = Some((dist, bell));
                            }
                        }
                    }
                }
                y += 16;
            }
        }
    }
    best.map(|(_, p)| p)
}

fn rejects_traders(ctx: &ServerContext, biome: &str) -> bool {
    ctx.commands
        .env
        .tag(
            "minecraft:worldgen/biome",
            "minecraft:without_wandering_trader_spawns",
        )
        .iter()
        .any(|b| b == biome)
}

fn biome_name(ctx: &ServerContext, p: Position) -> Option<String> {
    let id = ctx.world.biome(p, &ctx.data.blocks)?;
    let name = ctx.world.biome_name(id)?;
    Some(if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    })
}

/// Feet y on the world surface where a mob can stand, or nothing.
fn surface_stand(ctx: &ServerContext, x: i32, z: i32) -> Option<i32> {
    let y = ctx.world.top_where(x, z, |s| !ctx.data.blocks.is_air(s))?;
    standable_at(ctx, x, y, z).then_some(y)
}

fn standable_at(ctx: &ServerContext, x: i32, y: i32, z: i32) -> bool {
    let blocks = &ctx.data.blocks;
    let state = |y: i32| ctx.world.block(Position::new(x, y, z), blocks).unwrap_or(0);
    let below = state(y - 1);
    let open = |s: u32| !ctx.rules.blocks_motion(s) && !blocks.is_fluid(s);
    ctx.rules.blocks_motion(below)
        && !blocks.is_fluid(below)
        && !ctx.rules.is_leaves(below)
        && open(state(y))
        && open(state(y + 1))
}

/// Ten tries for a surface spot within `-range..=range-1` of `origin`.
fn find_spot(ctx: &ServerContext, origin: (i32, i32), range: i32) -> Option<(i32, i32, i32)> {
    for _ in 0..10 {
        let (dx, dz) = horizontal(range);
        let (x, z) = (origin.0 + dx, origin.1 + dz);
        if let Some(y) = surface_stand(ctx, x, z) {
            return Some((x, y, z));
        }
    }
    None
}

/// Spawns a wandering trader and two leashed llamas. The chance is the
/// caller's to reset.
fn try_spawn(ctx: &ServerContext, near: crate::context::Pose) -> bool {
    let player = Position::new(
        near.x.floor() as i32,
        near.y.floor() as i32,
        near.z.floor() as i32,
    );
    let target = nearest_bell(ctx, player).unwrap_or(player);
    for _ in 0..10 {
        let (dx, dz) = horizontal(TRADER_RANGE);
        let (x, z) = (target.x + dx, target.z + dz);
        let Some(y) = surface_stand(ctx, x, z) else {
            continue;
        };
        if biome_name(ctx, Position::new(x, y, z)).is_some_and(|n| rejects_traders(ctx, &n)) {
            return false;
        }
        let pos = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
        let Some(entity) = new_merchant(ctx, "minecraft:wandering_trader", pos) else {
            return false;
        };
        let (id, at) = (entity.id, entity.pos);
        spawn(ctx, entity);
        bring_llamas(ctx, id, at);
        return true;
    }
    false
}

/// Two trader llamas come with a wandering trader, on leads. Each gets ten
/// tries at a surface spot within `-4..=3` of the trader.
pub fn bring_llamas(ctx: &ServerContext, trader: i32, at: [f64; 3]) {
    let origin = (at[0].floor() as i32, at[2].floor() as i32);
    for _ in 0..2 {
        let Some((x, y, z)) = find_spot(ctx, origin, LLAMA_RANGE) else {
            continue;
        };
        let pos = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
        let llama = crate::mobs::new_mob(ctx, mc_entity::Species::TraderLlama, pos);
        let id = llama.id;
        crate::decor::spawn_all(ctx, vec![llama]);
        crate::decor::leash::attach(ctx, id, crate::decor::leash::Holder::Knot(trader));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::WorldLevel;

    #[test]
    fn workstations_give_professions() {
        assert_eq!(profession_for("minecraft:composter"), Some("farmer"));
        assert_eq!(
            profession_for("minecraft:water_cauldron"),
            Some("leatherworker")
        );
        assert_eq!(profession_for("minecraft:stone"), None);
    }

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    /// One tick of the villager, the points of interest caught up first.
    fn settle(ctx: &ServerContext, e: &mut Entity, now: i64) {
        crate::poi::sync(ctx);
        tick(ctx, e, now);
    }

    fn villager(ctx: &ServerContext) -> Entity {
        new_merchant(ctx, "minecraft:villager", [0.5, -60.0, 0.5]).expect("villager")
    }

    fn merchant(e: &mut Entity) -> &mut Merchant {
        match &mut e.kind {
            Kind::Merchant(m) => m,
            _ => unreachable!(),
        }
    }

    #[test]
    fn trade_sets_load() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let offers = ctx.trades.level_offers("farmer", 1, "minecraft:plains");
        assert_eq!(offers.len(), 2);
        assert!(offers.iter().all(|o| o.max_uses > 0 && o.result.count > 0));
        // The wandering trader sells: 2 + 5 + 2 trades.
        assert_eq!(ctx.trades.trader_offers().len(), 9);
        // Some professions have trades at every level (the enchanted ones need components, which are skipped).
        for p in ["butcher", "farmer", "mason"] {
            for level in 1..=5 {
                assert!(
                    !ctx.trades
                        .level_offers(p, level, "minecraft:plains")
                        .is_empty(),
                    "{p} {level}"
                );
            }
        }
    }

    #[test]
    fn unemployed_villager_takes_a_workstation() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let composter = ctx
            .data
            .blocks
            .default_state("minecraft:composter")
            .unwrap();
        let mut level = WorldLevel::new(&ctx);
        level.set_block(mc_protocol::Position::new(3, -60, 2), composter);
        let mut e = villager(&ctx);
        for _ in 0..200 {
            e.age += 1;
            settle(&ctx, &mut e, 3000);
        }
        let m = merchant(&mut e);
        assert_eq!(m.profession.as_deref(), Some("farmer"));
        assert_eq!(m.job_site, Some((3, -60, 2)));
        assert_eq!(m.offers.len(), 2);
        assert!(!crate::poi::claim(&ctx, [3, -60, 2]));
        // A second villager finds it taken.
        let mut other = villager(&ctx);
        for _ in 0..200 {
            other.age += 1;
            settle(&ctx, &mut other, 3000);
        }
        assert_eq!(merchant(&mut other).profession, None);
        // The workstation goes; a villager that never traded loses the job.
        level.set_block(mc_protocol::Position::new(3, -60, 2), 0);
        for _ in 0..40 {
            e.age += 1;
            settle(&ctx, &mut e, 3000);
        }
        let m = merchant(&mut e);
        assert_eq!((m.profession.clone(), m.offers.len()), (None, 0));
    }

    #[test]
    fn trading_earns_levels_and_restocks() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let composter = ctx
            .data
            .blocks
            .default_state("minecraft:composter")
            .unwrap();
        let mut level = WorldLevel::new(&ctx);
        level.set_block(mc_protocol::Position::new(3, -60, 2), composter);
        let mut e = villager(&ctx);
        for _ in 0..100 {
            e.age += 1;
            settle(&ctx, &mut e, 3000);
        }
        let id = e.id;
        merchant(&mut e).offers[0].xp = 10;
        ctx.entities.lock().unwrap().push(e);
        let snap = complete(&ctx, 1, id, 0, 1).expect("trade");
        assert_eq!(snap.offers[0].uses, 1);
        assert_eq!(snap.xp, 10);
        let mut e = ctx.entities.lock().unwrap().pop().unwrap();
        assert_eq!(merchant(&mut e).level_up_in, Some(LEVEL_UP_DELAY));
        for _ in 0..LEVEL_UP_DELAY {
            settle(&ctx, &mut e, 3000);
        }
        let m = merchant(&mut e);
        assert_eq!(m.level, 2);
        assert_eq!(m.offers.len(), 4);
        // Restocking in the morning resets uses and raises demand.
        m.offers[0].uses = m.offers[0].max_uses;
        e.age = 20 - e.id.unsigned_abs() % 20;
        settle(&ctx, &mut e, 3000);
        let m = merchant(&mut e);
        assert_eq!(m.offers[0].uses, 0);
        assert!(m.offers[0].demand > 0);
    }

    #[test]
    fn payments_are_taken_from_either_slot() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let emerald = ctx.items.id("minecraft:emerald").unwrap();
        let book = ctx.items.id("minecraft:book").unwrap();
        let offer = Offer {
            cost_a: Cost {
                item: emerald,
                count: 3,
            },
            cost_b: Some(Cost {
                item: book,
                count: 1,
            }),
            result: Cost {
                item: book,
                count: 1,
            },
            uses: 0,
            max_uses: 5,
            xp: 1,
            price_multiplier: 0.05,
            demand: 0,
            special_price: 0,
            reward_xp: true,
        };
        let mut grid = vec![
            Some(ItemStack::new(book, 3)),
            Some(ItemStack::new(emerald, 10)),
            None,
        ];
        assert_eq!(
            find_offer(
                &ctx,
                std::slice::from_ref(&offer),
                None,
                grid[0].as_ref(),
                grid[1].as_ref()
            ),
            Some(0)
        );
        let reported = grid.clone();
        assert_eq!(pay(&ctx, &offer, &mut grid, &reported, i32::MAX), 1);
        assert_eq!(grid[0].as_ref().map(|s| s.count), Some(2));
        assert_eq!(grid[1].as_ref().map(|s| s.count), Some(7));
        assert_eq!(pay(&ctx, &offer, &mut grid, &reported, 10), 2);
        assert!(grid[0].is_none());
        assert_eq!(grid[1].as_ref().map(|s| s.count), Some(1));
    }

    #[test]
    fn the_spawn_chance_rises_until_a_trader_appears() {
        let mut s = Spawner::load(None, 0);
        assert_eq!((s.delay, s.chance), (TRADER_DELAY, CHANCE_START));
        for _ in 0..19 {
            assert_eq!(minute(&mut s, true, 0, 0, true), Visit::Waiting);
        }
        assert_eq!(s.delay, TRADER_PERIOD);
        // First day: roll against 25, then the stored chance becomes 50.
        assert_eq!(minute(&mut s, true, 24, 0, true), Visit::Spawn);
        assert_eq!(s.chance, 50);
        s.chance = CHANCE_START;
        s.delay = TRADER_PERIOD;
        assert_eq!(minute(&mut s, true, 25, 0, true), Visit::Missed);
        assert_eq!(s.chance, 50);
        s.delay = TRADER_PERIOD;
        assert_eq!(minute(&mut s, true, 49, 0, true), Visit::Spawn);
        assert_eq!(s.chance, 75);
        s.delay = TRADER_PERIOD;
        assert_eq!(minute(&mut s, true, 74, 0, true), Visit::Spawn);
        assert_eq!(s.chance, 75);
        s.delay = TRADER_PERIOD;
        assert_eq!(minute(&mut s, true, 0, 0, false), Visit::NoPlayer);
        assert_eq!(s.chance, CHANCE_START);
        s.delay = TRADER_PERIOD;
        s.chance = 50;
        assert_eq!(minute(&mut s, true, 0, 1, true), Visit::Gated);
        assert_eq!(s.chance, 75);
        s.delay = TRADER_PERIOD;
        assert_eq!(minute(&mut s, false, 0, 0, true), Visit::MobsOff);
        assert_eq!((s.delay, s.chance), (TRADER_DELAY, 75));
    }

    #[test]
    fn spawn_counters_round_trip_through_wandering_trader_dat() {
        let dir = std::env::temp_dir().join(format!("mc-trader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Spawner::load(Some(&dir), 5023);
        s.delay = 12000;
        s.chance = 50;
        s.save();
        let loaded = Spawner::load(Some(&dir), 5023);
        assert_eq!((loaded.delay, loaded.chance), (12000, 50));
        let root = crate::world::read_nbt_gz_compound(&dir.join(TRADER_FILE)).unwrap();
        let Tag::Compound(data) = root.get("data").unwrap() else {
            panic!("data");
        };
        assert_eq!(data.get("spawn_delay"), Some(&Tag::Int(12000)));
        assert_eq!(data.get("spawn_chance"), Some(&Tag::Int(50)));
        assert!(data.get("WanderingTraderId").is_none());
        assert!(root.get("WanderingTraderSpawnDelay").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn horizontal_offsets_match_the_wiki_ranges() {
        for _ in 0..200 {
            let (x, z) = horizontal(TRADER_RANGE);
            assert!((-48..=47).contains(&x) && (-48..=47).contains(&z));
            let (x, z) = horizontal(LLAMA_RANGE);
            assert!((-4..=3).contains(&x) && (-4..=3).contains(&z));
        }
    }

    #[test]
    fn a_bell_within_48_blocks_is_the_spawn_target() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let _ = ctx
            .world
            .block(mc_protocol::Position::new(0, 0, 0), &ctx.data.blocks);
        let origin = Position::new(0, -60, 0);
        assert_eq!(nearest_bell(&ctx, origin), None);
        let bell = ctx.data.blocks.default_state("minecraft:bell").unwrap();
        ctx.world.set_block(
            Position::new(30, -60, 30),
            bell,
            &ctx.data.blocks,
            &ctx.rules,
        );
        assert_eq!(nearest_bell(&ctx, origin), Some(Position::new(30, -60, 30)));
        ctx.world
            .set_block(Position::new(30, -60, 30), 0, &ctx.data.blocks, &ctx.rules);
        ctx.world.set_block(
            Position::new(40, -60, 40),
            bell,
            &ctx.data.blocks,
            &ctx.rules,
        );
        assert_eq!(nearest_bell(&ctx, origin), None);
        assert!(rejects_traders(&ctx, "minecraft:the_void"));
        assert!(!rejects_traders(&ctx, "minecraft:plains"));
    }

    #[test]
    fn a_visit_spawns_a_trader_and_two_leashed_llamas() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        for cx in -3..=3 {
            for cz in -3..=3 {
                let _ = ctx.world.block(
                    mc_protocol::Position::new(cx * 16, -60, cz * 16),
                    &ctx.data.blocks,
                );
            }
        }
        let pose = crate::context::Pose {
            x: 0.5,
            y: -60.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        };
        assert!(try_spawn(&ctx, pose));
        let entities = crate::tick::lock(&ctx.entities);
        let trader = entities
            .iter()
            .find(|e| matches!(&e.kind, Kind::Merchant(m) if m.wandering))
            .expect("trader");
        assert_eq!(trader.age, 0);
        assert_eq!(TRADER_STAY.saturating_sub(trader.age), 48000);
        let id = trader.id;
        let llamas: Vec<i32> = entities
            .iter()
            .filter(
                |e| matches!(&e.kind, Kind::Mob(m) if m.species == mc_entity::Species::TraderLlama),
            )
            .map(|e| e.id)
            .collect();
        assert_eq!(llamas.len(), 2);
        drop(entities);
        for llama in llamas {
            assert_eq!(
                crate::tick::lock(&ctx.leashes).links.get(&llama),
                Some(&crate::decor::leash::Holder::Knot(id))
            );
        }
    }

    #[test]
    fn spawn_rules_gate_the_counter() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        {
            let mut s = ctx.trades.spawner.lock().unwrap();
            s.delay = TRADER_PERIOD;
            s.chance = 25;
        }
        ctx.game_rules
            .lock()
            .unwrap()
            .set("spawn_wandering_traders", crate::rules::Value::Bool(false));
        tick_spawner(&ctx, TRADER_PERIOD);
        {
            let s = ctx.trades.spawner.lock().unwrap();
            assert_eq!((s.delay, s.chance), (TRADER_PERIOD, 25));
        }
        ctx.game_rules
            .lock()
            .unwrap()
            .set("spawn_wandering_traders", crate::rules::Value::Bool(true));
        ctx.game_rules
            .lock()
            .unwrap()
            .set("spawn_mobs", crate::rules::Value::Bool(false));
        tick_spawner(&ctx, TRADER_PERIOD);
        let s = ctx.trades.spawner.lock().unwrap();
        assert_eq!(s.delay, TRADER_DELAY);
        assert_eq!(s.chance, 25);
    }
}
