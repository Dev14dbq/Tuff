//! What the block logic (`mc_tick::hopper`) needs from the server for
//! hoppers, droppers and dispensers: moving single items between containers,
//! the hopper's rest time, and picking items up off the ground.

use std::collections::HashSet;

use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::Dir;

use mc_entity::extra::{Cart, Craft};

use crate::containers::{self, Kind};
use crate::context::ServerContext;
use crate::tick::lock;

pub type Active = HashSet<(i32, i32, i32)>;

fn kind_at(ctx: &ServerContext, p: Position) -> Option<(Kind, &str)> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    let name = blocks.block_name(state)?;
    let kind = containers::kind_of(name)?;
    Some((kind, name))
}

/// Something holding items at a block's place: the block itself, or else an
/// entity holding items (a chest minecart...) whose box is in that space.
#[derive(Clone, Copy)]
struct Store<'a> {
    at: Position,
    kind: Kind,
    /// Block name (empty for an entity).
    name: &'a str,
}

fn store_at(ctx: &ServerContext, p: Position) -> Option<Store<'_>> {
    if let Some((kind, name)) = kind_at(ctx, p) {
        return Some(Store { at: p, kind, name });
    }
    let (lo, hi) = (
        [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
        [
            f64::from(p.x) + 1.0,
            f64::from(p.y) + 1.0,
            f64::from(p.z) + 1.0,
        ],
    );
    let found: Vec<(i32, Kind)> = lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| {
            let mc_entity::Kind::Vehicle(v) = &e.kind else {
                return None;
            };
            let kind = containers::entity_kind(&crate::vehicles::item_of(&v.craft))?;
            let b = e.aabb();
            (0..3)
                .all(|i| b.min[i] < hi[i] && b.max[i] > lo[i])
                .then_some((e.id, kind))
        })
        .collect();
    // One of them, at random.
    let pick = found.get((crate::util::random_u64() % found.len().max(1) as u64) as usize)?;
    Some(Store {
        at: containers::entity_key(pick.0),
        kind: pick.1,
        name: "",
    })
}

/// The slots a neighbour may push into through the face `side` of the
/// container (in the order they are tried), or pull out of.
fn sided_slots(name: &str, size: usize, side: Dir, inserting: bool) -> Vec<usize> {
    match name {
        // Brewing and autocrafting are not hopper-fed yet.
        "minecraft:brewing_stand" | "minecraft:crafter" | "minecraft:ender_chest" => Vec::new(),
        "minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker" => {
            if inserting {
                match side {
                    Dir::Up => vec![0],
                    Dir::Down => Vec::new(),
                    _ => vec![1],
                }
            } else if side == Dir::Down {
                vec![2, 1]
            } else {
                Vec::new()
            }
        }
        _ => (0..size).collect(),
    }
}

/// Moves one item from `from` into `to`. See `Level::container_transfer`.
pub fn transfer(
    ctx: &ServerContext,
    from: Position,
    from_side: Dir,
    to: Position,
    to_side: Dir,
) -> bool {
    let (Some(src), Some(dst)) = (store_at(ctx, from), store_at(ctx, to)) else {
        return false;
    };
    move_one(ctx, src, from_side, dst, to_side)
}

/// Moves one item from `src` (out through `from_side`) into `dst` (in
/// through `to_side`).
fn move_one(ctx: &ServerContext, src: Store, from_side: Dir, dst: Store, to_side: Dir) -> bool {
    let slots = containers::slots(ctx, src.at, src.kind);
    sided_slots(src.name, slots.len(), from_side, false)
        .into_iter()
        .filter(|&slot| slots[slot].is_some())
        .any(|slot| transfer_from_slot(ctx, src.at, src.kind, slot, dst, to_side))
}

pub fn cooldown(ctx: &ServerContext, p: Position) -> i32 {
    match ctx
        .world
        .block_entity(p, &ctx.data.blocks)
        .and_then(|be| be.data.get("TransferCooldown").cloned())
    {
        Some(Tag::Int(v)) => v,
        _ => 0,
    }
}

