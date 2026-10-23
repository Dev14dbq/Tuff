//! Furnaces, blast furnaces and smokers (minecraft.wiki: "Furnace",
//! "Smelting", "Fuel"). Slot 0 is the input, 1 the fuel, 2 the result.
//!
//! Each tick a lit furnace burns one tick of fuel. When it is out and there
//! is something to cook with room for the result, it takes one fuel item
//! (its burn time from the item's `cooking_fuel` component, evaluated from
//! the data pack's `context_int_provider` entries); while lit with something
//! to cook it cooks one tick; at the recipe's cooking time (divided by the
//! block's speed multiplier, 2 for blast furnaces and smokers) one input
//! becomes the result. Without fuel, progress goes back two ticks per tick.
//! The block is `lit` while burning. Progress is kept in the block entity
//! (`lit_time_remaining`, `lit_total_time`, `cooking_time_spent`,
//! `cooking_total_time`) and shown in the window as its four properties.

use std::collections::HashMap;

use mc_data::McData;
use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use serde_json::Value;

use crate::containers;
use crate::context::{Event, ServerContext};
use crate::tick::lock;

/// Burn time of each fuel item: in a furnace, and in a fast one.
#[derive(Debug, Default)]
pub struct Fuels {
    burn: HashMap<i32, (i32, i32)>,
    /// Cooking speed: normal, fast.
    speed: (f32, f32),
}

/// A furnace that is burning or cooking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub lit: i32,
    pub lit_total: i32,
    pub cook: i32,
    pub cook_total: i32,
    /// The input item being cooked (cooking starts over when it changes).
    input: i32,
}

impl Progress {
    pub fn values(&self) -> Vec<i16> {
        vec![
            self.lit as i16,
            self.lit_total as i16,
            self.cook as i16,
            self.cook_total as i16,
        ]
    }
}

pub type Active = HashMap<(i32, i32, i32), Progress>;

const FAST_BLOCKS: [&str; 2] = ["minecraft:blast_furnace", "minecraft:smoker"];

/// Evaluates a `context_int_provider`/`context_float_provider` value for a
/// furnace block: numbers, references to other entries, `div` and
/// `conditional` on a `match_block` predicate.
pub(crate) fn provider(
    data: &McData,
    registry: &str,
    v: &Value,
    block: &str,
    depth: u32,
) -> Option<f64> {
    if depth > 16 {
        return None;
    }
    let pack = data.data_pack();
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(id) => {
            let entry = pack.read_entry(registry, id).ok()?;
            provider(data, registry, &entry, block, depth + 1)
        }
        Value::Object(o) => match o.get("type")?.as_str()? {
            "minecraft:div" => {
                let l = provider(data, registry, o.get("left")?, block, depth + 1)?;
                let r = provider(data, registry, o.get("right")?, block, depth + 1)?;
                (r != 0.0).then(|| (l / r).trunc())
            }
            "minecraft:conditional" => {
                let holds = predicate(data, o.get("condition")?, block)?;
                let branch = if holds { "on_true" } else { "on_false" };
                provider(data, registry, o.get(branch)?, block, depth + 1)
            }
            "minecraft:constant" => o.get("value")?.as_f64(),
            _ => None,
        },
        _ => None,
    }
}

/// A `match_block` predicate (by id or inline) against a block name.
fn predicate(data: &McData, v: &Value, block: &str) -> Option<bool> {
    let entry = match v {
        Value::String(id) => data
            .data_pack()
            .read_entry("minecraft:predicate", id)
            .ok()?,
        other => other.clone(),
    };
    if entry["type"] != "minecraft:match_block" {
        return None;
    }
    Some(match &entry["blocks"] {
        Value::String(b) => b == block,
        Value::Array(list) => list.iter().any(|b| b == block),
        _ => false,
    })
}

impl Fuels {
    pub fn load(data: &McData, items: &crate::items::Items, ids: &HashMap<String, i32>) -> Self {
        let mut burn = HashMap::new();
        for &id in ids.values() {
            let Some((time, _)) = items.get(id).and_then(|i| i.fuel.clone()) else {
                continue;
            };
            let time = Value::String(time);
            let at = |block: &str| {
                provider(data, "minecraft:context_int_provider", &time, block, 0).unwrap_or(0.0)
                    as i32
            };
            burn.insert(id, (at("minecraft:furnace"), at(FAST_BLOCKS[0])));
        }
        let speed_id = Value::String("minecraft:cooking/speed_default".into());
        let speed = |block: &str| {
            provider(
                data,
                "minecraft:context_float_provider",
                &speed_id,
                block,
                0,
            )
            .unwrap_or(1.0) as f32
        };
        Self {
            burn,
            speed: (speed("minecraft:furnace"), speed(FAST_BLOCKS[0])),
        }
    }

