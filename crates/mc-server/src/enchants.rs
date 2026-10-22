//! Enchantments: their definitions from the data pack
//! (`data/minecraft/enchantment/*.json`), the levels on a stack, and how the
//! enchanting table picks them (minecraft.wiki, "Enchanting mechanics").

use std::collections::{HashMap, HashSet};

use mc_data::McData;
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::ItemStack;
use mc_worldgen::random::{Legacy, RandomSource};
use serde_json::Value;

/// One enchantment's definition.
#[derive(Clone, Debug)]
pub struct Enchantment {
    /// `minecraft:sharpness`
    pub id: String,
    /// Network id (Registry Data order).
    pub net_id: i32,
    pub max_level: i32,
    pub weight: i32,
    pub anvil_cost: i32,
    /// Base and per-level-above-first of the cost range on a table.
    pub min_cost: (i32, i32),
    pub max_cost: (i32, i32),
    pub supported: HashSet<i32>,
    /// Items a table offers it for; `None`: the supported ones.
    pub primary: Option<HashSet<i32>>,
    /// Enchantments it does not go with.
    pub exclusive: HashSet<String>,
    /// Equipment slot groups it works from (`mainhand`, `armor`...).
    pub slots: Vec<String>,
    /// The `effects` object.
    pub effects: Value,
}

impl Enchantment {
    pub fn min_cost(&self, level: i32) -> i32 {
        self.min_cost.0 + self.min_cost.1 * (level - 1)
    }

    pub fn max_cost(&self, level: i32) -> i32 {
        self.max_cost.0 + self.max_cost.1 * (level - 1)
    }
}

#[derive(Debug, Default)]
pub struct Enchantments {
    list: Vec<Enchantment>,
    by_name: HashMap<String, usize>,
    /// Enchantment tags (`minecraft:curse`) and their members.
    tags: HashMap<String, HashSet<String>>,
    book: Option<i32>,
    enchanted_book: Option<i32>,
}

/// Item ids an `items` field names: an id, a list, or a `#tag`.
fn item_set(
    v: &Value,
    items: &HashMap<String, i32>,
    tags: &HashMap<String, Vec<i32>>,
) -> HashSet<i32> {
    match v {
        Value::String(s) => match s.strip_prefix('#') {
            Some(t) => tags.get(t).into_iter().flatten().copied().collect(),
            None => items.get(s.as_str()).copied().into_iter().collect(),
        },
        Value::Array(l) => l.iter().flat_map(|e| item_set(e, items, tags)).collect(),
        _ => HashSet::new(),
    }
}

fn cost(v: &Value) -> (i32, i32) {
    (
        v["base"].as_i64().unwrap_or(0) as i32,
        v["per_level_above_first"].as_i64().unwrap_or(0) as i32,
    )
}