pub fn set_cooldown(ctx: &ServerContext, p: Position, ticks: i32) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: "minecraft:hopper".into(),
            data: mc_nbt::Compound::new(),
        });
    be.data.insert("TransferCooldown", Tag::Int(ticks));
    ctx.world.set_block_entity(be, blocks);
}

/// The hopper takes one item (a whole stack's worth, one at a time) from an
/// item entity in the space above it.
pub fn pickup(ctx: &ServerContext, p: Position) -> bool {
    let Some((kind, _)) = kind_at(ctx, p) else {
        return false;
    };
    let (x, y, z) = (f64::from(p.x), f64::from(p.y), f64::from(p.z));
    pickup_into(ctx, p, kind, [x, y + 0.6, z], [x + 1.0, y + 2.0, z + 1.0])
}

/// Takes one item entity whose position is in the box from `min` to `max`
/// into the container at `p`.
fn pickup_into(ctx: &ServerContext, p: Position, kind: Kind, min: [f64; 3], max: [f64; 3]) -> bool {
    let mut entities = lock(&ctx.entities);
    for e in entities.iter_mut() {
        let Some(found) = e.kind.as_stack() else {
            continue;
        };
        let (item, count) = (found.item, found.count);
        if e.removed || (0..3).any(|i| e.pos[i] < min[i] || e.pos[i] >= max[i]) {
            continue;
        }
        let mut slots = containers::slots(ctx, p, kind);
        let max = ctx.items.max_stack(item);
        let room = (0..slots.len())
            .find(|&i| {
                slots[i]
                    .as_ref()
                    .is_some_and(|s| s.same_kind(&found) && s.count < max)
            })
            .or_else(|| (0..slots.len()).find(|&i| slots[i].is_none()));
        let Some(i) = room else { continue };
        let add = count.min(max - slots[i].as_ref().map_or(0, |s| s.count));
        slots[i] = Some(found.with_count(slots[i].as_ref().map_or(0, |s| s.count) + add));
        containers::put(ctx, p, kind, &[(i, slots[i].clone())]);
        if add >= count {
            e.removed = true;
        } else {
            if let mc_entity::Kind::Item {
                count,
                pickup_delay,
                ..
            } = &mut e.kind
            {
                *count -= add;
                *pickup_delay = 0;
            }
        }
        return true;
    }
    false
}

