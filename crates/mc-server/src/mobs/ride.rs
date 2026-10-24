//! Riding mobs (minecraft.wiki, "Pig", "Horse", "Saddle"): a player gets on
//! a saddled pig or any horse by using it. A saddled, tamed horse — or a pig
//! while the rider holds a carrot on a stick — is steered by the rider's
//! client, which reports where it went; otherwise the mob walks where it
//! wants with the rider on. Sneaking gets off.

use mc_entity::mob::{MobEvent, Senses, slot};
use mc_entity::{Entity, Kind, Species};
use mc_protocol::packets::play::SetPassengers;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::tick::lock;

fn passengers(ctx: &ServerContext, vehicle: i32, riders: &[i32]) {
    fx::emit(
        ctx,
        fx::Fx::Passengers(SetPassengers {
            vehicle,
            passengers: riders.to_vec(),
        }),
    );
}

/// The uuid of a player by entity id.
fn uuid_of(ctx: &ServerContext, entity_id: i32) -> Option<u128> {
    lock(&ctx.players)
        .values()
        .find(|p| p.entity_id == entity_id)
        .map(|p| p.profile.uuid)
}

/// Whether the rider steers the mob (its client moves it).
pub fn steered_by(ctx: &ServerContext, e: &Entity) -> bool {
    let Kind::Mob(m) = &e.kind else {
        return false;
    };
    let Some(&rider) = m.riders.first() else {
        return false;
    };
    match m.species {
        Species::Horse | Species::Donkey | Species::Mule => m.saddled() && m.is_tame(),
        Species::Pig | Species::Strider => {
            let stick = stick_of(m.species);
            m.saddled()
                && uuid_of(ctx, rider).is_some_and(|u| {
                    crate::held::find(ctx, u, |n| n == stick).is_some_and(|h| {
                        h.slot == 45 || {
                            let inv = lock(&ctx.inventories);
                            inv.get(&u)
                                .is_some_and(|hands| hands.hand_slot(0) == h.slot)
                        }
                    })
                })
        }
        _ => false,
    }
}

/// The stick that steers a mob: carrot for pigs, warped fungus for striders.
fn stick_of(species: Species) -> &'static str {
    if species == Species::Strider {
        "minecraft:warped_fungus_on_a_stick"
    } else {
        "minecraft:carrot_on_a_stick"
    }
}

/// Whether the mob only lives this tick while its rider moves it. Kept
/// cheap: the check of the rider's hands happens in [`steered_by`].
pub fn steered(e: &Entity) -> bool {
    matches!(&e.kind, Kind::Mob(m) if !m.riders.is_empty() && m.saddled()
        && (m.species.horse() && m.is_tame() || matches!(m.species, Species::Pig | Species::Strider)))
}

/// One tick of a ridden mob: it lives (grows, burns, makes sounds) but the
/// rider moves it; a pig whose rider has no carrot on a stick walks on.
pub fn tick(ctx: &ServerContext, e: &mut Entity, s: &Senses, out: &mut Vec<MobEvent>) {
    if steered_by(ctx, e) {
        mc_entity::mob::tick_ridden(e, s, out);
    } else {
        mc_entity::mob::tick(e, s, out);
    }
}

/// A player gets on.
pub fn mount(ctx: &ServerContext, e: &mut Entity, rider: i32) -> bool {
    let id = e.id;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if !m.riders.is_empty() || m.is_baby() {
        return false;
    }
    m.riders.push(rider);
    m.nav.stop();
    m.sitting = false;
    passengers(ctx, id, &m.riders);
    true
}

/// Everyone gets off.
pub fn eject(ctx: &ServerContext, e: &mut Entity) {
    let id = e.id;
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.riders.is_empty() {
        return;
    }
    let riders = std::mem::take(&mut m.riders);
    passengers(ctx, id, &[]);
    for r in riders {
        let Some((uuid, pose)) = lock(&ctx.players)
            .values()
            .find(|p| p.entity_id == r)
            .and_then(|p| p.pose.map(|pose| (p.profile.uuid, pose)))
        else {
            continue;
        };
        let pose = crate::context::Pose {
            x: pos[0],
            y: pos[1] + 0.5,
            z: pos[2],
            on_ground: false,
            ..pose
        };
        let _ = ctx.events.send(Event::Teleport { uuid, pose });
    }
}

