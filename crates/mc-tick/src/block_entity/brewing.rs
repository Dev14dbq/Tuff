//! Brewing stands (minecraft.wiki: "Brewing Stand", "Brewing"). Slots 0–2
//! hold bottles, 3 the ingredient, 4 the fuel.
//!
//! The mixes are the data pack's `minecraft:brewing` recipes: an input item
//! with a potion, a reagent, and the output item with its potion. Fuel is
//! any item with a `brewing_fuel` component (blaze powder): with the fuel
//! bar empty, one item fills it with its `uses` (20). While some bottle has
//! a mix with the ingredient, a brew starts: it takes one use and lasts 400
//! ticks (over the fuel's speed multiplier). Taking the ingredient away, or
//! no bottle left to brew, stops it. At the end every bottle with a mix
//! becomes the output (keeping its other components), one ingredient is
//! used (dragon's breath leaves its bottle) and world event 1035 plays.
//! The block shows `has_bottle_0..2`. Progress is kept in the block entity
//! (`BrewTime`, `Fuel`) and shown in the window as its two properties.

use std::collections::{HashMap, HashSet};

use mc_data::Blocks as Table;
use mc_nbt::{Compound, Tag};

use super::transfer::Containers;
use super::{Effects, ItemStack, Items, Recipes};
use crate::dir::BlockPos;

/// Ticks one brew takes at normal speed.
pub const BREW_TIME: i32 = 400;

const ID: &str = "minecraft:brewing_stand";

/// One brewing recipe.
#[derive(Debug)]
pub struct Mix {
    pub input: i32,
    /// The input's potion (`minecraft:awkward`).
    pub potion: String,
    pub reagent: i32,
    pub output: i32,
    pub output_potion: String,
}

/// The brewing recipes and fuels.
#[derive(Debug, Default)]
pub struct Mixes {
    mixes: Vec<Mix>,
    reagents: HashSet<i32>,
    /// Uses and speed multiplier of each fuel item.
    fuels: HashMap<i32, (i32, f32)>,
}

/// A brewing stand that is brewing or about to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    /// Ticks left of the brew; 0 when not brewing.
    pub brew: i32,
    /// Uses of fuel left (0–20).
    pub fuel: i32,
    /// The ingredient the brew started with (`-1`: none).
    ingredient: i32,
    /// The speed of the last fuel put in.
    speed: f32,
}

impl Progress {
    /// The window's two properties.
    pub fn values(&self) -> Vec<i16> {
        vec![self.brew as i16, self.fuel as i16]
    }
}