/// What a dispenser does with an item.
enum Behavior {
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

fn done(leftover: Option<i32>) -> Behavior {
    Behavior::Done {
        leftover: leftover.map(|i| ItemStack::new(i, 1)),
        wear: 0,
    }
}

fn centre_of(p: Position) -> [f64; 3] {
    [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5]
}

/// The behaviour of a dispenser for `name`, acting on the block `front`.
fn behavior(
    ctx: &ServerContext,
    level: &mut crate::level::WorldLevel,
    name: &str,
    front: Position,
) -> Behavior {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(front, blocks).unwrap_or(0);
    let block = blocks.block_name(state).unwrap_or("");
    let item = |n: &str| ctx.items.id(n);
    match name {
        "minecraft:water_bucket" | "minecraft:lava_bucket" => {
            let fluid = if name == "minecraft:water_bucket" {
                "minecraft:water"
            } else {
                "minecraft:lava"
            };
            let placed = if fluid == "minecraft:water"
                && blocks.property(state, "waterlogged") == Some("false")
            {
                level.set_block(front, ctx.shapes.with(state, "waterlogged", "true"))
            } else if ctx.shapes.is_air(state) || ctx.rules.is_replaceable(state) {
                let source = ctx.shapes.default_state(fluid).unwrap_or(0);
                let same = block == fluid && blocks.property(state, "level") == Some("0");
                !same && level.set_block(front, source)
            } else {
                false
            };
            if placed {
                done(item("minecraft:bucket"))
            } else {
                Behavior::Default
            }
        }
        "minecraft:bucket" => {
            let (new, full) = if blocks.property(state, "waterlogged") == Some("true") {
                (
                    ctx.shapes.with(state, "waterlogged", "false"),
                    "minecraft:water_bucket",
                )
            } else if blocks.property(state, "level") != Some("0") {
                return Behavior::Default;
            } else {
                match block {
                    "minecraft:water" => (ctx.shapes.air(), "minecraft:water_bucket"),
                    "minecraft:lava" => (ctx.shapes.air(), "minecraft:lava_bucket"),
                    _ => return Behavior::Default,
                }
            };
            match item(full) {
                Some(full) if level.set_block(front, new) => done(Some(full)),
                _ => Behavior::Default,
            }
        }
        "minecraft:powder_snow_bucket" => Behavior::Default,
        "minecraft:bone_meal" => {
            if crate::growth::bone_meal(level, front) {
                done(None)
            } else {
                Behavior::Failed
            }
        }
        "minecraft:tnt" => {
            let mut tnt = crate::explosion::primed(ctx, front, crate::explosion::TNT_FUSE, None);
            tnt.pos = centre_of(front);
            tnt.vel = [0.0; 3];
            let _ = ctx
                .events
                .send(crate::context::Event::EntitySpawned(tnt.clone()));
            lock(&ctx.entities).push(tnt);
            done(None)
        }
        "minecraft:flint_and_steel" | "minecraft:fire_charge" => {
            let lit = block.ends_with("campfire") || block.ends_with("candle");
            let ok = if block == "minecraft:tnt" {
                crate::explosion::ignite(ctx, level, front, None)
            } else if lit
                && blocks.property(state, "lit") == Some("false")
                && blocks.property(state, "waterlogged") != Some("true")
            {
                level.set_block(front, ctx.shapes.with(state, "lit", "true"))
            } else if let Some(fire) =
                mc_tick::fire::can_light(&*level, level.logic(), crate::level::to_pos(front))
            {
                level.set_block(front, fire)
            } else {
                false
            };
            if !ok {
                Behavior::Failed
            } else if name == "minecraft:fire_charge" {
                done(None)
            } else {
                Behavior::Done {
                    leftover: None,
                    wear: 1,
                }
            }
        }
        "minecraft:shears" => {
            let honey = matches!(block, "minecraft:beehive" | "minecraft:bee_nest")
                && blocks.property(state, "honey_level") == Some("5");
            if honey {
                level.set_block(front, ctx.shapes.with(state, "honey_level", "0"));
                if let Some(comb) = item("minecraft:honeycomb") {
                    crate::tick::spawn_item_at(ctx, centre_of(front), comb, 3);
                }
                Behavior::Done {
                    leftover: None,
                    wear: 1,
                }
            } else {
                Behavior::Default
            }
        }
        "minecraft:glass_bottle" => {
            let honey = matches!(block, "minecraft:beehive" | "minecraft:bee_nest")
                && blocks.property(state, "honey_level") == Some("5");
            if honey {
                level.set_block(front, ctx.shapes.with(state, "honey_level", "0"));
                return done(item("minecraft:honey_bottle"));
            }
            let water =
                block == "minecraft:water" || blocks.property(state, "waterlogged") == Some("true");
            match item("minecraft:potion") {
                Some(potion) if water => {
                    let mut contents = mc_nbt::Compound::new();
                    contents.insert("potion", "minecraft:water");
                    let mut bottle = ItemStack::new(potion, 1);
                    bottle
                        .components
                        .set("potion_contents", Tag::Compound(contents));
                    Behavior::Done {
                        leftover: Some(bottle),
                        wear: 0,
                    }
                }
                _ => Behavior::Default,
            }
        }
        _ => Behavior::Default,
    }
}

/// The inventory slot an item is worn in, if it is armour.
fn armor_slot(name: &str) -> Option<usize> {
    let n = name.trim_start_matches("minecraft:");
    if n.ends_with("_helmet")
        || n.ends_with("_head")
        || n.ends_with("_skull")
        || n == "carved_pumpkin"
    {
        Some(5)
    } else if n.ends_with("_chestplate") || n == "elytra" {
        Some(6)
    } else if n.ends_with("_leggings") {
        Some(7)
    } else if n.ends_with("_boots") {
        Some(8)
    } else {
        None
    }
}

/// Armour dispensed at a player standing in front: it is put on if that
/// slot is free.
fn dress_player(ctx: &ServerContext, front: Position, stack: &ItemStack) -> bool {
    let Some(name) = ctx.items.name(stack.item) else {
        return false;
    };
    let Some(slot) = armor_slot(name) else {
        return false;
    };
    let (fx, fy, fz) = (f64::from(front.x), f64::from(front.y), f64::from(front.z));
    let candidates: Vec<u128> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter(|p| p.game_mode != 3)
        .filter_map(|p| p.pose.map(|pose| (p.profile.uuid, pose)))
        .filter(|(_, pose)| {
            pose.x + 0.3 > fx
                && pose.x - 0.3 < fx + 1.0
                && pose.z + 0.3 > fz
                && pose.z - 0.3 < fz + 1.0
                && pose.y + 1.8 > fy
                && pose.y < fy + 1.0
        })
        .map(|(uuid, _)| uuid)
        .collect();
    let mut inventories = lock(&ctx.inventories);
    for uuid in candidates {
        let Some(hands) = inventories.get_mut(&uuid) else {
            continue;
        };
        if hands.slots[slot].is_some() {
            continue;
        }
        hands.slots[slot] = Some(stack.with_count(1));
        let _ = ctx.events.send(crate::context::Event::InventorySlot {
            uuid,
            slot: slot as i16,
            item: hands.slots[slot].clone(),
        });
        return true;
    }
    false
}

/// Puts a returned item (an empty bucket...) into the container, merging
/// or in a free slot. Returns whether it fit.
fn give_back(ctx: &ServerContext, p: Position, kind: Kind, back: &ItemStack) -> bool {
    let slots = containers::slots(ctx, p, kind);
    let max = ctx.items.max_stack(back.item);
    let merge = (0..slots.len()).find(|&i| {
        slots[i]
            .as_ref()
            .is_some_and(|s| s.same_kind(back) && s.count < max)
    });
    let target = merge.or_else(|| (0..slots.len()).find(|&i| slots[i].is_none()));
    let Some(i) = target else { return false };
    let stack = back.with_count(slots[i].as_ref().map_or(0, |s| s.count) + back.count);
    containers::put(ctx, p, kind, &[(i, Some(stack))]);
    true
}

/// A dropper or dispenser fires: a random stocked slot gives one item,
/// which goes into the container in front (droppers) or out onto the
/// ground. A dispenser acts on the item first where it has a behaviour
/// (buckets, bone meal, TNT, flint and steel, fire charges, shears, glass
/// bottles, armour onto a player in front). Items that need entities the
/// server does not have yet (boats, minecarts, arrows, eggs, spawn eggs)
/// are dropped as by a dropper.
pub fn dispense(level: &mut crate::level::WorldLevel, p: Position, facing: Dir, dropper: bool) {
    let ctx = level.context();
    let Some((kind, _)) = kind_at(ctx, p) else {
        return;
    };
    let slots = containers::slots(ctx, p, kind);
    let filled: Vec<usize> = (0..slots.len()).filter(|&i| slots[i].is_some()).collect();
    if filled.is_empty() {
        return;
    }
    let slot = filled[(crate::util::random_u64() % filled.len() as u64) as usize];
    let Some(stack) = slots[slot].clone() else {
        return;
    };
    let (dx, dy, dz) = facing.offset();
    let front = Position::new(p.x + dx, p.y + dy, p.z + dz);
    if dropper && let Some(dst) = store_at(ctx, front) {
        transfer_from_slot(ctx, p, kind, slot, dst, facing.opposite());
        return;
    }
    let action = if dropper {
        Behavior::Default
    } else if let Some(name) = ctx.items.name(stack.item).map(str::to_owned) {
        match behavior(ctx, level, &name, front) {
            Behavior::Default if dress_player(ctx, front, &stack) => done(None),
            other => other,
        }
    } else {
        Behavior::Default
    };
    match action {
        Behavior::Failed => {}
        Behavior::Done { leftover, wear } => {
            let max = ctx.items.max_damage(stack.item);
            let worn = wear > 0 && max > 0;
            let left = if worn {
                // A tool is worn, not used up; it breaks at full damage.
                let damage = stack.damage + wear;
                (damage < max).then(|| ItemStack {
                    damage,
                    ..stack.clone()
                })
            } else {
                (stack.count > 1).then(|| ItemStack {
                    count: stack.count - 1,
                    ..stack.clone()
                })
            };
            let emptied = left.is_none();
            containers::put(ctx, p, kind, &[(slot, left)]);
            if let Some(back) = leftover {
                // An emptied slot takes the leftover itself.
                let placed = if emptied {
                    containers::put(ctx, p, kind, &[(slot, Some(back.clone()))]);
                    true
                } else {
                    give_back(ctx, p, kind, &back)
                };
                if !placed {
                    crate::tick::spawn_stack_at(ctx, drop_point(p, dx, dy, dz), &back);
                }
            }
        }
        Behavior::Default => {
            let left = (stack.count > 1).then(|| ItemStack {
                count: stack.count - 1,
                ..stack.clone()
            });
            containers::put(ctx, p, kind, &[(slot, left)]);
            crate::tick::spawn_stack_at(ctx, drop_point(p, dx, dy, dz), &stack.with_count(1));
        }
    }
}

fn drop_point(p: Position, dx: i32, dy: i32, dz: i32) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5 + f64::from(dx) * 0.7,
        f64::from(p.y) + 0.5 + f64::from(dy) * 0.7 - if dy == 0 { 0.125 } else { 0.0 },
        f64::from(p.z) + 0.5 + f64::from(dz) * 0.7,
    ]
}

