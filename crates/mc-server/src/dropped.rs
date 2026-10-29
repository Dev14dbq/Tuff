//! Dropped items (minecraft.wiki, "Item (entity)"): how they fall, float,
//! slide, burn, merge, despawn and get picked up, and when clients are told.
//! A nether star never despawns. An `Age` of -32768 never ages and never merges.
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
    // Flowing fluid carries it.
    let b = entity.aabb();
    crate::currents::push(ctx, b.min, b.max, &mut entity.vel);
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
    let pushed = push_out_of_blocks(ctx, entity);
    let horizontal = entity.vel[0] * entity.vel[0] + entity.vel[2] * entity.vel[2];
    // Resting items only check the ground every fourth tick.
    if !entity.on_ground
        || horizontal > 1.0e-5
        || (entity.age as i64 + i64::from(entity.id)) % 4 == 0
    {
        if pushed {
            // Stuck items pass through blocks on their way out.
            for (pos, vel) in entity.pos.iter_mut().zip(entity.vel) {
                *pos += vel;
            }
            entity.on_ground = false;
        } else {
            let ground = crate::mobs::Ground::new(ctx);
            let (moved, on_ground) = mc_entity::move_entity(entity.aabb(), entity.vel, &ground);
            for ((pos, vel), moved) in entity.pos.iter_mut().zip(&mut entity.vel).zip(moved) {
                *pos += moved;
                if (moved - *vel).abs() > 1.0e-9 {
                    *vel = 0.0;
                }
            }
            entity.on_ground = on_ground;
        }
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
    let ageless = matches!(entity.kind, Kind::Item { ageless: true, .. });
    let star = ctx.items.name(item) == Some("minecraft:nether_star");
    if !ageless {
        entity.age += 1;
    }
    if lava > 0.0 || in_fire(ctx, entity) {
        burn(ctx, entity, item, lava > 0.0);
    }
    if let Kind::Item { pickup_delay, .. } = &mut entity.kind
        && *pickup_delay != NEVER
        && *pickup_delay > 0
    {
        *pickup_delay -= 1;
    }
    if !ageless && !star && entity.age >= LIFETIME {
        entity.removed = true;
        return;
    }
    if entity.removed {
        return;
    }
    if pushed || entity.on_ground != was_on_ground || entity.age.is_multiple_of(UPDATE_INTERVAL) {
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
/// Only what has collision holds it (as clients see it): not a flower,
/// and a slab only in its lower half.
fn push_out_of_blocks(ctx: &ServerContext, entity: &mut Entity) -> bool {
    use mc_entity::Collide as _;
    let solid = crate::mobs::Ground::new(ctx);
    let centre = [
        entity.pos[0],
        (entity.aabb().min[1] + entity.aabb().max[1]) / 2.0,
        entity.pos[2],
    ];
    let at = centre.map(|c| c.floor() as i32);
    let up = centre[1] - f64::from(at[1]);
    if !solid
        .span(at[0], at[1], at[2])
        .is_some_and(|(lo, hi)| (lo..hi).contains(&up))
    {
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
        owner,
        thrower,
        ref components,
        ..
    } = entity.kind
    else {
        return;
    };
    let components = components.clone();
    // An owned item is its owner's until its last 10 seconds.
    let anyone = owner.is_none() || LIFETIME.saturating_sub(entity.age) <= 200;
    for (uuid, player_entity, mode, pose) in players {
        if *mode == 3 || !crate::tick::picks_up(pose, entity) || !anyone && owner != Some(*uuid) {
            continue;
        }
        let max = ctx.items.max_stack(item);
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(uuid) else {
            continue;
        };
        // The whole stack: enchantments, names and the rest come along.
        let stack = play::ItemStack {
            damage,
            components: components.clone(),
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
        crate::stats::item(ctx, *uuid, crate::stats::PICKED_UP, item, taken);
        if let Some(t) = thrower {
            crate::criteria::picked_up_thrown(ctx, *uuid, t, &stack);
        }
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
            ageless,
            ..
        } => {
            !e.removed
                && pickup_delay != NEVER
                && !ageless
                && (e.age < LIFETIME || ctx.items.name(item) == Some("minecraft:nether_star"))
                && count < ctx.items.max_stack(item)
        }
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
        if !e.age.is_multiple_of(period) {
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
                    owner: a_owner,
                    components: a_components,
                    ..
                },
                Kind::Item {
                    item: b_item,
                    count: b_count,
                    damage: b_damage,
                    owner: b_owner,
                    components: b_components,
                    ..
                },
            ) = (&entities[i].kind, &entities[j].kind)
            else {
                continue;
            };
            // Only stacks that fit together join.
            if (a_item, a_damage, a_owner) != (b_item, b_damage, b_owner)
                || a_components != b_components
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

#[cfg(test)]
mod tests {
    use super::*;
    use mc_protocol::Position;

    /// A superflat server context (ground surface at y = -60) with the
    /// chunks around the origin loaded, `None` without the game data.
    fn flat() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let ctx = ServerContext::new(crate::Config::new(dir), data).ok()?;
        for cx in -1..=1 {
            for cz in -1..=1 {
                let _ = ctx
                    .world
                    .block(Position::new(cx * 16, 0, cz * 16), &ctx.data.blocks);
            }
        }
        Some(ctx)
    }

    fn item(ctx: &ServerContext, name: &str, count: i32, pos: [f64; 3]) -> Entity {
        Entity {
            id: ctx.next_entity_id(),
            uuid: 1,
            kind: Kind::item(ctx.items.id(name).unwrap(), count, 0, 10),
            pos,
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }
    }

    /// Ticks one item `n` times; the rows clients got.
    fn run(ctx: &ServerContext, e: &mut Entity, n: usize) -> Vec<Sync> {
        let mut sync = Vec::new();
        for _ in 0..n {
            let level = WorldLevel::new(ctx);
            tick(ctx, &level, e, &[], &mut sync);
        }
        sync
    }

    fn set(ctx: &ServerContext, p: Position, block: &str) {
        let state = ctx.data.blocks.default_state(block).unwrap();
        WorldLevel::new(ctx).set_block(p, state);
    }

    #[test]
    fn a_picked_up_stack_keeps_its_enchantments_and_wear() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let sword = ctx.items.id("minecraft:diamond_sword").unwrap();
        let mut components = mc_protocol::components::Components::default();
        let mut levels = mc_nbt::Compound::new();
        levels.insert("minecraft:sharpness", mc_nbt::Tag::Int(5));
        components.set("enchantments", mc_nbt::Tag::Compound(levels));
        let stack = play::ItemStack {
            damage: 300,
            components: components.clone(),
            ..play::ItemStack::new(sword, 1)
        };
        let mut e = Entity {
            kind: Kind::stack(&stack, 0),
            ..item(&ctx, "minecraft:stone", 1, [0.5, -60.0, 0.5])
        };
        lock(&ctx.inventories).entry(9).or_default();
        let pose = crate::context::Pose {
            x: 0.5,
            y: -60.0,
            z: 0.5,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        };
        pick_up(&ctx, &mut e, &[(9, 90, 0, pose)]);
        assert!(e.removed, "not picked up");
        let got = lock(&ctx.inventories)[&9]
            .slots
            .iter()
            .flatten()
            .find(|s| s.item == sword)
            .cloned()
            .expect("no sword");
        assert_eq!(got.damage, 300);
        assert_eq!(got.components, components);
        // A plain sword and an enchanted one stay apart on the ground.
        let mut both = vec![
            Entity {
                kind: Kind::stack(&stack, 0),
                ..item(&ctx, "minecraft:stone", 1, [4.5, -60.0, 4.5])
            },
            Entity {
                kind: Kind::stack(
                    &play::ItemStack {
                        damage: 300,
                        ..play::ItemStack::new(sword, 1)
                    },
                    0,
                ),
                ..item(&ctx, "minecraft:stone", 1, [4.5, -60.0, 4.5])
            },
        ];
        let ids: Vec<i32> = both.iter().map(|e| e.id).collect();
        merge(&ctx, &mut both, &ids);
        assert!(both.iter().all(|e| !e.removed));
    }

    #[test]
    fn a_creative_slot_with_enchantments_keeps_them() {
        // The server's registries name the components.
        let Some(_ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // As a 26.3 client sends it: each component after its length.
        let body = [
            0x00, 0x29, 0x01, 0xa0, 0x08, 0x01, 0x00, 0x0d, 0x09, 0x03, 0x28, 0xff, 0x01, 0x17,
            0x01, 0x08, 0xff, 0x01,
        ];
        let packet = play::Serverbound::decode(
            "minecraft:set_creative_mode_slot",
            &mut mc_protocol::codec::Reader::new(&body),
        )
        .unwrap();
        let play::Serverbound::SetCreativeModeSlot {
            item: Some(item), ..
        } = packet
        else {
            panic!("{packet:?}");
        };
        let Some(mc_nbt::Tag::Compound(levels)) = item.components.get("enchantments") else {
            panic!("no enchantments: {:?}", item.components);
        };
        assert_eq!(levels.len(), 3, "{levels:?}");
        assert!(
            levels.iter().any(|(_, l)| *l == mc_nbt::Tag::Int(255)),
            "{levels:?}"
        );
        assert_eq!(item.damage, 0);
    }

    #[test]
    fn items_fall_land_and_are_synced_rarely() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = item(&ctx, "minecraft:stone", 1, [0.5, -50.0, 0.5]);
        run(&ctx, &mut e, 1);
        assert!((e.vel[1] + 0.04 * 0.98).abs() < 1e-9, "{:?}", e.vel);
        assert!((e.pos[1] + 50.04).abs() < 1e-9);
        let rows = run(&ctx, &mut e, 59);
        assert!(e.on_ground && (e.pos[1] + 60.0).abs() < 1e-9, "{:?}", e.pos);
        // Landing, and once every 20 ticks.
        assert!(rows.len() <= 4, "{} rows", rows.len());
        assert!(rows.iter().any(|r| r.on_ground));
    }

    #[test]
    fn items_slide_to_a_stop_faster_on_dirt_than_on_ice() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let slide = |ground: &str| {
            set(&ctx, Position::new(0, -61, 0), ground);
            let mut e = item(&ctx, "minecraft:stone", 1, [0.5, -60.0, 0.5]);
            e.on_ground = true;
            e.vel = [0.2, 0.0, 0.0];
            run(&ctx, &mut e, 1);
            e.vel[0]
        };
        assert!((slide("minecraft:dirt") - 0.2 * 0.6 * 0.98).abs() < 1e-9);
        assert!((slide("minecraft:ice") - 0.2 * 0.98 * 0.98).abs() < 1e-9);
    }

    #[test]
    fn items_float_up_in_water() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        for y in -60..-55 {
            set(&ctx, Position::new(3, y, 3), "minecraft:water");
        }
        let mut e = item(&ctx, "minecraft:stone", 1, [3.5, -59.0, 3.5]);
        run(&ctx, &mut e, 40);
        assert!(e.pos[1] > -59.0, "rises: {:?}", e.pos);
        assert!(e.vel[1] > 0.0 && e.vel[1] <= 0.0605);
    }

    #[test]
    fn lava_burns_items_but_not_netherite() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        set(&ctx, Position::new(-3, -60, -3), "minecraft:lava");
        let mut e = item(&ctx, "minecraft:stone", 1, [-2.5, -60.0, -2.5]);
        run(&ctx, &mut e, 2);
        assert!(e.removed, "burnt");
        let mut e = item(&ctx, "minecraft:netherite_ingot", 1, [-2.5, -60.0, -2.5]);
        run(&ctx, &mut e, 20);
        assert!(!e.removed, "netherite survives lava");
    }

    #[test]
    fn items_stuck_in_a_block_drift_out() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // Inside the dirt, near its top: the way out is up.
        let mut e = item(&ctx, "minecraft:stone", 1, [5.5, -61.3, 5.5]);
        let rows = run(&ctx, &mut e, 1);
        assert!(e.vel[1] > 0.05, "{:?}", e.vel);
        assert_eq!(rows.len(), 1, "a push clients cannot foresee");
    }

    #[test]
    fn items_fall_into_flowers_and_onto_slabs_as_clients_see_it() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // A flower has no collision: the item lies on the grass in its cell
        // and stays put, never pushed back up.
        set(&ctx, Position::new(8, -60, 8), "minecraft:dandelion");
        let mut e = item(&ctx, "minecraft:stone", 1, [8.5, -59.2, 8.5]);
        run(&ctx, &mut e, 40);
        assert!((e.pos[1] + 60.0).abs() < 1e-6, "{:?}", e.pos);
        let rows = run(&ctx, &mut e, 40);
        assert!((e.pos[1] + 60.0).abs() < 1e-6, "{:?}", e.pos);
        assert!(rows.iter().all(|r| r.pos == e.pos), "it moved: {rows:?}");
        // A bottom slab holds it half a block up.
        set(&ctx, Position::new(11, -60, 8), "minecraft:stone_slab");
        let mut e = item(&ctx, "minecraft:stone", 1, [11.5, -59.0, 8.5]);
        run(&ctx, &mut e, 40);
        assert!((e.pos[1] + 59.5).abs() < 1e-6, "{:?}", e.pos);
    }

    #[test]
    fn items_despawn_after_five_minutes() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = item(&ctx, "minecraft:stone", 1, [0.5, -60.0, 0.5]);
        e.age = LIFETIME - 2;
        run(&ctx, &mut e, 1);
        assert!(!e.removed);
        run(&ctx, &mut e, 1);
        assert!(e.removed);

        let mut star = item(&ctx, "minecraft:nether_star", 1, [2.5, -60.0, 0.5]);
        star.age = LIFETIME - 1;
        run(&ctx, &mut star, 3);
        assert!(!star.removed, "a nether star stays");
        assert!(star.age > LIFETIME);

        let mut pair = vec![
            item(&ctx, "minecraft:nether_star", 1, [4.5, -60.0, 0.5]),
            item(&ctx, "minecraft:nether_star", 1, [4.7, -60.0, 0.5]),
        ];
        pair[0].age = LIFETIME;
        pair[1].age = LIFETIME;
        let (gone, grown) = merge(&ctx, &mut pair, &[]);
        assert_eq!((gone, grown), (vec![1], vec![0]));
    }

    #[test]
    fn nearby_stacks_merge_into_the_larger() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut a = item(&ctx, "minecraft:stone", 3, [0.5, -60.0, 0.5]);
        a.age = 40;
        let mut b = item(&ctx, "minecraft:stone", 5, [0.9, -60.0, 0.5]);
        b.age = 80;
        if let Kind::Item { pickup_delay, .. } = &mut a.kind {
            *pickup_delay = 30;
        }
        let far = item(&ctx, "minecraft:stone", 1, [3.0, -60.0, 0.5]);
        let other = item(&ctx, "minecraft:dirt", 1, [0.6, -60.0, 0.5]);
        let mut all = vec![a, b, far, other];
        let (gone, grown) = merge(&ctx, &mut all, &[]);
        assert_eq!((gone, grown), (vec![0], vec![1]));
        assert!(all[0].removed);
        let Kind::Item {
            count,
            pickup_delay,
            ..
        } = all[1].kind
        else {
            panic!()
        };
        assert_eq!((count, pickup_delay, all[1].age), (8, 30, 40));
        // Full stacks do not join.
        let mut full = vec![
            item(&ctx, "minecraft:stone", 40, [0.5, -60.0, 0.5]),
            item(&ctx, "minecraft:stone", 40, [0.6, -60.0, 0.5]),
        ];
        assert_eq!(merge(&ctx, &mut full, &[]).0, Vec::<usize>::new());
    }

    #[test]
    fn items_are_saved_with_their_chunk_and_come_back() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        if mc_data::McData::load(&dir).is_err() {
            eprintln!("skipped: no game data");
            return;
        }
        let folder = std::env::temp_dir().join(format!("mc-items-{}", std::process::id()));
        let open = || {
            let mut config = crate::Config::new(dir.clone());
            config.world = Some(folder.clone());
            config.seed = Some(12345);
            ServerContext::new(config, mc_data::McData::load(&dir).unwrap()).unwrap()
        };
        let at = Position::new(40, 100, 40);
        let uuid = {
            let ctx = open();
            let _ = ctx.world.block(at, &ctx.data.blocks);
            crate::persist::load_stored(&ctx);
            let mut e = item(&ctx, "minecraft:diamond", 7, [40.5, 100.0, 40.5]);
            e.age = 1234;
            e.uuid = 0x1234_5678_9abc_def0_1122_3344_5566_7788;
            lock(&ctx.entities).push(e.clone());
            crate::save(&ctx);
            e.uuid
        };
        let ctx = open();
        let _ = ctx.world.block(at, &ctx.data.blocks);
        crate::persist::load_stored(&ctx);
        let back: Vec<Entity> = lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(e.kind, Kind::Item { .. }))
            .cloned()
            .collect();
        assert_eq!(back.len(), 1, "{back:?}");
        let e = &back[0];
        assert_eq!((e.uuid, e.age, e.pos), (uuid, 1234, [40.5, 100.0, 40.5]));
        assert!(matches!(e.kind, Kind::Item { count: 7, .. }));
        // Unloading writes the chunk's items and takes them out of the world;
        // loading the chunk again brings them back, once.
        crate::save(&ctx);
        let dropped = ctx.world.unload_unused();
        assert!(dropped.contains(&(2, 2)), "{dropped:?}");
        crate::persist::unloaded(&ctx, &dropped);
        assert!(lock(&ctx.entities).iter().all(|e| e.uuid != uuid));
        let _ = ctx.world.block(at, &ctx.data.blocks);
        let _ = ctx.world.block(at, &ctx.data.blocks);
        crate::persist::load_stored(&ctx);
        let n = lock(&ctx.entities)
            .iter()
            .filter(|e| e.uuid == uuid)
            .count();
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(n, 1);
    }
}
