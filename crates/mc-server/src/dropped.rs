//! Dropped items (minecraft.wiki, "Item (entity)"): how they fall, float,
//! slide, burn, merge, despawn and get picked up, and when clients are told.
//!
//! Clients move dropped items themselves with the same physics, so the
//! server sends the velocity with the spawn and then position and velocity
//! once a second, plus whenever something the client cannot foresee happens
//! (landing, being pushed out of a block). Sending positions every tick
//! instead fights the client's own prediction and the item shakes.

use mc_entity::{Entity, Kind};
use mc_protocol::packets::play;

use crate::context::{Event, ServerContext};
use crate::fx::Sync;
use crate::level::WorldLevel;
use crate::tick::lock;

/// A pickup delay that never counts down.
pub const NEVER: i32 = 32767;
/// Ticks until an item despawns (5 minutes).
pub const LIFETIME: u32 = 6000;
/// How often clients get the position and velocity of an item.
const UPDATE_INTERVAL: u32 = 20;

/// The player who might pick items up: uuid, entity id, game mode, pose.
pub type Player = (u128, i32, i32, crate::context::Pose);

/// One tick of a dropped item. Rows for clients go to `sync`.
pub fn tick(
    ctx: &ServerContext,
    level: &WorldLevel,
    entity: &mut Entity,
    players: &[Player],
    sync: &mut Vec<Sync>,
) {
    let Kind::Item { item, .. } = entity.kind else {
        return;
    };
    let was_on_ground = entity.on_ground;
    let (water, lava) = fluid_heights(ctx, entity);
    let v = &mut entity.vel;
    if water > 0.1 || lava > 0.1 {
        // Items float up slowly and are slowed sideways.
        let slow = if water > 0.1 { 0.99 } else { 0.95 };
        v[0] *= slow;
        v[2] *= slow;
        if v[1] < 0.06 {
            v[1] += 5.0e-4;
        }
    } else {
        v[1] -= mc_entity::GRAVITY;
    }
    let pushed = push_out_of_blocks(level, entity);
    let horizontal = entity.vel[0] * entity.vel[0] + entity.vel[2] * entity.vel[2];
    // Resting items only check the ground every fourth tick.
    if !entity.on_ground
        || horizontal > 1.0e-5
        || (entity.age as i64 + i64::from(entity.id)) % 4 == 0
    {
        let solid = crate::tick::Solid(level);
        let (moved, on_ground) = mc_entity::move_entity(entity.aabb(), entity.vel, &solid);
        for ((pos, vel), moved) in entity.pos.iter_mut().zip(&mut entity.vel).zip(moved) {
            *pos += moved;
            if (moved - *vel).abs() > 1.0e-9 {
                *vel = 0.0;
            }
        }
        entity.on_ground = on_ground;
        let friction = if entity.on_ground {
            slipperiness(ctx, level, entity) * 0.98
        } else {
            0.98
        };
        entity.vel[0] *= friction;
        entity.vel[1] *= 0.98;
        entity.vel[2] *= friction;
        if entity.on_ground && entity.vel[1] < 0.0 {
            entity.vel[1] *= -0.5;
        }
    }
    entity.age += 1;
    if lava > 0.0 || in_fire(ctx, entity) {
        burn(ctx, entity, item, lava > 0.0);
    }
    if let Kind::Item { pickup_delay, .. } = &mut entity.kind
        && *pickup_delay != NEVER
        && *pickup_delay > 0
    {
        *pickup_delay -= 1;
    }
    if entity.age >= LIFETIME {
        entity.removed = true;
        return;
    }
    if entity.removed {
        return;
    }
    if pushed || entity.on_ground != was_on_ground || entity.age % UPDATE_INTERVAL == 0 {
        sync.push(row(entity));
    }
    if matches!(
        entity.kind,
        Kind::Item {
            pickup_delay: 0,
            ..
        }
    ) {
        pick_up(ctx, entity, players);
    }
}

