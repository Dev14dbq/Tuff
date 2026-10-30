//! Items moving between containers one at a time (minecraft.wiki:
//! "Hopper", "Minecart with Hopper", "Dropper", "Dispenser"): which slots
//! each face of a container reaches, what may go in, hoppers picking items
//! up off the ground, droppers and dispensers firing, and the special
//! partners (jukeboxes, composters) that are not containers at all.
//!
//! The block side of hoppers (locking, cooldowns, the push-then-pull turn)
//! is [`crate::hopper`]; it calls [`container_transfer`] and [`pickup`]
//! through its `Level`.

use mc_data::Blocks as Table;
use mc_nbt::Tag;

use super::jukebox::{self, Jukeboxes};
use super::{Blocks, Effects, ItemStack, Items, Recipes, Slots, less_one};
use crate::dir::{BlockPos, Dir};
use crate::hopper::HOPPER_COOLDOWN;
use crate::level::Level;
use crate::updates::Ctx;

/// What kind of thing holds the items, for which slots each face reaches
/// (vanilla `WorldlyContainer.getSlotsForFace`) and what goes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// An entity (a chest minecart...): every slot from every side.
    Entity,
    Hopper,
    EnderChest,
    Crafter,
    BrewingStand,
    /// Furnaces, blast furnaces and smokers.
    Furnace,
    /// Any other container block: every slot from every side.
    Other,
}

/// Something holding items at a block's space: the block's container, or an
/// entity with items whose box is in that space (vanilla
/// `HopperBlockEntity.getContainerAt`). `kind` is the host's own handle.
#[derive(Clone, Copy, Debug)]
pub struct Store<K> {
    /// The block's position, or the host's key for an entity.
    pub at: BlockPos,
    pub kind: K,
    pub holder: Holder,
}

/// Containers in the world, read and written whole (vanilla `Container`).
/// Hoppers look at slots in place ([`Containers::with_slots`]) and change
/// one at a time.
pub trait Containers: Items + Blocks {
    /// The host's handle for a container's layout.
    type Kind: Copy;
    /// What holds items at the space `p`: its block, or else one of the
    /// item-holding entities there, chosen at random as vanilla does.
    fn store_at(&mut self, p: BlockPos) -> Option<Store<Self::Kind>>;
    /// [`Containers::store_at`] where the block at `p` is known to be
    /// `state` (`None`: not loaded).
    fn store_in(&mut self, p: BlockPos, state: Option<u32>) -> Option<Store<Self::Kind>> {
        let _ = state;
        self.store_at(p)
    }
    /// The container block at `p` only.
    fn block_store(&self, p: BlockPos) -> Option<Store<Self::Kind>>;
    fn slots(&self, store: &Store<Self::Kind>) -> Slots;
    /// Looks at the slots without copying them. `f` must not reach back
    /// into the host's containers.
    fn with_slots(&self, store: &Store<Self::Kind>, f: &mut dyn FnMut(&[Option<ItemStack>])) {
        f(&self.slots(store));
    }
    /// Changes some slots: stored, shown to whoever has it open, and
    /// comparators are told.
    fn put(&mut self, store: &Store<Self::Kind>, changes: &[(usize, Option<ItemStack>)]);
}

/// Item entities on the ground, as hoppers see them (vanilla
/// `HopperBlockEntity.getItemsAtAndAbove`).
pub trait ItemEntities {
    /// The item entities still in the world whose position is in the box
    /// from `min` (inclusive) to `max` (exclusive), in the world's order:
    /// each one's id and stack. Empty (and so not allocated) when there are
    /// none, which is the common case.
    fn items_in(&self, min: [f64; 3], max: [f64; 3]) -> Vec<(i32, ItemStack)>;
    /// `count` of the item entity `id` were picked up: all of it is gone, or
    /// the rest may be picked up at once.
    fn take_items(&mut self, id: i32, count: i32);
}