/// A rider's word about their mount: where it went, or getting off.
pub fn act(ctx: &ServerContext, actor: &crate::interact::Actor, act: crate::vehicles::Act) {
    let mut entities = lock(&ctx.entities);
    let Some(e) = entities.iter_mut().find(|e| {
        matches!(&e.kind, Kind::Mob(m) if m.riders.contains(&actor.entity_id)) && !e.removed
    }) else {
        return;
    };
    match act {
        crate::vehicles::Act::Move { pos, yaw } => {
            let near = (0..3).all(|i| (pos[i] - e.pos[i]).abs() < 10.0);
            if !near || !pos.iter().all(|c| c.is_finite()) {
                return;
            }
            let steered = steered_by(ctx, e);
            if !steered {
                return;
            }
            e.pos = pos;
            if let Kind::Mob(m) = &mut e.kind {
                m.yaw = yaw;
                m.head_yaw = yaw;
            }
            let moved = crate::mobs::MobMove {
                id: e.id,
                pos,
                yaw,
                pitch: 0.0,
                head_yaw: yaw,
                on_ground: true,
            };
            let _ = ctx.events.send(Event::MobsMoved(vec![moved]));
        }
        crate::vehicles::Act::Dismount => {
            let id = e.id;
            let pos = e.pos;
            let Kind::Mob(m) = &mut e.kind else {
                return;
            };
            m.riders.retain(|&r| r != actor.entity_id);
            let riders = m.riders.clone();
            drop(entities);
            passengers(ctx, id, &riders);
            let pose = crate::context::Pose {
                x: pos[0],
                y: pos[1] + 0.5,
                z: pos[2],
                on_ground: false,
                ..actor.pose
            };
            let _ = ctx.events.send(Event::Teleport {
                uuid: actor.uuid,
                pose,
            });
        }
        // Mounts have no paddles.
        crate::vehicles::Act::Paddle { .. } => {}
    }
}

/// Equips a saddle (from a player's hand); whether it went on.
pub fn saddle(ctx: &ServerContext, e: &mut Entity, item: i32) -> bool {
    let id = e.id;
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    let fits = match m.species {
        Species::Pig | Species::Strider => true,
        Species::Horse | Species::Donkey | Species::Mule => m.is_tame(),
        _ => false,
    };
    if !fits || m.saddled() || m.is_baby() {
        return false;
    }
    m.gear[slot::SADDLE] = Some(mc_entity::mob::Gear {
        item,
        count: 1,
        damage: 0,
    });
    m.persistent = true;
    let sound = if m.species == Species::Pig {
        "entity.pig.saddle"
    } else if m.species == Species::Strider {
        "entity.strider.saddle"
    } else {
        "entity.horse.saddle"
    };
    fx::sound(ctx, sound, fx::category::NEUTRAL, pos, 0.5, 1.0);
    fx::emit(
        ctx,
        fx::Fx::Equip(mc_protocol::packets::play::SetEquipment {
            entity_id: id,
            slots: crate::mobs::meta::worn(m),
        }),
    );
    true
}

/// A rider uses a carrot on a stick (or a warped fungus on a stick): the pig
/// (strider) they steer speeds up for 7 to 49 seconds and the stick wears by
/// 7 (1) (a broken one leaves a fishing rod). `None` when it does nothing
/// (not steering one, or it is still fast).
pub fn boost(ctx: &ServerContext, actor: &crate::interact::Actor, hand: i32) -> Option<()> {
    let held = crate::held::held(ctx, actor.uuid, hand)?;
    if !matches!(
        held.name.as_str(),
        "minecraft:carrot_on_a_stick" | "minecraft:warped_fungus_on_a_stick"
    ) {
        return None;
    }
    let species = if held.name == "minecraft:carrot_on_a_stick" {
        Species::Pig
    } else {
        Species::Strider
    };
    let mut entities = lock(&ctx.entities);
    let e = entities.iter_mut().find(|e| {
        matches!(&e.kind, Kind::Mob(m) if m.species == species
            && m.riders.first() == Some(&actor.entity_id))
            && !e.removed
    })?;
    if !steered_by(ctx, e) {
        return None;
    }
    let Kind::Mob(m) = &mut e.kind else {
        return None;
    };
    if m.boost > 0 {
        return None;
    }
    m.boost = 140 + m.below(841) as i32;
    m.boost_total = m.boost;
    m.dirty = false;
    let packet = crate::mobs::metadata_packet(ctx, e);
    drop(entities);
    if let Some(p) = packet {
        fx::emit(ctx, fx::Fx::Data(p));
    }
    if actor.game_mode != 1 {
        crate::held::wear_into(
            ctx,
            actor.uuid,
            held.slot,
            if species == Species::Pig { 7 } else { 1 },
            "minecraft:fishing_rod",
        );
    }
    let _ = ctx.events.send(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    Some(())
}