/// The position and velocity of an entity for clients.
pub fn row(entity: &Entity) -> Sync {
    Sync {
        id: entity.id,
        pos: entity.pos,
        vel: entity.vel,
        yaw: 0.0,
        pitch: 0.0,
        on_ground: entity.on_ground,
    }
}

/// How high water and lava reach above the bottom of the item's box.
fn fluid_heights(ctx: &ServerContext, entity: &Entity) -> (f64, f64) {
    let b = entity.aabb();
    let (mut water, mut lava) = (0.0_f64, 0.0_f64);
    for x in b.min[0].floor() as i32..=(b.max[0] - 1.0e-7).floor() as i32 {
        for y in b.min[1].floor() as i32..=(b.max[1] - 1.0e-7).floor() as i32 {
            for z in b.min[2].floor() as i32..=(b.max[2] - 1.0e-7).floor() as i32 {
                let Some(state) = crate::hits::block_at(ctx, [x, y, z]) else {
                    continue;
                };
                let Some((lava_here, height)) = fluid(ctx, state) else {
                    continue;
                };
                // The same fluid above fills the block to the top.
                let full = crate::hits::block_at(ctx, [x, y + 1, z])
                    .and_then(|s| fluid(ctx, s))
                    .is_some_and(|(l, _)| l == lava_here);
                let top = f64::from(y) + if full { 1.0 } else { height };
                if top < b.min[1] {
                    continue;
                }
                let depth = top - b.min[1];
                if lava_here {
                    lava = lava.max(depth);
                } else {
                    water = water.max(depth);
                }
            }
        }
    }
    (water, lava)
}

/// Lava (`true`) or water and the height of its surface in the block.
fn fluid(ctx: &ServerContext, state: u32) -> Option<(bool, f64)> {
    let blocks = &ctx.data.blocks;
    let lava = match blocks.block_name(state)? {
        "minecraft:water" => false,
        "minecraft:lava" => true,
        "minecraft:bubble_column"
        | "minecraft:kelp"
        | "minecraft:kelp_plant"
        | "minecraft:seagrass"
        | "minecraft:tall_seagrass" => return Some((false, 8.0 / 9.0)),
        _ if blocks.property(state, "waterlogged") == Some("true") => {
            return Some((false, 8.0 / 9.0));
        }
        _ => return None,
    };
    let level: i32 = blocks
        .property(state, "level")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    // Sources and falling fluid hold 8 parts of 9, flowing ones less.
    let amount = if level == 0 || level >= 8 {
        8
    } else {
        8 - level
    };
    Some((lava, f64::from(amount) / 9.0))
}

/// An item stuck inside a block drifts out towards the nearest open side.
fn push_out_of_blocks(level: &WorldLevel, entity: &mut Entity) -> bool {
    use mc_entity::Collide as _;
    let solid = crate::tick::Solid(level);
    let centre = [
        entity.pos[0],
        (entity.aabb().min[1] + entity.aabb().max[1]) / 2.0,
        entity.pos[2],
    ];
    let at = centre.map(|c| c.floor() as i32);
    if !solid.solid(at[0], at[1], at[2]) {
        return false;
    }
    let inside = centre.map(f64::fract);
    // Up is the way out when every other side is closed too.
    let mut best = (1, 1, 1.0 - inside[1]);
    let mut open = false;
    for (axis, sign) in [(0, -1), (0, 1), (2, -1), (2, 1), (1, -1), (1, 1)] {
        let mut n = at;
        n[axis] += sign;
        if solid.solid(n[0], n[1], n[2]) {
            continue;
        }
        let distance = if sign < 0 {
            inside[axis]
        } else {
            1.0 - inside[axis]
        };
        if !open || distance < best.2 {
            best = (axis, sign, distance);
            open = true;
        }
    }
    let speed = crate::util::random_f64() * 0.2 + 0.1;
    for v in &mut entity.vel {
        *v *= 0.75;
    }
    entity.vel[best.0] = f64::from(best.1) * speed;
    true
}

