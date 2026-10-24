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

/// Players with an ender chest's menu open, by block. The chest holds
/// nothing itself (the items are the player's); only the lid is shared.
pub type EnderChests = HashMap<(i32, i32, i32), Vec<u128>>;

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
            Some(data) => (read_items(ctx, &data, 27), Vec::new()),
            None => return (Vec::new(), Vec::new()),
        },
    };
    (slots.into_iter().flatten().collect(), viewers)
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
    if let Some(id) = entity_of(p) {
        ctx.entity_items
            .lock()
            .expect("entity items lock")
            .entry(id)
            .or_default()
            .insert("Items", write_items(ctx, &open.slots));
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
        Some(data) => read_items(ctx, &data, kind.slots),
        None => vec![None; kind.slots],
    }
}

/// Changes slots of the container at `p` (commands, furnaces): stored, seen
/// by whoever has it open, and comparators are told.
pub fn put(ctx: &ServerContext, p: Position, kind: Kind, changes: &[(usize, Option<ItemStack>)]) {
    let mut all = ctx.containers.lock().expect("containers lock");
    let open = all.entry(key(p)).or_insert_with(|| {
        let slots = match stored_data(ctx, p) {
            Some(data) => read_items(ctx, &data, kind.slots),
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

/// A generated chest (or minecart, barrel...) still holds only the name of
/// its loot table: roll it into the slots, the first time anyone looks
/// (minecraft.wiki, "Loot table" → "Containers"). Stacks are split to fill
/// empty slots and spread over them at random.
fn unpack_loot(ctx: &ServerContext, p: Position, size: usize) {
    let Some(mut data) = stored_data(ctx, p) else {
        return;
    };
    let Some(Tag::String(table)) = data.remove("LootTable") else {
        return;
    };
    data.remove("LootTableSeed");
    let mut slots = read_items(ctx, &data, size);
    if let Some(table) = ctx.loot.table(&table).cloned() {
        let cx = crate::loot::LootCtx {
            origin: Some([
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ]),
            ..crate::loot::LootCtx::default()
        };
        let env = crate::commands::loot::Env(ctx);
        let rolled: Vec<ItemStack> = ctx
            .loot
            .roll(&env, &table, &cx)
            .iter()
            .map(crate::loot::Drop::stack)
            .collect();
        fill(&mut slots, rolled, &mut || crate::util::random_u64());
    }
    data.insert("Items", write_items(ctx, &slots));
    match entity_of(p) {
        Some(id) => {
            ctx.entity_items
                .lock()
                .expect("entity items lock")
                .insert(id, data);
        }
        None => {
            let blocks = &ctx.data.blocks;
            if let Some(mut be) = ctx.world.block_entity(p, blocks) {
                be.data = data;
                ctx.world.set_block_entity(be, blocks);
            }
        }
    }
}

/// Puts rolled loot in the empty slots of `slots`: stacks are halved at
/// random while there are more empty slots than stacks, then each goes to
/// a random empty slot. What does not fit is lost, as in vanilla.
fn fill(slots: &mut [Option<ItemStack>], rolled: Vec<ItemStack>, rng: &mut dyn FnMut() -> u64) {
    let mut empty: Vec<usize> = (0..slots.len()).filter(|&i| slots[i].is_none()).collect();
    for i in (1..empty.len()).rev() {
        empty.swap(i, (rng() % (i as u64 + 1)) as usize);
    }
    let (mut splittable, mut done): (Vec<ItemStack>, Vec<ItemStack>) = rolled
        .into_iter()
        .filter(|s| s.count > 0)
        .partition(|s| s.count > 1);
    while !splittable.is_empty() && done.len() + splittable.len() < empty.len() {
        let at = (rng() % splittable.len() as u64) as usize;
        let mut stack = splittable.swap_remove(at);
        let half = stack.count / 2;
        let part = 1 + (rng() % half as u64) as i32;
        let mut piece = stack.clone();
        piece.count = part;
        stack.count -= part;
        for s in [stack, piece] {
            if s.count > 1 && rng().is_multiple_of(2) {
                splittable.push(s);
            } else {
                done.push(s);
            }
        }
    }
    done.extend(splittable);
    for i in (1..done.len()).rev() {
        done.swap(i, (rng() % (i as u64 + 1)) as usize);
    }
    for (slot, stack) in empty.into_iter().zip(done) {
        slots[slot] = Some(stack);
    }
}

/// Lids show how many look in: chests and shulker boxes by a block event
/// (both halves of a double chest), barrels by their `open` state, with the
/// sound when the first opens it and the last closes it.
fn lid(ctx: &ServerContext, p: Position, viewers: usize) {
    if entity_of(p).is_some() {
        return;
    }
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return;
    };
    let Some(name) = blocks.block_name(state) else {
        return;
    };
    let edge = viewers <= 1;
    let opening = viewers > 0;
    let centre = [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ];
    let pitch = || (crate::util::random_u64() >> 40) as f32 / (1u64 << 24) as f32 * 0.1 + 0.9;
    if name == "minecraft:barrel" {
        let want = if opening { "true" } else { "false" };
        if blocks.property(state, "open") != Some(want) {
            let new = ctx.shapes.with(state, "open", want);
            let mut level = crate::level::WorldLevel::new(ctx);
            level.set_block(p, new);
            let changes = level.into_changes();
            let _ = ctx
                .events
                .send(crate::context::Event::BlocksChanged { by: 0, changes });
            let sound = if opening {
                "block.barrel.open"
            } else {
                "block.barrel.close"
            };
            crate::fx::sound(
                ctx,
                sound,
                crate::fx::category::BLOCKS,
                centre,
                0.5,
                pitch(),
            );
        }
        return;
    }
    let chest = matches!(name, "minecraft:chest" | "minecraft:trapped_chest");
    let shulker = name.ends_with("shulker_box");
    if !chest && !shulker {
        return;
    }
    let block = ctx.data.builtin.id("minecraft:block", name).unwrap_or(0);
    let mut at = vec![p];
    if chest && let Some(other) = chest_partner(ctx, p, state) {
        at.push(other);
    }
    for &q in &at {
        let _ = ctx.events.send(crate::context::Event::BlockAction {
            position: q,
            action: 1,
            param: viewers.min(255) as u8,
            block,
        });
    }
    if edge {
        let mid = if at.len() == 2 {
            [
                (f64::from(at[0].x) + f64::from(at[1].x)) / 2.0 + 0.5,
                f64::from(p.y) + 0.5,
                (f64::from(at[0].z) + f64::from(at[1].z)) / 2.0 + 0.5,
            ]
        } else {
            centre
        };
        let sound = match (shulker, opening) {
            (true, true) => "block.shulker_box.open",
            (true, false) => "block.shulker_box.close",
            (false, true) => "block.chest.open",
            (false, false) => "block.chest.close",
        };
        crate::fx::sound(ctx, sound, crate::fx::category::BLOCKS, mid, 0.5, pitch());
    }
}

/// The other half of a double chest: clockwise of `facing` for the left
/// half, anticlockwise for the right.
fn chest_partner(ctx: &ServerContext, p: Position, state: u32) -> Option<Position> {
    let blocks = &ctx.data.blocks;
    let (dx, dz) = match blocks.property(state, "facing")? {
        "north" => (1, 0),
        "east" => (0, 1),
        "south" => (-1, 0),
        "west" => (0, -1),
        _ => return None,
    };
    let (dx, dz) = match blocks.property(state, "type")? {
        "left" => (dx, dz),
        "right" => (-dx, -dz),
        _ => return None,
    };
    Some(Position::new(p.x + dx, p.y, p.z + dz))
}

/// A player opens the container at `p`: its kind and current slots.
pub fn open(ctx: &ServerContext, uuid: u128, p: Position, kind: Kind) -> Vec<Option<ItemStack>> {
    unpack_loot(ctx, p, kind.slots);
    let mut all = ctx.containers.lock().expect("containers lock");
    let open = all.entry(key(p)).or_insert_with(|| {
        let data = stored_data(ctx, p).unwrap_or_default();
        let slots = read_items(ctx, &data, kind.slots);
        Open {
            kind,
            slots,
            viewers: Vec::new(),
        }
    });
    let first = !open.viewers.contains(&uuid);
    if first {
        open.viewers.push(uuid);
    }
    let viewers = open.viewers.len();
    let slots = open.slots.clone();
    drop(all);
    if first {
        lid(ctx, p, viewers);
    }
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
        let before = open.viewers.len();
        open.viewers.retain(|&v| v != uuid);
        let viewers = open.viewers.len();
        if open.viewers.is_empty() {
            store(ctx, p, open);
            all.remove(&key(p));
        }
        drop(all);
        if viewers != before {
            lid(ctx, p, viewers);
        }
        ctx.container_updates
            .lock()
            .expect("container updates lock")
            .push(p);
    }
}

/// The block at `p` was broken: its items, and who had it open.
pub fn broken(ctx: &ServerContext, p: Position) -> (Vec<ItemStack>, Vec<u128>) {
    unpack_loot(ctx, p, 54);
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
    // The menu may have been opened in another dimension than the one they
    // leave from.
    let dims: Vec<_> = ctx
        .dims
        .iter()
        .filter_map(|cell| cell.lock().expect("dimensions lock").clone())
        .collect();
    let mut covered = false;
    for dim in &dims {
        if std::ptr::eq(dim.as_ref(), ctx) {
            covered = true;
        }
        ender_drop(dim, uuid);
    }
    if !covered {
        ender_drop(ctx, uuid);
    }
}

fn ender_drop(ctx: &ServerContext, uuid: u128) {
    let open: Vec<(i32, i32, i32)> = ctx
        .ender_chests
        .lock()
        .expect("ender chests lock")
        .iter()
        .filter(|(_, viewers)| viewers.contains(&uuid))
        .map(|(k, _)| *k)
        .collect();
    for (x, y, z) in open {
        ender_close(ctx, uuid, Position::new(x, y, z));
    }
}

/// A player opened the ender chest menu at `p`.
pub fn ender_open(ctx: &ServerContext, uuid: u128, p: Position) {
    if ctx
        .world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        != Some("minecraft:ender_chest")
    {
        return;
    }
    let mut all = ctx.ender_chests.lock().expect("ender chests lock");
    let viewers = all.entry(key(p)).or_default();
    if viewers.contains(&uuid) {
        return;
    }
    viewers.push(uuid);
    let n = viewers.len();
    drop(all);
    ender_lid(ctx, p, n, true);
}

/// A player closed the ender chest menu at `p`.
pub fn ender_close(ctx: &ServerContext, uuid: u128, p: Position) {
    let mut all = ctx.ender_chests.lock().expect("ender chests lock");
    let Some(viewers) = all.get_mut(&key(p)) else {
        return;
    };
    let before = viewers.len();
    viewers.retain(|&v| v != uuid);
    let n = viewers.len();
    if n == 0 {
        all.remove(&key(p));
    }
    drop(all);
    if n != before {
        ender_lid(ctx, p, n, false);
    }
}

/// The ender chest at `p` is gone: nobody is looking any more, and windows
/// on it close. The block may already be air.
pub fn ender_broken(ctx: &ServerContext, p: Position) {
    let had = {
        let mut all = ctx.ender_chests.lock().expect("ender chests lock");
        all.remove(&key(p)).is_some_and(|v| !v.is_empty())
    };
    if had {
        ender_lid(ctx, p, 0, false);
    }
    let _ = ctx
        .events
        .send(crate::context::Event::ContainerGone { position: p });
}

/// The lid shows how many look in (a block event), with the sound when the
/// first opens it and the last closes it. `added` is a player joining.
fn ender_lid(ctx: &ServerContext, p: Position, viewers: usize, added: bool) {
    let block = ctx
        .data
        .builtin
        .id("minecraft:block", "minecraft:ender_chest")
        .unwrap_or(0);
    let _ = ctx.events.send(crate::context::Event::BlockAction {
        position: p,
        action: 1,
        param: viewers.min(255) as u8,
        block,
    });
    if !((added && viewers == 1) || (!added && viewers == 0)) {
        return;
    }
    let sound = if added {
        "block.ender_chest.open"
    } else {
        "block.ender_chest.close"
    };
    let pitch = (crate::util::random_u64() >> 40) as f32 / (1u64 << 24) as f32 * 0.1 + 0.9;
    crate::fx::sound(
        ctx,
        sound,
        crate::fx::category::BLOCKS,
        [
            f64::from(p.x) + 0.5,
            f64::from(p.y) + 0.5,
            f64::from(p.z) + 0.5,
        ],
        0.5,
        pitch,
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loot_is_split_and_spread_over_empty_slots() {
        let mut seed = 1u64;
        let mut rng = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 16
        };
        let mut slots = vec![None; 27];
        slots[0] = Some(ItemStack::new(5, 1));
        fill(
            &mut slots,
            vec![ItemStack::new(1, 20), ItemStack::new(2, 1)],
            &mut rng,
        );
        let total: i32 = slots
            .iter()
            .flatten()
            .filter(|s| s.item == 1)
            .map(|s| s.count)
            .sum();
        assert_eq!(total, 20, "nothing is lost or made");
        assert!(
            slots.iter().flatten().filter(|s| s.item == 1).count() > 1,
            "the 20 are split"
        );
        assert_eq!(
            slots[0].as_ref().map(|s| s.item),
            Some(5),
            "full slots are kept"
        );
        assert_eq!(slots.iter().flatten().filter(|s| s.item == 2).count(), 1);
    }

    #[test]
    fn a_generated_chest_is_filled_once_and_its_lid_opens() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Ok(data) = mc_data::McData::load(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let ctx = ServerContext::new(crate::Config::new(dir), data).unwrap();
        let p = Position::new(0, -60, 0);
        let chest = ctx.data.blocks.default_state("minecraft:chest").unwrap();
        crate::level::WorldLevel::new(&ctx).set_block(p, chest);
        let mut nbt = Compound::new();
        nbt.insert(
            "LootTable",
            Tag::String("minecraft:chests/simple_dungeon".into()),
        );
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: "minecraft:chest".into(),
                data: nbt,
            },
            &ctx.data.blocks,
        );
        let mut events = ctx.events.subscribe();
        let kind = kind_of("minecraft:chest").unwrap();
        let first = open(&ctx, 1, p, kind);
        assert!(first.iter().any(Option::is_some), "the dungeon loot is in");
        let be = ctx.world.block_entity(p, &ctx.data.blocks).unwrap();
        assert!(be.data.get("LootTable").is_none(), "rolled only once");
        let mut lid = None;
        while let Some(e) = events.try_changed() {
            if let crate::context::Event::BlockAction {
                action: 1, param, ..
            } = e
            {
                lid = Some(param);
            }
        }
        assert_eq!(lid, Some(1));
        close(&ctx, 1, p);
        assert_eq!(
            open(&ctx, 2, p, kind),
            first,
            "the same items the next time"
        );
    }

    /// Lid counts, ender-chest sounds, and whether the menu was told to close.
    fn ender_lid_events(
        events: &mut crate::context::EventReceiver,
        block: i32,
    ) -> (Vec<u8>, Vec<String>, bool) {
        let mut params = Vec::new();
        let mut sounds = Vec::new();
        let mut gone = false;
        while let Some(e) = events.try_changed() {
            match e {
                crate::context::Event::BlockAction {
                    action: 1,
                    param,
                    block: id,
                    position,
                } => {
                    assert_eq!(id, block);
                    assert_eq!(position, Position::new(0, -60, 0));
                    params.push(param);
                }
                crate::context::Event::Fx(crate::fx::Fx::Sound(s))
                    if s.name.starts_with("minecraft:block.ender_chest.") =>
                {
                    assert_eq!(s.volume, 0.5);
                    assert!(
                        (0.9..1.0).contains(&s.pitch),
                        "pitch {} is outside 0.9..1.0",
                        s.pitch
                    );
                    assert_eq!((s.x, s.y, s.z), (0.5, -59.5, 0.5));
                    sounds.push(s.name);
                }
                crate::context::Event::ContainerGone { position }
                    if position == Position::new(0, -60, 0) =>
                {
                    gone = true;
                }
                _ => {}
            }
        }
        (params, sounds, gone)
    }

    #[test]
    fn an_ender_chest_lid_opens_and_closes_with_its_viewers() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Ok(data) = mc_data::McData::load(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let ctx = ServerContext::new(crate::Config::new(dir), data).unwrap();
        let p = Position::new(0, -60, 0);
        let state = ctx
            .data
            .blocks
            .default_state("minecraft:ender_chest")
            .unwrap();
        crate::level::WorldLevel::new(&ctx).set_block(p, state);
        let block = ctx
            .data
            .builtin
            .id("minecraft:block", "minecraft:ender_chest")
            .unwrap();
        let mut events = ctx.events.subscribe();
        ender_open(&ctx, 1, p);
        let (params, sounds, _) = ender_lid_events(&mut events, block);
        assert_eq!(params, vec![1]);
        assert_eq!(sounds, ["minecraft:block.ender_chest.open"]);
        ender_open(&ctx, 2, p);
        let (params, sounds, _) = ender_lid_events(&mut events, block);
        assert_eq!(params, vec![2]);
        assert!(sounds.is_empty(), "only the first open plays");
        ender_close(&ctx, 1, p);
        let (params, sounds, _) = ender_lid_events(&mut events, block);
        assert_eq!(params, vec![1], "one viewer still looks in");
        assert!(sounds.is_empty(), "it stays open");
        player_left(&ctx, 2);
        let (params, sounds, _) = ender_lid_events(&mut events, block);
        assert_eq!(params, vec![0]);
        assert_eq!(sounds, ["minecraft:block.ender_chest.close"]);
        ender_open(&ctx, 1, p);
        let _ = ender_lid_events(&mut events, block);
        crate::level::WorldLevel::new(&ctx).set_block(p, ctx.shapes.air());
        let (params, sounds, gone) = ender_lid_events(&mut events, block);
        assert_eq!(params, vec![0], "breaking it shuts the lid");
        assert_eq!(sounds, ["minecraft:block.ender_chest.close"]);
        assert!(gone, "the open menu is closed");
    }
}
