//! Brewing stands on the server: the rules are in
//! `mc_tick::block_entity::brewing`; this loads the mixes and fuels from the
//! data pack and keeps the stands that have something to do.

use std::collections::HashMap;

use mc_data::McData;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::BlockPos;
use mc_tick::block_entity::brewing::{self, Mix};

use crate::block_entities::Host;
use crate::context::ServerContext;
use crate::level::to_pos;
use crate::tick::lock;

pub use mc_tick::block_entity::brewing::{Mixes, Progress};

pub type Active = HashMap<(i32, i32, i32), Progress>;

/// The data pack's `minecraft:brewing` recipes, and the uses and speed of
/// every item with a `brewing_fuel` component.
pub fn load_mixes(data: &McData, items: &crate::items::Items) -> Mixes {
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
    let mut fuels = HashMap::new();
    for id in 0..items.count() as i32 {
        let Some(fuel) = items.default_component(id, "minecraft:brewing_fuel") else {
            continue;
        };
        fuels.insert(id, fuel_values(data, fuel));
    }
    Mixes::new(mixes, fuels)
}

/// `brewing_fuel` component's uses and speed multiplier.
pub(crate) fn fuel_values(data: &McData, v: &serde_json::Value) -> (i32, f32) {
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
#[cfg(test)]
fn accepts(
    ctx: &ServerContext,
    slots: &[Option<ItemStack>],
    slot: usize,
    stack: &ItemStack,
) -> bool {
    brewing::accepts(&Host::new(ctx), slots, slot, stack)
}

fn is_stand(ctx: &ServerContext, p: Position) -> bool {
    ctx.world
        .block(p, &ctx.data.blocks)
        .is_some_and(|s| brewing::is_stand(&ctx.data.blocks, s))
}

/// Progress of the stand at `p` as its window shows it.
pub fn progress(ctx: &ServerContext, p: Position) -> Progress {
    lock(&ctx.brewers)
        .get(&(p.x, p.y, p.z))
        .copied()
        .unwrap_or_else(|| brewing::stored(&Host::new(ctx), to_pos(p)))
}

/// The contents of a stand changed (or its chunk was read).
pub fn wake(ctx: &ServerContext, p: Position) {
    if is_stand(ctx, p) {
        lock(&ctx.brewers)
            .entry((p.x, p.y, p.z))
            .or_insert_with(|| brewing::stored(&Host::new(ctx), to_pos(p)));
    }
}

/// One tick of every stand with something to do.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<((i32, i32, i32), Progress)> =
        lock(&ctx.brewers).iter().map(|(&k, &v)| (k, v)).collect();
    let mut host = Host::new(ctx);
    for (key, before) in active {
        if !crate::tickets::blocks_run(ctx, key.0 >> 4, key.2 >> 4) {
            continue;
        }
        let p = BlockPos::new(key.0, key.1, key.2);
        let keep = brewing::tick(&mut host, &ctx.data.blocks, p, before);
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

/// What crafting or brewing with one `item` leaves behind (buckets and
/// bottles come back).
pub fn remainder(ctx: &ServerContext, item: i32) -> Option<ItemStack> {
    brewing::remainder(&Host::new(ctx), item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers;
    use crate::level::WorldLevel;
    use mc_nbt::{Compound, Tag};
    use mc_tick::block_entity::brewing::{BREW_TIME, potion_of};

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
