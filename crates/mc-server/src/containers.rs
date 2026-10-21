//! Blocks that hold items (chests, barrels): their windows and contents.
//!
//! An open container's slots live here, shared by everyone who has it open,
//! and are written back to the block entity's `Items` (`{Slot, id, count,
//! components}`, as in chunk files) on every change.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::{HashedStack, ItemStack};

use crate::context::ServerContext;

/// What a container block opens.
#[derive(Clone, Copy, Debug)]
pub struct Kind {
    /// Block entity id.
    pub entity: &'static str,
    pub slots: usize,
    /// `minecraft:menu` entry.
    pub menu: &'static str,
    pub title: &'static str,
}

pub fn kind_of(block: &str) -> Option<Kind> {
    let kind = |entity, title| Kind {
        entity,
        slots: 27,
        menu: "minecraft:generic_9x3",
        title,
    };
    let other = |entity, slots, menu, title| Kind {
        entity,
        slots,
        menu,
        title,
    };
    match block {
        "minecraft:chest" => Some(kind("minecraft:chest", "Chest")),
        "minecraft:trapped_chest" => Some(kind("minecraft:trapped_chest", "Chest")),
        "minecraft:barrel" => Some(kind("minecraft:barrel", "Barrel")),
        "minecraft:furnace" => Some(other(
            "minecraft:furnace",
            3,
            "minecraft:furnace",
            "Furnace",
        )),
        "minecraft:blast_furnace" => Some(other(
            "minecraft:blast_furnace",
            3,
            "minecraft:blast_furnace",
            "Blast Furnace",
        )),
        "minecraft:smoker" => Some(other("minecraft:smoker", 3, "minecraft:smoker", "Smoker")),
        "minecraft:dispenser" => Some(other(
            "minecraft:dispenser",
            9,
            "minecraft:generic_3x3",
            "Dispenser",
        )),
        "minecraft:dropper" => Some(other(
            "minecraft:dropper",
            9,
            "minecraft:generic_3x3",
            "Dropper",
        )),
        "minecraft:brewing_stand" => Some(other(
            "minecraft:brewing_stand",
            5,
            "minecraft:brewing_stand",
            "Brewing Stand",
        )),
        "minecraft:hopper" => Some(other("minecraft:hopper", 5, "minecraft:hopper", "Hopper")),
        b if b.ends_with("shulker_box") => Some(other(
            "minecraft:shulker_box",
            27,
            "minecraft:shulker_box",
            "Shulker Box",
        )),
        _ => None,
    }
}

#[derive(Debug)]
pub struct Open {
    pub kind: Kind,
    pub slots: Vec<Option<ItemStack>>,
    /// Players with it open.
    pub viewers: Vec<u128>,
}

pub type Containers = HashMap<(i32, i32, i32), Open>;

fn key(p: Position) -> (i32, i32, i32) {
    (p.x, p.y, p.z)
}

pub fn item_name(ctx: &ServerContext, item: i32) -> Option<String> {
    ctx.items.name(item).map(str::to_owned)
}

/// Reads `Items` of a block entity into slots.
pub fn read_items(ctx: &ServerContext, data: &Compound, size: usize) -> Vec<Option<ItemStack>> {
    let mut slots = vec![None; size];
    let items = ctx.data.builtin.get("minecraft:item");
    if let Some(Tag::List(list)) = data.get("Items") {
        for entry in list {
            let Tag::Compound(e) = entry else { continue };
            let (Some(Tag::Byte(slot)), Some(Tag::String(id))) = (e.get("Slot"), e.get("id"))
            else {
                continue;
            };
            let Some(&item) = items.and_then(|m| m.get(id)) else {
                continue;
            };
            let slot = *slot as u8 as usize;
            if slot < size {
                slots[slot] = Some(ItemStack::from_nbt_with(e, item));
            }
        }
    }
    slots
}

/// The `Items` list of `slots`, as chunk files keep it.
pub fn write_items(ctx: &ServerContext, slots: &[Option<ItemStack>]) -> Tag {
    let mut list = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        let Some(stack) = slot else { continue };
        let Some(name) = item_name(ctx, stack.item) else {
            continue;
        };
        let mut e = Compound::new();
        e.insert("Slot", Tag::Byte(i as i8));
        for (k, v) in stack.to_nbt_with(&name).iter() {
            e.insert(k, v.clone());
        }
        list.push(Tag::Compound(e));
    }
    Tag::List(list)
}

