//! Loot tables from the data pack (`loot_table/**/*.json`): what a broken
//! block drops, what `/loot` hands out. Format from the public data pack
//! documentation (minecraft.wiki, "Loot table"): pools with weighted
//! entries, `condition` and `modifier` (item function) lists, number
//! providers, `minecraft:` predicates by name.
//!
//! Not modelled: explosions (a table never sees a radius), damage sources,
//! locations, weather, time and scoreboard values (those conditions fail),
//! structure maps and the wear of items beyond `set_damage`.

use std::collections::HashMap;

use mc_data::McData;
use mc_nbt::{Compound, Tag};
use serde_json::Value;

use crate::util::random_u64;

#[derive(Debug, Default)]
pub struct BlockLoot {
    tables: HashMap<String, Value>,
    predicates: HashMap<String, Value>,
    items: HashMap<String, i32>,
    /// Highest level of each enchantment.
    enchant_max: HashMap<String, i32>,
}

/// One stack handed out.
#[derive(Clone, Debug, PartialEq)]
pub struct Drop {
    pub item: i32,
    pub count: i32,
    /// Component data the table's functions set (`custom_name`, ...).
    pub components: Compound,
}

/// What a table may ask the server.
pub trait LootEnv {
    /// The members of a tag (`#` left off).
    fn tag(&self, registry: &str, tag: &str) -> Vec<String>;
    fn max_stack(&self, item: i32) -> i32;
    /// `max_damage` of an item, 0 if it does not wear.
    fn max_damage(&self, item: i32) -> i32;
    fn item_name(&self, item: i32) -> String;
    /// What a furnace makes of an item.
    fn smelt(&self, item: i32) -> Option<i32>;
}

/// An entity as conditions see it.
#[derive(Clone, Debug)]
pub struct Ent {
    /// `minecraft:cow`
    pub kind: String,
}

/// The tool in a hand.
#[derive(Clone, Debug, Default)]
pub struct Tool {
    pub item: String,
    pub enchants: Vec<(String, i32)>,
}

/// Where and by what a table is rolled.
#[derive(Debug, Default)]
pub struct LootCtx {
    pub origin: Option<[f64; 3]>,
    /// Block name and state properties.
    pub block: Option<(String, Vec<(String, String)>)>,
    pub tool: Option<Tool>,
    pub this: Option<Ent>,
    pub killer: Option<Ent>,
    pub player_kill: bool,
    pub luck: f32,
}