    /// Ticks one `item` burns in the block `block`.
    pub fn burn_time(&self, item: i32, block: &str) -> i32 {
        self.burn.get(&item).map_or(0, |&(normal, fast)| {
            if FAST_BLOCKS.contains(&block) {
                fast
            } else {
                normal
            }
        })
    }

    fn speed(&self, block: &str) -> f32 {
        if FAST_BLOCKS.contains(&block) {
            self.speed.1
        } else {
            self.speed.0
        }
    }
}

fn recipe_kind(block: &str) -> Option<&'static str> {
    match block {
        "minecraft:furnace" => Some("minecraft:smelting"),
        "minecraft:blast_furnace" => Some("minecraft:blasting"),
        "minecraft:smoker" => Some("minecraft:smoking"),
        _ => None,
    }
}

fn short(data: &mc_nbt::Compound, key: &str) -> i32 {
    match data.get(key) {
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Int(v)) => *v,
        _ => 0,
    }
}

/// Progress as stored in the furnace's block entity.
fn stored(ctx: &ServerContext, p: Position) -> Progress {
    match ctx.world.block_entity(p, &ctx.data.blocks) {
        Some(be) => Progress {
            lit: short(&be.data, "lit_time_remaining"),
            lit_total: short(&be.data, "lit_total_time"),
            cook: short(&be.data, "cooking_time_spent"),
            cook_total: short(&be.data, "cooking_total_time"),
            input: -1,
        },
        None => Progress::default(),
    }
}

fn store(ctx: &ServerContext, p: Position, block: &str, progress: &Progress) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: block.to_owned(),
            data: mc_nbt::Compound::new(),
        });
    for (key, value) in [
        ("lit_time_remaining", progress.lit),
        ("lit_total_time", progress.lit_total),
        ("cooking_time_spent", progress.cook),
        ("cooking_total_time", progress.cook_total),
    ] {
        be.data.insert(key, Tag::Short(value as i16));
    }
    ctx.world.set_block_entity(be, blocks);
}

/// Notes one more smelt of recipe `recipe` for the experience owed to
/// whoever takes the output (`RecipesUsed`).
fn used_recipe(ctx: &ServerContext, p: Position, block: &str, recipe: &str) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: block.to_owned(),
            data: mc_nbt::Compound::new(),
        });
    let mut used = match be.data.get("RecipesUsed") {
        Some(Tag::Compound(c)) => c.clone(),
        _ => mc_nbt::Compound::new(),
    };
    let n = match used.get(recipe) {
        Some(Tag::Int(n)) => *n,
        _ => 0,
    };
    used.insert(recipe, Tag::Int(n + 1));
    be.data.insert("RecipesUsed", Tag::Compound(used));
    ctx.world.set_block_entity(be, blocks);
}

/// Clears the smelts owed and returns the experience points they earn: each
/// recipe's total, rounded down, plus one more by chance of the fraction.
pub fn collect_xp(ctx: &ServerContext, p: Position) -> i32 {
    let blocks = &ctx.data.blocks;
    let Some(mut be) = ctx.world.block_entity(p, blocks) else {
        return 0;
    };
    let Some(Tag::Compound(used)) = be.data.remove("RecipesUsed") else {
        return 0;
    };
    let mut points = 0;
    for (recipe, n) in used.iter() {
        let Tag::Int(n) = n else { continue };
        if let Some(r) = ctx.recipes.cooking.iter().find(|r| r.name == *recipe) {
            points += crate::xp::roll_fraction(r.xp * *n as f32);
        }
    }
    ctx.world.set_block_entity(be, blocks);
    points
}

/// Whether the block at `p` is a furnace, blast furnace or smoker.
pub fn is_furnace(ctx: &ServerContext, p: Position) -> bool {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        .is_some_and(|n| recipe_kind(n).is_some())
}

/// Progress of the furnace at `p` as its window shows it.
pub fn progress(ctx: &ServerContext, p: Position) -> Progress {
    lock(&ctx.furnaces)
        .get(&(p.x, p.y, p.z))
        .copied()
        .unwrap_or_else(|| stored(ctx, p))
}

/// The contents of a furnace changed: it may have something to do now.
pub fn wake(ctx: &ServerContext, p: Position) {
    let is_furnace = ctx
        .world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        .is_some_and(|n| recipe_kind(n).is_some());
    if is_furnace {
        lock(&ctx.furnaces)
            .entry((p.x, p.y, p.z))
            .or_insert_with(|| stored(ctx, p));
    }
}

