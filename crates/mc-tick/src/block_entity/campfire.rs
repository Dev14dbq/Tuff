//! Campfires and soul campfires (minecraft.wiki: "Campfire").
//!
//! Four slots of food, put on with a right click (any `campfire_cooking`
//! input, one item to the first free slot, lit or not). A lit campfire
//! cooks each one tick a tick; at the recipe's time the result pops out
//! above the block and the slot empties. Unlit, the cooking goes back two
//! ticks a tick. The block entity (`minecraft:campfire` for both) keeps
//! `Items`, `CookingTimes` and `CookingTotalTimes`; clients see the
//! `Items` to draw them. A lit campfire hurts whoever stands in it: 1 (2
//! for a soul campfire) of `campfire` damage.

use mc_data::Blocks as Table;
use mc_nbt::{Compound, Tag};

use super::{Blocks, Cooking, Effects, ItemStack, Items, Recipes};
use crate::dir::BlockPos;

/// The block entity id of both campfires.
pub const ID: &str = "minecraft:campfire";

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

/// The food on the campfire at `p`.
pub fn food<H: Blocks + Items>(host: &H, p: BlockPos) -> Food {
    let Some(data) = host.block_entity(p) else {
        return Food::default();
    };
    let slots = host.read_items(&data, 4);
    let mut items: [Option<ItemStack>; 4] = Default::default();
    for (i, s) in slots.into_iter().enumerate() {
        items[i] = s;
    }
    Food {
        items,
        times: ints(&data, "CookingTimes"),
        totals: ints(&data, "CookingTotalTimes"),
    }
}

/// Writes the food back; with `show`, clients are told what is on it.
fn store<H: Blocks + Items + Effects>(host: &mut H, p: BlockPos, food: &Food, show: bool) {
    let items = host.write_items(&food.items);
    let mut saved = None;
    host.edit_block_entity(p, Some(ID), &mut |data| {
        data.insert("Items", items.clone());
        data.insert("CookingTimes", Tag::IntArray(food.times.to_vec()));
        data.insert("CookingTotalTimes", Tag::IntArray(food.totals.to_vec()));
        if show {
            saved = Some(data.clone());
        }
    });
    if let Some(data) = saved {
        host.show_block_entity(p, ID, &data);
    }
}

/// The campfire recipe for `item`: its result and cooking time.
fn recipe(host: &impl Recipes, item: i32) -> Option<Cooking> {
    host.cooking("minecraft:campfire_cooking", item)
}

/// Puts one `stack` on the campfire at `p`; `false` when it is full or the
/// item does not cook. The host then keeps it ticking.
pub fn place<H>(host: &mut H, p: BlockPos, stack: &ItemStack) -> bool
where
    H: Blocks + Items + Effects + Recipes,
{
    let Some(r) = recipe(host, stack.item) else {
        return false;
    };
    let mut food = food(host, p);
    let Some(i) = food.items.iter().position(Option::is_none) else {
        return false;
    };
    food.items[i] = Some(stack.with_count(1));
    food.times[i] = 0;
    food.totals[i] = r.time;
    store(host, p, &food, true);
    true
}

/// One tick of the campfire at `p`: `false` when there is nothing left to
/// cook (or no campfire).
pub fn tick<H>(host: &mut H, blocks: &Table, p: BlockPos) -> bool
where
    H: Blocks + Items + Effects + Recipes,
{
    let Some(state) = host.block(p) else {
        return false;
    };
    if !blocks.block_name(state).is_some_and(is_campfire) {
        return false;
    }
    let lit = blocks.property(state, "lit") == Some("true");
    let before = food(host, p);
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
        if let Some(r) = recipe(host, item.item) {
            let at = [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 1.0,
                f64::from(p.z) + 0.5,
            ];
            host.spawn_stack(at, &r.result);
        }
        food.items[i] = None;
        shown = true;
    }
    if food != before {
        store(host, p, &food, shown);
    }
    food.items
        .iter()
        .enumerate()
        .any(|(i, s)| s.is_some() && (lit || food.times[i] > 0))
}

/// Damage a lit campfire at the feet does: 1, or 2 for a soul campfire.
pub fn damage(blocks: &Table, state: u32) -> Option<f32> {
    if blocks.property(state, "lit") != Some("true") {
        return None;
    }
    match blocks.block_name(state)? {
        "minecraft:campfire" => Some(1.0),
        "minecraft:soul_campfire" => Some(2.0),
        _ => None,
    }
}