/// Moves one item out of a particular slot (a dropper facing a container).
fn transfer_from_slot(
    ctx: &ServerContext,
    from: Position,
    from_kind: Kind,
    slot: usize,
    dst: Store,
    side: Dir,
) -> bool {
    let src = containers::slots(ctx, from, from_kind);
    let Some(stack) = src[slot].clone() else {
        return false;
    };
    if !insert_one(ctx, dst, side, &stack) {
        return false;
    }
    let left = (stack.count > 1).then(|| ItemStack {
        count: stack.count - 1,
        ..stack.clone()
    });
    containers::put(ctx, from, from_kind, &[(slot, left)]);
    true
}

/// Puts one of `stack` into `dst` through its face `side`,
/// onto a matching stack first. `false` if there is no room.
fn insert_one(ctx: &ServerContext, dst: Store, side: Dir, stack: &ItemStack) -> bool {
    let (to, to_kind, to_name) = (dst.at, dst.kind, dst.name);
    let mut dst = containers::slots(ctx, to, to_kind);
    let into = sided_slots(to_name, dst.len(), side, true);
    let max = ctx.items.max_stack(stack.item);
    let merge = into.iter().copied().find(|&i| {
        dst[i]
            .as_ref()
            .is_some_and(|d| d.same_kind(stack) && d.count < max)
    });
    let Some(t) = merge.or_else(|| into.iter().copied().find(|&i| dst[i].is_none())) else {
        return false;
    };
    let moved = stack.with_count(dst[t].as_ref().map_or(0, |d| d.count) + 1);
    dst[t] = Some(moved.clone());
    containers::put(ctx, to, to_kind, &[(t, Some(moved))]);
    true
}