/// One tick of every furnace with something to do.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<((i32, i32, i32), Progress)> =
        lock(&ctx.furnaces).iter().map(|(&k, &v)| (k, v)).collect();
    for (key, before) in active {
        let p = Position::new(key.0, key.1, key.2);
        let keep = tick_one(ctx, p, before);
        let mut all = lock(&ctx.furnaces);
        match keep {
            Some(progress) => {
                all.insert(key, progress);
            }
            None => {
                all.remove(&key);
            }
        }
    }
}

/// Returns the new progress, or `None` when the furnace has nothing more to
/// do (or is gone).
fn tick_one(ctx: &ServerContext, p: Position, before: Progress) -> Option<Progress> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    let name = blocks.block_name(state)?;
    let kind_name = recipe_kind(name)?;
    let kind = containers::kind_of(name)?;
    let mut slots = containers::slots(ctx, p, kind);
    let mut progress = before;
    let mut changed = Vec::new();
    let was_lit = progress.lit > 0;
    if progress.lit > 0 {
        progress.lit -= 1;
    }
    let input = slots[0].clone();
    let recipe = input.as_ref().and_then(|stack| {
        ctx.recipes
            .cooking
            .iter()
            .find(|r| r.kind == kind_name && r.input.contains(&stack.item))
    });
    let room = |r: &crate::crafting::Cooking, out: &Option<ItemStack>| match out {
        None => true,
        Some(s) => {
            s.item == r.result.item && s.count + r.result.count <= ctx.items.max_stack(s.item)
        }
    };
    let can_cook = recipe.is_some_and(|r| room(r, &slots[2]));
    let input_item = input.as_ref().map_or(-1, |s| s.item);
    if input_item != progress.input {
        progress.cook = 0;
        progress.input = input_item;
    }
    let fuel = slots[1].clone();
    let burn = fuel
        .as_ref()
        .map_or(0, |f| ctx.fuels.burn_time(f.item, name));
    if progress.lit == 0
        && can_cook
        && burn > 0
        && let Some(fuel) = fuel
    {
        progress.lit = burn;
        progress.lit_total = burn;
        let left = fuel.count - 1;
        slots[1] = if left > 0 {
            Some(ItemStack {
                count: left,
                ..fuel
            })
        } else if ctx.items.name(fuel.item) == Some("minecraft:lava_bucket") {
            // The bucket stays behind.
            ctx.items.id("minecraft:bucket").map(|item| ItemStack {
                count: 1,
                item,
                damage: 0,
                components: Default::default(),
            })
        } else {
            None
        };
        changed.push(1);
    }
    if let (true, Some(r)) = (progress.lit > 0 && can_cook, recipe) {
        progress.cook_total = (r.time as f32 / ctx.fuels.speed(name)) as i32;
        progress.cook += 1;
        if progress.cook >= progress.cook_total {
            progress.cook = 0;
            let input = slots[0].clone().expect("cooking something");
            slots[0] = (input.count > 1).then(|| ItemStack {
                count: input.count - 1,
                ..input
            });
            slots[2] = Some(match slots[2].clone() {
                Some(out) => ItemStack {
                    count: out.count + r.result.count,
                    ..out
                },
                None => r.result.clone(),
            });
            changed.extend([0, 2]);
            used_recipe(ctx, p, name, &r.name);
        }
    } else if progress.lit == 0 && progress.cook > 0 {
        progress.cook = (progress.cook - 2).max(0);
    } else if !can_cook {
        progress.cook = 0;
    }
    if (progress.lit > 0) != was_lit {
        let lit = if progress.lit > 0 { "true" } else { "false" };
        if let Some(new) = blocks.with_property(state, "lit", lit) {
            let mut level = crate::level::WorldLevel::new(ctx);
            level.set_block(p, new);
            let changes = level.into_changes();
            let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
        }
    }
    if !changed.is_empty() {
        let changes: Vec<(usize, Option<ItemStack>)> =
            changed.into_iter().map(|i| (i, slots[i].clone())).collect();
        containers::put(ctx, p, kind, &changes);
    }
    if progress != before {
        store(ctx, p, name, &progress);
        let _ = ctx.events.send(Event::FurnaceProgress {
            position: p,
            values: progress.values(),
        });
    }
    let busy = progress.lit > 0 || progress.cook > 0 || (can_cook && slots[1].is_some());
    busy.then_some(progress)
}
