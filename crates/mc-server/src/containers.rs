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
    pub components: HashMap<usize, Tag>,
    /// Players with it open.
    pub viewers: Vec<u128>,
}

pub type Containers = HashMap<(i32, i32, i32), Open>;

fn key(p: Position) -> (i32, i32, i32) {
    (p.x, p.y, p.z)
}

/// Containers that are entities (chest minecarts...) are keyed like blocks,
/// at a place no block can be: `x` is `i32::MIN`, `y` the entity id.
pub fn entity_key(id: i32) -> Position {
    Position::new(i32::MIN, id, 0)
}

/// The entity id behind a key from [`entity_key`].
pub fn entity_of(p: Position) -> Option<i32> {
    (p.x == i32::MIN).then_some(p.y)
}

/// What an entity holding items opens.
pub fn entity_kind(name: &str) -> Option<Kind> {
    let chest = |title| Kind {
        entity: "",
        slots: 27,
        menu: "minecraft:generic_9x3",
        title,
    };
    Some(match name {
        "minecraft:chest_minecart" => chest("Minecart with Chest"),
        "minecraft:hopper_minecart" => Kind {
            entity: "",
            slots: 5,
            menu: "minecraft:hopper",
            title: "Minecart with Hopper",
        },
        "minecraft:oak_chest_boat" => chest("Oak Boat with Chest"),
        "minecraft:spruce_chest_boat" => chest("Spruce Boat with Chest"),
        "minecraft:birch_chest_boat" => chest("Birch Boat with Chest"),
        "minecraft:jungle_chest_boat" => chest("Jungle Boat with Chest"),
        "minecraft:acacia_chest_boat" => chest("Acacia Boat with Chest"),
        "minecraft:cherry_chest_boat" => chest("Cherry Boat with Chest"),
        "minecraft:dark_oak_chest_boat" => chest("Dark Oak Boat with Chest"),
        "minecraft:pale_oak_chest_boat" => chest("Pale Oak Boat with Chest"),
        "minecraft:mangrove_chest_boat" => chest("Mangrove Boat with Chest"),
        "minecraft:bamboo_chest_raft" => chest("Bamboo Raft with Chest"),
        _ => return None,
    })
}

/// The stored data (`Items`) of the container at `p`: a block entity, or an
/// entity's contents.
fn stored_data(ctx: &ServerContext, p: Position) -> Option<Compound> {
    match entity_of(p) {
        Some(id) => ctx
            .entity_items
            .lock()
            .expect("entity items lock")
            .get(&id)
            .cloned(),
        None => ctx
            .world
            .block_entity(p, &ctx.data.blocks)
            .map(|be| be.data),
    }
}

/// The entity `id` holding items is gone: its items (to drop), and who had
/// it open.
pub fn entity_gone(ctx: &ServerContext, id: i32) -> (Vec<ItemStack>, Vec<u128>) {
    let p = entity_key(id);
    let mut all = ctx.containers.lock().expect("containers lock");
    let data = ctx
        .entity_items
        .lock()
        .expect("entity items lock")
        .remove(&id);
    let (slots, viewers) = match all.remove(&key(p)) {
        Some(open) => (open.slots, open.viewers),
        None => match data {
            Some(data) => (read_items(ctx, &data, 27).0, Vec::new()),
            None => return (Vec::new(), Vec::new()),
        },
    };
    (slots.into_iter().flatten().collect(), viewers)
}

pub fn item_name(ctx: &ServerContext, item: i32) -> Option<String> {
    ctx.items.name(item).map(str::to_owned)
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
                let (damage, rest) = crate::items::split_damage(e.get("components"));
                slots[slot] = Some(ItemStack {
                    count,
                    item,
                    damage,
                    has_component_changes: rest.is_some(),
                });
                if let Some(c) = rest {
                    components.insert(slot, c);
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
        if let Some(c) = crate::items::join_damage(stack.damage, open.components.get(&i)) {
            e.insert("components", c);
        }
        list.push(Tag::Compound(e));
    }
    Tag::List(list)
}

