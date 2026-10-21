//! Boats and minecarts: putting them down, getting in and out, hitting
//! them, and what they do each tick (`boat`, `cart`).

pub mod boat;
pub mod cart;

use mc_entity::extra::{Cart, Craft, Vehicle};
use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::SetPassengers;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::hits::{self, Hit, Tick};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Something a player's client says about the vehicle they ride.
#[derive(Clone, Copy, Debug)]
pub enum Act {
    /// Where it is now (boats are steered by the client).
    Move { pos: [f64; 3], yaw: f32 },
    /// The player asks to get off.
    Dismount,
}

fn craft_of(item: &str) -> Option<Craft> {
    let n = item.trim_start_matches("minecraft:");
    Some(if let Some(w) = n.strip_suffix("_chest_boat") {
        Craft::Boat {
            wood: w.into(),
            chest: true,
        }
    } else if let Some(w) = n.strip_suffix("_boat") {
        Craft::Boat {
            wood: w.into(),
            chest: false,
        }
    } else if n == "bamboo_raft" {
        Craft::Boat {
            wood: "bamboo".into(),
            chest: false,
        }
    } else if n == "bamboo_chest_raft" {
        Craft::Boat {
            wood: "bamboo".into(),
            chest: true,
        }
    } else {
        Craft::Minecart(match n {
            "minecart" => Cart::Rideable,
            "chest_minecart" => Cart::Chest,
            "furnace_minecart" => Cart::Furnace,
            "hopper_minecart" => Cart::Hopper,
            "tnt_minecart" => Cart::Tnt,
            _ => return None,
        })
    })
}

/// The item a vehicle is put down with (and drops as).
pub fn item_of(craft: &Craft) -> String {
    match craft {
        Craft::Boat { wood, chest } => match (wood.as_str(), chest) {
            ("bamboo", false) => "minecraft:bamboo_raft".into(),
            ("bamboo", true) => "minecraft:bamboo_chest_raft".into(),
            (w, false) => format!("minecraft:{w}_boat"),
            (w, true) => format!("minecraft:{w}_chest_boat"),
        },
        Craft::Minecart(c) => match c {
            Cart::Rideable => "minecraft:minecart",
            Cart::Chest => "minecraft:chest_minecart",
            Cart::Furnace => "minecraft:furnace_minecart",
            Cart::Hopper => "minecraft:hopper_minecart",
            Cart::Tnt => "minecraft:tnt_minecart",
        }
        .into(),
    }
}

fn capacity(craft: &Craft) -> usize {
    match craft {
        Craft::Boat { chest, .. } => boat::seats(*chest),
        Craft::Minecart(_) => 1,
    }
}

pub(crate) fn new(ctx: &ServerContext, craft: Craft, pos: [f64; 3], yaw: f32) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Vehicle(Vehicle::new(craft, yaw)),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

