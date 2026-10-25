//! Boats and minecarts: putting them down, getting in and out, hitting
//! them, and what they do each tick (`boat`, `cart`).

pub mod boat;
pub mod cart;

use mc_entity::extra::{Cart, Craft, Vehicle};
use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::{MetadataValue, SetEntityData, SetPassengers};

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
    /// The driver turns a boat's paddles.
    Paddle { left: bool, right: bool },
}

/// The shaking of a vehicle that was hit, as clients draw it: strength
/// (ticks left), direction and damage.
pub fn shake_meta(v: &Vehicle) -> Vec<(u8, MetadataValue)> {
    vec![
        (8, MetadataValue::VarInt(v.wobble)),
        (9, MetadataValue::VarInt(v.hurt_dir)),
        (10, MetadataValue::Float(v.damage)),
    ]
}

/// The fields of a vehicle clients need besides its kind: its shaking, a
/// boat's paddles, whether a furnace minecart burns.
pub fn meta(v: &Vehicle) -> Vec<(u8, MetadataValue)> {
    let mut out = Vec::new();
    if v.wobble > 0 || v.damage > 0.0 {
        out.extend(shake_meta(v));
    }
    match v.craft {
        Craft::Boat { .. } => {
            if v.paddles != [false; 2] {
                out.push((11, MetadataValue::Boolean(v.paddles[0])));
                out.push((12, MetadataValue::Boolean(v.paddles[1])));
            }
        }
        Craft::Minecart(Cart::Furnace) if v.fuel > 0 => {
            out.push((13, MetadataValue::Boolean(true)));
        }
        _ => {}
    }
    out
}

fn send_meta(ctx: &ServerContext, id: i32, entries: Vec<(u8, MetadataValue)>) {
    let _ = ctx.events.send(Event::EntitiesData(vec![SetEntityData {
        entity_id: id,
        entries,
    }]));
}

/// A hit: it shakes the other way, and the damage builds up.
fn hit(ctx: &ServerContext, id: i32, v: &mut Vehicle, amount: f32) {
    v.hurt_dir = -v.hurt_dir;
    v.wobble = 10;
    v.damage += amount * 10.0;
    send_meta(ctx, id, shake_meta(v));
}

