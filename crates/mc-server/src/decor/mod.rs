//! Item frames, paintings, armor stands and lead knots: placing them,
//! using and hitting them, and what happens when what they hang on goes.
//!
//! Frames and paintings hang on the face of a block (`Hung::anchor` is that
//! block, `facing` the way the face points); they come off when the block
//! does. Armor stands stand where they are put and fall when the ground goes.
//! Follows minecraft.wiki ("Item Frame", "Armor Stand", "Painting", "Lead").

pub mod leash;

use mc_entity::extra::{Decor, Frame, Hung, Stand};
use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::{MetadataValue, SetEntityData};

use crate::blocks::{FACES, offset};
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held;
use crate::hits::{self, Hit, Tick};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Centre of a frame or painting in `cell` against its wall.
pub fn hung_pos(cell: [i32; 3], facing: u8) -> [f64; 3] {
    let (dx, dy, dz) = FACES[usize::from(facing)];
    [
        f64::from(cell[0]) + 0.5 - f64::from(dx) * 0.46875,
        f64::from(cell[1]) + 0.5 - f64::from(dy) * 0.46875,
        f64::from(cell[2]) + 0.5 - f64::from(dz) * 0.46875,
    ]
}

fn entity(ctx: &ServerContext, pos: [f64; 3], hung: Hung) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Hung(hung),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Adds entities to the world.
pub fn spawn_all(ctx: &ServerContext, fresh: Vec<Entity>) {
    for e in fresh {
        let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
        lock(&ctx.entities).push(e);
    }
}

/// A stack lying in the world, ready to be added.
pub fn dropped(
    ctx: &ServerContext,
    pos: [f64; 3],
    item: &str,
    count: i32,
    damage: i32,
) -> Option<Entity> {
    let id = ctx.items.id(item)?;
    let r = |s: f64| hits::rnd() * s - s / 2.0;
    Some(Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::item(id, count, damage, 10),
        pos,
        vel: [r(0.2), 0.2, r(0.2)],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    })
}

fn dropped_id(ctx: &ServerContext, pos: [f64; 3], item: i32, damage: i32) -> Option<Entity> {
    let name = ctx.items.name(item)?.to_owned();
    dropped(ctx, pos, &name, 1, damage)
}

/// Whether a frame or painting already hangs in `cell` facing `facing`.
fn taken(ctx: &ServerContext, pos: [f64; 3], facing: u8) -> bool {
    lock(&ctx.entities).iter().any(|e| {
        !e.removed
            && matches!(&e.kind, Kind::Hung(h) if h.facing == facing
                && matches!(h.decor, Decor::Frame(_) | Decor::Painting { .. }))
            && (0..3).all(|i| (e.pos[i] - pos[i]).abs() < 0.01)
    })
}

fn is_fence(ctx: &ServerContext, p: Position) -> bool {
    hits::block_at(ctx, [p.x, p.y, p.z])
        .and_then(|s| ctx.data.blocks.block_name(s))
        .is_some_and(|n| n.ends_with("_fence"))
}

/// Which equipment slot an item goes in on an armor stand: 2 feet, 3 legs,
/// 4 chest, 5 head, else the main hand.
pub fn slot_for(item: &str) -> usize {
    let n = item.trim_start_matches("minecraft:");
    if n.ends_with("_helmet")
        || n.ends_with("_head")
        || n.ends_with("_skull")
        || n == "carved_pumpkin"
    {
        5
    } else if n.ends_with("_chestplate") || n == "elytra" {
        4
    } else if n.ends_with("_leggings") {
        3
    } else if n.ends_with("_boots") {
        2
    } else {
        0
    }
}