/// Hoppers and composters (minecraft.wiki, "Composter" → "Hopper"): one
/// above puts compostable items in from the top, one below takes the bone
/// meal out. `None` if neither end is a composter.
pub fn composter_transfer(
    level: &mut crate::level::WorldLevel,
    from: Position,
    from_side: Dir,
    to: Position,
    to_side: Dir,
) -> Option<bool> {
    let ctx = level.context();
    let logic = level.logic();
    let blocks = &ctx.data.blocks;
    let composter = |p: Position| {
        ctx.world
            .block(p, blocks)
            .filter(|&s| blocks.block_name(s) == Some("minecraft:composter"))
    };
    let layers = |s: u32| -> i32 {
        blocks
            .property(s, "level")
            .and_then(|l| l.parse().ok())
            .unwrap_or(0)
    };
    if let Some(state) = composter(to) {
        if to_side != Dir::Up || layers(state) >= 7 {
            return Some(false);
        }
        let Some(src) = store_at(ctx, from) else {
            return Some(false);
        };
        let slots = containers::slots(ctx, src.at, src.kind);
        let pick = sided_slots(src.name, slots.len(), from_side, false)
            .into_iter()
            .find_map(|i| {
                let s = slots[i].as_ref()?;
                let chance = crate::useblocks::compost_chance(ctx.items.name(s.item)?);
                (chance > 0).then_some((i, s.clone(), chance))
            });
        let Some((slot, stack, chance)) = pick else {
            return Some(false);
        };
        let left = (stack.count > 1).then(|| ItemStack {
            count: stack.count - 1,
            ..stack
        });
        containers::put(ctx, src.at, src.kind, &[(slot, left)]);
        let success = crate::util::random_u64() % 100 < u64::from(chance);
        mc_tick::composter::add(level, logic, crate::level::to_pos(to), state, success);
        return Some(true);
    }
    let state = composter(from)?;
    if from_side != Dir::Down || layers(state) != 8 {
        return Some(false);
    }
    let (Some(dst), Some(bone_meal)) = (store_at(ctx, to), ctx.items.id("minecraft:bone_meal"))
    else {
        return Some(false);
    };
    if !insert_one(ctx, dst, to_side, &ItemStack::new(bone_meal, 1)) {
        return Some(false);
    }
    mc_tick::composter::empty(level, logic, crate::level::to_pos(from), state);
    Some(true)
}

