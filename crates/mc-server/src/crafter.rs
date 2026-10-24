//! Crafters (minecraft.wiki: "Crafter").
//!
//! Nine slots that craft like a crafting table's grid when the block is
//! triggered by redstone (`mc_tick::hopper`): the result goes into the
//! container it faces, or out onto the ground with world events 1049 and
//! 2010; the leftovers (empty buckets...) follow it; each filled slot
//! loses one item. With no recipe it plays world event 1050 instead. The
//! block shows `crafting` for 6 ticks after a craft.
//!
//! Slots can be switched off from the window (`disabled_slots` in the
//! block entity): they take nothing. Hoppers spread what they bring over
//! the slots, each item going to the first slot that has no emptier or
//! smaller same stack after it. A comparator reads how many slots are
//! filled or switched off.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::Dir;

use crate::containers;
use crate::context::{Event, ServerContext};
use crate::tick::lock;

/// Crafters showing `crafting`, with the ticks left.
pub type Active = HashMap<(i32, i32, i32), i32>;

pub const ID: &str = "minecraft:crafter";
const CRAFTING_TICKS: i32 = 6;

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
    let mut out = [false; 9];
    if let Some(be) = ctx.world.block_entity(p, &ctx.data.blocks)
        && let Some(Tag::IntArray(list)) = be.data.get("disabled_slots")
    {
        for &i in list {
            if let Some(o) = usize::try_from(i).ok().and_then(|i| out.get_mut(i)) {
                *o = true;
            }
        }
    }
    out
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
        .craft(&containers::slots(ctx, p, kind())[..9], 3)
}

/// A comparator's reading: slots filled or switched off.
pub fn signal(ctx: &ServerContext, p: Position) -> u8 {
    let slots = containers::slots(ctx, p, kind());
    let off = disabled(ctx, p);
    (0..9).filter(|&i| slots[i].is_some() || off[i]).count() as u8
}

/// Whether a hopper may put `stack` into slot `slot` (vanilla's even
/// spreading: not while a later slot is emptier).
pub fn accepts(
    ctx: &ServerContext,
    p: Position,
    slots: &[Option<ItemStack>],
    slot: usize,
    stack: &ItemStack,
) -> bool {
    let off = disabled(ctx, p);
    if slot >= 9 || off[slot] {
        return false;
    }
    let Some(have) = &slots[slot] else {
        return true;
    };
    if !have.same_kind(stack) || have.count >= ctx.items.max_stack(have.item) {
        return false;
    }
    !(slot + 1..9).any(|j| {
        !off[j]
            && match &slots[j] {
                None => true,
                Some(s) => s.count < have.count && s.same_kind(have),
            }
    })
}

/// The way the crafter faces (`orientation` is front then top).
fn front(ctx: &ServerContext, state: u32) -> Dir {
    ctx.data
        .blocks
        .property(state, "orientation")
        .and_then(|o| o.split('_').next())
        .and_then(Dir::from_name)
        .unwrap_or(Dir::North)
}

/// The crafter at `p` was triggered.
pub fn craft(level: &mut crate::level::WorldLevel, p: Position) {
    let ctx = level.context();
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return;
    };
    let at = [f64::from(p.x), f64::from(p.y), f64::from(p.z)];
    let mut slots = containers::slots(ctx, p, kind());
    let Some(made) = ctx.recipes.craft(&slots[..9], 3) else {
        crate::fx::level_event(ctx, 1050, at, 0);
        return;
    };
    lock(&ctx.crafters).insert((p.x, p.y, p.z), CRAFTING_TICKS);
    if blocks.property(state, "crafting") != Some("true") {
        level.set_block(p, ctx.shapes.with(state, "crafting", "true"));
    }
    let facing = front(ctx, state);
    let leftovers: Vec<ItemStack> = slots
        .iter()
        .flatten()
        .filter_map(|s| crate::brewing::remainder(ctx, s.item))
        .collect();
    let mut changes = Vec::new();
    for (i, slot) in slots.iter_mut().enumerate() {
        if let Some(s) = slot.take() {
            *slot = (s.count > 1).then(|| s.with_count(s.count - 1));
            changes.push((i, slot.clone()));
        }
    }
    containers::put(ctx, p, kind(), &changes);
    out(ctx, p, facing, made);
    for left in leftovers {
        out(ctx, p, facing, left);
    }
}

/// One crafted stack leaves the crafter: into the container in front, the
/// rest onto the ground.
fn out(ctx: &ServerContext, p: Position, facing: Dir, stack: ItemStack) {
    let (dx, dy, dz) = facing.offset();
    let front = Position::new(p.x + dx, p.y + dy, p.z + dz);
    let mut left = stack.count;
    while left > 0 && crate::hoppers::insert_at(ctx, front, facing.opposite(), &stack) {
        left -= 1;
    }
    if left == 0 {
        return;
    }
    let pos = [
        f64::from(p.x) + 0.5 + f64::from(dx) * 0.7,
        f64::from(p.y) + 0.5 + f64::from(dy) * 0.7 - if dy == 0 { 0.15625 } else { 0.125 },
        f64::from(p.z) + 0.5 + f64::from(dz) * 0.7,
    ];
    crate::tick::spawn_stack_at(ctx, pos, &stack.with_count(left));
    let at = [f64::from(p.x), f64::from(p.y), f64::from(p.z)];
    crate::fx::level_event(ctx, 1049, at, 0);
    crate::fx::level_event(ctx, 2010, at, dir_data(facing));
}

/// A direction's 3D data value (down, up, north, south, west, east).
fn dir_data(d: Dir) -> i32 {
    match d {
        Dir::Down => 0,
        Dir::Up => 1,
        Dir::North => 2,
        Dir::South => 3,
        Dir::West => 4,
        Dir::East => 5,
    }
}

/// Crafters stop showing `crafting` 6 ticks after a craft.
pub fn tick(ctx: &ServerContext) {
    let done: Vec<(i32, i32, i32)> = {
        let mut all = lock(&ctx.crafters);
        all.values_mut().for_each(|t| *t -= 1);
        let done = all
            .iter()
            .filter(|(_, t)| **t <= 0)
            .map(|(k, _)| *k)
            .collect();
        all.retain(|_, t| *t > 0);
        done
    };
    for key in done {
        let p = Position::new(key.0, key.1, key.2);
        let Some(state) = ctx.world.block(p, &ctx.data.blocks) else {
            continue;
        };
        if ctx.data.blocks.block_name(state) == Some(ID)
            && ctx.data.blocks.property(state, "crafting") == Some("true")
        {
            let mut level = crate::level::WorldLevel::new(ctx);
            level.set_block(p, ctx.shapes.with(state, "crafting", "false"));
            let changes = level.into_changes();
            let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
        }
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
