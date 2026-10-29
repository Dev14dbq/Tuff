//! Furnaces, blast furnaces and smokers (minecraft.wiki: "Furnace",
//! "Smelting", "Fuel"). Slot 0 is the input, 1 the fuel, 2 the result.
//!
//! Each tick a lit furnace burns one tick of fuel. When it is out and there
//! is something to cook with room for the result, it takes one fuel item
//! (its burn time from the item's `cooking_fuel` component); while lit with
//! something to cook it cooks one tick; at the recipe's cooking time
//! (divided by the block's speed multiplier, 2 for blast furnaces and
//! smokers) one input becomes the result. Without fuel, progress goes back
//! two ticks per tick. The block is `lit` while burning. Progress is kept in
//! the block entity (`lit_time_remaining`, `lit_total_time`,
//! `cooking_time_spent`, `cooking_total_time`) and shown in the window as
//! its four properties.

use std::collections::HashMap;

use mc_data::Blocks as Table;
use mc_nbt::{Compound, Tag};

use super::transfer::Containers;
use super::{Effects, ItemStack, Recipes};
use crate::dir::BlockPos;

/// The blocks that cook at the fast speed.
pub const FAST_BLOCKS: [&str; 2] = ["minecraft:blast_furnace", "minecraft:smoker"];

/// Burn time of each fuel item: in a furnace, and in a fast one; and the
/// cooking speed of each.
#[derive(Debug, Default)]
pub struct Fuels {
    burn: HashMap<i32, (i32, i32)>,
    /// Cooking speed: normal, fast.
    speed: (f32, f32),
}

impl Fuels {
    /// `burn` maps each fuel item to its burn ticks in a furnace and in a
    /// fast one; `speed` is the cooking speed of each.
    pub fn new(burn: HashMap<i32, (i32, i32)>, speed: (f32, f32)) -> Self {
        Self { burn, speed }
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
    /// The window's four properties.
    pub fn values(&self) -> Vec<i16> {
        vec![
            self.lit as i16,
            self.lit_total as i16,
            self.cook as i16,
            self.cook_total as i16,
        ]
    }
}

/// The recipe type a furnace block cooks.
pub fn recipe_kind(block: &str) -> Option<&'static str> {
    match block {
        "minecraft:furnace" => Some("minecraft:smelting"),
        "minecraft:blast_furnace" => Some("minecraft:blasting"),
        "minecraft:smoker" => Some("minecraft:smoking"),
        _ => None,
    }
}

fn short(data: &Compound, key: &str) -> i32 {
    match data.get(key) {
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Int(v)) => *v,
        _ => 0,
    }
}

/// Progress as stored in the furnace's block entity.
pub fn stored(host: &impl super::Blocks, p: BlockPos) -> Progress {
    match host.block_entity(p) {
        Some(data) => Progress {
            lit: short(&data, "lit_time_remaining"),
            lit_total: short(&data, "lit_total_time"),
            cook: short(&data, "cooking_time_spent"),
            cook_total: short(&data, "cooking_total_time"),
            input: -1,
        },
        None => Progress::default(),
    }
}

fn store(host: &mut impl super::Blocks, p: BlockPos, block: &str, progress: &Progress) {
    host.edit_block_entity(p, Some(block), &mut |data| {
        for (key, value) in [
            ("lit_time_remaining", progress.lit),
            ("lit_total_time", progress.lit_total),
            ("cooking_time_spent", progress.cook),
            ("cooking_total_time", progress.cook_total),
        ] {
            data.insert(key, Tag::Short(value as i16));
        }
    });
}

/// Notes one more smelt of recipe `recipe` for the experience owed to
/// whoever takes the output (`RecipesUsed`).
pub fn used_recipe(host: &mut impl super::Blocks, p: BlockPos, block: &str, recipe: &str) {
    host.edit_block_entity(p, Some(block), &mut |data| {
        let mut used = match data.get("RecipesUsed") {
            Some(Tag::Compound(c)) => c.clone(),
            _ => Compound::new(),
        };
        let n = match used.get(recipe) {
            Some(Tag::Int(n)) => *n,
            _ => 0,
        };
        used.insert(recipe, Tag::Int(n + 1));
        data.insert("RecipesUsed", Tag::Compound(used));
    });
}

