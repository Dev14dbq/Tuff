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

use mc_data::McData;
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use crate::containers;
use crate::context::{Event, ServerContext};
use crate::tick::lock;

/// Ticks one brew takes at normal speed.
pub const BREW_TIME: i32 = 400;

/// One brewing recipe.
#[derive(Debug)]
struct Mix {
    input: i32,
    /// The input's potion (`minecraft:awkward`).
    potion: String,
    reagent: i32,
    output: i32,
    output_potion: String,
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
    pub fn values(&self) -> Vec<i16> {
        vec![self.brew as i16, self.fuel as i16]
    }
}

pub type Active = HashMap<(i32, i32, i32), Progress>;

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
    pub fn load(data: &McData, items: &crate::items::Items) -> Self {
        let pack = data.data_pack();
        let mut mixes = Vec::new();
        for name in pack
            .registry_entries("minecraft:recipe")
            .unwrap_or_default()
        {
            let Ok(json) = pack.read_entry("minecraft:recipe", &name) else {
                continue;
            };
            if json["type"] != "minecraft:brewing" {
                continue;
            }
            let item = |v: &serde_json::Value| v.as_str().and_then(|n| items.id(n));
            let (Some(input), Some(reagent), Some(output)) = (
                item(&json["input"]["item"]),
                item(&json["reagent"]["item"]),
                item(&json["output"]["id"]),
            ) else {
                continue;
            };
            let (Some(potion), Some(output_potion)) = (
                json["input"]["potion_contents"]["potions"].as_str(),
                json["output"]["components"]["minecraft:potion_contents"]["potion"].as_str(),
            ) else {
                continue;
            };
            mixes.push(Mix {
                input,
                potion: potion.to_owned(),
                reagent,
                output,
                output_potion: output_potion.to_owned(),
            });
        }
        let reagents = mixes.iter().map(|m| m.reagent).collect();
        let mut fuels = HashMap::new();
        for id in 0..items.count() as i32 {
            let Some(fuel) = items.default_component(id, "minecraft:brewing_fuel") else {
                continue;
            };
            fuels.insert(id, fuel_values(data, fuel));
        }
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

    /// Uses and speed of a fuel stack (a `brewing_fuel` on the stack wins
    /// over the item's).
    pub fn fuel(&self, data: &McData, stack: &ItemStack) -> Option<(i32, f32)> {
        if let Some(Tag::Compound(c)) = stack.components.get("brewing_fuel") {
            let json = mc_command::text::to_json(&Tag::Compound(c.clone()));
            return Some(fuel_values(data, &json));
        }
        self.fuels.get(&stack.item).copied()
    }
}

/// `brewing_fuel` component's uses and speed multiplier.
fn fuel_values(data: &McData, v: &serde_json::Value) -> (i32, f32) {
    let uses = crate::furnaces::provider(
        data,
        "minecraft:context_int_provider",
        &v["uses"],
        "minecraft:brewing_stand",
        0,
    )
    .unwrap_or(20.0) as i32;
    let speed = crate::furnaces::provider(
        data,
        "minecraft:context_float_provider",
        &v["speed_multiplier"],
        "minecraft:brewing_stand",
        0,
    )
    .unwrap_or(1.0) as f32;
    (uses, speed)
}