pub(crate) fn craft_of(item: &str) -> Option<Craft> {
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
            "command_block_minecart" => Cart::Command,
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
            Cart::Command => "minecraft:command_block_minecart",
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
    let (craft, before) = match &e.kind {
        Kind::Vehicle(v) => (v.craft.clone(), (v.wobble, v.damage, v.fuel > 0)),
        _ => return,
    };
    match craft {
        Craft::Boat { .. } => boat::tick(t, e),
        Craft::Minecart(_) => cart::tick(t, e),
    }
    if let Kind::Vehicle(v) = &e.kind {
        // The shaking dies down, the damage heals: clients see it.
        if (v.wobble, v.damage) != (before.0, before.1) {
            send_meta(ctx, e.id, shake_meta(v));
        }
        if matches!(v.craft, Craft::Minecart(Cart::Furnace)) && (v.fuel > 0) != before.2 {
            send_meta(ctx, e.id, vec![(13, MetadataValue::Boolean(v.fuel > 0))]);
        }
    }
    // A vehicle carries its riders along (a boat's driver also says where
    // it is).
    if let Kind::Vehicle(v) = &e.kind {
        let seat = |i: usize| match v.craft {
            // Two in a boat sit one behind the other.
            Craft::Boat { .. } if v.riders.len() > 1 => {
                let back = if i == 0 { 0.2 } else { -0.6 };
                let yaw = f64::from(v.yaw).to_radians();
                [-yaw.sin() * back, yaw.cos() * back]
            }
            _ => [0.0, 0.0],
        };
        for (i, &r) in v.riders.iter().enumerate() {
            let off = seat(i);
            if let Some(uuid) = rider_uuid(ctx, r)
                && let Some(p) = lock(&ctx.players).get_mut(&uuid)
                && let Some(pose) = &mut p.pose
            {
                (pose.x, pose.y, pose.z) = (e.pos[0] + off[0], e.pos[1], e.pos[2] + off[1]);
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
    // With `entity_drops` off neither it nor what it held drops.
    let mut kept = Vec::new();
    let fresh = if ctx.game_rule_bool("entity_drops") {
        fresh
    } else {
        &mut kept
    };
    if drop_item {
        // A command block minecart leaves only the minecart.
        let item = match v.craft {
            Craft::Minecart(Cart::Command) => "minecraft:minecart".to_owned(),
            _ => item_of(&v.craft),
        };
        fresh.extend(crate::decor::dropped(ctx, e.pos, &item, 1, 0));
    }
    // What it held spills out, and windows on it close.
    let (items, viewers) = crate::containers::entity_gone(ctx, e.id);
    for stack in items {
        if let Some(name) = ctx.items.name(stack.item) {
            fresh.extend(crate::decor::dropped(
                ctx,
                e.pos,
                name,
                stack.count,
                stack.damage,
            ));
        }
    }
    if !viewers.is_empty() {
        let _ = ctx.events.send(Event::ContainerGone {
            position: crate::containers::entity_key(e.id),
        });
    }
    e.removed = true;
}

/// Hurt by a projectile or a punch: the damage builds up, and past 40 the
/// vehicle breaks.
pub fn hurt(ctx: &ServerContext, e: &mut Entity, hit: &Hit, fresh: &mut Vec<Entity>) -> Vec<Event> {
    let id = e.id;
    let Kind::Vehicle(v) = &mut e.kind else {
        return Vec::new();
    };
    self::hit(ctx, id, v, hit.amount);
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
    (hand, attack): (i32, bool),
    leashed: &[Entity],
    outcome: &mut Outcome,
    fresh: &mut Vec<Entity>,
) {
    let id = e.id;
    // Leads: boats take them and hold them.
    if !attack {
        let held = crate::held::held(ctx, actor.uuid, hand);
        let leash_held = held.as_ref().map(|h| (h.name.as_str(), h.slot));
        if crate::decor::leash::use_on(
            ctx,
            e,
            (actor.uuid, actor.game_mode, actor.sneaking),
            leash_held,
            leashed,
            fresh,
        ) {
            outcome.events.push(Event::Swing {
                uuid: actor.uuid,
                entity_id: actor.entity_id,
                times: 1,
            });
            return;
        }
    }
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
            hit(ctx, id, v, amount);
            if v.damage > 40.0 {
                destroy(ctx, e, fresh, true);
            }
        }
        return;
    }
    let Kind::Vehicle(v) = &mut e.kind else {
        return;
    };
    // Carts with a chest or hopper open their inventory; boats with a chest
    // do when the player sneaks or there is no seat left for them.
    let boat = matches!(v.craft, Craft::Boat { .. });
    let seat = v.riders.len() < capacity(&v.craft) && !v.riders.contains(&actor.entity_id);
    if let Some(kind) = crate::containers::entity_kind(&item_of(&v.craft))
        && (!boat || actor.sneaking || !seat)
    {
        outcome.open = Some((crate::containers::entity_key(id), kind));
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return;
    }
    if actor.sneaking {
        return;
    }
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
        fx::status(ctx, id, 10);
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
    // Only the plain minecart is ridden; a command block one opens its
    // window in the operator's game.
    if matches!(
        v.craft,
        Craft::Minecart(Cart::Chest | Cart::Hopper | Cart::Command | Cart::Furnace | Cart::Tnt)
    ) {
        return;
    }
    if v.riders.len() < capacity(&v.craft) && !v.riders.contains(&actor.entity_id) {
        v.riders.push(actor.entity_id);
        crate::criteria::started_riding(actor.uuid);
        passengers(ctx, id, &v.riders.clone());
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
    }
}

/// The inventory of the vehicle `rider` sits in (a boat with a chest), which
/// the rider's inventory key opens.
pub fn ridden_inventory(
    ctx: &ServerContext,
    rider: i32,
) -> Option<(Position, crate::containers::Kind)> {
    lock(&ctx.entities).iter().find_map(|e| match &e.kind {
        Kind::Vehicle(v) if !e.removed && v.riders.contains(&rider) => Some((
            crate::containers::entity_key(e.id),
            crate::containers::entity_kind(&item_of(&v.craft))?,
        )),
        _ => None,
    })
}

/// Where a rider getting off is put (minecraft.wiki, "Boat" and
/// "Minecart"): beside the vehicle, the way they look for a boat, else to
/// its right, left, behind or in front (a minecart starts from its right),
/// standing on something solid with room; on top of the vehicle when there
/// is no such place.
fn dismount_spot(ctx: &ServerContext, craft: &Craft, pos: [f64; 3], yaw: f32) -> [f64; 3] {
    let width = match craft {
        Craft::Boat { .. } => 1.375,
        Craft::Minecart(_) => 0.98,
    };
    // A box corner to corner, plus the player's: diagonal distance.
    let reach = (width * std::f64::consts::SQRT_2 + 0.6 * std::f64::consts::SQRT_2) / 2.0;
    let look = f64::from(yaw).to_radians();
    let (fx, fz) = (-look.sin(), look.cos());
    let (rx, rz) = (-fz, fx);
    let tries: Vec<(f64, f64)> = match craft {
        Craft::Boat { .. } => vec![(fx, fz), (rx, rz), (-rx, -rz), (-fx, -fz)],
        Craft::Minecart(_) => vec![(rx, rz), (-rx, -rz), (-fx, -fz), (fx, fz)],
    };
    for (dx, dz) in tries {
        let x = pos[0] + dx * reach;
        let z = pos[2] + dz * reach;
        for dy in [0, 1, -1] {
            let cell = [
                x.floor() as i32,
                pos[1].floor() as i32 + dy,
                z.floor() as i32,
            ];
            let free =
                !hits::stops(ctx, cell) && !hits::stops(ctx, [cell[0], cell[1] + 1, cell[2]]);
            if free && hits::stops(ctx, [cell[0], cell[1] - 1, cell[2]]) {
                return [x, f64::from(cell[1]), z];
            }
        }
    }
    [pos[0], pos[1] + 0.2, pos[2]]
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
        Act::Paddle { left, right } => {
            if matches!(v.craft, Craft::Boat { .. }) && v.paddles != [left, right] {
                v.paddles = [left, right];
                send_meta(
                    ctx,
                    id,
                    vec![
                        (11, MetadataValue::Boolean(left)),
                        (12, MetadataValue::Boolean(right)),
                    ],
                );
            }
        }
        Act::Dismount => {
            v.riders.retain(|&r| r != actor.entity_id);
            if v.paddles != [false; 2] && v.riders.is_empty() {
                v.paddles = [false; 2];
            }
            passengers(ctx, id, &v.riders.clone());
            let at = dismount_spot(ctx, &v.craft, pos, actor.pose.yaw);
            let pose = crate::context::Pose {
                x: at[0],
                y: at[1],
                z: at[2],
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
        interact(
            &ctx,
            actor(0),
            &mut e,
            (0, false),
            &[],
            &mut outcome,
            &mut fresh,
        );
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!(v.riders, vec![4]);
        // One seat only.
        let mut other = actor(0);
        other.entity_id = 5;
        interact(
            &ctx,
            other,
            &mut e,
            (0, false),
            &[],
            &mut outcome,
            &mut fresh,
        );
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!(v.riders, vec![4]);
        // Five bare-handed punches (10 each) break it; it drops itself.
        for _ in 0..4 {
            interact(
                &ctx,
                actor(0),
                &mut e,
                (0, true),
                &[],
                &mut outcome,
                &mut fresh,
            );
            assert!(!e.removed);
        }
        interact(
            &ctx,
            actor(0),
            &mut e,
            (0, true),
            &[],
            &mut outcome,
            &mut fresh,
        );
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

    #[test]
    fn a_chest_boat_seats_one_and_opens_when_full_or_sneaking() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let craft = Craft::Boat {
            wood: "oak".into(),
            chest: true,
        };
        let mut e = new(&ctx, craft, [0.5, -60.0, 0.5], 0.0);
        let (mut outcome, mut fresh) = (Outcome::default(), Vec::new());
        // Sneaking opens the chest.
        let mut sneaker = actor(0);
        sneaker.sneaking = true;
        interact(
            &ctx,
            sneaker,
            &mut e,
            (0, false),
            &[],
            &mut outcome,
            &mut fresh,
        );
        assert!(outcome.open.take().is_some());
        // Otherwise the player gets in; there is one seat.
        interact(
            &ctx,
            actor(0),
            &mut e,
            (0, false),
            &[],
            &mut outcome,
            &mut fresh,
        );
        assert!(outcome.open.take().is_none());
        let Kind::Vehicle(v) = &e.kind else { panic!() };
        assert_eq!(v.riders, vec![4]);
        // Full: the next one opens the chest instead.
        let mut other = actor(0);
        other.entity_id = 5;
        interact(
            &ctx,
            other,
            &mut e,
            (0, false),
            &[],
            &mut outcome,
            &mut fresh,
        );
        let (key, kind) = outcome.open.take().unwrap();
        assert_eq!((key, kind.slots), (crate::containers::entity_key(e.id), 27));
        // The rider's inventory key opens it too.
        lock(&ctx.entities).push(e.clone());
        assert!(ridden_inventory(&ctx, 4).is_some());
        assert!(ridden_inventory(&ctx, 5).is_none());
    }

    #[test]
    fn a_broken_chest_minecart_drops_itself_and_its_contents() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = new(&ctx, Craft::Minecart(Cart::Chest), [0.5, -60.0, 0.5], 0.0);
        let kind = crate::containers::entity_kind("minecraft:chest_minecart").unwrap();
        let stick = ctx.items.id("minecraft:stick").unwrap();
        crate::containers::put(
            &ctx,
            crate::containers::entity_key(e.id),
            kind,
            &[(
                5,
                Some(mc_protocol::packets::play::ItemStack::new(stick, 9)),
            )],
        );
        let mut fresh = Vec::new();
        destroy(&ctx, &mut e, &mut fresh, true);
        let mut names: Vec<(String, i32)> = fresh
            .iter()
            .filter_map(|f| match f.kind {
                Kind::Item { item, count, .. } => Some((ctx.items.name(item)?.to_owned(), count)),
                _ => None,
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                ("minecraft:chest_minecart".to_owned(), 1),
                ("minecraft:stick".to_owned(), 9)
            ]
        );
    }

    #[test]
    fn a_powered_activator_rail_turns_a_hopper_minecart_off() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let rail = |powered: &str| {
            blocks
                .state_id(
                    "minecraft:activator_rail",
                    &[("shape", "east_west"), ("powered", powered)],
                )
                .unwrap()
        };
        let cell = mc_protocol::Position::new(700, -60, 0);
        ctx.world.set_block(cell, rail("true"), blocks, &ctx.rules);
        let chest = mc_protocol::Position::new(700, -59, 0);
        ctx.world.set_block(
            chest,
            blocks.default_state("minecraft:chest").unwrap(),
            blocks,
            &ctx.rules,
        );
        let stick = ctx.items.id("minecraft:stick").unwrap();
        let chest_kind = crate::containers::kind_of("minecraft:chest").unwrap();
        crate::containers::put(
            &ctx,
            chest,
            chest_kind,
            &[(
                0,
                Some(mc_protocol::packets::play::ItemStack::new(stick, 5)),
            )],
        );
        let cart = new(
            &ctx,
            Craft::Minecart(Cart::Hopper),
            [700.5, -59.9375, 0.5],
            0.0,
        );
        let id = cart.id;
        lock(&ctx.entities).push(cart);
        let step = |ctx: &ServerContext| {
            let mut e = lock(&ctx.entities)
                .iter()
                .find(|e| e.id == id)
                .cloned()
                .unwrap();
            let mut out = hits::Out::default();
            let mut t = Tick {
                ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, &mut e);
            *lock(&ctx.entities).iter_mut().find(|x| x.id == id).unwrap() = e;
            crate::hoppers::tick_carts(ctx);
        };
        let left = |ctx: &ServerContext| {
            crate::containers::slots(ctx, chest, chest_kind)[0]
                .as_ref()
                .map_or(0, |s| s.count)
        };
        for _ in 0..10 {
            step(&ctx);
        }
        assert_eq!(left(&ctx), 5, "off while the rail is powered");
        ctx.world.set_block(cell, rail("false"), blocks, &ctx.rules);
        step(&ctx);
        assert_eq!(left(&ctx), 4, "back on: it pulls at once");
        // And one more every tick: it does not rest like a hopper.
        step(&ctx);
        assert_eq!(left(&ctx), 3);
    }
}