fn random() -> f64 {
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// Whole number in `lo..=hi`.
fn rand_int(lo: i32, hi: i32) -> i32 {
    if hi <= lo {
        return lo;
    }
    lo + (random_u64() % (i64::from(hi) - i64::from(lo) + 1) as u64) as i32
}

fn json_tag(v: &Value) -> Tag {
    match v {
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Tag::Int(i as i32),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(a) => Tag::List(a.iter().map(json_tag).collect()),
        Value::Object(o) => {
            let mut c = Compound::new();
            for (k, v) in o {
                c.insert(k.clone(), json_tag(v));
            }
            Tag::Compound(c)
        }
        Value::Null => Tag::Byte(0),
    }
}

fn full(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

impl BlockLoot {
    pub fn load(data: &McData) -> Self {
        let pack = data.data_pack();
        let mut tables = HashMap::new();
        if let Ok(names) = pack.registry_entries("minecraft:loot_table") {
            for name in names {
                if let Ok(json) = pack.read_entry("minecraft:loot_table", &name) {
                    tables.insert(name, json);
                }
            }
        }
        let mut predicates = HashMap::new();
        if let Ok(names) = pack.registry_entries("minecraft:predicate") {
            for name in names {
                if let Ok(json) = pack.read_entry("minecraft:predicate", &name) {
                    predicates.insert(name, json);
                }
            }
        }
        let mut enchant_max = HashMap::new();
        if let Ok(names) = pack.registry_entries("minecraft:enchantment") {
            for name in names {
                if let Ok(json) = pack.read_entry("minecraft:enchantment", &name) {
                    enchant_max.insert(name, json["max_level"].as_i64().unwrap_or(1) as i32);
                }
            }
        }
        let items = data
            .builtin
            .get("minecraft:item")
            .cloned()
            .unwrap_or_default();
        Self {
            tables,
            predicates,
            items,
            enchant_max,
        }
    }

    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn has(&self, table: &str) -> bool {
        self.tables.contains_key(table)
    }

    pub fn table(&self, table: &str) -> Option<&Value> {
        self.tables.get(table)
    }

    pub fn tables(&self) -> impl Iterator<Item = &String> {
        self.tables.keys()
    }

    /// Highest level of an enchantment (1 for unknown ones).
    pub fn max_level(&self, enchantment: &str) -> i32 {
        self.enchant_max.get(enchantment).copied().unwrap_or(1)
    }

    /// Drops for breaking `block` whose state has `properties`, bare-handed.
    pub fn drops(&self, block: &str, properties: &dyn Fn(&str) -> Option<String>) -> Vec<Drop> {
        let Some(table) = self.tables.get(&format!(
            "minecraft:blocks/{}",
            block.trim_start_matches("minecraft:")
        )) else {
            return Vec::new();
        };
        let props = BLOCK_PROPS
            .iter()
            .filter_map(|k| Some((k.to_string(), properties(k)?)))
            .collect();
        let cx = LootCtx {
            block: Some((block.to_owned(), props)),
            ..LootCtx::default()
        };
        self.roll(&Plain, table, &cx)
    }

    /// Runs item functions (an inline `/item modify` modifier) over stacks.
    pub fn modify(&self, env: &dyn LootEnv, cx: &LootCtx, modifier: &Value, drops: &mut Vec<Drop>) {
        Eval {
            loot: self,
            env,
            cx,
        }
        .functions(modifier, drops);
    }

    /// Rolls a table (named or inline): its stacks, split to the size limit.
    pub fn roll(&self, env: &dyn LootEnv, table: &Value, cx: &LootCtx) -> Vec<Drop> {
        Eval {
            loot: self,
            env,
            cx,
        }
        .table(table, 0)
    }
}

/// Properties blocks' loot conditions test.
pub const BLOCK_PROPS: &[&str] = &[
    "facing",
    "half",
    "type",
    "part",
    "waterlogged",
    "age",
    "bites",
    "candles",
    "eggs",
    "layers",
    "level",
    "pickles",
    "hatch",
    "distance",
    "persistent",
    "open",
    "lit",
    "powered",
    "short",
    "extended",
    "double_block_half",
    "slot_0_occupied",
    "slot_1_occupied",
    "slot_2_occupied",
    "slot_3_occupied",
    "slot_4_occupied",
    "slot_5_occupied",
    "honey_level",
    "flower_amount",
    "segment_amount",
    "leaf_litter_amount",
];

struct Plain;

impl LootEnv for Plain {
    fn tag(&self, _: &str, _: &str) -> Vec<String> {
        Vec::new()
    }
    fn max_stack(&self, _: i32) -> i32 {
        64
    }
    fn max_damage(&self, _: i32) -> i32 {
        0
    }
    fn item_name(&self, _: i32) -> String {
        String::new()
    }
    fn smelt(&self, _: i32) -> Option<i32> {
        None
    }
}

struct Eval<'a> {
    loot: &'a BlockLoot,
    env: &'a dyn LootEnv,
    cx: &'a LootCtx,
}

fn list(v: &Value) -> Vec<&Value> {
    match v {
        Value::Null => Vec::new(),
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    }
}

impl Eval<'_> {
    fn table(&self, table: &Value, depth: usize) -> Vec<Drop> {
        let mut out = Vec::new();
        if depth > 8 {
            return out;
        }
        for pool in table["pools"].as_array().into_iter().flatten() {
            self.pool(pool, &mut out, depth);
        }
        self.functions(&table["modifier"], &mut out);
        let mut split = Vec::new();
        for d in out {
            let max = self.env.max_stack(d.item).max(1);
            let mut left = d.count;
            while left > 0 {
                let n = left.min(max);
                split.push(Drop {
                    count: n,
                    ..d.clone()
                });
                left -= n;
            }
        }
        split
    }

    fn pool(&self, pool: &Value, out: &mut Vec<Drop>, depth: usize) {
        if !self.cond(&pool["condition"]) {
            return;
        }
        let rolls = self.int(&pool["rolls"])
            + (self.float(&pool["bonus_rolls"]) * self.cx.luck).floor() as i32;
        let mut mine = Vec::new();
        for _ in 0..rolls.max(0) {
            let mut choices = Vec::new();
            for entry in pool["entries"].as_array().into_iter().flatten() {
                self.expand(entry, &mut choices);
            }
            let total: i64 = choices.iter().map(|c| c.1).sum();
            if total <= 0 {
                continue;
            }
            let mut pick = (random_u64() % total as u64) as i64;
            for (entry, weight) in &choices {
                if pick < *weight {
                    self.leaf(entry, &mut mine, depth);
                    break;
                }
                pick -= weight;
            }
        }
        self.functions(&pool["modifier"], &mut mine);
        out.extend(mine);
    }

    /// The entries an entry stands for, with their weights; false if it
    /// does not apply.
    fn expand<'v>(&self, entry: &'v Value, out: &mut Vec<(&'v Value, i64)>) -> bool {
        if !self.cond(&entry["condition"]) {
            return false;
        }
        let children = || entry["children"].as_array().into_iter().flatten();
        match entry["type"].as_str().unwrap_or("") {
            "minecraft:alternatives" => children().any(|c| self.expand(c, out)),
            "minecraft:group" => {
                for c in children() {
                    self.expand(c, out);
                }
                true
            }
            "minecraft:sequence" => {
                for c in children() {
                    if !self.expand(c, out) {
                        break;
                    }
                }
                true
            }
            _ => {
                let weight = entry["weight"].as_i64().unwrap_or(1);
                let quality = entry["quality"].as_i64().unwrap_or(0);
                let w = (weight + (quality as f32 * self.cx.luck).floor() as i64).max(0);
                out.push((entry, w));
                true
            }
        }
    }

    fn leaf(&self, entry: &Value, out: &mut Vec<Drop>, depth: usize) {
        let mut mine = Vec::new();
        match entry["type"].as_str().unwrap_or("") {
            "minecraft:item" => {
                if let Some(&item) = entry["name"].as_str().and_then(|n| self.loot.items.get(n)) {
                    mine.push(drop_of(item, 1));
                }
            }
            "minecraft:tag" => {
                let members: Vec<i32> = self
                    .env
                    .tag("minecraft:item", entry["name"].as_str().unwrap_or(""))
                    .iter()
                    .filter_map(|n| self.loot.items.get(n).copied())
                    .collect();
                if entry["expand"].as_bool().unwrap_or(false) {
                    if !members.is_empty() {
                        mine.push(drop_of(
                            members[rand_int(0, members.len() as i32 - 1) as usize],
                            1,
                        ));
                    }
                } else {
                    mine.extend(members.into_iter().map(|i| drop_of(i, 1)));
                }
            }
            "minecraft:loot_table" => {
                let value = &entry["value"];
                let nested = match value.as_str() {
                    Some(name) => self.loot.tables.get(name),
                    None => Some(value),
                };
                if let Some(t) = nested {
                    mine.extend(self.table(t, depth + 1));
                }
            }
            _ => {}
        }
        self.functions(&entry["modifier"], &mut mine);
        out.extend(mine.into_iter().filter(|d| d.count > 0));
    }

    // -- number providers

    fn float(&self, v: &Value) -> f32 {
        if let Some(n) = v.as_f64() {
            return n as f32;
        }
        match v["type"]
            .as_str()
            .map(|t| t.trim_start_matches("minecraft:"))
        {
            Some("constant") => self.float(&v["value"]),
            Some("uniform") => {
                let (lo, hi) = (self.float(&v["min"]), self.float(&v["max"]));
                lo + random() as f32 * (hi - lo)
            }
            Some("binomial") => self.int(v) as f32,
            _ => 0.0,
        }
    }

    fn int(&self, v: &Value) -> i32 {
        if let Some(n) = v.as_f64() {
            return n as i32;
        }
        match v["type"]
            .as_str()
            .map(|t| t.trim_start_matches("minecraft:"))
        {
            Some("constant") => self.int(&v["value"]),
            Some("uniform") => rand_int(self.int(&v["min"]), self.int(&v["max"])),
            Some("binomial") => {
                let p = f64::from(self.float(&v["p"]));
                (0..self.int(&v["n"]).max(0))
                    .filter(|_| random() < p)
                    .count() as i32
            }
            _ => 0,
        }
    }

    // -- conditions

    fn tool_level(&self, enchantment: &str) -> i32 {
        let name = full(enchantment);
        self.cx
            .tool
            .as_ref()
            .and_then(|t| t.enchants.iter().find(|(n, _)| *n == name))
            .map_or(0, |e| e.1)
    }

    fn cond(&self, c: &Value) -> bool {
        match c {
            Value::Null => true,
            Value::Array(a) => a.iter().all(|c| self.cond(c)),
            Value::String(name) => self.loot.predicates.get(name).is_some_and(|p| self.cond(p)),
            _ => self.cond_one(c),
        }
    }

    fn cond_one(&self, c: &Value) -> bool {
        let kind = c["type"]
            .as_str()
            .or_else(|| c["condition"].as_str())
            .unwrap_or("");
        match kind.trim_start_matches("minecraft:") {
            "survives_explosion" => true,
            "match_block" => self.block_matches(&c["blocks"], &c["state"]),
            "block_state_property" => {
                let Some((name, props)) = &self.cx.block else {
                    return false;
                };
                c["block"].as_str().is_some_and(|b| b == name)
                    && c["properties"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .all(|(k, v)| {
                            props
                                .iter()
                                .any(|(pk, pv)| pk == k && Some(pv.as_str()) == v.as_str())
                        })
            }
            "match_tool" => self.tool_matches(&c["predicate"]),
            "random_chance" => random() < f64::from(self.float(&c["chance"])),
            "random_chance_with_enchanted_bonus" => {
                random() < f64::from(self.float(&c["unenchanted_chance"]))
            }
            "table_bonus" => {
                let level = self.tool_level(c["enchantment"].as_str().unwrap_or(""));
                let chances = c["chances"].as_array();
                let chance = chances
                    .and_then(|a| a.get((level as usize).min(a.len().saturating_sub(1))))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                random() < chance
            }
            "inverted" => !self.cond(&c["term"]),
            "any_of" | "alternative" => list(&c["terms"]).into_iter().any(|t| self.cond(t)),
            "all_of" => list(&c["terms"]).into_iter().all(|t| self.cond(t)),
            "killed_by_player" => self.cx.player_kill,
            "reference" => c["name"]
                .as_str()
                .is_some_and(|n| self.cond(&Value::String(n.to_owned()))),
            "entity_properties" => {
                let who = match c["entity"].as_str().unwrap_or("this") {
                    "this" => &self.cx.this,
                    "attacker" | "direct_attacker" | "attacking_player" => &self.cx.killer,
                    _ => &None,
                };
                who.as_ref()
                    .is_some_and(|e| self.entity_matches(e, &c["predicate"]))
            }
            "value_check" => {
                let v = self.float(&c["value"]);
                let r = &c["range"];
                let lo = r.as_f64().or_else(|| r["min"].as_f64()).unwrap_or(f64::MIN);
                let hi = r.as_f64().or_else(|| r["max"].as_f64()).unwrap_or(f64::MAX);
                (lo..=hi).contains(&f64::from(v))
            }
            _ => false,
        }
    }

    fn matches_id(&self, registry: &str, spec: &Value, name: &str) -> bool {
        list(spec)
            .into_iter()
            .filter_map(Value::as_str)
            .any(|s| match s.strip_prefix('#') {
                Some(tag) => self.env.tag(registry, tag).iter().any(|n| n == name),
                None => full(s) == name,
            })
    }

    fn block_matches(&self, blocks: &Value, state: &Value) -> bool {
        let Some((name, props)) = &self.cx.block else {
            return false;
        };
        (blocks.is_null() || self.matches_id("minecraft:block", blocks, name))
            && state.as_object().into_iter().flatten().all(|(k, v)| {
                props
                    .iter()
                    .any(|(pk, pv)| pk == k && Some(pv.as_str()) == v.as_str())
            })
    }

    fn tool_matches(&self, p: &Value) -> bool {
        let Some(tool) = &self.cx.tool else {
            return false;
        };
        if !p["items"].is_null() && !self.matches_id("minecraft:item", &p["items"], &tool.item) {
            return false;
        }
        for e in list(&p["predicates"]["minecraft:enchantments"]) {
            let (lo, hi) = (
                e["levels"].as_i64().or_else(|| e["levels"]["min"].as_i64()),
                e["levels"].as_i64().or_else(|| e["levels"]["max"].as_i64()),
            );
            let ok = tool.enchants.iter().any(|(n, l)| {
                (e["enchantments"].is_null()
                    || self.matches_id("minecraft:enchantment", &e["enchantments"], n))
                    && lo.is_none_or(|lo| i64::from(*l) >= lo)
                    && hi.is_none_or(|hi| i64::from(*l) <= hi)
            });
            if !ok {
                return false;
            }
        }
        true
    }

    fn entity_matches(&self, e: &Ent, p: &Value) -> bool {
        let Some(o) = p.as_object() else {
            return true;
        };
        o.iter().all(|(k, v)| match k.as_str() {
            "type" => self.matches_id("minecraft:entity_type", v, &e.kind),
            _ => false,
        })
    }

    // -- functions

    fn functions(&self, spec: &Value, drops: &mut Vec<Drop>) {
        for f in list(spec) {
            if !self.cond(&f["conditions"]) || !self.cond(&f["condition"]) {
                continue;
            }
            self.function(f, drops);
        }
    }

    fn function(&self, f: &Value, drops: &mut Vec<Drop>) {
        let kind = f["type"]
            .as_str()
            .or_else(|| f["function"].as_str())
            .unwrap_or("");
        match kind.trim_start_matches("minecraft:") {
            "set_count" => {
                let add = f["add"].as_bool().unwrap_or(false);
                for d in drops.iter_mut() {
                    let n = self.int(&f["count"]);
                    d.count = if add { d.count + n } else { n }.max(0);
                }
            }
            "limit_count" => {
                let l = &f["limit"];
                let lo = l
                    .as_f64()
                    .or_else(|| l.get("min").map(|v| f64::from(self.float(v))));
                let hi = l
                    .as_f64()
                    .or_else(|| l.get("max").map(|v| f64::from(self.float(v))));
                for d in drops.iter_mut() {
                    if let Some(lo) = lo {
                        d.count = d.count.max(lo as i32);
                    }
                    if let Some(hi) = hi {
                        d.count = d.count.min(hi as i32);
                    }
                }
            }
            "apply_bonus" => {
                let level = self.tool_level(f["enchantment"].as_str().unwrap_or(""));
                let p = &f["parameters"];
                for d in drops.iter_mut() {
                    match f["formula"]
                        .as_str()
                        .unwrap_or("")
                        .trim_start_matches("minecraft:")
                    {
                        "ore_drops" if level > 0 => {
                            d.count *= (rand_int(0, level + 1) - 1).max(0) + 1;
                        }
                        "uniform_bonus_count" => {
                            let m = p["bonusMultiplier"].as_i64().unwrap_or(1) as i32;
                            d.count += rand_int(0, m * level);
                        }
                        "binomial_with_bonus_count" => {
                            let extra = p["extra"].as_i64().unwrap_or(0) as i32;
                            let chance = p["probability"].as_f64().unwrap_or(0.5);
                            d.count +=
                                (0..level + extra).filter(|_| random() < chance).count() as i32;
                        }
                        _ => {}
                    }
                }
            }
            "set_damage" => {
                for d in drops.iter_mut() {
                    let max = self.env.max_damage(d.item);
                    if max > 0 {
                        let left = 1.0 - f64::from(self.float(&f["damage"])).clamp(0.0, 1.0);
                        d.components.insert(
                            "minecraft:damage",
                            Tag::Int((left * f64::from(max)).floor() as i32),
                        );
                    }
                }
            }
            "set_name" => {
                if !f["name"].is_null() {
                    for d in drops.iter_mut() {
                        d.components
                            .insert("minecraft:custom_name", json_tag(&f["name"]));
                    }
                }
            }
            "set_lore" => {
                for d in drops.iter_mut() {
                    let mut lines = match f["mode"].as_str() {
                        Some("append") => match d.components.get("minecraft:lore") {
                            Some(Tag::List(l)) => l.clone(),
                            _ => Vec::new(),
                        },
                        _ => Vec::new(),
                    };
                    lines.extend(list(&f["lore"]).into_iter().map(json_tag));
                    d.components.insert("minecraft:lore", Tag::List(lines));
                }
            }
            "set_components" => {
                for d in drops.iter_mut() {
                    for (k, v) in f["components"].as_object().into_iter().flatten() {
                        d.components.insert(full(k), json_tag(v));
                    }
                }
            }
            "set_custom_data" => {
                for d in drops.iter_mut() {
                    d.components
                        .insert("minecraft:custom_data", json_tag(&f["tag"]));
                }
            }
            "set_enchantments" => {
                for d in drops.iter_mut() {
                    let key = enchant_key(self.loot, d.item);
                    let mut have = match d.components.get(key) {
                        Some(Tag::Compound(c)) => c.clone(),
                        _ => Compound::new(),
                    };
                    for (name, level) in f["enchantments"].as_object().into_iter().flatten() {
                        let n = self.int(level);
                        let add = f["add"].as_bool().unwrap_or(false);
                        let old = match have.get(name) {
                            Some(Tag::Int(v)) => *v,
                            _ => 0,
                        };
                        have.insert(name.clone(), Tag::Int(if add { old + n } else { n }));
                    }
                    d.components.insert(key, Tag::Compound(have));
                }
            }
            "enchant_randomly" | "enchant_with_levels" => {
                let pool: Vec<String> = match f["options"].as_str() {
                    Some(tag) => self
                        .env
                        .tag("minecraft:enchantment", tag.trim_start_matches('#')),
                    None => self
                        .env
                        .tag("minecraft:enchantment", "minecraft:on_random_loot"),
                };
                for d in drops.iter_mut() {
                    if pool.is_empty() {
                        break;
                    }
                    let name = pool[rand_int(0, pool.len() as i32 - 1) as usize].clone();
                    let max = self.loot.max_level(&name);
                    let level = if kind.ends_with("levels") {
                        (self.int(&f["levels"]) / 10 + 1).clamp(1, max)
                    } else {
                        rand_int(1, max)
                    };
                    if self.env.item_name(d.item) == "minecraft:book"
                        && let Some(&book) = self.loot.items.get("minecraft:enchanted_book")
                    {
                        d.item = book;
                    }
                    let key = enchant_key(self.loot, d.item);
                    let mut c = Compound::new();
                    c.insert(name, Tag::Int(level));
                    d.components.insert(key, Tag::Compound(c));
                }
            }
            "set_potion" => {
                for d in drops.iter_mut() {
                    let mut c = Compound::new();
                    c.insert("potion", f["id"].as_str().unwrap_or("minecraft:water"));
                    d.components
                        .insert("minecraft:potion_contents", Tag::Compound(c));
                }
            }
            "set_instrument" | "set_ominous_bottle_amplifier" | "set_stew_effect" => {}
            "furnace_smelt" => {
                for d in drops.iter_mut() {
                    if let Some(out) = self.env.smelt(d.item) {
                        d.item = out;
                    }
                }
            }
            "exploration_map" => {
                if let Some(&map) = self.loot.items.get("minecraft:filled_map") {
                    for d in drops.iter_mut() {
                        d.item = map;
                    }
                }
            }
            "discard" => drops.clear(),
            "filtered" => {
                let wanted = &f["item_filter"]["items"];
                let pass = |d: &Drop| {
                    wanted.is_null()
                        || self.matches_id("minecraft:item", wanted, &self.env.item_name(d.item))
                };
                let (ok, no): (Vec<Drop>, Vec<Drop>) = drops.drain(..).partition(pass);
                let mut ok = ok;
                let mut no = no;
                self.functions(&f["on_pass"], &mut ok);
                self.functions(&f["on_fail"], &mut no);
                drops.extend(ok);
                drops.extend(no);
            }
            "sequence" => self.functions(&f["functions"], drops),
            _ => {}
        }
    }
}

fn enchant_key(loot: &BlockLoot, item: i32) -> &'static str {
    match loot.items.get("minecraft:enchanted_book") {
        Some(&b) if b == item => "minecraft:stored_enchantments",
        _ => "minecraft:enchantments",
    }
}