/// The potion in a stack's `potion_contents` (saved as a compound or as
/// just the potion's name).
pub fn potion_of(stack: &ItemStack) -> Option<&str> {
    match stack.components.get("potion_contents")? {
        Tag::String(name) => Some(name),
        Tag::Compound(c) => match c.get("potion")? {
            Tag::String(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// `stack` turned into `item` holding `potion`, its custom colour, effects
/// and other components kept.
fn with_potion(stack: &ItemStack, item: i32, potion: &str) -> ItemStack {
    let mut out = stack.clone();
    out.item = item;
    let mut contents = match stack.components.get("potion_contents") {
        Some(Tag::Compound(c)) => c.clone(),
        _ => Compound::new(),
    };
    contents.insert("potion", Tag::from(potion));
    out.components
        .set("potion_contents", Tag::Compound(contents));
    out
}

impl Mixes {
    /// The data pack's mixes, and each fuel item's uses and speed.
    pub fn new(mixes: Vec<Mix>, fuels: HashMap<i32, (i32, f32)>) -> Self {
        let reagents = mixes.iter().map(|m| m.reagent).collect();
        Self {
            mixes,
            reagents,
            fuels,
        }
    }

    /// What brewing `bottle` with `reagent` makes, if anything.
    pub fn mix(&self, bottle: &ItemStack, reagent: i32) -> Option<ItemStack> {
        let potion = potion_of(bottle)?;
        let potion = potion.strip_prefix("minecraft:").unwrap_or(potion);
        let m = self.mixes.iter().find(|m| {
            m.input == bottle.item
                && m.reagent == reagent
                && m.potion.strip_prefix("minecraft:").unwrap_or(&m.potion) == potion
        })?;
        Some(with_potion(bottle, m.output, &m.output_potion))
    }

    /// Whether `item` goes in the ingredient slot.
    pub fn is_reagent(&self, item: i32) -> bool {
        self.reagents.contains(&item)
    }
}

/// Uses and speed of a fuel stack (a `brewing_fuel` on the stack wins over
/// the item's).
pub fn fuel(host: &impl Recipes, stack: &ItemStack) -> Option<(i32, f32)> {
    if let Some(Tag::Compound(c)) = stack.components.get("brewing_fuel") {
        return Some(host.brewing_fuel(c));
    }
    host.mixes().fuels.get(&stack.item).copied()
}

/// What a brewing stand's slot `slot` takes from a hopper or a player.
pub fn accepts<H: Recipes + Items>(
    host: &H,
    slots: &[Option<ItemStack>],
    slot: usize,
    stack: &ItemStack,
) -> bool {
    match slot {
        3 => host.mixes().is_reagent(stack.item),
        4 => fuel(host, stack).is_some(),
        0..=2 => {
            slots[slot].is_none()
                && matches!(
                    host.name(stack.item),
                    Some(
                        "minecraft:potion"
                            | "minecraft:splash_potion"
                            | "minecraft:lingering_potion"
                            | "minecraft:glass_bottle"
                    )
                )
        }
        _ => false,
    }
}

fn number(data: &Compound, key: &str) -> i32 {
    match data.get(key) {
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Byte(v)) => i32::from(*v),
        Some(Tag::Int(v)) => *v,
        _ => 0,
    }
}

/// Whether the state is a brewing stand.
pub fn is_stand(blocks: &Table, state: u32) -> bool {
    blocks.block_name(state) == Some(ID)
}

/// Progress as stored in the stand's block entity.
pub fn stored(host: &impl super::Blocks, p: BlockPos) -> Progress {
    match host.block_entity(p) {
        Some(data) => Progress {
            brew: number(&data, "BrewTime"),
            fuel: number(&data, "Fuel"),
            ingredient: -1,
            speed: 1.0,
        },
        None => Progress {
            ingredient: -1,
            speed: 1.0,
            ..Progress::default()
        },
    }
}

fn store(host: &mut impl super::Blocks, p: BlockPos, progress: &Progress) {
    host.edit_block_entity(p, Some(ID), &mut |data| {
        data.insert("BrewTime", Tag::Short(progress.brew as i16));
        data.insert("Fuel", Tag::Byte(progress.fuel as i8));
    });
}

/// What crafting or brewing with one `item` leaves behind (minecraft.wiki,
/// "Dragon's Breath", "Crafting": buckets and bottles come back).
pub fn remainder(items: &impl Items, item: i32) -> Option<ItemStack> {
    let left = match items.name(item)? {
        "minecraft:dragon_breath" | "minecraft:honey_bottle" => "minecraft:glass_bottle",
        "minecraft:water_bucket"
        | "minecraft:lava_bucket"
        | "minecraft:milk_bucket"
        | "minecraft:powder_snow_bucket" => "minecraft:bucket",
        _ => return None,
    };
    items.id(left).map(|id| ItemStack::new(id, 1))
}

fn brewable(host: &impl Recipes, slots: &[Option<ItemStack>]) -> bool {
    let Some(reagent) = &slots[3] else {
        return false;
    };
    let mixes = host.mixes();
    mixes.is_reagent(reagent.item)
        && slots[..3]
            .iter()
            .flatten()
            .any(|b| mixes.mix(b, reagent.item).is_some())
}

const SHOWN: [&str; 3] = ["has_bottle_0", "has_bottle_1", "has_bottle_2"];

/// One tick of the stand at `p`, which had `before`. Returns the new
/// progress, or `None` when the stand has nothing more to do (or is gone).
pub fn tick<H>(host: &mut H, blocks: &Table, p: BlockPos, before: Progress) -> Option<Progress>
where
    H: Containers + Effects + Recipes,
{
    let state = host.block(p)?;
    if blocks.block_name(state)? != ID {
        return None;
    }
    let store_at = host.block_store(p)?;
    let mut slots = host.slots(&store_at);
    let mut progress = before;
    let mut changed = Vec::new();
    // An empty fuel bar takes one fuel item.
    if progress.fuel <= 0
        && let Some(f) = slots[4].clone()
        && let Some((uses, speed)) = fuel(host, &f)
    {
        progress.fuel = uses;
        progress.speed = speed;
        slots[4] = super::less_one(&f);
        changed.push(4);
    }
    let can_brew = brewable(host, &slots);
    let ingredient = slots[3].as_ref().map_or(-1, |s| s.item);
    if progress.brew > 0 {
        progress.brew -= 1;
        if progress.brew == 0 && can_brew {
            brew(host, p, &mut slots, &mut changed);
        } else if !can_brew || ingredient != progress.ingredient {
            progress.brew = 0;
        }
    } else if can_brew && progress.fuel > 0 {
        progress.fuel -= 1;
        let speed = if progress.speed > 0.0 {
            progress.speed
        } else {
            1.0
        };
        progress.brew = (BREW_TIME as f32 / speed) as i32;
        progress.ingredient = ingredient;
    }
    // The bottles the stand shows.
    let mut shown = state;
    for (i, slot) in slots.iter().enumerate().take(3) {
        let has = if slot.is_some() { "true" } else { "false" };
        if let Some(s) = blocks.with_property(shown, SHOWN[i], has) {
            shown = s;
        }
    }
    if shown != state {
        host.set_block(p, shown);
    }
    if !changed.is_empty() {
        changed.sort_unstable();
        changed.dedup();
        let changes: Vec<(usize, Option<ItemStack>)> =
            changed.into_iter().map(|i| (i, slots[i].clone())).collect();
        host.put(&store_at, &changes);
    }
    if progress.brew != before.brew || progress.fuel != before.fuel {
        store(host, p, &progress);
        host.window_values(p, &progress.values());
    }
    let busy = progress.brew > 0 || (can_brew && (progress.fuel > 0 || slots[4].is_some()));
    busy.then_some(progress)
}

/// The brew is done: the bottles change, one ingredient is used.
fn brew<H: Items + Effects + Recipes>(
    host: &mut H,
    p: BlockPos,
    slots: &mut [Option<ItemStack>],
    changed: &mut Vec<usize>,
) {
    let Some(reagent) = slots[3].clone() else {
        return;
    };
    for (i, slot) in slots.iter_mut().enumerate().take(3) {
        if let Some(made) = slot
            .as_ref()
            .and_then(|b| host.mixes().mix(b, reagent.item))
        {
            *slot = Some(made);
            changed.push(i);
        }
    }
    let left = super::less_one(&reagent);
    let rest = remainder(host, reagent.item);
    slots[3] = match (left, rest) {
        (None, rest) => rest,
        (Some(left), Some(rest)) => {
            let at = [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ];
            host.spawn_stack(at, &rest);
            Some(left)
        }
        (Some(left), None) => Some(left),
    };
    changed.push(3);
    host.level_event(p, 1035, 0);
}
