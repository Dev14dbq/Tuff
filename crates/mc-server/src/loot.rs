//! Block loot tables from the data pack (`loot_table/blocks/*.json`): what a
//! broken block drops. Format from the public data pack documentation
//! (minecraft.wiki, "Loot table").
//!
//! Evaluated without a tool for now: silk touch, fortune and tool checks are
//! false, explosions do not happen. Supported: item, alternatives, group and
//! sequence entries; survives_explosion, match_block / block_state_property,
//! random_chance, table_bonus, inverted, any_of, all_of conditions; set_count
//! and limit_count functions.

use std::collections::HashMap;

use mc_data::McData;
use serde_json::Value;

use crate::util::random_u64;

#[derive(Debug, Default)]
pub struct BlockLoot {
    tables: HashMap<String, Value>,
    items: HashMap<String, i32>,
}

/// One dropped stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Drop {
    pub item: i32,
    pub count: i32,
}

fn random() -> f64 {
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

impl BlockLoot {
    pub fn load(data: &McData) -> Self {
        let pack = data.data_pack();
        let mut tables = HashMap::new();
        if let Ok(names) = pack.registry_entries("minecraft:loot_table") {
            for name in names {
                if let Some(block) = name.strip_prefix("minecraft:blocks/")
                    && let Ok(json) = pack.read_entry("minecraft:loot_table", &name)
                {
                    tables.insert(format!("minecraft:{block}"), json);
                }
            }
        }
        let items = data
            .builtin
            .get("minecraft:item")
            .cloned()
            .unwrap_or_default();
        Self { tables, items }
    }

    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// Drops for breaking `block` whose state has `properties`.
    pub fn drops(&self, block: &str, properties: &dyn Fn(&str) -> Option<String>) -> Vec<Drop> {
        let Some(table) = self.tables.get(block) else {
            return Vec::new();
        };
        let ctx = Ctx { block, properties };
        let mut out = Vec::new();
        for pool in table["pools"].as_array().into_iter().flatten() {
            if !conditions(&ctx, pool) {
                continue;
            }
            let rolls = number(&pool["rolls"]).round().max(0.0) as u32;
            for _ in 0..rolls {
                for entry in pool["entries"].as_array().into_iter().flatten() {
                    if self.entry(&ctx, entry, &mut out) {
                        // Entries of a pool are alternatives weighted by
                        // `weight`; with the usual single entry this is it.
                        break;
                    }
                }
            }
            apply_functions(&ctx, pool, &mut out);
        }
        out.retain(|d| d.count > 0);
        out
    }

    /// Adds what an entry yields; returns whether it applied.
    fn entry(&self, ctx: &Ctx, entry: &Value, out: &mut Vec<Drop>) -> bool {
        if !conditions(ctx, entry) {
            return false;
        }
        match entry["type"].as_str().unwrap_or("") {
            "minecraft:item" => {
                let Some(&item) = entry["name"].as_str().and_then(|n| self.items.get(n)) else {
                    return false;
                };
                let mut drops = vec![Drop { item, count: 1 }];
                apply_functions(ctx, entry, &mut drops);
                out.extend(drops);
                true
            }
            "minecraft:alternatives" => entry["children"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|child| self.entry(ctx, child, out)),
            "minecraft:group" => {
                for child in entry["children"].as_array().into_iter().flatten() {
                    self.entry(ctx, child, out);
                }
                true
            }
            "minecraft:sequence" => {
                for child in entry["children"].as_array().into_iter().flatten() {
                    if !self.entry(ctx, child, out) {
                        break;
                    }
                }
                true
            }
            "minecraft:empty" => true,
            _ => false,
        }
    }
}

struct Ctx<'a> {
    block: &'a str,
    properties: &'a dyn Fn(&str) -> Option<String>,
}

/// A number provider: a constant or a uniform / binomial distribution.
fn number(v: &Value) -> f64 {
    if let Some(n) = v.as_f64() {
        return n;
    }
    match v["type"].as_str().unwrap_or("minecraft:uniform") {
        "minecraft:uniform" => {
            let (lo, hi) = (number(&v["min"]), number(&v["max"]));
            lo + random() * (hi - lo)
        }
        "minecraft:binomial" => {
            let (n, p) = (number(&v["n"]) as u32, number(&v["p"]));
            (0..n).filter(|_| random() < p).count() as f64
        }
        "minecraft:constant" => number(&v["value"]),
        _ => 1.0,
    }
}

fn conditions(ctx: &Ctx, holder: &Value) -> bool {
    let single = &holder["condition"];
    let list = &holder["conditions"];
    let all: Vec<&Value> = if single.is_null() {
        list.as_array().into_iter().flatten().collect()
    } else {
        vec![single]
    };
    all.into_iter().all(|c| condition(ctx, c))
}

fn condition(ctx: &Ctx, c: &Value) -> bool {
    // A reference to a named predicate: the ones blocks use test the tool.
    if c.is_string() {
        return false;
    }
    match c["type"]
        .as_str()
        .or_else(|| c["condition"].as_str())
        .unwrap_or("")
    {
        "minecraft:survives_explosion" => true,
        "minecraft:match_block" | "minecraft:block_state_property" => {
            let block = c["blocks"].as_str().or_else(|| c["block"].as_str());
            if block.is_some_and(|b| b != ctx.block) {
                return false;
            }
            c["state"]
                .as_object()
                .or_else(|| c["properties"].as_object())
                .into_iter()
                .flatten()
                .all(|(k, v)| (ctx.properties)(k).as_deref() == v.as_str())
        }
        "minecraft:random_chance" => random() < number(&c["chance"]),
        "minecraft:table_bonus" => {
            let chance = c["chances"].get(0).and_then(Value::as_f64).unwrap_or(0.0);
            random() < chance
        }
        "minecraft:inverted" => !condition(ctx, &c["term"]),
        "minecraft:any_of" | "minecraft:alternative" => c["terms"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|t| condition(ctx, t)),
        "minecraft:all_of" => c["terms"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|t| condition(ctx, t)),
        _ => false,
    }
}

fn apply_functions(ctx: &Ctx, holder: &Value, drops: &mut [Drop]) {
    for f in holder["functions"].as_array().into_iter().flatten() {
        if !conditions(ctx, f) {
            continue;
        }
        match f["function"].as_str().unwrap_or("") {
            "minecraft:set_count" => {
                let n = number(&f["count"]).floor() as i32;
                let add = f["add"].as_bool().unwrap_or(false);
                for d in drops.iter_mut() {
                    d.count = if add { d.count + n } else { n };
                }
            }
            "minecraft:limit_count" => {
                let lo = f["limit"]["min"].as_f64().map(|v| v as i32);
                let hi = f["limit"]["max"].as_f64().map(|v| v as i32);
                for d in drops.iter_mut() {
                    if let Some(lo) = lo {
                        d.count = d.count.max(lo);
                    }
                    if let Some(hi) = hi {
                        d.count = d.count.min(hi);
                    }
                }
            }
            _ => {}
        }
    }
}