impl Enchantments {
    /// Reads the definitions; `ids` are the network ids of the enchantment
    /// registry, `item_tags` the item tags with their ids.
    pub fn load(
        data: &McData,
        ids: &HashMap<String, i32>,
        item_tags: &HashMap<String, Vec<i32>>,
    ) -> Self {
        let pack = data.data_pack();
        let Some(items) = data.builtin.get("minecraft:item") else {
            return Self::default();
        };
        let ench_ids: HashMap<String, i32> = ids.clone();
        let tags: HashMap<String, HashSet<String>> = pack
            .tags("minecraft:enchantment")
            .map(|t| {
                let (resolved, _) = mc_data::resolve_tags(&t, |name| ench_ids.get(name).copied());
                let names: HashMap<i32, &String> = ench_ids.iter().map(|(k, v)| (*v, k)).collect();
                resolved
                    .into_iter()
                    .map(|(tag, members)| {
                        let set = members
                            .iter()
                            .filter_map(|id| names.get(id).map(|n| (*n).clone()))
                            .collect();
                        (tag, set)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut sorted: Vec<(&String, &i32)> = ids.iter().collect();
        sorted.sort_by_key(|(_, id)| **id);
        let mut list = Vec::new();
        let mut by_name = HashMap::new();
        for (name, &net_id) in sorted {
            let Ok(v) = pack.read_entry("minecraft:enchantment", name) else {
                continue;
            };
            let exclusive = match &v["exclusive_set"] {
                Value::String(s) => match s.strip_prefix('#') {
                    Some(t) => tags.get(t).cloned().unwrap_or_default(),
                    None => std::iter::once(s.clone()).collect(),
                },
                Value::Array(l) => l
                    .iter()
                    .filter_map(|e| e.as_str().map(str::to_owned))
                    .collect(),
                _ => HashSet::new(),
            };
            by_name.insert(name.clone(), list.len());
            list.push(Enchantment {
                id: name.clone(),
                net_id,
                max_level: v["max_level"].as_i64().unwrap_or(1) as i32,
                weight: v["weight"].as_i64().unwrap_or(1) as i32,
                anvil_cost: v["anvil_cost"].as_i64().unwrap_or(1) as i32,
                min_cost: cost(&v["min_cost"]),
                max_cost: cost(&v["max_cost"]),
                supported: item_set(&v["supported_items"], items, item_tags),
                primary: v
                    .get("primary_items")
                    .map(|p| item_set(p, items, item_tags)),
                exclusive,
                slots: v["slots"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect(),
                effects: v["effects"].clone(),
            });
        }
        Self {
            list,
            by_name,
            tags,
            book: items.get("minecraft:book").copied(),
            enchanted_book: items.get("minecraft:enchanted_book").copied(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&Enchantment> {
        let name = mc_protocol::components::full_name(name);
        self.list.get(*self.by_name.get(name.as_ref())?)
    }

    pub fn all(&self) -> &[Enchantment] {
        &self.list
    }

    /// Whether `name` is in the enchantment tag `tag` (`#` left off).
    pub fn in_tag(&self, name: &str, tag: &str) -> bool {
        let tag = mc_protocol::components::full_name(tag);
        self.tags
            .get(tag.as_ref())
            .is_some_and(|s| s.contains(mc_protocol::components::full_name(name).as_ref()))
    }

    pub fn is_curse(&self, name: &str) -> bool {
        self.in_tag(name, "curse")
    }

    /// Whether two different enchantments may be on one item.
    pub fn compatible(&self, a: &str, b: &str) -> bool {
        if a == b {
            return false;
        }
        let excludes = |x: &str, y: &str| self.get(x).is_some_and(|e| e.exclusive.contains(y));
        !excludes(a, b) && !excludes(b, a)
    }

    pub fn is_book(&self, item: i32) -> bool {
        Some(item) == self.book
    }

    pub fn is_enchanted_book(&self, item: i32) -> bool {
        Some(item) == self.enchanted_book
    }

    pub fn enchanted_book(&self) -> Option<i32> {
        self.enchanted_book
    }

    pub fn book(&self) -> Option<i32> {
        self.book
    }

    /// Where a stack keeps its enchantments: books store them.
    pub fn key(&self, item: i32) -> &'static str {
        if self.is_enchanted_book(item) {
            "minecraft:stored_enchantments"
        } else {
            "minecraft:enchantments"
        }
    }
}

/// The levels under `key` (`enchantments` or `stored_enchantments`).
pub fn levels_in(stack: &ItemStack, key: &str) -> Vec<(String, i32)> {
    let Some(Tag::Compound(c)) = stack.components.get(key) else {
        return Vec::new();
    };
    // Flat since 1.21.5; older worlds have them under `levels`.
    let c = match c.get("levels") {
        Some(Tag::Compound(l)) => l,
        _ => c,
    };
    c.iter()
        .filter_map(|(k, v)| {
            let level = match v {
                Tag::Int(l) => *l,
                Tag::Short(l) => i32::from(*l),
                Tag::Byte(l) => i32::from(*l),
                _ => return None,
            };
            Some((mc_protocol::components::full_name(k).into_owned(), level))
        })
        .collect()
}

/// The enchantments that act (not those a book stores).
pub fn levels(stack: &ItemStack) -> Vec<(String, i32)> {
    levels_in(stack, "minecraft:enchantments")
}

/// Sets the levels under `key`; none left removes the component.
pub fn set_levels(stack: &mut ItemStack, key: &str, list: &[(String, i32)]) {
    let mut c = Compound::new();
    for (name, level) in list {
        if *level > 0 {
            c.insert(name.clone(), Tag::Int((*level).min(255)));
        }
    }
    if c.is_empty() {
        stack.components.reset(key);
    } else {
        stack.components.set(key, Tag::Compound(c));
    }
}

/// `repair_cost` of a stack.
pub fn repair_cost(stack: &ItemStack) -> i32 {
    match stack.components.get("repair_cost") {
        Some(Tag::Int(c)) => *c,
        _ => 0,
    }
}

pub fn set_repair_cost(stack: &mut ItemStack, cost: i32) {
    if cost == 0 {
        stack.components.reset("repair_cost");
    } else {
        stack.components.set("repair_cost", Tag::Int(cost));
    }
}

/// The cost after one more anvil use.
pub fn increased_repair_cost(cost: i32) -> i32 {
    (i64::from(cost) * 2 + 1).min(i64::from(i32::MAX)) as i32
}

// ---------------------------------------------------------------------------
// The enchanting table

/// Levels a table slot costs (0: nothing offered), for `shelves` bookshelves.
pub fn slot_cost(random: &mut Legacy, slot: i32, shelves: i32, enchantable: i32) -> i32 {
    if enchantable <= 0 {
        return 0;
    }
    let shelves = shelves.min(15);
    let i = random.next_int_bound(8) + 1 + (shelves >> 1) + random.next_int_bound(shelves + 1);
    match slot {
        0 => (i / 3).max(1),
        1 => i * 2 / 3 + 1,
        _ => i.max(shelves * 2),
    }
}

/// Weighted pick, as vanilla's `WeightedRandom.getRandomItem`: an index.
fn weighted(random: &mut Legacy, list: &[(&Enchantment, i32)]) -> Option<usize> {
    let total: i32 = list.iter().map(|(e, _)| e.weight).sum();
    if total <= 0 {
        return None;
    }
    let mut n = random.next_int_bound(total);
    for (i, entry) in list.iter().enumerate() {
        n -= entry.0.weight;
        if n < 0 {
            return Some(i);
        }
    }
    None
}

/// The enchantments a table can give `item` at a modified `level`: for each
/// one the highest level whose cost range holds it.
pub fn available<'a>(
    all: &'a Enchantments,
    level: i32,
    item: i32,
    pool: impl Iterator<Item = &'a Enchantment>,
) -> Vec<(&'a Enchantment, i32)> {
    let book = all.is_book(item);
    let mut out = Vec::new();
    for e in pool {
        let fits = book || e.primary.as_ref().unwrap_or(&e.supported).contains(&item);
        if !fits {
            continue;
        }
        for l in (1..=e.max_level).rev() {
            if level >= e.min_cost(l) && level <= e.max_cost(l) {
                out.push((e, l));
                break;
            }
        }
    }
    out
}

/// Picks enchantments for `item` at `level` (the table's and loot's
/// `enchant_with_levels`).
pub fn select<'a>(
    all: &'a Enchantments,
    random: &mut Legacy,
    item: i32,
    enchantable: i32,
    level: i32,
    pool: &[&'a Enchantment],
) -> Vec<(&'a Enchantment, i32)> {
    if enchantable <= 0 {
        return Vec::new();
    }
    let mut level = level
        + 1
        + random.next_int_bound(enchantable / 4 + 1)
        + random.next_int_bound(enchantable / 4 + 1);
    let f = (random.next_float() + random.next_float() - 1.0) * 0.15;
    level = ((level as f32 + level as f32 * f).round() as i32).clamp(1, i32::MAX);
    let mut list = available(all, level, item, pool.iter().copied());
    let mut chosen: Vec<(&Enchantment, i32)> = Vec::new();
    if list.is_empty() {
        return chosen;
    }
    if let Some(i) = weighted(random, &list) {
        chosen.push(list[i]);
    }
    while random.next_int_bound(50) <= level {
        if let Some(last) = chosen.last() {
            let last = last.0.id.clone();
            list.retain(|(e, _)| all.compatible(&e.id, &last));
        }
        if list.is_empty() {
            break;
        }
        if let Some(i) = weighted(random, &list) {
            chosen.push(list[i]);
        }
        level /= 2;
    }
    chosen
}

/// What a table slot gives: seeded by the player's enchantment seed and the
/// slot. A book gets one fewer when it would get several.
pub fn table_offer<'a>(
    all: &'a Enchantments,
    random: &mut Legacy,
    seed: i32,
    slot: i32,
    item: i32,
    enchantable: i32,
    cost: i32,
) -> Vec<(&'a Enchantment, i32)> {
    random.set_seed(i64::from(seed) + i64::from(slot));
    let pool: Vec<&Enchantment> = all
        .all()
        .iter()
        .filter(|e| all.in_tag(&e.id, "in_enchanting_table"))
        .collect();
    let mut list = select(all, random, item, enchantable, cost, &pool);
    if all.is_book(item) && list.len() > 1 {
        let i = random.next_int_bound(list.len() as i32) as usize;
        list.remove(i);
    }
    list
}

/// The three costs and clues a table shows: `[costs; 3]`, enchantment
/// network ids (−1 none) and levels.
pub fn table_values(
    all: &Enchantments,
    seed: i32,
    shelves: i32,
    item: i32,
    enchantable: i32,
) -> ([i32; 3], [i32; 3], [i32; 3]) {
    let mut random = Legacy::new(i64::from(seed));
    let mut costs = [0; 3];
    for (slot, c) in costs.iter_mut().enumerate() {
        *c = slot_cost(&mut random, slot as i32, shelves, enchantable);
        if *c < slot as i32 + 1 {
            *c = 0;
        }
    }
    let mut clues = [-1; 3];
    let mut clue_levels = [-1; 3];
    for slot in 0..3 {
        if costs[slot] > 0 {
            let list = table_offer(
                all,
                &mut random,
                seed,
                slot as i32,
                item,
                enchantable,
                costs[slot],
            );
            if !list.is_empty() {
                let (e, l) = list[random.next_int_bound(list.len() as i32) as usize];
                clues[slot] = e.net_id;
                clue_levels[slot] = l;
            }
        }
    }
    (costs, clues, clue_levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_costs_follow_the_shelves() {
        let mut r = Legacy::new(42);
        for _ in 0..100 {
            let c = slot_cost(&mut r, 2, 15, 10);
            assert!((30..=31).contains(&c), "{c}");
        }
        let mut r = Legacy::new(1);
        let c = slot_cost(&mut r, 0, 0, 10);
        assert!((1..=3).contains(&c));
    }

    #[test]
    fn repair_cost_doubles() {
        let mut c = 0;
        let mut seen = Vec::new();
        for _ in 0..5 {
            c = increased_repair_cost(c);
            seen.push(c);
        }
        assert_eq!(seen, [1, 3, 7, 15, 31]);
    }
}
