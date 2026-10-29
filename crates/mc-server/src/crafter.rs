//! Crafters on the server: the rules are in `mc_tick::block_entity::crafter`;
//! this keeps the crafters showing `crafting`, and answers the window
//! (switching slots off, the result shown) and comparators (how many slots
//! are filled or switched off).

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::block_entity::crafter::{self, CRAFTING_TICKS};

use crate::block_entities::Host;
use crate::containers;
use crate::context::ServerContext;
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

pub use mc_tick::block_entity::crafter::ID;

/// Crafters showing `crafting`, with the ticks left.
pub type Active = HashMap<(i32, i32, i32), i32>;

pub fn kind() -> containers::Kind {
    containers::Kind {
        entity: ID,
        // Nine grid slots and the result, which is only shown.
        slots: 10,
        menu: "minecraft:crafter_3x3",
        title: "Crafter",
    }
}

fn is_crafter(ctx: &ServerContext, p: Position) -> bool {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        == Some(ID)
}

/// The switched-off slots of the crafter at `p`.
pub fn disabled(ctx: &ServerContext, p: Position) -> [bool; 9] {
    crafter::disabled(&Host::new(ctx), to_pos(p))
}

/// Switches slot `slot` on or off (only an empty one goes off).
pub fn set_slot_enabled(ctx: &ServerContext, p: Position, slot: usize, enabled: bool) -> bool {
    if slot >= 9 || !is_crafter(ctx, p) {
        return false;
    }
    if !enabled && containers::slots(ctx, p, kind())[slot].is_some() {
        return false;
    }
    let mut off = disabled(ctx, p);
    if off[slot] != enabled {
        return false;
    }
    off[slot] = !enabled;
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
    let list: Vec<i32> = (0..9).filter(|&i| off[i]).map(|i| i as i32).collect();
    be.data.insert("disabled_slots", Tag::IntArray(list));
    ctx.world.set_block_entity(be, blocks);
    lock(&ctx.container_updates).push(p);
    true
}

/// The window's properties: each slot's state (1 off), then `triggered`.
pub fn values(ctx: &ServerContext, p: Position) -> Vec<i16> {
    let mut out: Vec<i16> = disabled(ctx, p).iter().map(|&d| i16::from(d)).collect();
    let triggered = ctx
        .world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.property(s, "triggered"))
        == Some("true");
    out.push(i16::from(triggered));
    out
}

/// What the grid makes now (the window's result slot).
pub fn result(ctx: &ServerContext, p: Position) -> Option<ItemStack> {
    ctx.recipes
        .get()
        .craft(&containers::slots(ctx, p, kind())[..9], 3)
}

/// A comparator's reading: slots filled or switched off.
pub fn signal(ctx: &ServerContext, p: Position) -> u8 {
    let slots = containers::slots(ctx, p, kind());
    let off = disabled(ctx, p);
    (0..9).filter(|&i| slots[i].is_some() || off[i]).count() as u8
}

/// Whether a hopper may put `stack` into slot `slot`.
#[cfg(test)]
fn accepts(
    ctx: &ServerContext,
    p: Position,
    slots: &[Option<ItemStack>],
    slot: usize,
    stack: &ItemStack,
) -> bool {
    crafter::accepts(&Host::new(ctx), to_pos(p), slots, slot, stack)
}

/// The crafter at `p` was triggered.
pub fn craft(level: &mut WorldLevel, p: Position) {
    let ctx = level.context();
    let logic = level.logic();
    let crafted = crafter::craft(
        level,
        logic,
        &mut Host::new(ctx),
        &ctx.data.blocks,
        to_pos(p),
    );
    if crafted {
        lock(&ctx.crafters).insert((p.x, p.y, p.z), CRAFTING_TICKS);
    }
}

/// Crafters stop showing `crafting` 6 ticks after a craft.
pub fn tick(ctx: &ServerContext) {
    let done: Vec<(i32, i32, i32)> = {
        let mut all = lock(&ctx.crafters);
        for (key, t) in all.iter_mut() {
            if crate::tickets::blocks_run(ctx, key.0 >> 4, key.2 >> 4) {
                *t -= 1;
            }
        }
        let done = all
            .iter()
            .filter(|(_, t)| **t <= 0)
            .map(|(k, _)| *k)
            .collect();
        all.retain(|_, t| *t > 0);
        done
    };
    let mut host = Host::new(ctx);
    for key in done {
        let p = mc_tick::BlockPos::new(key.0, key.1, key.2);
        crafter::finish(&mut host, &ctx.data.blocks, p);
    }
}

