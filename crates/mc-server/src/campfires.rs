//! Campfires and soul campfires (minecraft.wiki: "Campfire").
//!
//! Four slots of food, put on with a right click (any `campfire_cooking`
//! input, one item to the first free slot, lit or not). A lit campfire
//! cooks each one tick a tick; at the recipe's time the result pops out
//! above the block and the slot empties. Unlit, the cooking goes back two
//! ticks a tick. The block entity (`minecraft:campfire` for both) keeps
//! `Items`, `CookingTimes` and `CookingTotalTimes`; clients see the
//! `Items` to draw them. Breaking it drops what is on it. A lit campfire
//! hurts whoever stands in it: 1 (2 for a soul campfire) of `campfire`
//! damage.

use std::collections::HashSet;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use crate::context::{Event, ServerContext};
use crate::tick::lock;

pub type Active = HashSet<(i32, i32, i32)>;

const ID: &str = "minecraft:campfire";

/// What is on a campfire.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Food {
    pub items: [Option<ItemStack>; 4],
    pub times: [i32; 4],
    pub totals: [i32; 4],
}

fn ints(data: &Compound, key: &str) -> [i32; 4] {
    let mut out = [0; 4];
    if let Some(Tag::IntArray(v)) = data.get(key) {
        for (o, v) in out.iter_mut().zip(v) {
            *o = *v;
        }
    }
    out
}

pub fn is_campfire(name: &str) -> bool {
    matches!(name, "minecraft:campfire" | "minecraft:soul_campfire")
}

fn block_name(ctx: &ServerContext, p: Position) -> Option<&str> {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
}

/// The food on the campfire at `p`.
pub fn food(ctx: &ServerContext, p: Position) -> Food {
    let Some(be) = ctx.world.block_entity(p, &ctx.data.blocks) else {
        return Food::default();
    };
    let slots = crate::containers::read_items(ctx, &be.data, 4);
    let mut items: [Option<ItemStack>; 4] = Default::default();
    for (i, s) in slots.into_iter().enumerate() {
        items[i] = s;
    }
    Food {
        items,
        times: ints(&be.data, "CookingTimes"),
        totals: ints(&be.data, "CookingTotalTimes"),
    }
}

/// Writes the food back; with `show`, clients are told what is on it.
fn store(ctx: &ServerContext, p: Position, food: &Food, show: bool) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: ID.to_owned(),
            data: Compound::new(),
        });
    be.data
        .insert("Items", crate::containers::write_items(ctx, &food.items));
    be.data
        .insert("CookingTimes", Tag::IntArray(food.times.to_vec()));
    be.data
        .insert("CookingTotalTimes", Tag::IntArray(food.totals.to_vec()));
    let data = crate::world::client_data(ID, &be.data);
    ctx.world.set_block_entity(be, blocks);
    if show
        && let (Some(kind), Some(data)) = (ctx.world.block_entity_kind(ID), data)
    {
        let _ = ctx.events.send(Event::BlockEntity {
            position: p,
            kind,
            data,
        });
    }
}

/// The campfire recipe for `item`: its result and cooking time.
fn recipe(ctx: &ServerContext, item: i32) -> Option<&crate::crafting::Cooking> {
    ctx.recipes
        .cooking
        .iter()
        .find(|r| r.kind == "minecraft:campfire_cooking" && r.input.contains(&item))
}

/// Puts one `stack` on the campfire at `p`; `false` when it is full or the
/// item does not cook.
pub fn place(ctx: &ServerContext, p: Position, stack: &ItemStack) -> bool {
    let Some(r) = recipe(ctx, stack.item) else {
        return false;
    };
    let mut food = food(ctx, p);
    let Some(i) = food.items.iter().position(Option::is_none) else {
        return false;
    };
    food.items[i] = Some(stack.with_count(1));
    food.times[i] = 0;
    food.totals[i] = r.time;
    store(ctx, p, &food, true);
    wake(ctx, p);
    true
}

/// The campfire at `p` is gone: what was on it, to drop.
pub fn broken(ctx: &ServerContext, p: Position) -> Vec<ItemStack> {
    lock(&ctx.campfires).remove(&(p.x, p.y, p.z));
    food(ctx, p).items.into_iter().flatten().collect()
}

/// The campfire at `p` may have food to cook.
pub fn wake(ctx: &ServerContext, p: Position) {
    if block_name(ctx, p).is_some_and(is_campfire) {
        lock(&ctx.campfires).insert((p.x, p.y, p.z));
    }
}

