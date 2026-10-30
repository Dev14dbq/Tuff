//! Furnaces, blast furnaces and smokers on the server: the rules are in
//! `mc_tick::block_entity::furnace`; this loads the fuels from the data
//! pack, keeps the furnaces that have something to do, and pays the
//! experience owed when the output is taken.
//!
//! Burn times come from each item's `cooking_fuel` component and the speed
//! from `minecraft:cooking/speed_default`, evaluated from the data pack's
//! `context_int_provider`/`context_float_provider` entries for each block.

use std::collections::HashMap;

use mc_data::McData;
use mc_protocol::Position;
use mc_tick::block_entity::Blocks;
use mc_tick::block_entity::furnace::{self, FAST_BLOCKS, recipe_kind};
use serde_json::Value;

use crate::block_entities::Host;
use crate::context::ServerContext;
use crate::level::to_pos;
use crate::tick::lock;

pub use mc_tick::block_entity::furnace::{Fuels, Progress};

pub type Active = HashMap<(i32, i32, i32), Progress>;

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

/// Burn time of each fuel item in a furnace and in a fast one, and the
/// cooking speed of each.
pub fn load_fuels(data: &McData, items: &crate::items::Items, ids: &HashMap<String, i32>) -> Fuels {
    let mut burn = HashMap::new();
    for &id in ids.values() {
        let Some((time, _)) = items.get(id).and_then(|i| i.fuel.clone()) else {
            continue;
        };
        let time = Value::String(time);
        let at = |block: &str| {
            provider(data, "minecraft:context_int_provider", &time, block, 0).unwrap_or(0.0) as i32
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
    Fuels::new(burn, (speed("minecraft:furnace"), speed(FAST_BLOCKS[0])))
}

/// Clears the smelts owed and returns the experience points they earn: each
/// recipe's total, rounded down, plus one more by chance of the fraction.
pub fn collect_xp(ctx: &ServerContext, p: Position) -> i32 {
    let mut host = Host::new(ctx);
    let Some(mut data) = host.block_entity(to_pos(p)) else {
        return 0;
    };
    let Some(used) = furnace::take_recipes_used(&mut data) else {
        return 0;
    };
    host.edit_block_entity(to_pos(p), None, &mut |saved| *saved = data.clone());
    let recipes = ctx.recipes.get();
    let mut points = 0;
    for (recipe, n) in used {
        if let Some(r) = recipes.cooking.iter().find(|r| r.name == recipe) {
            points += crate::xp::roll_fraction(r.xp * n as f32);
        }
    }
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
        .unwrap_or_else(|| furnace::stored(&Host::new(ctx), to_pos(p)))
}

/// The contents of a furnace changed: it may have something to do now.
pub fn wake(ctx: &ServerContext, p: Position) {
    if is_furnace(ctx, p) {
        lock(&ctx.furnaces)
            .entry((p.x, p.y, p.z))
            .or_insert_with(|| furnace::stored(&Host::new(ctx), to_pos(p)));
    }
}

/// One tick of every furnace with something to do.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<((i32, i32, i32), Progress)> =
        lock(&ctx.furnaces).iter().map(|(&k, &v)| (k, v)).collect();
    let mut host = Host::new(ctx);
    for (key, before) in active {
        let p = Position::new(key.0, key.1, key.2);
        // A furnace outside the block-ticking square waits, progress and all.
        if !crate::tickets::blocks_run(ctx, key.0 >> 4, key.2 >> 4) {
            continue;
        }
        let keep = furnace::tick(&mut host, &ctx.data.blocks, to_pos(p), before);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taking_the_output_pays_the_smelts_once() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Some(ctx) = mc_data::McData::load(&dir)
            .ok()
            .and_then(|data| ServerContext::new(crate::Config::new(dir), data).ok())
        else {
            eprintln!("skipped: no game data");
            return;
        };
        let recipes = ctx.recipes.get();
        let recipe = recipes
            .cooking
            .iter()
            .find(|r| r.kind == "minecraft:smelting" && r.xp >= 1.0)
            .expect("a smelting recipe worth a point");
        let p = Position::new(3, -60, 3);
        let block = ctx.data.blocks.default_state("minecraft:furnace").unwrap();
        ctx.world
            .set_block(p, block, &ctx.data.blocks, &ctx.rules)
            .unwrap();
        for _ in 0..10 {
            furnace::used_recipe(
                &mut Host::new(&ctx),
                to_pos(p),
                "minecraft:furnace",
                &recipe.name,
            );
        }
        let points = collect_xp(&ctx, p);
        let owed = recipe.xp * 10.0;
        assert!(
            points == owed.floor() as i32 || points == owed.ceil() as i32,
            "{points} for {owed}"
        );
        assert_eq!(collect_xp(&ctx, p), 0, "paid twice");
    }
}