/// The slots a neighbour may push into through the face `side` of the
/// container (in the order they are tried), or pull out of.
fn sided_slots(holder: Holder, size: usize, side: Dir, inserting: bool) -> Faces {
    match holder {
        Holder::EnderChest => Faces::Some(&[]),
        // Nothing comes out of a crafter, and the tenth slot is only shown.
        Holder::Crafter if !inserting => Faces::Some(&[]),
        Holder::Crafter => Faces::All(9),
        // The ingredient through the top, bottles and fuel through the
        // sides, bottles and the ingredient through the bottom.
        Holder::BrewingStand => match side {
            Dir::Up => Faces::Some(&[3]),
            Dir::Down => Faces::Some(&[0, 1, 2, 3]),
            _ => Faces::Some(&[0, 1, 2, 4]),
        },
        Holder::Furnace => {
            if inserting {
                match side {
                    Dir::Up => Faces::Some(&[0]),
                    Dir::Down => Faces::Some(&[]),
                    _ => Faces::Some(&[1]),
                }
            } else if side == Dir::Down {
                Faces::Some(&[2, 1])
            } else {
                Faces::Some(&[])
            }
        }
        _ => Faces::All(size),
    }
}

/// The slots one face of a container reaches (see [`sided_slots`]).
#[derive(Clone, Copy, Debug)]
enum Faces {
    /// Slots 0 up to this.
    All(usize),
    Some(&'static [usize]),
}

impl Faces {
    fn iter(self) -> impl Iterator<Item = usize> {
        let (all, some) = match self {
            Faces::All(n) => (0..n, &[][..]),
            Faces::Some(list) => (0..0, list),
        };
        all.chain(some.iter().copied())
    }
}

/// The state at `p` if it is the block `name`.
fn is_block(host: &impl Blocks, blocks: &Table, p: BlockPos, name: &str) -> Option<u32> {
    host.block(p)
        .filter(|&s| blocks.block_name(s) == Some(name))
}

/// What a hopper's push or pull does (`Level::container_transfer`): one
/// item from `from` (out through `from_side`) into `to` (in through
/// `to_side`). Composters first, then jukeboxes, then containers.
#[allow(clippy::too_many_arguments)]
pub fn container_transfer<H>(
    level: &mut dyn Level,
    ctx: Ctx,
    host: &mut H,
    blocks: &Table,
    from: BlockPos,
    from_side: Dir,
    to: BlockPos,
    to_side: Dir,
) -> bool
where
    H: Containers + Effects + Recipes + Jukeboxes,
{
    // Mostly neither end is a composter or a jukebox: each block is read
    // once to know.
    let special = |s: Option<u32>| {
        s.and_then(|s| blocks.block_name(s))
            .is_some_and(|n| n == "minecraft:composter" || n == jukebox::ID)
    };
    let (to_state, from_state) = (host.block(to), host.block(from));
    if !special(to_state) && !special(from_state) {
        let (Some(src), Some(dst)) = (host.store_in(from, from_state), host.store_in(to, to_state))
        else {
            return false;
        };
        return move_one(host, src, from_side, dst, to_side);
    }
    if let Some(done) = composter_transfer(level, ctx, host, blocks, from, from_side, to, to_side) {
        return done;
    }
    transfer(host, blocks, from, from_side, to, to_side)
}

/// Moves one item between whatever holds items at `from` and at `to`.
fn move_between<H>(host: &mut H, from: BlockPos, from_side: Dir, to: BlockPos, to_side: Dir) -> bool
where
    H: Containers + Recipes,
{
    let (Some(src), Some(dst)) = (host.store_at(from), host.store_at(to)) else {
        return false;
    };
    move_one(host, src, from_side, dst, to_side)
}

/// Moves one item from whatever holds items at `from` into whatever holds
/// them at `to` (a jukebox takes and gives discs).
pub fn transfer<H>(
    host: &mut H,
    blocks: &Table,
    from: BlockPos,
    from_side: Dir,
    to: BlockPos,
    to_side: Dir,
) -> bool
where
    H: Containers + Effects + Recipes + Jukeboxes,
{
    if let Some(done) = jukebox_transfer(host, blocks, from, from_side, to, to_side) {
        return done;
    }
    move_between(host, from, from_side, to, to_side)
}

/// Hoppers and jukeboxes: a hopper pointing into an empty one puts a disc
/// in, and one under it takes the disc out. `None` if neither end is a
/// jukebox.
fn jukebox_transfer<H>(
    host: &mut H,
    blocks: &Table,
    from: BlockPos,
    from_side: Dir,
    to: BlockPos,
    to_side: Dir,
) -> Option<bool>
where
    H: Containers + Effects + Recipes + Jukeboxes,
{
    if is_block(host, blocks, to, jukebox::ID).is_some() {
        let Some(src) = host.store_at(from) else {
            return Some(false);
        };
        let slots = host.slots(&src);
        let pick = sided_slots(src.holder, slots.len(), from_side, false)
            .iter()
            .find_map(|i| {
                let s = slots[i].clone()?;
                let name = host.name(s.item)?;
                host.song(name).map(|_| (i, s))
            });
        let Some((slot, stack)) = pick else {
            return Some(false);
        };
        if !jukebox::insert(host, blocks, to, &stack) {
            return Some(false);
        }
        host.put(&src, &[(slot, less_one(&stack))]);
        return Some(true);
    }
    if is_block(host, blocks, from, jukebox::ID).is_some() {
        // Out of the bottom only, and the signal keeps hoppers still while
        // a song plays.
        if from_side != Dir::Down || host.playing(from).is_some() {
            return Some(false);
        }
        let (Some(dst), Some(disc)) = (host.store_at(to), jukebox::record(host, from)) else {
            return Some(false);
        };
        let Some(item) = host.id(&disc) else {
            return Some(false);
        };
        if !insert_one(host, dst, to_side, &ItemStack::new(item, 1)) {
            return Some(false);
        }
        jukebox::take(host, blocks, from);
        return Some(true);
    }
    None
}

/// Moves one item from `src` (out through `from_side`) into `dst` (in
/// through `to_side`).
fn move_one<H: Containers + Recipes>(
    host: &mut H,
    src: Store<H::Kind>,
    from_side: Dir,
    dst: Store<H::Kind>,
    to_side: Dir,
) -> bool {
    // The slots that may give an item, in the order they are tried (at most
    // 64: no container is larger).
    let mut ready = 0u64;
    let reader = &*host;
    reader.with_slots(&src, &mut |slots| {
        for slot in sided_slots(src.holder, slots.len(), from_side, false).iter() {
            let Some(stack) = &slots[slot] else { continue };
            // Only an empty bottle comes out of a brewing stand's ingredient
            // slot.
            if src.holder == Holder::BrewingStand
                && slot == 3
                && reader.name(stack.item) != Some("minecraft:glass_bottle")
            {
                continue;
            }
            if slot < 64 {
                ready |= 1 << slot;
            }
        }
    });
    let order = sided_slots(src.holder, 64, from_side, false);
    order
        .iter()
        .filter(|&slot| slot < 64 && ready & (1 << slot) != 0)
        .any(|slot| transfer_from_slot(host, &src, slot, dst, to_side))
}

/// The rest a hopper still has (its block entity's `TransferCooldown`).
pub fn cooldown(host: &impl Blocks, p: BlockPos) -> i32 {
    let mut ticks = 0;
    host.read_block_entity(p, &mut |data| {
        if let Some(Tag::Int(v)) = data.get("TransferCooldown") {
            ticks = *v;
        }
    });
    ticks
}

/// One tick of the hopper's rest at `p` goes by (see
/// `Level::count_down_hopper`); what it was.
pub fn count_down(host: &mut impl Blocks, p: BlockPos) -> i32 {
    let mut was = 0;
    host.change_block_entity(p, &mut |data| match data.get_mut("TransferCooldown") {
        Some(Tag::Int(v)) => {
            was = *v;
            if *v > 0 {
                *v -= 1;
                true
            } else {
                false
            }
        }
        _ => false,
    });
    was
}

pub fn set_cooldown(host: &mut impl Blocks, p: BlockPos, ticks: i32) {
    host.edit_block_entity(p, Some("minecraft:hopper"), &mut |data| {
        data.insert("TransferCooldown", Tag::Int(ticks));
    });
}

/// The hopper at `p` takes one item (a whole stack's worth, one at a time)
/// from an item entity in the space above it.
pub fn pickup<H: Containers + ItemEntities>(host: &mut H, p: BlockPos) -> bool {
    let Some(store) = host.block_store(p) else {
        return false;
    };
    let (x, y, z) = (f64::from(p.x), f64::from(p.y), f64::from(p.z));
    pickup_into(host, &store, [x, y + 0.6, z], [x + 1.0, y + 2.0, z + 1.0])
}

/// Takes one item entity whose position is in the box from `min` to `max`
/// into `store`.
fn pickup_into<H: Containers + ItemEntities>(
    host: &mut H,
    store: &Store<H::Kind>,
    min: [f64; 3],
    max: [f64; 3],
) -> bool {
    let found = host.items_in(min, max);
    if found.is_empty() {
        return false;
    }
    // Nothing changes until one is taken, so the slots are read once.
    let mut slots = host.slots(store);
    for (id, stack) in found {
        let max = host.max_stack(stack.item);
        let room = (0..slots.len())
            .find(|&i| {
                slots[i]
                    .as_ref()
                    .is_some_and(|s| s.same_kind(&stack) && s.count < max)
            })
            .or_else(|| (0..slots.len()).find(|&i| slots[i].is_none()));
        let Some(i) = room else { continue };
        let had = slots[i].as_ref().map_or(0, |s| s.count);
        let add = stack.count.min(max - had);
        slots[i] = Some(stack.with_count(had + add));
        host.put(store, &[(i, slots[i].clone())]);
        host.take_items(id, add);
        return true;
    }
    false
}

/// One game tick of a hopper minecart that is on, at `pos`, holding items
/// as `cart` (minecraft.wiki, "Minecart with Hopper"): it pulls one item
/// from the container above it, or else picks up an item entity around it,
/// every tick (it does not rest like a hopper).
pub fn tick_cart<H>(host: &mut H, cart: Store<H::Kind>, pos: [f64; 3])
where
    H: Containers + ItemEntities + Recipes,
{
    // The container above: its bottom 0.5 (exclusive) to 1.5 above the
    // cart's bottom.
    let above = BlockPos::new(
        pos[0].floor() as i32,
        (pos[1] + 0.5 + 1.0).floor() as i32,
        pos[2].floor() as i32,
    );
    // With a container there it only pulls; else it picks items up.
    if let Some(src) = host.block_store(above) {
        move_one(host, src, Dir::Down, cart, Dir::Up);
    } else {
        let (x, y, z) = (pos[0], pos[1] + 0.5, pos[2]);
        pickup_into(
            host,
            &cart,
            [x - 0.5, y - 0.5, z - 0.5],
            [x + 0.5, y + 1.0, z + 0.5],
        );
    }
}

/// What a dispenser does with an item (vanilla `DispenseItemBehavior`).
#[derive(Debug)]
pub enum Behavior {
    /// It acted: one item is used up (or worn, for tools) and `leftover`
    /// (an empty bucket, a full bottle) comes back, with its components.
    Done {
        leftover: Option<ItemStack>,
        wear: i32,
    },
    /// It tried and could not: nothing is dispensed, the item stays.
    Failed,
    /// No special behaviour: the item is dropped.
    Default,
}

/// [`Behavior::Done`] leaving one of `leftover` (an item id) behind.
pub fn done(leftover: Option<i32>) -> Behavior {
    Behavior::Done {
        leftover: leftover.map(|i| ItemStack::new(i, 1)),
        wear: 0,
    }
}

/// The behaviours of dispensers for particular items (buckets, bone meal,
/// TNT, armour...): the host's registry of `DispenseItemBehavior`s.
pub trait Dispensing {
    /// What the dispenser at `p` facing `facing` does with `stack`, an item
    /// called `name`.
    fn behavior(&mut self, name: &str, stack: &ItemStack, p: BlockPos, facing: Dir) -> Behavior;
    /// The world event an item that acted makes (a launch for projectiles);
    /// `None` for none.
    fn sound(&self, name: &str) -> Option<i32>;
}

/// A dropper or dispenser fires: a random stocked slot gives one item,
/// which goes into the container in front (droppers) or out onto the
/// ground. A dispenser acts on the item first where `behaviors` has
/// something for it, with a click and smoke.
pub fn dispense<H>(
    host: &mut H,
    behaviors: &mut impl Dispensing,
    p: BlockPos,
    facing: Dir,
    dropper: bool,
) where
    H: Containers + Effects + Recipes,
{
    let Some(src) = host.block_store(p) else {
        return;
    };
    let slots = host.slots(&src);
    let filled: Vec<usize> = (0..slots.len()).filter(|&i| slots[i].is_some()).collect();
    if filled.is_empty() {
        // Empty: a higher click.
        host.level_event(p, 1001, 0);
        return;
    }
    let slot = filled[(host.random() % filled.len() as u64) as usize];
    let Some(stack) = slots[slot].clone() else {
        return;
    };
    let (dx, dy, dz) = facing.offset();
    let front = p.offset(facing);
    if dropper && let Some(dst) = host.store_at(front) {
        // An empty hopper handed an item rests 8 ticks.
        let empty_hopper =
            dst.holder == Holder::Hopper && host.slots(&dst).iter().all(Option::is_none);
        if transfer_from_slot(host, &src, slot, dst, facing.opposite()) && empty_hopper {
            set_cooldown(host, front, HOPPER_COOLDOWN);
        }
        return;
    }
    let name = host.name(stack.item).unwrap_or("").to_owned();
    let action = if dropper || name.is_empty() {
        Behavior::Default
    } else {
        behaviors.behavior(&name, &stack, p, facing)
    };
    // The click (a launch for projectiles, a failing click when it could
    // not act) and smoke out of its face.
    let sound = match action {
        Behavior::Failed => Some(1001),
        Behavior::Default => Some(1000),
        Behavior::Done { .. } => behaviors.sound(&name),
    };
    if let Some(event) = sound {
        host.level_event(p, event, 0);
    }
    host.level_event(p, 2000, facing as i32);
    match action {
        Behavior::Failed => {}
        Behavior::Done { leftover, wear } => {
            let max = host.max_damage(stack.item);
            let worn = wear > 0 && max > 0;
            let left = if worn {
                // A tool is worn, not used up; it breaks at full damage.
                let damage = stack.damage + wear;
                (damage < max).then(|| ItemStack {
                    damage,
                    ..stack.clone()
                })
            } else {
                less_one(&stack)
            };
            let emptied = left.is_none();
            host.put(&src, &[(slot, left)]);
            if let Some(back) = leftover {
                // An emptied slot takes the leftover itself.
                let placed = if emptied {
                    host.put(&src, &[(slot, Some(back.clone()))]);
                    true
                } else {
                    give_back(host, &src, &back)
                };
                if !placed {
                    host.spawn_stack(drop_point(p, dx, dy, dz), &back);
                }
            }
        }
        Behavior::Default => {
            host.put(&src, &[(slot, less_one(&stack))]);
            host.spawn_stack(drop_point(p, dx, dy, dz), &stack.with_count(1));
        }
    }
}

/// Puts a returned item (an empty bucket...) into the container, merging
/// or in a free slot. Returns whether it fit.
fn give_back<H: Containers>(host: &mut H, store: &Store<H::Kind>, back: &ItemStack) -> bool {
    let slots = host.slots(store);
    let max = host.max_stack(back.item);
    let merge = (0..slots.len()).find(|&i| {
        slots[i]
            .as_ref()
            .is_some_and(|s| s.same_kind(back) && s.count < max)
    });
    let target = merge.or_else(|| (0..slots.len()).find(|&i| slots[i].is_none()));
    let Some(i) = target else { return false };
    let stack = back.with_count(slots[i].as_ref().map_or(0, |s| s.count) + back.count);
    host.put(store, &[(i, Some(stack))]);
    true
}

/// Where a dispensed item appears: out of the face, a little lower when
/// shot sideways.
fn drop_point(p: BlockPos, dx: i32, dy: i32, dz: i32) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5 + f64::from(dx) * 0.7,
        f64::from(p.y) + 0.5 + f64::from(dy) * 0.7 - if dy == 0 { 0.125 } else { 0.0 },
        f64::from(p.z) + 0.5 + f64::from(dz) * 0.7,
    ]
}