/// A right click on a block with an item that places one of these.
/// Returns whether it did (the click is then used up).
#[allow(clippy::too_many_arguments)]
pub fn place(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    name: &str,
    clicked: Position,
    face: usize,
) -> bool {
    let cell = offset(clicked, face);
    let cell = [cell.x, cell.y, cell.z];
    let wall = [clicked.x, clicked.y, clicked.z];
    let face8 = face as u8;
    let (hung, sound) = match name {
        "minecraft:item_frame" | "minecraft:glow_item_frame" => (
            Hung {
                decor: Decor::Frame(Frame {
                    glow: name == "minecraft:glow_item_frame",
                    item: None,
                    rotation: 0,
                    invisible: false,
                    fixed: false,
                }),
                facing: face8,
                yaw: 0.0,
                anchor: wall,
            },
            "entity.item_frame.place",
        ),
        "minecraft:painting" if face >= 2 => (
            Hung {
                decor: Decor::Painting {
                    variant: pick_painting(),
                    size: (1, 1),
                },
                facing: face8,
                yaw: 0.0,
                anchor: wall,
            },
            "entity.painting.place",
        ),
        "minecraft:armor_stand" => {
            // Stands stand in the block clicked into, two tall, turned to
            // face the player in eighths of a turn.
            let head = [cell[0], cell[1] + 1, cell[2]];
            if hits::stops(ctx, cell) || hits::stops(ctx, head) {
                return false;
            }
            let yaw = ((actor.pose.yaw - 180.0 + 22.5) / 45.0).floor() * 45.0;
            let pos = [
                f64::from(cell[0]) + 0.5,
                f64::from(cell[1]),
                f64::from(cell[2]) + 0.5,
            ];
            let stand = entity(
                ctx,
                pos,
                Hung {
                    decor: Decor::Stand(Stand::default()),
                    facing: 0,
                    yaw,
                    anchor: cell,
                },
            );
            return finish(ctx, outcome, actor, hand, stand, "entity.armor_stand.place");
        }
        "minecraft:lead" if is_fence(ctx, clicked) => {
            return leash::tie_to_fence(ctx, outcome, actor, hand, clicked);
        }
        _ => return false,
    };
    // Hangs on a solid block in free space.
    if !hits::stops(ctx, wall) || hits::stops(ctx, cell) {
        return false;
    }
    let pos = hung_pos(cell, face8);
    if taken(ctx, pos, face8) {
        return false;
    }
    finish(ctx, outcome, actor, hand, entity(ctx, pos, hung), sound)
}

/// The painting a player gets: any of the single-block ones at random.
fn pick_painting() -> String {
    const SMALL: [&str; 7] = [
        "kebab",
        "aztec",
        "alban",
        "aztec2",
        "bomb",
        "plant",
        "wasteland",
    ];
    format!(
        "minecraft:{}",
        SMALL[(hits::rnd() * SMALL.len() as f64) as usize]
    )
}