/// One tick of the furnace at `p`, which had `before`. Returns the new
/// progress, or `None` when the furnace has nothing more to do (or is gone).
pub fn tick<H>(host: &mut H, blocks: &Table, p: BlockPos, before: Progress) -> Option<Progress>
where
    H: Containers + Effects + Recipes,
{
    let state = host.block(p)?;
    let name = blocks.block_name(state)?;
    let kind_name = recipe_kind(name)?;
    let store_at = host.block_store(p)?;
    let mut slots = host.slots(&store_at);
    let mut progress = before;
    let mut changed = Vec::new();
    let was_lit = progress.lit > 0;
    if progress.lit > 0 {
        progress.lit -= 1;
    }
    let input = slots[0].clone();
    let recipe = input
        .as_ref()
        .and_then(|stack| host.cooking(kind_name, stack.item));
    let room = |r: &super::Cooking, out: &Option<ItemStack>| match out {
        None => true,
        Some(s) => s.item == r.result.item && s.count + r.result.count <= host.max_stack(s.item),
    };
    let can_cook = recipe.as_ref().is_some_and(|r| room(r, &slots[2]));
    let input_item = input.as_ref().map_or(-1, |s| s.item);
    if input_item != progress.input {
        progress.cook = 0;
        progress.input = input_item;
    }
    let fuel = slots[1].clone();
    let burn = fuel
        .as_ref()
        .map_or(0, |f| host.fuels().burn_time(f.item, name));
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
        } else if host.name(fuel.item) == Some("minecraft:lava_bucket") {
            // The bucket stays behind.
            host.id("minecraft:bucket")
                .map(|item| ItemStack::new(item, 1))
        } else {
            None
        };
        changed.push(1);
    }
    if let (true, Some(r)) = (progress.lit > 0 && can_cook, recipe) {
        progress.cook_total = (r.time as f32 / host.fuels().speed(name)) as i32;
        progress.cook += 1;
        if progress.cook >= progress.cook_total {
            progress.cook = 0;
            let input = slots[0].clone().expect("cooking something");
            slots[0] = super::less_one(&input);
            slots[2] = Some(match slots[2].clone() {
                Some(out) => ItemStack {
                    count: out.count + r.result.count,
                    ..out
                },
                None => r.result.clone(),
            });
            changed.extend([0, 2]);
            used_recipe(host, p, name, &r.name);
        }
    } else if progress.lit == 0 && progress.cook > 0 {
        progress.cook = (progress.cook - 2).max(0);
    } else if !can_cook {
        progress.cook = 0;
    }
    if (progress.lit > 0) != was_lit {
        let lit = if progress.lit > 0 { "true" } else { "false" };
        if let Some(new) = blocks.with_property(state, "lit", lit) {
            host.set_block(p, new);
        }
    }
    if !changed.is_empty() {
        let changes: Vec<(usize, Option<ItemStack>)> =
            changed.into_iter().map(|i| (i, slots[i].clone())).collect();
        host.put(&store_at, &changes);
    }
    if progress != before {
        store(host, p, name, &progress);
        host.window_values(p, &progress.values());
    }
    let busy = progress.lit > 0 || progress.cook > 0 || (can_cook && slots[1].is_some());
    busy.then_some(progress)
}

/// Takes the smelts owed out of a furnace's data (`RecipesUsed`): each
/// recipe's name and how many times it was made.
pub fn take_recipes_used(data: &mut Compound) -> Option<Vec<(String, i32)>> {
    let Some(Tag::Compound(used)) = data.remove("RecipesUsed") else {
        return None;
    };
    Some(
        used.iter()
            .filter_map(|(recipe, n)| match n {
                Tag::Int(n) => Some((recipe.to_owned(), *n)),
                _ => None,
            })
            .collect(),
    )
}
