//! What the block logic (`mc_tick::hopper`) needs from the server for
//! hoppers, droppers and dispensers: moving single items between containers,
//! the hopper's rest time, and picking items up off the ground.

use std::collections::HashSet;

use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_tick::Dir;

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
    let (Some((from_kind, from_name)), Some(_)) = (kind_at(ctx, from), kind_at(ctx, to)) else {
        return false;
    };
    let src = containers::slots(ctx, from, from_kind);
    sided_slots(from_name, src.len(), from_side, false)
        .into_iter()
        .filter(|&slot| src[slot].is_some())
        .any(|slot| transfer_from_slot(ctx, from, from_kind, slot, to, to_side))
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
    let mut entities = lock(&ctx.entities);
    for e in entities.iter_mut() {
        let mc_entity::Kind::Item { item, count, damage, health, .. } = e.kind else {
            continue;
        };
        if e.removed
            || e.pos[0] < f64::from(p.x)
            || e.pos[0] >= f64::from(p.x) + 1.0
            || e.pos[2] < f64::from(p.z)
            || e.pos[2] >= f64::from(p.z) + 1.0
            || e.pos[1] < f64::from(p.y) + 0.6
            || e.pos[1] >= f64::from(p.y) + 2.0
        {
            continue;
        }
        let mut slots = containers::slots(ctx, p, kind);
        let max = ctx.items.max_stack(item);
        let comps = containers::components(ctx, p, kind);
        let room = (0..slots.len())
            .find(|&i| {
                slots[i]
                    .as_ref()
                    .is_some_and(|s| s.item == item && s.count < max && !comps.contains_key(&i))
            })
            .or_else(|| (0..slots.len()).find(|&i| slots[i].is_none()));
        let Some(i) = room else { continue };
        let add = count.min(max - slots[i].as_ref().map_or(0, |s| s.count));
        slots[i] = Some(ItemStack {
            count: slots[i].as_ref().map_or(0, |s| s.count) + add,
            item,
            damage,
            has_component_changes: false,
        });
        containers::put(ctx, p, kind, &[(i, slots[i].clone())]);
        if add >= count {
            e.removed = true;
        } else {
            e.kind = mc_entity::Kind::Item {
                item,
                count: count - add,
                damage,
                health,
                pickup_delay: 0,
            };
        }
        return true;
    }
    false
}

/// A dropper or dispenser fires: a random stocked slot gives one item,
/// which goes into the container in front (droppers) or out onto the
/// ground. Per-item dispenser actions (buckets, boats, bones, eggs, TNT,
/// arrows) are still to come: until then a dispenser drops like a dropper.
pub fn dispense(ctx: &ServerContext, p: Position, facing: Dir, dropper: bool) {
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
    if dropper && kind_at(ctx, front).is_some() {
        transfer_from_slot(ctx, p, kind, slot, front, facing.opposite());
    } else {
        let left = (stack.count > 1).then(|| ItemStack {
            count: stack.count - 1,
            ..stack.clone()
        });
        let kept = left.is_none().then(|| Tag::Compound(Default::default()));
        containers::put_with(ctx, p, kind, &[(slot, left, kept)]);
        let at = [
            f64::from(p.x) + 0.5 + f64::from(dx) * 0.7,
            f64::from(p.y) + 0.5 + f64::from(dy) * 0.7 - if dy == 0 { 0.125 } else { 0.0 },
            f64::from(p.z) + 0.5 + f64::from(dz) * 0.7,
        ];
        crate::tick::spawn_item_at(ctx, at, stack.item, 1);
    }
}

/// Moves one item out of a particular slot (a dropper facing a container).
fn transfer_from_slot(
    ctx: &ServerContext,
    from: Position,
    from_kind: Kind,
    slot: usize,
    to: Position,
    side: Dir,
) -> bool {
    let Some((to_kind, to_name)) = kind_at(ctx, to) else {
        return false;
    };
    let src = containers::slots(ctx, from, from_kind);
    let src_comps = containers::components(ctx, from, from_kind);
    let mut dst = containers::slots(ctx, to, to_kind);
    let dst_comps = containers::components(ctx, to, to_kind);
    let Some(stack) = src[slot].clone() else {
        return false;
    };
    let into = sided_slots(to_name, dst.len(), side, true);
    let comps = src_comps.get(&slot);
    let max = ctx.items.max_stack(stack.item);
    let merge = into.iter().copied().find(|&i| {
        dst[i]
            .as_ref()
            .is_some_and(|d| d.item == stack.item && d.count < max && dst_comps.get(&i) == comps)
    });
    let Some(t) = merge.or_else(|| into.iter().copied().find(|&i| dst[i].is_none())) else {
        return false;
    };
    let moved = ItemStack {
        count: dst[t].as_ref().map_or(0, |d| d.count) + 1,
        item: stack.item,
        damage: stack.damage,
        has_component_changes: stack.has_component_changes,
    };
    dst[t] = Some(moved.clone());
    containers::put_with(
        ctx,
        to,
        to_kind,
        &[(
            t,
            Some(moved),
            comps.cloned().or(Some(Tag::Compound(Default::default()))),
        )],
    );
    let left = (stack.count > 1).then(|| ItemStack {
        count: stack.count - 1,
        ..stack.clone()
    });
    let kept = left.is_none().then(|| Tag::Compound(Default::default()));
    containers::put_with(ctx, from, from_kind, &[(slot, left, kept)]);
    true
}

/// A hopper exists at `p`: it gets ticked.
pub fn register(ctx: &ServerContext, p: Position) {
    lock(&ctx.hoppers).insert((p.x, p.y, p.z));
}

/// One game tick of every known hopper.
pub fn tick(ctx: &ServerContext, level: &mut crate::level::WorldLevel) {
    let logic = level.logic();
    let all: Vec<_> = lock(&ctx.hoppers).iter().copied().collect();
    for key in all {
        let p = Position::new(key.0, key.1, key.2);
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