fn finish(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    e: Entity,
    sound: &str,
) -> bool {
    fx::sound(
        ctx,
        sound,
        fx::category::BLOCKS,
        e.pos,
        1.0,
        0.8 + hits::rnd() as f32 * 0.2,
    );
    spawn_all(ctx, vec![e]);
    if actor.game_mode != 1
        && let Some(h) = held::held(ctx, actor.uuid, hand)
    {
        held::consume(ctx, actor.uuid, h.slot, 1);
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    true
}

/// A frame's metadata after it changed.
fn frame_data(e: &Entity, f: &Frame) -> Event {
    Event::EntitiesData(vec![SetEntityData {
        entity_id: e.id,
        entries: vec![
            (
                8,
                MetadataValue::Item(f.item.map(|(item, damage)| {
                    mc_protocol::packets::play::ItemStack {
                        damage,
                        ..mc_protocol::packets::play::ItemStack::new(item, 1)
                    }
                })),
            ),
            (9, MetadataValue::VarInt(i32::from(f.rotation))),
        ],
    }])
}

/// An armor stand's gear after it changed.
fn gear_packet(e: &Entity, s: &Stand) -> Event {
    let slots = s
        .gear
        .iter()
        .enumerate()
        .map(|(i, g)| {
            (
                i as u8,
                g.map(|(item, damage)| mc_protocol::packets::play::ItemStack {
                    damage,
                    ..mc_protocol::packets::play::ItemStack::new(item, 1)
                }),
            )
        })
        .collect();
    Event::Fx(fx::Fx::Equip(mc_protocol::packets::play::SetEquipment {
        entity_id: e.id,
        slots,
    }))
}

/// Takes the entity out of the world, with what it held dropped.
pub fn break_hung(ctx: &ServerContext, e: &mut Entity, fresh: &mut Vec<Entity>, drop_self: bool) {
    let Kind::Hung(h) = &e.kind else {
        return;
    };
    let at = e.pos;
    let (self_item, sound): (Option<&str>, &str) = match &h.decor {
        Decor::Frame(f) => {
            if let Some((item, damage)) = f.item {
                fresh.extend(dropped_id(ctx, at, item, damage));
            }
            (
                Some(if f.glow {
                    "minecraft:glow_item_frame"
                } else {
                    "minecraft:item_frame"
                }),
                "entity.item_frame.break",
            )
        }
        Decor::Painting { .. } => (Some("minecraft:painting"), "entity.painting.break"),
        Decor::Stand(s) => {
            for (item, damage) in s.gear.iter().flatten() {
                fresh.extend(dropped_id(ctx, at, *item, *damage));
            }
            (Some("minecraft:armor_stand"), "entity.armor_stand.break")
        }
        Decor::LeashKnot => (None, "entity.leash_knot.break"),
    };
    if drop_self && let Some(name) = self_item {
        fresh.extend(dropped(ctx, at, name, 1, 0));
    }
    if matches!(h.decor, Decor::LeashKnot) {
        leash::knot_gone(ctx, e.id);
    }
    fx::sound(ctx, sound, fx::category::BLOCKS, at, 1.0, 1.0);
    e.removed = true;
}

/// One tick of a hanging entity.
pub fn tick(t: &mut Tick, e: &mut Entity) {
    if e.removed {
        return;
    }
    let Kind::Hung(h) = &e.kind else {
        return;
    };
    e.age += 1;
    match &h.decor {
        Decor::Frame(_) | Decor::Painting { .. } | Decor::LeashKnot => {
            // Checked once a second: does the wall hold?
            if e.age.is_multiple_of(20)
                && !hits::stops(t.ctx, h.anchor)
                && !matches!(h.decor, Decor::LeashKnot)
            {
                let mut fresh = Vec::new();
                break_hung(t.ctx, e, &mut fresh, true);
                t.out.spawn.extend(fresh);
            } else if e.age.is_multiple_of(20)
                && matches!(h.decor, Decor::LeashKnot)
                && !is_fence(t.ctx, Position::new(h.anchor[0], h.anchor[1], h.anchor[2]))
            {
                let mut fresh = Vec::new();
                break_hung(t.ctx, e, &mut fresh, false);
                t.out.spawn.extend(fresh);
            }
        }
        Decor::Stand(s) => {
            if s.marker {
                return;
            }
            let ctx = t.ctx;
            let below = [
                e.pos[0].floor() as i32,
                (e.pos[1] - 0.01).floor() as i32,
                e.pos[2].floor() as i32,
            ];
            let grounded = hits::stops(ctx, below);
            if grounded && e.vel[1] <= 0.0 {
                if e.vel != [0.0; 3] || e.on_ground != grounded {
                    // Landed: rests on top of the block.
                    e.pos[1] = f64::from(below[1] + 1);
                    e.vel = [0.0; 3];
                    e.on_ground = true;
                    sync(t, e);
                }
                return;
            }
            e.on_ground = false;
            e.vel[1] = (e.vel[1] - 0.04) * 0.98;
            e.pos[1] += e.vel[1];
            if e.pos[1] < -80.0 {
                e.removed = true;
                return;
            }
            sync(t, e);
        }
    }
}

fn sync(t: &mut Tick, e: &Entity) {
    let Kind::Hung(h) = &e.kind else {
        return;
    };
    t.out.sync.push(fx::Sync {
        id: e.id,
        pos: e.pos,
        vel: e.vel,
        yaw: h.yaw,
        pitch: 0.0,
        on_ground: e.on_ground,
    });
}

/// Something hit it (a projectile, an explosion): frames lose their item
/// and then break, stands take damage.
pub fn hurt(ctx: &ServerContext, e: &mut Entity, hit: &Hit, fresh: &mut Vec<Entity>) -> Vec<Event> {
    let Kind::Hung(h) = &mut e.kind else {
        return Vec::new();
    };
    let mut events = Vec::new();
    let now = lock(&ctx.time).age;
    match &mut h.decor {
        Decor::Frame(f) => {
            if f.fixed {
                return events;
            }
            if let Some((item, damage)) = f.item.take() {
                fresh.extend(dropped_id(ctx, e.pos, item, damage));
                fx::sound(
                    ctx,
                    "entity.item_frame.remove_item",
                    fx::category::BLOCKS,
                    e.pos,
                    1.0,
                    1.0,
                );
                let f = f.clone();
                events.push(frame_data(e, &f));
            } else {
                break_hung(ctx, e, fresh, true);
            }
        }
        Decor::Painting { .. } => break_hung(ctx, e, fresh, true),
        Decor::Stand(s) => {
            if s.marker || hit.amount <= 0.0 {
                return events;
            }
            s.health -= hit.amount;
            s.last_hit = now;
            if s.health <= 0.0 {
                break_hung(ctx, e, fresh, true);
            } else {
                fx::status(ctx, e.id, 32);
            }
        }
        Decor::LeashKnot => {}
    }
    events
}

/// A player clicks a hanging entity.
#[allow(clippy::too_many_arguments)]
pub fn interact(
    ctx: &ServerContext,
    actor: Actor,
    e: &mut Entity,
    hand: i32,
    attack: bool,
    at: Option<[f64; 3]>,
    outcome: &mut Outcome,
    fresh: &mut Vec<Entity>,
) {
    let Kind::Hung(h) = &mut e.kind else {
        return;
    };
    let creative = actor.game_mode == 1;
    let held = held::held(ctx, actor.uuid, hand);
    let now = lock(&ctx.time).age;
    match &mut h.decor {
        Decor::Frame(f) if attack => {
            if f.fixed {
                return;
            }
            if let Some((item, damage)) = f.item.take() {
                if !creative {
                    fresh.extend(dropped_id(ctx, e.pos, item, damage));
                }
                fx::sound(
                    ctx,
                    "entity.item_frame.remove_item",
                    fx::category::BLOCKS,
                    e.pos,
                    1.0,
                    1.0,
                );
                let f = f.clone();
                outcome.events.push(frame_data(e, &f));
            } else {
                break_hung(ctx, e, fresh, !creative);
            }
        }
        Decor::Frame(f) => {
            if f.item.is_some() {
                // Another eighth of a turn.
                f.rotation = (f.rotation + 1) % 8;
                fx::sound(
                    ctx,
                    "entity.item_frame.rotate_item",
                    fx::category::BLOCKS,
                    e.pos,
                    1.0,
                    1.0,
                );
            } else if let Some(held) = held {
                f.item = Some((held.stack.item, held.stack.damage));
                if !creative {
                    held::consume(ctx, actor.uuid, held.slot, 1);
                }
                fx::sound(
                    ctx,
                    "entity.item_frame.add_item",
                    fx::category::BLOCKS,
                    e.pos,
                    1.0,
                    1.0,
                );
            } else {
                return;
            }
            let f = f.clone();
            outcome.events.push(frame_data(e, &f));
        }
        Decor::Painting { .. } if attack => break_hung(ctx, e, fresh, !creative),
        Decor::Stand(s) if attack => {
            if s.marker {
                return;
            }
            // Creative players break it at once; others hit it twice
            // within five ticks.
            if creative || now - s.last_hit <= 5 {
                break_hung(ctx, e, fresh, !creative);
            } else {
                s.last_hit = now;
                fx::status(ctx, e.id, 32);
            }
        }
        Decor::Stand(s) => {
            if s.marker {
                return;
            }
            let rel = at.map_or(1.0, |a| a[1] - e.pos[1]);
            let slot = match &held {
                Some(h) => slot_for(&h.name),
                None => click_slot(s, rel),
            };
            let new = held.as_ref().map(|h| (h.stack.item, h.stack.damage));
            if new.is_none() && s.gear[slot].is_none() {
                return;
            }
            let old = std::mem::replace(&mut s.gear[slot], new);
            if let Some(h) = &held
                && !creative
            {
                held::consume(ctx, actor.uuid, h.slot, 1);
            }
            // The old piece goes to the player's hands, or to the floor.
            if let Some((item, damage)) = old {
                let stack = mc_protocol::packets::play::ItemStack {
                    damage,
                    ..mc_protocol::packets::play::ItemStack::new(item, 1)
                };
                if held::give(ctx, actor.uuid, &stack) > 0 {
                    fresh.extend(dropped_id(ctx, e.pos, item, damage));
                }
            }
            fx::sound(
                ctx,
                "entity.armor_stand.hit",
                fx::category::BLOCKS,
                e.pos,
                1.0,
                1.0,
            );
            let s = s.clone();
            outcome.events.push(gear_packet(e, &s));
        }
        Decor::LeashKnot if !attack => {
            // Using a knot frees what is tied to it.
            leash::untie_knot(ctx, outcome, actor, e.id);
        }
        _ => {}
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
}

/// Which gear slot a click at height `rel` above the stand's feet is on.
fn click_slot(s: &Stand, rel: f64) -> usize {
    let k = if s.small { 0.5 } else { 1.0 };
    if rel >= 0.1 * k && rel < 0.1 * k + 0.45 * k && s.gear[2].is_some() {
        2
    } else if rel >= 0.9 * k && rel < 0.9 * k + 0.6 * k && s.gear[4].is_some() {
        4
    } else if rel >= 0.4 * k && rel < 0.4 * k + 0.8 * k && s.gear[3].is_some() {
        3
    } else if rel >= 1.6 * k && s.gear[5].is_some() {
        5
    } else if s.gear[0].is_some() {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;

    fn actor(mode: i32) -> Actor {
        Actor {
            uuid: 7,
            entity_id: 1,
            game_mode: mode,
            pose: crate::context::Pose {
                x: 0.5,
                y: -60.0,
                z: 0.5,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
            sneaking: false,
        }
    }

    fn hung(ctx: &ServerContext) -> Vec<Entity> {
        lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(e.kind, Kind::Hung(_)))
            .cloned()
            .collect()
    }

    #[test]
    fn a_frame_hangs_flat_on_the_wall_it_is_put_on() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut outcome = Outcome::default();
        // The top of the flat world's ground, y = -61 is grass.
        let ok = place(
            &ctx,
            &mut outcome,
            actor(1),
            0,
            "minecraft:item_frame",
            Position::new(0, -61, 0),
            1,
        );
        assert!(ok);
        let [e] = &hung(&ctx)[..] else { panic!() };
        assert!((e.pos[1] - (-60.0 + 0.03125)).abs() < 1e-9, "{:?}", e.pos);
        let b = e.aabb();
        assert!(b.max[1] - b.min[1] < 0.07 && b.max[0] - b.min[0] > 0.7);
        // The same spot is taken.
        assert!(!place(
            &ctx,
            &mut outcome,
            actor(1),
            0,
            "minecraft:item_frame",
            Position::new(0, -61, 0),
            1
        ));
        // Nothing to hang on in the air.
        assert!(!place(
            &ctx,
            &mut outcome,
            actor(1),
            0,
            "minecraft:item_frame",
            Position::new(0, -50, 0),
            1
        ));
    }

    #[test]
    fn a_frame_takes_an_item_turns_and_breaks() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut outcome = Outcome::default();
        assert!(place(
            &ctx,
            &mut outcome,
            actor(1),
            0,
            "minecraft:item_frame",
            Position::new(3, -61, 3),
            1
        ));
        let mut e = hung(&ctx).remove(0);
        let stone = ctx.items.id("minecraft:stone").unwrap();
        lock(&ctx.inventories).insert(7, {
            let mut h = crate::interact::Hands::default();
            h.slots[36] = Some(mc_protocol::packets::play::ItemStack::new(stone, 5));
            h
        });
        let mut fresh = Vec::new();
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            false,
            None,
            &mut outcome,
            &mut fresh,
        );
        let Kind::Hung(h) = &e.kind else { panic!() };
        let Decor::Frame(f) = &h.decor else { panic!() };
        assert_eq!(f.item, Some((stone, 0)));
        assert_eq!(
            lock(&ctx.inventories)[&7].slots[36].as_ref().unwrap().count,
            4
        );
        // Using it again turns the item.
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            false,
            None,
            &mut outcome,
            &mut fresh,
        );
        let Kind::Hung(h) = &e.kind else { panic!() };
        let Decor::Frame(f) = &h.decor else { panic!() };
        assert_eq!(f.rotation, 1);
        // A punch takes the item out, the next breaks the frame.
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            true,
            None,
            &mut outcome,
            &mut fresh,
        );
        assert!(!e.removed && fresh.len() == 1);
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            true,
            None,
            &mut outcome,
            &mut fresh,
        );
        assert!(e.removed && fresh.len() == 2);
    }

    #[test]
    fn armor_stands_wear_what_they_are_given() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut outcome = Outcome::default();
        assert!(place(
            &ctx,
            &mut outcome,
            actor(1),
            0,
            "minecraft:armor_stand",
            Position::new(5, -61, 5),
            1
        ));
        let mut e = hung(&ctx).remove(0);
        assert_eq!(e.pos, [5.5, -60.0, 5.5]);
        let helmet = ctx.items.id("minecraft:iron_helmet").unwrap();
        lock(&ctx.inventories).insert(7, {
            let mut h = crate::interact::Hands::default();
            h.slots[36] = Some(mc_protocol::packets::play::ItemStack::new(helmet, 1));
            h
        });
        let mut fresh = Vec::new();
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            false,
            Some([5.5, -58.5, 5.5]),
            &mut outcome,
            &mut fresh,
        );
        let Kind::Hung(h) = &e.kind else { panic!() };
        let Decor::Stand(s) = &h.decor else { panic!() };
        assert_eq!(s.gear[5], Some((helmet, 0)));
        assert!(lock(&ctx.inventories)[&7].slots[36].is_none());
        // An empty hand takes it back by where the click lands.
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            false,
            Some([5.5, -58.2, 5.5]),
            &mut outcome,
            &mut fresh,
        );
        let Kind::Hung(h) = &e.kind else { panic!() };
        let Decor::Stand(s) = &h.decor else { panic!() };
        assert_eq!(s.gear[5], None);
        // The first punch wobbles it, the second breaks it.
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            true,
            None,
            &mut outcome,
            &mut fresh,
        );
        assert!(!e.removed);
        interact(
            &ctx,
            actor(0),
            &mut e,
            0,
            true,
            None,
            &mut outcome,
            &mut fresh,
        );
        assert!(e.removed);
    }

    #[test]
    fn gear_goes_to_the_right_slot() {
        assert_eq!(slot_for("minecraft:iron_helmet"), 5);
        assert_eq!(slot_for("minecraft:diamond_chestplate"), 4);
        assert_eq!(slot_for("minecraft:leather_leggings"), 3);
        assert_eq!(slot_for("minecraft:golden_boots"), 2);
        assert_eq!(slot_for("minecraft:stick"), 0);
    }
}