/// One tick of every campfire with food on it.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<(i32, i32, i32)> = lock(&ctx.campfires).iter().copied().collect();
    for key in active {
        let p = Position::new(key.0, key.1, key.2);
        if !tick_one(ctx, p) {
            lock(&ctx.campfires).remove(&key);
        }
    }
}

/// `false` when there is nothing left to cook (or no campfire).
fn tick_one(ctx: &ServerContext, p: Position) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return false;
    };
    if !blocks.block_name(state).is_some_and(is_campfire) {
        return false;
    }
    let lit = blocks.property(state, "lit") == Some("true");
    let before = food(ctx, p);
    let mut food = before.clone();
    let mut shown = false;
    for i in 0..4 {
        let Some(item) = food.items[i].clone() else {
            continue;
        };
        if !lit {
            food.times[i] = (food.times[i] - 2).max(0);
            continue;
        }
        food.times[i] += 1;
        if food.times[i] < food.totals[i] {
            continue;
        }
        if let Some(r) = recipe(ctx, item.item) {
            let at = [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 1.0,
                f64::from(p.z) + 0.5,
            ];
            crate::tick::spawn_stack_at(ctx, at, &r.result);
        }
        food.items[i] = None;
        shown = true;
    }
    if food != before {
        store(ctx, p, &food, shown);
    }
    food.items.iter().enumerate().any(|(i, s)| s.is_some() && (lit || food.times[i] > 0))
}

/// `state` with water poured in: a lit campfire goes out with a hiss.
pub fn waterlogged(ctx: &ServerContext, p: Position, state: u32) -> u32 {
    let wet = ctx.shapes.with(state, "waterlogged", "true");
    let blocks = &ctx.data.blocks;
    if blocks.block_name(state).is_some_and(is_campfire)
        && blocks.property(state, "lit") == Some("true")
    {
        crate::fx::sound(
            ctx,
            "block.fire.extinguish",
            crate::fx::category::BLOCKS,
            crate::blockuse::centre(p),
            1.0,
            1.0,
        );
        return ctx.shapes.with(wet, "lit", "false");
    }
    wet
}

/// Damage a lit campfire at the feet does: 1, or 2 for a soul campfire.
pub fn damage(ctx: &ServerContext, state: u32) -> Option<f32> {
    let blocks = &ctx.data.blocks;
    if blocks.property(state, "lit") != Some("true") {
        return None;
    }
    match blocks.block_name(state)? {
        "minecraft:campfire" => Some(1.0),
        "minecraft:soul_campfire" => Some(2.0),
        _ => None,
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

    #[test]
    fn cooks_four_and_cools_unlit() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        let blocks = &ctx.data.blocks;
        let lit = blocks.default_state("minecraft:campfire").unwrap();
        assert_eq!(blocks.property(lit, "lit"), Some("true"));
        WorldLevel::new(&ctx).set_block(p, lit);
        let beef = ItemStack::new(ctx.items.id("minecraft:beef").unwrap(), 5);
        let dirt = ItemStack::new(ctx.items.id("minecraft:dirt").unwrap(), 1);
        assert!(!place(&ctx, p, &dirt));
        for _ in 0..4 {
            assert!(place(&ctx, p, &beef));
        }
        assert!(!place(&ctx, p, &beef), "only four fit");
        for _ in 0..100 {
            tick(&ctx);
        }
        assert_eq!(food(&ctx, p).times, [100; 4]);
        // Put out: cooking goes back two a tick.
        let out = ctx.shapes.with(lit, "lit", "false");
        WorldLevel::new(&ctx).set_block(p, out);
        for _ in 0..10 {
            tick(&ctx);
        }
        assert_eq!(food(&ctx, p).times, [80; 4]);
        WorldLevel::new(&ctx).set_block(p, lit);
        wake(&ctx, p);
        for _ in 0..520 {
            tick(&ctx);
        }
        let f = food(&ctx, p);
        assert!(f.items.iter().all(Option::is_none), "{f:?}");
        let cooked = ctx.items.id("minecraft:cooked_beef").unwrap();
        let popped = lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(&e.kind, mc_entity::Kind::Item { item, .. } if *item == cooked))
            .count();
        assert_eq!(popped, 4);
        assert!(lock(&ctx.campfires).is_empty());
        assert_eq!(damage(&ctx, lit), Some(1.0));
        let soul = blocks.default_state("minecraft:soul_campfire").unwrap();
        assert_eq!(damage(&ctx, soul), Some(2.0));
        assert_eq!(damage(&ctx, out), None);
    }
}