/// How slippery the block under the item is (most blocks 0.6).
fn slipperiness(ctx: &ServerContext, level: &WorldLevel, entity: &Entity) -> f64 {
    use mc_tick::Level as _;
    let below = mc_tick::BlockPos::new(
        entity.pos[0].floor() as i32,
        (entity.pos[1] - 0.999).floor() as i32,
        entity.pos[2].floor() as i32,
    );
    match ctx.data.blocks.block_name(level.block(below)) {
        Some("minecraft:ice" | "minecraft:packed_ice" | "minecraft:frosted_ice") => 0.98,
        Some("minecraft:blue_ice") => 0.989,
        Some("minecraft:slime_block") => 0.8,
        _ => 0.6,
    }
}

fn in_fire(ctx: &ServerContext, entity: &Entity) -> bool {
    let (x, y, z) = entity.block_pos();
    crate::hits::block_at(ctx, [x, y, z]).is_some_and(|s| {
        matches!(
            ctx.data.blocks.block_name(s),
            Some("minecraft:fire" | "minecraft:soul_fire")
        )
    })
}

/// Fire takes 1 of an item's 5 points a tick and lava 4, unless the item
/// resists fire (netherite).
fn burn(ctx: &ServerContext, entity: &mut Entity, item: i32, lava: bool) {
    if ctx.items.get(item).is_some_and(|i| i.fire_resistant) {
        return;
    }
    let Kind::Item { health, .. } = &mut entity.kind else {
        return;
    };
    *health -= if lava { 4.0 } else { 1.0 };
    if lava {
        let _ = ctx.events.send(Event::WorldSound {
            pos: entity.pos,
            sound: "minecraft:entity.generic.burn",
            category: 6,
            volume: 0.4,
            pitch: 2.0 + crate::util::random_f64() as f32 * 0.4,
        });
    }
    if *health <= 0.0 {
        entity.removed = true;
    }
}

/// The item's stack as clients see it in its metadata.
pub fn metadata(entity: &Entity) -> Option<play::SetEntityData> {
    let Kind::Item {
        item,
        count,
        damage,
        ..
    } = entity.kind
    else {
        return None;
    };
    Some(play::SetEntityData {
        entity_id: entity.id,
        entries: vec![(
            play::metadata::ITEM,
            play::MetadataValue::Item(Some(play::ItemStack {
                damage,
                ..play::ItemStack::new(item, count)
            })),
        )],
    })
}

/// The first nearby player with room takes the item (all of it, or what
/// fits). Clients hear of it in vanilla's order: the pickup, then the
/// changed slots, then (at the end of the tick) the item's removal.
fn pick_up(ctx: &ServerContext, entity: &mut Entity, players: &[Player]) {
    let Kind::Item {
        item,
        count,
        damage,
        ..
    } = entity.kind
    else {
        return;
    };
    for (uuid, player_entity, mode, pose) in players {
        if *mode == 3 || !crate::tick::picks_up(pose, entity) {
            continue;
        }
        let max = ctx.items.max_stack(item);
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(uuid) else {
            continue;
        };
        let stack = play::ItemStack {
            damage,
            ..play::ItemStack::new(item, count)
        };
        let (taken, changed) = hands.insert_stack(&stack, max);
        if taken == 0 {
            continue;
        }
        let slots = changed
            .into_iter()
            .map(|slot| (slot as i16, hands.slots[slot].clone()))
            .collect();
        drop(inventories);
        let _ = ctx.events.send(Event::ItemTaken {
            item_entity: entity.id,
            collector: *player_entity,
            count: taken,
            slots,
        });
        if taken >= count {
            entity.removed = true;
        } else {
            if let Kind::Item { count, .. } = &mut entity.kind {
                *count -= taken;
            }
            if let Some(p) = metadata(entity) {
                let _ = ctx.events.send(Event::Fx(crate::fx::Fx::Data(p)));
            }
        }
        return;
    }
}