/// Writes the slots back to the block entity.
fn store(ctx: &ServerContext, p: Position, open: &Open) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: open.kind.entity.to_owned(),
            data: Compound::new(),
        });
    be.data.insert("Items", write_items(ctx, &open.slots));
    ctx.world.set_block_entity(be, blocks);
}

/// How full the container at `p` is, as a comparator reads it
/// (minecraft.wiki, "Redstone Comparator"): 0 when empty, else 1 plus 14
/// times the fullness (each stack's share of its stack size, averaged over
/// the slots), rounded down. `None` if there is no container there.
pub fn signal(ctx: &ServerContext, p: Position) -> Option<u8> {
    let state = ctx.world.block(p, &ctx.data.blocks)?;
    let kind = kind_of(ctx.data.blocks.block_name(state)?)?;
    let mut slots = self::slots(ctx, p, kind);
    // Both halves of a large chest count.
    if let Some(d) = ctx.shapes.chest_partner(state) {
        let (dx, dy, dz) = d.offset();
        let other = Position::new(p.x + dx, p.y + dy, p.z + dz);
        let partner = ctx.world.block(other, &ctx.data.blocks);
        if partner.is_some_and(|s| ctx.shapes.chest_partner(s) == Some(d.opposite())) {
            slots.extend(self::slots(ctx, other, kind));
        }
    }
    let total = slots.len();
    let fullness: f64 = slots
        .iter()
        .flatten()
        .map(|s| f64::from(s.count) / f64::from(ctx.items.max_stack(s.item).max(1)))
        .sum::<f64>()
        / total as f64;
    Some(if slots.iter().all(Option::is_none) {
        0
    } else {
        (1.0 + fullness * 14.0).floor() as u8
    })
}

/// The slots of the container at `p` as they are now.
pub fn slots(ctx: &ServerContext, p: Position, kind: Kind) -> Vec<Option<ItemStack>> {
    if let Some(open) = ctx.containers.lock().expect("containers lock").get(&key(p)) {
        return open.slots.clone();
    }
    match ctx.world.block_entity(p, &ctx.data.blocks) {
        Some(be) => read_items(ctx, &be.data, kind.slots),
        None => vec![None; kind.slots],
    }
}

/// Changes slots of the container at `p` (commands, furnaces): stored, seen
/// by whoever has it open, and comparators are told.
pub fn put(ctx: &ServerContext, p: Position, kind: Kind, changes: &[(usize, Option<ItemStack>)]) {
    let mut all = ctx.containers.lock().expect("containers lock");
    let open = all.entry(key(p)).or_insert_with(|| {
        let slots = match ctx.world.block_entity(p, &ctx.data.blocks) {
            Some(be) => read_items(ctx, &be.data, kind.slots),
            None => vec![None; kind.slots],
        };
        Open {
            kind,
            slots,
            viewers: Vec::new(),
        }
    });
    for (slot, stack) in changes {
        if *slot < open.slots.len() {
            open.slots[*slot] = stack.clone();
        }
    }
    store(ctx, p, open);
    let viewers = open.viewers.clone();
    if open.viewers.is_empty() {
        all.remove(&key(p));
    }
    drop(all);
    ctx.container_updates
        .lock()
        .expect("container updates lock")
        .push(p);
    if !viewers.is_empty() {
        let _ = ctx.events.send(crate::context::Event::ContainerSlots {
            position: p,
            viewers,
            slots: changes.to_vec(),
        });
    }
}

/// A player opens the container at `p`: its kind and current slots.
pub fn open(ctx: &ServerContext, uuid: u128, p: Position, kind: Kind) -> Vec<Option<ItemStack>> {
    let mut all = ctx.containers.lock().expect("containers lock");
    let open = all.entry(key(p)).or_insert_with(|| {
        let data = ctx
            .world
            .block_entity(p, &ctx.data.blocks)
            .map(|be| be.data)
            .unwrap_or_default();
        let slots = read_items(ctx, &data, kind.slots);
        Open {
            kind,
            slots,
            viewers: Vec::new(),
        }
    });
    if !open.viewers.contains(&uuid) {
        open.viewers.push(uuid);
    }
    open.slots.clone()
}