/// A hopper exists at `p`: it gets ticked.
pub fn register(ctx: &ServerContext, p: Position) {
    lock(&ctx.hoppers).insert((p.x, p.y, p.z));
}

/// One game tick of every hopper minecart that is on: it pulls one item
/// from the container above it, or else picks up an item entity around it,
/// then rests 4 ticks.
pub fn tick_carts(ctx: &ServerContext) {
    let carts: Vec<(i32, [f64; 3])> = lock(&ctx.entities)
        .iter()
        .filter(|e| {
            !e.removed
                && matches!(&e.kind, mc_entity::Kind::Vehicle(v)
                    if v.enabled && v.craft == Craft::Minecart(Cart::Hopper))
        })
        .map(|e| (e.id, e.pos))
        .collect();
    let Some(kind) = containers::entity_kind("minecraft:hopper_minecart") else {
        return;
    };
    let mut rests = lock(&ctx.cart_rests);
    rests.retain(|id, _| carts.iter().any(|c| c.0 == *id));
    let resting: Vec<i32> = rests
        .iter_mut()
        .filter_map(|(&id, left)| {
            *left -= 1;
            (*left > 0).then_some(id)
        })
        .collect();
    rests.retain(|_, left| *left > 0);
    drop(rests);
    for (id, pos) in carts {
        if resting.contains(&id) {
            continue;
        }
        let dst = Store {
            at: containers::entity_key(id),
            kind,
            name: "",
        };
        // The container above: the block over the cart's middle.
        let above = Position::new(
            pos[0].floor() as i32,
            (pos[1] + 0.5 + 1.0).floor() as i32,
            pos[2].floor() as i32,
        );
        let pulled = match kind_at(ctx, above) {
            Some((k, name)) => {
                let src = Store {
                    at: above,
                    kind: k,
                    name,
                };
                move_one(ctx, src, Dir::Down, dst, Dir::Up)
            }
            None => false,
        };
        let moved = pulled || {
            let (x, y, z) = (pos[0], pos[1] + 0.5, pos[2]);
            pickup_into(
                ctx,
                dst.at,
                kind,
                [x - 0.5, y - 0.5, z - 0.5],
                [x + 0.5, y + 1.0, z + 0.5],
            )
        };
        if moved {
            lock(&ctx.cart_rests).insert(id, 4);
        }
    }
}