/// Whether an item can still take more or join another stack.
fn mergeable(ctx: &ServerContext, e: &Entity) -> bool {
    match e.kind {
        Kind::Item {
            item,
            count,
            pickup_delay,
            ..
        } => !e.removed && pickup_delay != NEVER && count < ctx.items.max_stack(item),
        _ => false,
    }
}

/// Nearby equal stacks join: the smaller into the larger, which keeps the
/// longer pickup delay and the younger age. Items look for partners every
/// 40 ticks, or every 2 while moving between blocks. Returns the removed
/// items and the ones whose count changed.
pub fn merge(
    ctx: &ServerContext,
    entities: &mut [Entity],
    moved: &[i32],
) -> (Vec<usize>, Vec<usize>) {
    let (mut gone, mut grown) = (Vec::new(), Vec::new());
    for i in 0..entities.len() {
        let e = &entities[i];
        if !mergeable(ctx, e) {
            continue;
        }
        let period = if moved.contains(&e.id) { 2 } else { 40 };
        if e.age % period != 0 {
            continue;
        }
        let reach = {
            let b = e.aabb();
            mc_entity::Aabb {
                min: [b.min[0] - 0.5, b.min[1], b.min[2] - 0.5],
                max: [b.max[0] + 0.5, b.max[1], b.max[2] + 0.5],
            }
        };
        for j in 0..entities.len() {
            if i == j || !mergeable(ctx, &entities[i]) || !mergeable(ctx, &entities[j]) {
                continue;
            }
            let b = entities[j].aabb();
            if !(0..3).all(|a| b.min[a] <= reach.max[a] && b.max[a] >= reach.min[a]) {
                continue;
            }
            let (
                Kind::Item {
                    item: a_item,
                    count: a_count,
                    damage: a_damage,
                    ..
                },
                Kind::Item {
                    item: b_item,
                    count: b_count,
                    damage: b_damage,
                    ..
                },
            ) = (&entities[i].kind, &entities[j].kind)
            else {
                continue;
            };
            // Only stacks that fit together join.
            if (a_item, a_damage) != (b_item, b_damage)
                || a_count + b_count > ctx.items.max_stack(*a_item)
            {
                continue;
            }
            // The larger stack takes the smaller one.
            let (into, from) = if a_count < b_count { (j, i) } else { (i, j) };
            if join(ctx, entities, into, from) {
                gone.push(from);
            }
            if !grown.contains(&into) {
                grown.push(into);
            }
        }
    }
    grown.retain(|i| !gone.contains(i));
    (gone, grown)
}

/// Moves what fits from `from` onto `into`; true if `from` is used up.
fn join(ctx: &ServerContext, entities: &mut [Entity], into: usize, from: usize) -> bool {
    let (age, delay, taken) = {
        let Kind::Item {
            item,
            count: into_count,
            ..
        } = entities[into].kind
        else {
            return false;
        };
        let Kind::Item {
            count: from_count,
            pickup_delay,
            ..
        } = entities[from].kind
        else {
            return false;
        };
        let room = ctx.items.max_stack(item) - into_count;
        (
            entities[from].age,
            pickup_delay,
            room.min(from_count).max(0),
        )
    };
    if taken == 0 {
        return false;
    }
    let target = &mut entities[into];
    target.age = target.age.min(age);
    if let Kind::Item {
        count,
        pickup_delay,
        ..
    } = &mut target.kind
    {
        *count += taken;
        *pickup_delay = (*pickup_delay).max(delay);
    }
    let source = &mut entities[from];
    if let Kind::Item { count, .. } = &mut source.kind {
        *count -= taken;
        if *count <= 0 {
            source.removed = true;
            return true;
        }
    }
    false
}