/// Moves one item out of a particular slot of `src` (a dropper facing a
/// container).
fn transfer_from_slot<H: Containers + Recipes>(
    host: &mut H,
    src: &Store<H::Kind>,
    slot: usize,
    dst: Store<H::Kind>,
    side: Dir,
) -> bool {
    let mut stack = None;
    host.with_slots(src, &mut |slots| stack = slots.get(slot).cloned().flatten());
    let Some(stack) = stack else {
        return false;
    };
    if !insert_one(host, dst, side, &stack) {
        return false;
    }
    host.put(src, &[(slot, less_one(&stack))]);
    true
}

/// Puts one of `stack` into whatever holds items at `p` (a jukebox takes a
/// disc), through its face `side`. `false` if there is nothing there or no
/// room.
pub fn insert_at<H>(host: &mut H, blocks: &Table, p: BlockPos, side: Dir, stack: &ItemStack) -> bool
where
    H: Containers + Effects + Recipes + Jukeboxes,
{
    if is_block(host, blocks, p, jukebox::ID).is_some() {
        return jukebox::insert(host, blocks, p, stack);
    }
    host.store_at(p)
        .is_some_and(|dst| insert_one(host, dst, side, stack))
}

/// Puts one of `stack` into `dst` through its face `side`, onto a matching
/// stack first. `false` if there is no room.
fn insert_one<H: Containers + Recipes>(
    host: &mut H,
    dst: Store<H::Kind>,
    side: Dir,
    stack: &ItemStack,
) -> bool {
    let max = host.max_stack(stack.item);
    // The slot it goes in and how many are there then.
    let mut target = None;
    let reader = &*host;
    reader.with_slots(&dst, &mut |slots| {
        let into = sided_slots(dst.holder, slots.len(), side, true);
        let count = |t: usize| slots[t].as_ref().map_or(0, |d| d.count) + 1;
        // A crafter takes it in the first slot that may have it.
        if dst.holder == Holder::Crafter {
            target = into
                .iter()
                .find(|&i| super::crafter::accepts(reader, dst.at, slots, i, stack))
                .map(|t| (t, count(t)));
            return;
        }
        let allowed = |i: usize| {
            dst.holder != Holder::BrewingStand || super::brewing::accepts(reader, slots, i, stack)
        };
        let merge = into.iter().find(|&i| {
            allowed(i)
                && slots[i]
                    .as_ref()
                    .is_some_and(|d| d.same_kind(stack) && d.count < max)
        });
        target = merge
            .or_else(|| into.iter().find(|&i| allowed(i) && slots[i].is_none()))
            .map(|t| (t, count(t)));
    });
    let Some((t, count)) = target else {
        return false;
    };
    host.put(&dst, &[(t, Some(stack.with_count(count)))]);
    true
}

