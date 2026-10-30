//! Crafters (minecraft.wiki: "Crafter").
//!
//! Nine slots that craft like a crafting table's grid when the block is
//! triggered by redstone ([`crate::hopper`]): the result goes into the
//! container it faces, or out onto the ground with world events 1049 and
//! 2010; the leftovers (empty buckets...) follow it; each filled slot
//! loses one item. With no recipe it plays world event 1050 instead. The
//! block shows `crafting` for [`CRAFTING_TICKS`] ticks after a craft.
//!
//! Slots can be switched off from the window (`disabled_slots` in the
//! block entity): they take nothing. Hoppers spread what they bring over
//! the slots, each item going to the first slot that has no emptier or
//! smaller same stack after it.

use mc_data::Blocks as Table;
use mc_nbt::Tag;

use super::jukebox::Jukeboxes;
use super::transfer::{self, Containers};
use super::{Blocks, Effects, ItemStack, Items, Recipes};
use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

pub const ID: &str = "minecraft:crafter";
/// Ticks a crafter shows `crafting` after it crafted.
pub const CRAFTING_TICKS: i32 = 6;

/// The switched-off slots of the crafter at `p`.
pub fn disabled(host: &impl Blocks, p: BlockPos) -> [bool; 9] {
    let mut out = [false; 9];
    if let Some(data) = host.block_entity(p)
        && let Some(Tag::IntArray(list)) = data.get("disabled_slots")
    {
        for &i in list {
            if let Some(o) = usize::try_from(i).ok().and_then(|i| out.get_mut(i)) {
                *o = true;
            }
        }
    }
    out
}

/// Whether a hopper may put `stack` into slot `slot` of the crafter at `p`
/// holding `slots` (vanilla's even spreading: not while a later slot is
/// emptier).
pub fn accepts<H: Blocks + Items>(
    host: &H,
    p: BlockPos,
    slots: &[Option<ItemStack>],
    slot: usize,
    stack: &ItemStack,
) -> bool {
    let off = disabled(host, p);
    if slot >= 9 || off[slot] {
        return false;
    }
    let Some(have) = &slots[slot] else {
        return true;
    };
    if !have.same_kind(stack) || have.count >= host.max_stack(have.item) {
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
fn front(blocks: &Table, state: u32) -> Dir {
    blocks
        .property(state, "orientation")
        .and_then(|o| o.split('_').next())
        .and_then(Dir::from_name)
        .unwrap_or(Dir::North)
}

/// The crafter at `p` was triggered: it crafts what its grid holds.
/// Returns whether it crafted (and so shows `crafting` for a while).
pub fn craft<H>(level: &mut dyn Level, ctx: Ctx, host: &mut H, blocks: &Table, p: BlockPos) -> bool
where
    H: Containers + Effects + Recipes + Jukeboxes,
{
    let Some(state) = host.block(p) else {
        return false;
    };
    let Some(store) = host.block_store(p) else {
        return false;
    };
    let mut slots = host.slots(&store);
    let Some((made, recipe)) = host.craft(&slots[..9]) else {
        host.level_event(p, 1050, 0);
        return false;
    };
    // Players about the crafter saw it made.
    let used: Vec<ItemStack> = slots[..9].iter().flatten().cloned().collect();
    let centre = [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ];
    host.crafter_crafted(centre, 17.0, &recipe, &used);
    if blocks.property(state, "crafting") != Some("true") {
        let new = ctx.shapes.with(state, "crafting", "true");
        set_block(level, ctx, p, new, Flags::ALL);
    }
    let facing = front(blocks, state);
    let leftovers: Vec<ItemStack> = slots
        .iter()
        .flatten()
        .filter_map(|s| super::brewing::remainder(host, s.item))
        .collect();
    let mut changes = Vec::new();
    for (i, slot) in slots.iter_mut().enumerate() {
        if let Some(s) = slot.take() {
            *slot = super::less_one(&s);
            changes.push((i, slot.clone()));
        }
    }
    host.put(&store, &changes);
    out(host, blocks, p, facing, made);
    for left in leftovers {
        out(host, blocks, p, facing, left);
    }
    true
}

/// One crafted stack leaves the crafter: into the container in front, the
/// rest onto the ground.
fn out<H>(host: &mut H, blocks: &Table, p: BlockPos, facing: Dir, stack: ItemStack)
where
    H: Containers + Effects + Recipes + Jukeboxes,
{
    let (dx, dy, dz) = facing.offset();
    let front = p.offset(facing);
    let mut left = stack.count;
    while left > 0 && transfer::insert_at(host, blocks, front, facing.opposite(), &stack) {
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
    host.spawn_stack(pos, &stack.with_count(left));
    host.level_event(p, 1049, 0);
    host.level_event(p, 2010, dir_data(facing));
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

/// The crafter at `p` has shown `crafting` long enough: it stops.
pub fn finish(host: &mut (impl Blocks + Effects), blocks: &Table, p: BlockPos) {
    let Some(state) = host.block(p) else {
        return;
    };
    if blocks.block_name(state) == Some(ID)
        && blocks.property(state, "crafting") == Some("true")
        && let Some(new) = blocks.with_property(state, "crafting", "false")
    {
        host.set_block(p, new);
    }
}