fn used(ctx: &ServerContext, outcome: &mut Outcome, actor: Actor, hand: i32, e: Entity) {
    crate::decor::spawn_all(ctx, vec![e]);
    if actor.game_mode != 1
        && let Some(h) = crate::held::held(ctx, actor.uuid, hand)
    {
        crate::held::consume(ctx, actor.uuid, h.slot, 1);
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
}

/// A minecart used on a rail is put on it.
pub fn place_cart(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    name: &str,
    clicked: Position,
) -> bool {
    let Some(craft @ Craft::Minecart(_)) = craft_of(name) else {
        return false;
    };
    let c = [clicked.x, clicked.y, clicked.z];
    let Some(rail) = cart::find_rail(
        ctx,
        [
            f64::from(c[0]) + 0.5,
            f64::from(c[1]) + 0.5,
            f64::from(c[2]) + 0.5,
        ],
    ) else {
        return false;
    };
    if rail.cell != c {
        return false;
    }
    let pos = [
        f64::from(c[0]) + 0.5,
        f64::from(c[1]) + 0.0625,
        f64::from(c[2]) + 0.5,
    ];
    used(
        ctx,
        outcome,
        actor,
        hand,
        new(ctx, craft, pos, actor.pose.yaw),
    );
    fx::sound(
        ctx,
        "entity.minecart.inside",
        fx::category::NEUTRAL,
        pos,
        0.1,
        1.0,
    );
    true
}

/// A boat used in the air: put on the water (or ground) looked at.
pub fn place_boat(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    name: &str,
    yaw: f32,
    pitch: f32,
) -> bool {
    let Some(craft @ Craft::Boat { .. }) = craft_of(name) else {
        return false;
    };
    let Some((hit, face)) = crate::interact::look_at(ctx, actor, yaw, pitch, true) else {
        return false;
    };
    let at = [hit.x, hit.y, hit.z];
    let pos = if hits::in_water(
        ctx,
        [
            f64::from(at[0]) + 0.5,
            f64::from(at[1]) + 0.5,
            f64::from(at[2]) + 0.5,
        ],
    ) {
        [
            f64::from(at[0]) + 0.5,
            f64::from(at[1]) + 0.8,
            f64::from(at[2]) + 0.5,
        ]
    } else {
        let cell = crate::blocks::offset(hit, face);
        [
            f64::from(cell.x) + 0.5,
            f64::from(cell.y),
            f64::from(cell.z) + 0.5,
        ]
    };
    used(ctx, outcome, actor, hand, new(ctx, craft, pos, yaw));
    true
}

fn passengers(ctx: &ServerContext, vehicle: i32, riders: &[i32]) {
    fx::emit(
        ctx,
        fx::Fx::Passengers(SetPassengers {
            vehicle,
            passengers: riders.to_vec(),
        }),
    );
}

/// Everyone gets off.
pub fn eject(ctx: &ServerContext, id: i32, v: &mut Vehicle) {
    if v.riders.is_empty() {
        return;
    }
    let riders = std::mem::take(&mut v.riders);
    passengers(ctx, id, &[]);
    let _ = riders;
}

fn rider_uuid(ctx: &ServerContext, entity_id: i32) -> Option<u128> {
    lock(&ctx.players)
        .values()
        .find(|p| p.entity_id == entity_id)
        .map(|p| p.profile.uuid)
}

/// One tick of a vehicle.
pub fn tick(t: &mut Tick, e: &mut Entity) {
    if e.removed {
        return;
    }
    let ctx = t.ctx;
    // Riders who left the game get off.
    if let Kind::Vehicle(v) = &mut e.kind {
        let before = v.riders.len();
        v.riders.retain(|&r| rider_uuid(ctx, r).is_some());
        if v.riders.len() != before {
            passengers(ctx, e.id, &v.riders.clone());
        }
    }
    let craft = match &e.kind {
        Kind::Vehicle(v) => v.craft.clone(),
        _ => return,
    };
    match craft {
        Craft::Boat { .. } => boat::tick(t, e),
        Craft::Minecart(_) => cart::tick(t, e),
    }
    // A cart carries its rider along (boats are told by the rider).
    if let Kind::Vehicle(v) = &e.kind
        && matches!(v.craft, Craft::Minecart(_))
    {
        for &r in &v.riders {
            if let Some(uuid) = rider_uuid(ctx, r)
                && let Some(p) = lock(&ctx.players).get_mut(&uuid)
                && let Some(pose) = &mut p.pose
            {
                (pose.x, pose.y, pose.z) = (e.pos[0], e.pos[1], e.pos[2]);
            }
        }
    }
}

/// Destroys a vehicle: riders get off, it drops as an item.
pub fn destroy(ctx: &ServerContext, e: &mut Entity, fresh: &mut Vec<Entity>, drop_item: bool) {
    let Kind::Vehicle(v) = &mut e.kind else {
        return;
    };
    eject(ctx, e.id, v);
    if drop_item {
        fresh.extend(crate::decor::dropped(ctx, e.pos, &item_of(&v.craft), 1, 0));
    }
    e.removed = true;
}

/// Hurt by a projectile or a punch: the damage builds up, and past 40 the
/// vehicle breaks.
pub fn hurt(ctx: &ServerContext, e: &mut Entity, hit: &Hit, fresh: &mut Vec<Entity>) -> Vec<Event> {
    let Kind::Vehicle(v) = &mut e.kind else {
        return Vec::new();
    };
    v.damage += hit.amount * 10.0;
    v.wobble = 10;
    if v.damage > 40.0 {
        destroy(ctx, e, fresh, true);
    }
    Vec::new()
}

/// A player clicks a vehicle.
#[allow(clippy::too_many_arguments)]
pub fn interact(
    ctx: &ServerContext,
    actor: Actor,
    e: &mut Entity,
    hand: i32,
    attack: bool,
    outcome: &mut Outcome,
    fresh: &mut Vec<Entity>,
) {
    let id = e.id;
    if attack {
        let amount = {
            let held = crate::held::held(ctx, actor.uuid, 0);
            crate::vitals::melee_damage(ctx, held.map(|h| h.stack.item))
        };
        let Kind::Vehicle(v) = &mut e.kind else {
            return;
        };
        if actor.game_mode == 1 {
            destroy(ctx, e, fresh, false);
        } else {
            v.damage += amount * 10.0;
            v.wobble = 10;
            if v.damage > 40.0 {
                destroy(ctx, e, fresh, true);
            }
        }
        return;
    }
    if actor.sneaking {
        return;
    }
    let Kind::Vehicle(v) = &mut e.kind else {
        return;
    };
    // Coal feeds a furnace cart.
    if v.craft == Craft::Minecart(Cart::Furnace)
        && let Some(h) = crate::held::held(ctx, actor.uuid, hand)
        && matches!(h.name.as_str(), "minecraft:coal" | "minecraft:charcoal")
    {
        v.fuel += 3600;
        if actor.game_mode != 1 {
            crate::held::consume(ctx, actor.uuid, h.slot, 1);
        }
        return;
    }
    // Flint and steel lights a TNT cart.
    if v.craft == Craft::Minecart(Cart::Tnt)
        && v.fuse < 0
        && crate::held::held(ctx, actor.uuid, hand)
            .is_some_and(|h| h.name == "minecraft:flint_and_steel")
    {
        v.fuse = 80;
        fx::sound(
            ctx,
            "entity.tnt.primed",
            fx::category::BLOCKS,
            e.pos,
            1.0,
            1.0,
        );
        return;
    }
    if matches!(v.craft, Craft::Minecart(Cart::Chest | Cart::Hopper)) {
        return;
    }
    if v.riders.len() < capacity(&v.craft) && !v.riders.contains(&actor.entity_id) {
        v.riders.push(actor.entity_id);
        passengers(ctx, id, &v.riders.clone());
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
    }
}

/// A rider's own word about their vehicle.
pub fn act(ctx: &ServerContext, actor: Actor, act: Act) -> Outcome {
    let mut entities = lock(&ctx.entities);
    let Some(e) = entities.iter_mut().find(|e| {
        matches!(&e.kind, Kind::Vehicle(v) if v.riders.contains(&actor.entity_id)) && !e.removed
    }) else {
        drop(entities);
        // Not a boat or a cart: a saddled pig or a horse.
        crate::mobs::ride::act(ctx, &actor, act);
        return Outcome::default();
    };
    let id = e.id;
    let pos = e.pos;
    let Kind::Vehicle(v) = &mut e.kind else {
        return Outcome::default();
    };
    match act {
        Act::Move { pos: to, yaw } => {
            // Only boats are driven by their rider, and not far at once.
            let near = (0..3).all(|i| (to[i] - pos[i]).abs() < 10.0);
            if matches!(v.craft, Craft::Boat { .. }) && near && to.iter().all(|c| c.is_finite()) {
                e.pos = to;
                v.yaw = yaw;
                let sync = fx::Sync {
                    id,
                    pos: to,
                    vel: [0.0; 3],
                    yaw,
                    pitch: 0.0,
                    on_ground: false,
                };
                let _ = ctx.events.send(Event::EntitySync(vec![sync]));
            }
        }
        Act::Dismount => {
            v.riders.retain(|&r| r != actor.entity_id);
            passengers(ctx, id, &v.riders.clone());
            let pose = crate::context::Pose {
                x: pos[0],
                y: pos[1] + 0.2,
                z: pos[2],
                on_ground: true,
                ..actor.pose
            };
            let _ = ctx.events.send(Event::Teleport {
                uuid: actor.uuid,
                pose,
            });
        }
    }
    Outcome::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;

    fn actor(mode: i32) -> Actor {
        Actor {
            uuid: 9,
            entity_id: 4,
            game_mode: mode,
            pose: crate::context::Pose {
                x: 0.0,
                y: -60.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
            sneaking: false,
        }
    }

    #[test]
    fn a_boat_floats_on_water() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let water = ctx
            .data
            .blocks
            .state_id("minecraft:water", &[("level", "0")])
            .unwrap();
        for x in 600..604 {
            for z in 0..4 {
                ctx.world.set_block(
                    mc_protocol::Position::new(x, -60, z),
                    water,
                    &ctx.data.blocks,
                    &ctx.rules,
                );
            }
        }
        let mut e = new(
            &ctx,
            Craft::Boat {
                wood: "oak".into(),
                chest: false,
            },
            [601.5, -58.5, 1.5],
            0.0,
        );
        let mut out = hits::Out::default();
        for _ in 0..120 {
            let mut t = Tick {
                ctx: &ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
        }
        // Settles with its bottom just under the surface (-59.1).
        assert!((e.pos[1] + 59.3).abs() < 0.05, "{:?}", e.pos);
    }

    #[test]
    fn riders_get_in_and_vehicles_break_after_hits() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = new(
            &ctx,
            Craft::Minecart(Cart::Rideable),
            [0.5, -60.0, 0.5],
            0.0,
        );
        let (mut outcome, mut fresh) = (Outcome::default(), Vec::new());
        interact(&ctx, actor(0), &mut e, 0, false, &mut outcome, &mut fresh);
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!(v.riders, vec![4]);
        // One seat only.
        let mut other = actor(0);
        other.entity_id = 5;
        interact(&ctx, other, &mut e, 0, false, &mut outcome, &mut fresh);
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!(v.riders, vec![4]);
        // Five bare-handed punches (10 each) break it; it drops itself.
        for _ in 0..4 {
            interact(&ctx, actor(0), &mut e, 0, true, &mut outcome, &mut fresh);
            assert!(!e.removed);
        }
        interact(&ctx, actor(0), &mut e, 0, true, &mut outcome, &mut fresh);
        assert!(e.removed);
        assert_eq!(fresh.len(), 1);
        let Kind::Item { item, .. } = fresh[0].kind else {
            panic!()
        };
        assert_eq!(ctx.items.name(item), Some("minecraft:minecart"));
    }

    #[test]
    fn items_name_their_vehicles() {
        assert_eq!(
            craft_of("minecraft:oak_boat"),
            Some(Craft::Boat {
                wood: "oak".into(),
                chest: false
            })
        );
        assert_eq!(
            craft_of("minecraft:bamboo_raft"),
            Some(Craft::Boat {
                wood: "bamboo".into(),
                chest: false
            })
        );
        assert_eq!(
            craft_of("minecraft:dark_oak_chest_boat"),
            Some(Craft::Boat {
                wood: "dark_oak".into(),
                chest: true
            })
        );
        assert_eq!(
            craft_of("minecraft:tnt_minecart"),
            Some(Craft::Minecart(Cart::Tnt))
        );
        assert_eq!(craft_of("minecraft:stick"), None);
        let c = Craft::Boat {
            wood: "cherry".into(),
            chest: true,
        };
        assert_eq!(item_of(&c), "minecraft:cherry_chest_boat");
    }
}