/// Hoppers and composters (minecraft.wiki, "Composter" → "Hopper"): one
/// above puts compostable items in from the top, one below takes the bone
/// meal out. `None` if neither end is a composter.
#[allow(clippy::too_many_arguments)]
fn composter_transfer<H>(
    level: &mut dyn Level,
    ctx: Ctx,
    host: &mut H,
    blocks: &Table,
    from: BlockPos,
    from_side: Dir,
    to: BlockPos,
    to_side: Dir,
) -> Option<bool>
where
    H: Containers + Effects + Recipes,
{
    let layers = |s: u32| -> i32 {
        blocks
            .property(s, "level")
            .and_then(|l| l.parse().ok())
            .unwrap_or(0)
    };
    if let Some(state) = is_block(host, blocks, to, "minecraft:composter") {
        if to_side != Dir::Up || layers(state) >= 7 {
            return Some(false);
        }
        let Some(src) = host.store_at(from) else {
            return Some(false);
        };
        let slots = host.slots(&src);
        let pick = sided_slots(src.holder, slots.len(), from_side, false)
            .iter()
            .find_map(|i| {
                let s = slots[i].as_ref()?;
                let chance = crate::composter::chance(host.name(s.item)?);
                (chance > 0).then_some((i, s.clone(), chance))
            });
        let Some((slot, stack, chance)) = pick else {
            return Some(false);
        };
        host.put(&src, &[(slot, less_one(&stack))]);
        let success = host.random() % 100 < u64::from(chance);
        crate::composter::add(level, ctx, to, state, success);
        return Some(true);
    }
    let state = is_block(host, blocks, from, "minecraft:composter")?;
    if from_side != Dir::Down || layers(state) != 8 {
        return Some(false);
    }
    let (Some(dst), Some(bone_meal)) = (host.store_at(to), host.id("minecraft:bone_meal")) else {
        return Some(false);
    };
    if !insert_one(host, dst, to_side, &ItemStack::new(bone_meal, 1)) {
        return Some(false);
    }
    crate::composter::empty(level, ctx, from, state);
    Some(true)
}
