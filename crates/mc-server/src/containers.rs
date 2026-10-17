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
    match block {
        "minecraft:chest" => Some(kind("minecraft:chest", "Chest")),
        "minecraft:trapped_chest" => Some(kind("minecraft:trapped_chest", "Chest")),
        "minecraft:barrel" => Some(kind("minecraft:barrel", "Barrel")),
        _ => None,
    }
}

#[derive(Debug)]
pub struct Open {
    pub kind: Kind,
    pub slots: Vec<Option<ItemStack>>,
    pub components: HashMap<usize, Tag>,
    /// Players with it open.
    pub viewers: Vec<u128>,
}

pub type Containers = HashMap<(i32, i32, i32), Open>;

fn key(p: Position) -> (i32, i32, i32) {
    (p.x, p.y, p.z)
}

fn item_name(ctx: &ServerContext, item: i32) -> Option<String> {
    ctx.data
        .builtin
        .get("minecraft:item")?
        .iter()
        .find(|(_, id)| **id == item)
        .map(|(n, _)| n.clone())
}

/// Reads `Items` of a block entity into slots.
fn read_items(
    ctx: &ServerContext,
    data: &Compound,
    size: usize,
) -> (Vec<Option<ItemStack>>, HashMap<usize, Tag>) {
    let mut slots = vec![None; size];
    let mut components = HashMap::new();
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
            let count = match e.get("count") {
                Some(Tag::Int(c)) => *c,
                Some(Tag::Byte(c)) => i32::from(*c),
                _ => 1,
            };
            let slot = *slot as u8 as usize;
            if slot < size {
                let has = e.get("components").is_some();
                slots[slot] = Some(ItemStack {
                    count,
                    item,
                    has_component_changes: has,
                });
                if let Some(c) = e.get("components") {
                    components.insert(slot, c.clone());
                }
            }
        }
    }
    (slots, components)
}

fn write_items(ctx: &ServerContext, open: &Open) -> Tag {
    let mut list = Vec::new();
    for (i, slot) in open.slots.iter().enumerate() {
        let Some(stack) = slot else { continue };
        let Some(name) = item_name(ctx, stack.item) else {
            continue;
        };
        let mut e = Compound::new();
        e.insert("Slot", Tag::Byte(i as i8));
        e.insert("id", Tag::String(name));
        e.insert("count", Tag::Int(stack.count));
        if let Some(c) = open.components.get(&i) {
            e.insert("components", c.clone());
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
    be.data.insert("Items", write_items(ctx, open));
    ctx.world.set_block_entity(be, blocks);
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
        let (slots, components) = read_items(ctx, &data, kind.slots);
        Open {
            kind,
            slots,
            components,
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
) -> (Vec<u128>, Vec<(usize, Option<ItemStack>)>) {
    let mut all = ctx.containers.lock().expect("containers lock");
    let Some(open) = all.get_mut(&key(p)) else {
        return (Vec::new(), Vec::new());
    };
    let mut done = Vec::new();
    for (slot, stack) in changed {
        if *slot >= open.slots.len() {
            continue;
        }
        let new = stack.as_ref().map(|h| ItemStack {
            count: h.count,
            item: h.item,
            has_component_changes: h.has_component_changes,
        });
        if new.as_ref().map(|s| s.item) != open.slots[*slot].as_ref().map(|s| s.item) {
            open.components.remove(slot);
        }
        open.slots[*slot] = new.clone();
        done.push((*slot, new));
    }
    store(ctx, p, open);
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
            (read_items(ctx, &be.data, 54).0, Vec::new())
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