/// One game tick of every known hopper.
pub fn tick(ctx: &ServerContext, level: &mut crate::level::WorldLevel) {
    let logic = level.logic();
    let all: Vec<_> = lock(&ctx.hoppers).iter().copied().collect();
    for key in all {
        let p = Position::new(key.0, key.1, key.2);
        // Hoppers in unloaded chunks wait; loading the chunk wakes them again.
        if !ctx.world.is_loaded(p) {
            lock(&ctx.hoppers).remove(&key);
            continue;
        }
        let is_hopper = ctx
            .world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            == Some("minecraft:hopper");
        if is_hopper {
            mc_tick::hopper::tick(level, logic, crate::level::to_pos(p));
        } else {
            lock(&ctx.hoppers).remove(&key);
        }
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

    /// A dispenser at the origin holding `item`, facing east.
    fn loaded(ctx: &ServerContext, item: &str, count: i32) -> Position {
        let p = Position::new(0, -60, 0);
        let state = ctx
            .data
            .blocks
            .default_state("minecraft:dispenser")
            .unwrap();
        WorldLevel::new(ctx).set_block(p, state);
        let item = ctx.items.id(item).unwrap();
        containers::put(
            ctx,
            p,
            containers::kind_of("minecraft:dispenser").unwrap(),
            &[(0, Some(ItemStack::new(item, count)))],
        );
        p
    }

    fn fire(ctx: &ServerContext, p: Position) {
        let mut level = WorldLevel::new(ctx);
        dispense(&mut level, p, Dir::East, false);
    }

    fn slot0(ctx: &ServerContext, p: Position) -> Option<ItemStack> {
        let kind = containers::kind_of("minecraft:dispenser").unwrap();
        containers::slots(ctx, p, kind)[0].clone()
    }

    #[test]
    fn bucket_pours_and_scoops() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:water_bucket", 1);
        fire(&ctx, p);
        let front = Position::new(1, -60, 0);
        let name = |ctx: &ServerContext| {
            ctx.world
                .block(front, &ctx.data.blocks)
                .and_then(|s| ctx.data.blocks.block_name(s).map(str::to_owned))
        };
        assert_eq!(name(&ctx).as_deref(), Some("minecraft:water"));
        assert_eq!(
            slot0(&ctx, p).map(|s| s.item),
            ctx.items.id("minecraft:bucket")
        );
        // The empty bucket takes the water back.
        fire(&ctx, p);
        assert_eq!(name(&ctx).as_deref(), Some("minecraft:air"));
        assert_eq!(
            slot0(&ctx, p).map(|s| s.item),
            ctx.items.id("minecraft:water_bucket")
        );
    }

    #[test]
    fn tnt_is_primed_not_dropped() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:tnt", 2);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(1));
        let entities = lock(&ctx.entities);
        assert!(matches!(
            entities[..],
            [mc_entity::Entity {
                kind: mc_entity::Kind::Tnt { .. },
                ..
            }]
        ));
    }

    #[test]
    fn bone_meal_needs_a_plant() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:bone_meal", 3);
        // Nothing to grow: the meal stays and nothing drops.
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(3));
        assert!(lock(&ctx.entities).is_empty());
        let front = Position::new(1, -60, 0);
        let wheat = ctx.data.blocks.default_state("minecraft:wheat").unwrap();
        WorldLevel::new(&ctx).set_block(front, wheat);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(2));
        let age = ctx
            .world
            .block(front, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.property(s, "age").map(str::to_owned));
        assert!(age.is_some_and(|a| a != "0"));
    }

    #[test]
    fn flint_and_steel_wears() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:flint_and_steel", 1);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.damage), Some(1));
        assert!(lock(&ctx.entities).is_empty());
    }

    #[test]
    fn plain_items_drop() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = loaded(&ctx, "minecraft:stick", 2);
        fire(&ctx, p);
        assert_eq!(slot0(&ctx, p).map(|s| s.count), Some(1));
        assert_eq!(lock(&ctx.entities).len(), 1);
    }

    fn cart(ctx: &ServerContext, cart: Cart, pos: [f64; 3]) -> i32 {
        let id = ctx.next_entity_id();
        lock(&ctx.entities).push(mc_entity::Entity {
            id,
            uuid: 1,
            kind: mc_entity::Kind::Vehicle(mc_entity::extra::Vehicle::new(
                Craft::Minecart(cart),
                0.0,
            )),
            pos,
            vel: [0.0; 3],
            on_ground: true,
            age: 0,
            removed: false,
            tags: Vec::new(),
        });
        id
    }

    #[test]
    fn hoppers_and_minecarts_pass_items() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let hopper = Position::new(0, -60, 0);
        WorldLevel::new(&ctx).set_block(hopper, blocks.default_state("minecraft:hopper").unwrap());
        WorldLevel::new(&ctx).set_block(
            Position::new(0, -59, 0),
            blocks.default_state("minecraft:rail").unwrap(),
        );
        // A chest minecart on the rail over the hopper is emptied into it.
        let chest = cart(&ctx, Cart::Chest, [0.5, -59.0, 0.5]);
        let chest_kind = containers::entity_kind("minecraft:chest_minecart").unwrap();
        let stick = ctx.items.id("minecraft:stick").unwrap();
        containers::put(
            &ctx,
            containers::entity_key(chest),
            chest_kind,
            &[(3, Some(ItemStack::new(stick, 2)))],
        );
        assert!(transfer(
            &ctx,
            Position::new(0, -59, 0),
            Dir::Down,
            hopper,
            Dir::Up
        ));
        let hopper_kind = containers::kind_of("minecraft:hopper").unwrap();
        assert_eq!(
            containers::slots(&ctx, hopper, hopper_kind)[0]
                .as_ref()
                .map(|s| s.count),
            Some(1)
        );
        assert_eq!(
            containers::slots(&ctx, containers::entity_key(chest), chest_kind)[3]
                .as_ref()
                .map(|s| s.count),
            Some(1)
        );
        // A hopper minecart under a chest takes from it.
        let box_at = Position::new(5, -59, 0);
        WorldLevel::new(&ctx).set_block(box_at, blocks.default_state("minecraft:chest").unwrap());
        let chest_block = containers::kind_of("minecraft:chest").unwrap();
        containers::put(
            &ctx,
            box_at,
            chest_block,
            &[(0, Some(ItemStack::new(stick, 1)))],
        );
        let hc = cart(&ctx, Cart::Hopper, [5.5, -60.0, 0.5]);
        tick_carts(&ctx);
        let hc_kind = containers::entity_kind("minecraft:hopper_minecart").unwrap();
        assert_eq!(
            containers::slots(&ctx, containers::entity_key(hc), hc_kind)[0]
                .as_ref()
                .map(|s| s.item),
            Some(stick)
        );
        assert!(containers::slots(&ctx, box_at, chest_block)[0].is_none());
    }
}