/// A crafter read from disk while showing `crafting` stops soon.
pub fn wake(ctx: &ServerContext, p: Position) {
    let showing = ctx
        .world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.property(s, "crafting"))
        == Some("true");
    if showing && is_crafter(ctx, p) {
        lock(&ctx.crafters)
            .entry((p.x, p.y, p.z))
            .or_insert(CRAFTING_TICKS);
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

    fn item(ctx: &ServerContext, name: &str, count: i32) -> ItemStack {
        ItemStack::new(ctx.items.id(name).unwrap(), count)
    }

    fn place(ctx: &ServerContext, p: Position, orientation: &str) {
        let s = ctx
            .data
            .blocks
            .state_id(
                ID,
                &[
                    ("orientation", orientation),
                    ("crafting", "false"),
                    ("triggered", "false"),
                ],
            )
            .unwrap();
        WorldLevel::new(ctx).set_block(p, s);
    }

    #[test]
    fn crafts_into_a_chest_and_onto_the_ground() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        place(&ctx, p, "east_up");
        // Nothing to craft: no change.
        craft(&mut WorldLevel::new(&ctx), p);
        assert!(lock(&ctx.crafters).is_empty());
        // Two oak planks above each other make sticks.
        containers::put(
            &ctx,
            p,
            kind(),
            &[
                (1, Some(item(&ctx, "minecraft:oak_planks", 2))),
                (4, Some(item(&ctx, "minecraft:oak_planks", 1))),
            ],
        );
        assert_eq!(signal(&ctx, p), 2);
        assert_eq!(
            result(&ctx, p).map(|s| (ctx.items.name(s.item).map(str::to_owned), s.count)),
            Some((Some("minecraft:stick".to_owned()), 4))
        );
        craft(&mut WorldLevel::new(&ctx), p);
        let slots = containers::slots(&ctx, p, kind());
        assert_eq!(slots[1].as_ref().map(|s| s.count), Some(1));
        assert!(slots[4].is_none());
        let state = ctx.world.block(p, &ctx.data.blocks).unwrap();
        assert_eq!(ctx.data.blocks.property(state, "crafting"), Some("true"));
        let sticks = ctx.items.id("minecraft:stick").unwrap();
        let on_ground: i32 = lock(&ctx.entities)
            .iter()
            .filter_map(|e| match &e.kind {
                mc_entity::Kind::Item { item, count, .. } if *item == sticks => Some(*count),
                _ => None,
            })
            .sum();
        assert_eq!(on_ground, 4);
        for _ in 0..6 {
            tick(&ctx);
        }
        let state = ctx.world.block(p, &ctx.data.blocks).unwrap();
        assert_eq!(ctx.data.blocks.property(state, "crafting"), Some("false"));

        // With a chest in front the sticks go in.
        let chest = ctx.data.blocks.default_state("minecraft:chest").unwrap();
        WorldLevel::new(&ctx).set_block(Position::new(1, -60, 0), chest);
        containers::put(
            &ctx,
            p,
            kind(),
            &[(4, Some(item(&ctx, "minecraft:oak_planks", 1)))],
        );
        craft(&mut WorldLevel::new(&ctx), p);
        let chest_slots = containers::slots(
            &ctx,
            Position::new(1, -60, 0),
            containers::kind_of("minecraft:chest").unwrap(),
        );
        assert_eq!(chest_slots[0].as_ref().map(|s| s.count), Some(4));
    }

    #[test]
    fn disabled_slots_and_even_spreading() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        place(&ctx, p, "north_up");
        assert!(set_slot_enabled(&ctx, p, 0, false));
        assert!(disabled(&ctx, p)[0]);
        assert_eq!(signal(&ctx, p), 1);
        assert_eq!(values(&ctx, p)[0], 1);
        let slots = containers::slots(&ctx, p, kind());
        let dirt = item(&ctx, "minecraft:dirt", 1);
        assert!(!accepts(&ctx, p, &slots, 0, &dirt));
        assert!(accepts(&ctx, p, &slots, 1, &dirt));
        // A filled slot waits until the later ones catch up.
        containers::put(&ctx, p, kind(), &[(1, Some(dirt.clone()))]);
        let slots = containers::slots(&ctx, p, kind());
        assert!(!accepts(&ctx, p, &slots, 1, &dirt));
        assert!(accepts(&ctx, p, &slots, 2, &dirt));
        // A filled slot cannot be switched off.
        assert!(!set_slot_enabled(&ctx, p, 1, false));
        assert!(set_slot_enabled(&ctx, p, 0, true));
        assert_eq!(signal(&ctx, p), 1);
    }
}