fn drop_of(item: i32, count: i32) -> Drop {
    Drop {
        item,
        count,
        components: Compound::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roll(table: &str) -> Vec<Drop> {
        let loot = BlockLoot {
            items: [
                ("minecraft:apple".to_owned(), 1),
                ("minecraft:stick".to_owned(), 2),
            ]
            .into(),
            ..BlockLoot::default()
        };
        loot.roll(
            &Plain,
            &serde_json::from_str(table).unwrap(),
            &LootCtx::default(),
        )
    }

    #[test]
    fn rolls_counts_and_splits_stacks() {
        let t = r#"{"pools":[{"rolls":2,"entries":[{"type":"minecraft:item","name":"minecraft:apple",
            "modifier":{"type":"minecraft:set_count","count":70}}]}]}"#;
        let counts: Vec<i32> = roll(t).iter().map(|d| d.count).collect();
        assert_eq!(counts, [64, 6, 64, 6]);
    }

    #[test]
    fn conditions_and_weights() {
        let never = r#"{"pools":[{"rolls":1,"condition":{"type":"minecraft:random_chance","chance":0},
            "entries":[{"type":"minecraft:item","name":"minecraft:apple"}]}]}"#;
        assert!(roll(never).is_empty());
        let weighted = r#"{"pools":[{"rolls":1,"entries":[
            {"type":"minecraft:item","name":"minecraft:apple","weight":0},
            {"type":"minecraft:item","name":"minecraft:stick","weight":3}]}]}"#;
        assert_eq!(roll(weighted)[0].item, 2);
        let alt = r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:alternatives","children":[
            {"type":"minecraft:item","name":"minecraft:apple","condition":{"type":"minecraft:killed_by_player"}},
            {"type":"minecraft:item","name":"minecraft:stick"}]}]}]}"#;
        assert_eq!(roll(alt)[0].item, 2);
    }
}