/// Writes the slots back to the block entity.
fn store(ctx: &ServerContext, p: Position, open: &Open) {
    if let Some(id) = entity_of(p) {
        ctx.entity_items
            .lock()
            .expect("entity items lock")
            .entry(id)
            .or_default()
            .insert("Items", write_items(ctx, open));
        return;
    }
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
    Some(fullness(ctx, &slots))
}

/// The comparator reading of these slots (see [`signal`]).
pub fn fullness(ctx: &ServerContext, slots: &[Option<ItemStack>]) -> u8 {
    let total = slots.len().max(1);
    let fullness: f64 = slots
        .iter()
        .flatten()
        .map(|s| f64::from(s.count) / f64::from(ctx.items.max_stack(s.item).max(1)))
        .sum::<f64>()
        / total as f64;
    if slots.iter().all(Option::is_none) {
        0
    } else {
        (1.0 + fullness * 14.0).floor() as u8
    }
}

/// The slots of the container at `p` as they are now.
pub fn slots(ctx: &ServerContext, p: Position, kind: Kind) -> Vec<Option<ItemStack>> {
    if let Some(open) = ctx.containers.lock().expect("containers lock").get(&key(p)) {
        return open.slots.clone();
    }
    match stored_data(ctx, p) {
        Some(data) => read_items(ctx, &data, kind.slots).0,
        None => vec![None; kind.slots],
    }
}

/// Changes slots of the container at `p` (commands, furnaces): stored, seen
/// by whoever has it open, and comparators are told.
pub fn put(ctx: &ServerContext, p: Position, kind: Kind, changes: &[(usize, Option<ItemStack>)]) {
    let full: Vec<_> = changes.iter().map(|(i, s)| (*i, s.clone(), None)).collect();
    put_with(ctx, p, kind, &full);
}

/// The component data of the container's stacks, by slot.
pub fn components(ctx: &ServerContext, p: Position, kind: Kind) -> HashMap<usize, Tag> {
    if let Some(open) = ctx.containers.lock().expect("containers lock").get(&key(p)) {
        return open.components.clone();
    }
    match stored_data(ctx, p) {
        Some(data) => read_items(ctx, &data, kind.slots).1,
        None => HashMap::new(),
    }
}

/// Like [`put`], with component data: `Some` replaces a slot's (an empty
/// compound removes it), `None` keeps it while the item stays the same.
pub fn put_with(
    ctx: &ServerContext,
    p: Position,
    kind: Kind,
    changes: &[(usize, Option<ItemStack>, Option<Tag>)],
) {
    let mut all = ctx.containers.lock().expect("containers lock");
    let open = all.entry(key(p)).or_insert_with(|| {
        let (slots, components) = match stored_data(ctx, p) {
            Some(data) => read_items(ctx, &data, kind.slots),
            None => (vec![None; kind.slots], HashMap::new()),
        };
        Open {
            kind,
            slots,
            components,
            viewers: Vec::new(),
        }
    });
    for (slot, stack, comps) in changes {
        if *slot < open.slots.len() {
            if open.slots[*slot].as_ref().map(|s| s.item) != stack.as_ref().map(|s| s.item) {
                open.components.remove(slot);
            }
            match comps {
                Some(Tag::Compound(c)) if !c.is_empty() => {
                    open.components.insert(*slot, Tag::Compound(c.clone()));
                }
                Some(_) => {
                    open.components.remove(slot);
                }
                None => {}
            }
            open.slots[*slot] = stack.clone();
        }
    }
    let changes: Vec<(usize, Option<ItemStack>)> =
        changes.iter().map(|(i, s, _)| (*i, s.clone())).collect();
    let changes = &changes[..];
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
        let data = stored_data(ctx, p).unwrap_or_default();
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
    let slots = open.slots.clone();
    drop(all);
    // Trapped chests give power by how many look in.
    ctx.container_updates
        .lock()
        .expect("container updates lock")
        .push(p);
    slots
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
        if new.as_ref().map(|s| s.item) != open.slots[*slot].as_ref().map(|s| s.item) {
            open.components.remove(slot);
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
        drop(all);
        ctx.container_updates
            .lock()
            .expect("container updates lock")
            .push(p);
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