/// What a brewing stand's slot `slot` takes from a hopper or a player.
pub fn accepts(
    ctx: &ServerContext,
    slots: &[Option<ItemStack>],
    slot: usize,
    stack: &ItemStack,
) -> bool {
    match slot {
        3 => ctx.brewing.is_reagent(stack.item),
        4 => ctx.brewing.fuel(&ctx.data, stack).is_some(),
        0..=2 => {
            slots[slot].is_none()
                && matches!(
                    ctx.items.name(stack.item),
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

fn is_stand(ctx: &ServerContext, p: Position) -> bool {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        == Some("minecraft:brewing_stand")
}

/// Progress as stored in the stand's block entity.
fn stored(ctx: &ServerContext, p: Position) -> Progress {
    match ctx.world.block_entity(p, &ctx.data.blocks) {
        Some(be) => Progress {
            brew: number(&be.data, "BrewTime"),
            fuel: number(&be.data, "Fuel"),
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

fn store(ctx: &ServerContext, p: Position, progress: &Progress) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: "minecraft:brewing_stand".to_owned(),
            data: Compound::new(),
        });
    be.data.insert("BrewTime", Tag::Short(progress.brew as i16));
    be.data.insert("Fuel", Tag::Byte(progress.fuel as i8));
    ctx.world.set_block_entity(be, blocks);
}

/// Progress of the stand at `p` as its window shows it.
pub fn progress(ctx: &ServerContext, p: Position) -> Progress {
    lock(&ctx.brewers)
        .get(&(p.x, p.y, p.z))
        .copied()
        .unwrap_or_else(|| stored(ctx, p))
}

/// The contents of a stand changed (or its chunk was read).
pub fn wake(ctx: &ServerContext, p: Position) {
    if is_stand(ctx, p) {
        lock(&ctx.brewers)
            .entry((p.x, p.y, p.z))
            .or_insert_with(|| stored(ctx, p));
    }
}

/// One tick of every stand with something to do.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<((i32, i32, i32), Progress)> =
        lock(&ctx.brewers).iter().map(|(&k, &v)| (k, v)).collect();
    for (key, before) in active {
        let p = Position::new(key.0, key.1, key.2);
        let keep = tick_one(ctx, p, before);
        let mut all = lock(&ctx.brewers);
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

/// What crafting or brewing with one `item` leaves behind (minecraft.wiki,
/// "Dragon's Breath", "Crafting": buckets and bottles come back).
pub fn remainder(ctx: &ServerContext, item: i32) -> Option<ItemStack> {
    let left = match ctx.items.name(item)? {
        "minecraft:dragon_breath" | "minecraft:honey_bottle" => "minecraft:glass_bottle",
        "minecraft:water_bucket"
        | "minecraft:lava_bucket"
        | "minecraft:milk_bucket"
        | "minecraft:powder_snow_bucket" => "minecraft:bucket",
        _ => return None,
    };
    ctx.items.id(left).map(|id| ItemStack::new(id, 1))
}

fn brewable(ctx: &ServerContext, slots: &[Option<ItemStack>]) -> bool {
    let Some(reagent) = &slots[3] else {
        return false;
    };
    ctx.brewing.is_reagent(reagent.item)
        && slots[..3]
            .iter()
            .flatten()
            .any(|b| ctx.brewing.mix(b, reagent.item).is_some())
}

/// Returns the new progress, or `None` when the stand has nothing more to
/// do (or is gone).
fn tick_one(ctx: &ServerContext, p: Position, before: Progress) -> Option<Progress> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    if blocks.block_name(state)? != "minecraft:brewing_stand" {
        return None;
    }
    let kind = containers::kind_of("minecraft:brewing_stand")?;
    let mut slots = containers::slots(ctx, p, kind);
    let mut progress = before;
    let mut changed = Vec::new();
    // An empty fuel bar takes one fuel item.
    if progress.fuel <= 0
        && let Some(fuel) = slots[4].clone()
        && let Some((uses, speed)) = ctx.brewing.fuel(&ctx.data, &fuel)
    {
        progress.fuel = uses;
        progress.speed = speed;
        slots[4] = (fuel.count > 1).then(|| fuel.with_count(fuel.count - 1));
        changed.push(4);
    }
    let can_brew = brewable(ctx, &slots);
    let ingredient = slots[3].as_ref().map_or(-1, |s| s.item);
    if progress.brew > 0 {
        progress.brew -= 1;
        if progress.brew == 0 && can_brew {
            brew(ctx, p, &mut slots, &mut changed);
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
        if let Some(s) = blocks.with_property(shown, &format!("has_bottle_{i}"), has) {
            shown = s;
        }
    }
    if shown != state {
        let mut level = crate::level::WorldLevel::new(ctx);
        level.set_block(p, shown);
        let changes = level.into_changes();
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
    if !changed.is_empty() {
        changed.sort_unstable();
        changed.dedup();
        let changes: Vec<(usize, Option<ItemStack>)> =
            changed.into_iter().map(|i| (i, slots[i].clone())).collect();
        containers::put(ctx, p, kind, &changes);
    }
    if progress.brew != before.brew || progress.fuel != before.fuel {
        store(ctx, p, &progress);
        let _ = ctx.events.send(Event::FurnaceProgress {
            position: p,
            values: progress.values(),
        });
    }
    let busy = progress.brew > 0 || (can_brew && (progress.fuel > 0 || slots[4].is_some()));
    busy.then_some(progress)
}

/// The brew is done: the bottles change, one ingredient is used.
fn brew(
    ctx: &ServerContext,
    p: Position,
    slots: &mut [Option<ItemStack>],
    changed: &mut Vec<usize>,
) {
    let Some(reagent) = slots[3].clone() else {
        return;
    };
    for (i, slot) in slots.iter_mut().enumerate().take(3) {
        if let Some(made) = slot.as_ref().and_then(|b| ctx.brewing.mix(b, reagent.item)) {
            *slot = Some(made);
            changed.push(i);
        }
    }
    let left = (reagent.count > 1).then(|| reagent.with_count(reagent.count - 1));
    let rest = remainder(ctx, reagent.item);
    slots[3] = match (left, rest) {
        (None, rest) => rest,
        (Some(left), Some(rest)) => {
            let at = [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ];
            crate::tick::spawn_stack_at(ctx, at, &rest);
            Some(left)
        }
        (Some(left), None) => Some(left),
    };
    changed.push(3);
    crate::fx::level_event(
        ctx,
        1035,
        [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
        0,
    );
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

    fn potion(ctx: &ServerContext, item: &str, potion: &str) -> ItemStack {
        let mut stack = ItemStack::new(ctx.items.id(item).unwrap(), 1);
        let mut c = Compound::new();
        c.insert("potion", Tag::from(potion));
        stack.components.set("potion_contents", Tag::Compound(c));
        stack
    }

    fn item(ctx: &ServerContext, name: &str, count: i32) -> ItemStack {
        ItemStack::new(ctx.items.id(name).unwrap(), count)
    }

    fn run(ctx: &ServerContext, ticks: i32) {
        for _ in 0..ticks {
            crate::tick::containers_changed(ctx);
            tick(ctx);
        }
    }

    #[test]
    fn brews_water_into_awkward_then_splash() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        let stand = ctx
            .data
            .blocks
            .default_state("minecraft:brewing_stand")
            .unwrap();
        WorldLevel::new(&ctx).set_block(p, stand);
        let kind = containers::kind_of("minecraft:brewing_stand").unwrap();
        let water = potion(&ctx, "minecraft:potion", "minecraft:water");
        containers::put(
            &ctx,
            p,
            kind,
            &[
                (0, Some(water.clone())),
                (2, Some(water)),
                (3, Some(item(&ctx, "minecraft:nether_wart", 2))),
                (4, Some(item(&ctx, "minecraft:blaze_powder", 1))),
            ],
        );
        run(&ctx, 2);
        // Fuel went in: 20 uses, one of them for this brew.
        let pr = progress(&ctx, p);
        assert_eq!(pr.fuel, 19);
        assert_eq!(pr.brew, BREW_TIME - 1);
        let slots = containers::slots(&ctx, p, kind);
        assert!(slots[4].is_none());
        let state = ctx.world.block(p, &ctx.data.blocks).unwrap();
        assert_eq!(
            ctx.data.blocks.property(state, "has_bottle_0"),
            Some("true")
        );
        assert_eq!(
            ctx.data.blocks.property(state, "has_bottle_1"),
            Some("false")
        );
        run(&ctx, BREW_TIME);
        let slots = containers::slots(&ctx, p, kind);
        for i in [0, 2] {
            let b = slots[i].as_ref().unwrap();
            assert_eq!(potion_of(b), Some("minecraft:awkward"));
        }
        assert_eq!(slots[3].as_ref().map(|s| s.count), Some(1));
        // The second wart has nothing to brew: no new brew starts.
        assert_eq!(progress(&ctx, p).brew, 0);
        assert_eq!(progress(&ctx, p).fuel, 19);

        // Awkward + gunpowder does nothing; a strength potion turns splash.
        containers::put(
            &ctx,
            p,
            kind,
            &[
                (
                    0,
                    Some(potion(&ctx, "minecraft:potion", "minecraft:strength")),
                ),
                (3, Some(item(&ctx, "minecraft:gunpowder", 1))),
            ],
        );
        run(&ctx, BREW_TIME + 2);
        let slots = containers::slots(&ctx, p, kind);
        let b = slots[0].as_ref().unwrap();
        assert_eq!(ctx.items.name(b.item), Some("minecraft:splash_potion"));
        assert_eq!(potion_of(b), Some("minecraft:strength"));
        assert_eq!(
            potion_of(slots[2].as_ref().unwrap()),
            Some("minecraft:awkward")
        );
        assert!(slots[3].is_none());

        // Dragon's breath makes it lingering and leaves its bottle.
        containers::put(
            &ctx,
            p,
            kind,
            &[(3, Some(item(&ctx, "minecraft:dragon_breath", 1)))],
        );
        run(&ctx, BREW_TIME + 2);
        let slots = containers::slots(&ctx, p, kind);
        let b = slots[0].as_ref().unwrap();
        assert_eq!(ctx.items.name(b.item), Some("minecraft:lingering_potion"));
        assert_eq!(
            slots[3].as_ref().map(|s| ctx.items.name(s.item)),
            Some(Some("minecraft:glass_bottle"))
        );
    }

    #[test]
    fn taking_the_ingredient_stops_the_brew() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        let stand = ctx
            .data
            .blocks
            .default_state("minecraft:brewing_stand")
            .unwrap();
        WorldLevel::new(&ctx).set_block(p, stand);
        let kind = containers::kind_of("minecraft:brewing_stand").unwrap();
        containers::put(
            &ctx,
            p,
            kind,
            &[
                (
                    1,
                    Some(potion(&ctx, "minecraft:potion", "minecraft:awkward")),
                ),
                (3, Some(item(&ctx, "minecraft:sugar", 1))),
                (4, Some(item(&ctx, "minecraft:blaze_powder", 1))),
            ],
        );
        run(&ctx, 10);
        assert!(progress(&ctx, p).brew > 0);
        containers::put(&ctx, p, kind, &[(3, None)]);
        run(&ctx, 2);
        assert_eq!(progress(&ctx, p).brew, 0);
        let slots = containers::slots(&ctx, p, kind);
        assert_eq!(
            potion_of(slots[1].as_ref().unwrap()),
            Some("minecraft:awkward")
        );
        // Only reagents and fuel go in through a hopper.
        let wart = item(&ctx, "minecraft:dirt", 1);
        assert!(!accepts(&ctx, &slots, 3, &wart));
        assert!(accepts(
            &ctx,
            &slots,
            3,
            &item(&ctx, "minecraft:nether_wart", 1)
        ));
        assert!(!accepts(
            &ctx,
            &slots,
            1,
            &item(&ctx, "minecraft:glass_bottle", 1)
        ));
        assert!(accepts(
            &ctx,
            &slots,
            0,
            &item(&ctx, "minecraft:glass_bottle", 1)
        ));
    }
}