/// Applies the client's view of changed container slots (0..size). Returns
/// the other viewers to tell, with the slots that changed.
pub fn change(
    ctx: &ServerContext,
    uuid: u128,
    p: Position,
    changed: &[(usize, Option<HashedStack>)],
    known: &[ItemStack],
) -> (Vec<u128>, Vec<(usize, Option<ItemStack>)>) {
    let mut all = ctx.containers.lock().expect("containers lock");
    let Some(open) = all.get_mut(&key(p)) else {
        return (Vec::new(), Vec::new());
    };
    let before: Vec<ItemStack> = open
        .slots
        .iter()
        .flatten()
        .filter(|s| s.has_changes())
        .chain(known)
        .cloned()
        .collect();
    let mut done = Vec::new();
    for (slot, stack) in changed {
        if *slot >= open.slots.len() {
            continue;
        }
        let new = stack.as_ref().map(|h| h.resolve(&before));
        // Taking from a furnace's output pays the experience it earned.
        if *slot == 2
            && open.slots[2].as_ref().map_or(0, |s| s.count) > new.as_ref().map_or(0, |s| s.count)
            && crate::furnaces::is_furnace(ctx, p)
        {
            let points = crate::furnaces::collect_xp(ctx, p);
            if points > 0 {
                crate::xp::award(ctx, uuid, points, true);
            }
        }
        open.slots[*slot] = new.clone();
        done.push((*slot, new));
    }
    store(ctx, p, open);
    ctx.container_updates
        .lock()
        .expect("container updates lock")
        .push(p);
    let others = open
        .viewers
        .iter()
        .copied()
        .filter(|&v| v != uuid)
        .collect();
    (others, done)
}

/// A player closed the container at `p`.
pub fn close(ctx: &ServerContext, uuid: u128, p: Position) {
    let mut all = ctx.containers.lock().expect("containers lock");
    if let Some(open) = all.get_mut(&key(p)) {
        open.viewers.retain(|&v| v != uuid);
        if open.viewers.is_empty() {
            store(ctx, p, open);
            all.remove(&key(p));
        }
    }
}

/// The block at `p` was broken: its items, and who had it open.
pub fn broken(ctx: &ServerContext, p: Position) -> (Vec<ItemStack>, Vec<u128>) {
    let mut all = ctx.containers.lock().expect("containers lock");
    let (slots, viewers) = match all.remove(&key(p)) {
        Some(open) => (open.slots, open.viewers),
        None => {
            let Some(be) = ctx.world.block_entity(p, &ctx.data.blocks) else {
                return (Vec::new(), Vec::new());
            };
            (read_items(ctx, &be.data, 54), Vec::new())
        }
    };
    (slots.into_iter().flatten().collect(), viewers)
}

/// A player left: they no longer have anything open.
pub fn player_left(ctx: &ServerContext, uuid: u128) {
    let open: Vec<(i32, i32, i32)> = ctx
        .containers
        .lock()
        .expect("containers lock")
        .iter()
        .filter(|(_, o)| o.viewers.contains(&uuid))
        .map(|(k, _)| *k)
        .collect();
    for (x, y, z) in open {
        close(ctx, uuid, Position::new(x, y, z));
    }
}

/// A container block was placed at `p`: give it its empty block entity.
pub fn placed(ctx: &ServerContext, p: Position, state: u32) {
    let blocks = &ctx.data.blocks;
    let Some(kind) = blocks.block_name(state).and_then(kind_of) else {
        return;
    };
    if kind.entity == "minecraft:hopper" {
        crate::hoppers::register(ctx, p);
    }
    if ctx
        .world
        .block_entity(p, blocks)
        .is_some_and(|be| be.id == kind.entity)
    {
        return;
    }
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: kind.entity.to_owned(),
            data: Compound::new(),
        },
        blocks,
    );
}
