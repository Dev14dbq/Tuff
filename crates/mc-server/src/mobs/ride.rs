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
        // Land animals (Cursor): camels need no taming.
        Species::Camel | Species::CamelHusk => m.saddled(),
        // Aquatic (Claude): a nautilus must be tamed first, its undead kin not.
        Species::Nautilus => m.saddled() && m.is_tame(),
        Species::ZombieNautilus => m.saddled() && m.is_tame(),
        // Flying mobs (flying-mobs): the first rider flies a harnessed ghast.
        Species::HappyGhast => m.gear[slot::BODY].is_some(),
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
        && (m.species.horse() && m.is_tame()
            || m.species == Species::Nautilus && m.is_tame()
            || m.species == Species::ZombieNautilus && m.is_tame()
            || m.species == Species::HappyGhast && m.gear[slot::BODY].is_some()
            || matches!(m.species, Species::Pig | Species::Strider | Species::Camel | Species::CamelHusk)))
}

/// One tick of a ridden mob: it lives (grows, burns, makes sounds) but the
/// rider moves it; a pig whose rider has no carrot on a stick walks on.
pub fn tick(ctx: &ServerContext, e: &mut Entity, s: &Senses, out: &mut Vec<MobEvent>) {
    if steered_by(ctx, e) {
        mc_entity::mob::tick_ridden(e, s, out);
        dash_tick(ctx, e, out);
    } else {
        mc_entity::mob::tick(e, s, out);
    }
}

/// A horse that reared up at a jump comes down, ridden or not.
pub fn rear_tick(e: &mut Entity) {
    if let Kind::Mob(m) = &mut e.kind
        && m.quirks.land.rearing > 0
    {
        m.quirks.land.rearing -= 1;
        if m.quirks.land.rearing == 0 {
            m.dirty = true;
        }
    }
}

/// Ticks from a nautilus's dash to the next ("Nautilus": every two seconds).
const DASH_EVERY: i32 = 40;
/// Ticks the dash shows in its data.
const DASH_SHOWN: i32 = 2;
/// Ticks a horse rears up at its rider's jump (as a vanilla server shows it).
const REARING: i32 = 20;

fn nautilus(species: Species) -> Option<&'static str> {
    match species {
        Species::Nautilus => Some("nautilus"),
        Species::ZombieNautilus => Some("zombie_nautilus"),
        _ => None,
    }
}

/// `entity.<kind>.<what>`, the `_land` one out of water.
fn dash_sound(kind: &str, what: &str, wet: bool) -> String {
    let land = if wet { "" } else { "_land" };
    format!("entity.{kind}.{what}{land}")
}

/// The rider let go of the jump key; their client moves the mount. A horse
/// rears up with its jump sound; a nautilus dashes: the server sounds it,
/// shows it in the data and counts down to the next.
pub fn jump(ctx: &ServerContext, actor: &crate::interact::Actor, power: i32) {
    let mut entities = lock(&ctx.entities);
    let Some(e) = entities.iter_mut().find(|e| {
        matches!(&e.kind, Kind::Mob(m) if m.riders.first() == Some(&actor.entity_id)) && !e.removed
    }) else {
        return;
    };
    if power <= 0 || !steered_by(ctx, e) {
        return;
    }
    let pos = e.pos;
    let wet = crate::hits::in_water(ctx, pos);
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.species.horse() {
        m.quirks.land.rearing = REARING;
        m.dirty = true;
        drop(entities);
        fx::sound(
            ctx,
            "entity.horse.jump",
            fx::category::NEUTRAL,
            pos,
            0.4,
            1.0,
        );
        return;
    }
    let Some(kind) = nautilus(m.species) else {
        return;
    };
    if m.quirks.aqua.dash_cd > 0 {
        return;
    }
    m.quirks.aqua.dash_cd = DASH_EVERY;
    m.quirks.aqua.dashing = DASH_SHOWN;
    m.dirty = true;
    let pitch = m.voice_pitch();
    let category = crate::mobs::category(m.species);
    drop(entities);
    fx::sound(
        ctx,
        &dash_sound(kind, "dash", wet),
        category,
        pos,
        1.0,
        pitch,
    );
}

/// A ridden nautilus's dash wears off, and it is ready for the next.
fn dash_tick(ctx: &ServerContext, e: &mut Entity, out: &mut Vec<MobEvent>) {
    let wet = crate::hits::in_water(ctx, e.pos);
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    let Some(kind) = nautilus(m.species) else {
        return;
    };
    let a = &mut m.quirks.aqua;
    if a.dashing > 0 {
        a.dashing -= 1;
        if a.dashing == 0 {
            m.dirty = true;
        }
    }
    if m.quirks.aqua.dash_cd > 0 {
        m.quirks.aqua.dash_cd -= 1;
        if m.quirks.aqua.dash_cd == 0 {
            let pitch = m.voice_pitch();
            out.push(MobEvent::Sound {
                name: dash_sound(kind, "dash_ready", wet),
                volume: 1.0,
                pitch,
            });
        }
    }
}

/// A player gets on.
pub fn mount(ctx: &ServerContext, e: &mut Entity, rider: i32) -> bool {
    let id = e.id;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    // A camel carries two.
    let seats = match m.species {
        Species::Camel | Species::CamelHusk => 2,
        // Flying mobs (flying-mobs): four ride a happy ghast.
        Species::HappyGhast => 4,
        _ => 1,
    };
    if m.riders.len() >= seats || m.riders.contains(&rider) || m.is_baby() {
        return false;
    }
    m.riders.push(rider);
    if let Some(uuid) = lock(&ctx.players)
        .values()
        .find(|p| p.entity_id == rider)
        .map(|p| p.profile.uuid)
    {
        crate::criteria::started_riding(uuid);
    }
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
        crate::vehicles::Act::Move {
            pos,
            yaw,
            on_ground,
        } => {
            let near = (0..3).all(|i| (pos[i] - e.pos[i]).abs() < 10.0);
            if !near || !pos.iter().all(|c| c.is_finite()) {
                return;
            }
            let steered = steered_by(ctx, e);
            if !steered {
                return;
            }
            let down = e.pos[1] - pos[1];
            e.pos = pos;
            let wet = crate::hits::in_water(ctx, pos);
            let mut landed = None;
            if let Kind::Mob(m) = &mut e.kind {
                m.yaw = yaw;
                m.head_yaw = yaw;
                landed = fall(m, down, on_ground, wet);
            }
            let moved = crate::mobs::MobMove {
                id: e.id,
                pos,
                yaw,
                pitch: 0.0,
                head_yaw: yaw,
                on_ground,
            };
            let _ = ctx.events.send(Event::MobsMoved(vec![moved]));
            if let Some(fell) = landed {
                let riders = land(ctx, e, fell);
                drop(entities);
                riders_land(ctx, &riders, pos, fell);
            }
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

/// A steered mob's fall, from where its rider's client moved it: the
/// distance of a fall that just ended.
fn fall(m: &mut mc_entity::mob::Mob, drop: f64, on_ground: bool, wet: bool) -> Option<f64> {
    if wet {
        m.fall_distance = 0.0;
        None
    } else if on_ground {
        // The move that lands is part of the fall.
        let fell = m.fall_distance + drop.max(0.0);
        m.fall_distance = 0.0;
        (fell > 0.0).then_some(fell)
    } else {
        m.fall_distance += drop.max(0.0);
        None
    }
}

/// A steered mob lands `fell` blocks down: it is hurt as it would be
/// without a rider. The riders, when the fall was past its safe distance.
fn land(ctx: &ServerContext, e: &mut Entity, fell: f64) -> Vec<i32> {
    let Kind::Mob(m) = &e.kind else {
        return Vec::new();
    };
    let riders = if fell > m.species.safe_fall() {
        m.riders.clone()
    } else {
        Vec::new()
    };
    let damage = m
        .species
        .fall_damage(fell, f64::from(m.effect_level("jump_boost")));
    if damage > 0.0 && m.species.takes_fall_damage() {
        let harm = crate::mobs::hurt::Harm::new(damage, "minecraft:fall");
        crate::mobs::hurt::harm(ctx, e, &harm);
    }
    riders
}

/// Riders of a mount that fell past its safe distance ("Horse") take the
/// fall as usual.
fn riders_land(ctx: &ServerContext, riders: &[i32], pos: [f64; 3], fell: f64) {
    if riders.is_empty() {
        return;
    }
    let below = mc_protocol::Position::new(
        pos[0].floor() as i32,
        (pos[1] - 0.2).floor() as i32,
        pos[2].floor() as i32,
    );
    let block = ctx
        .world
        .block(below, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s));
    let key = crate::damage::fall_key(block, false);
    let players: Vec<u128> = lock(&ctx.players)
        .values()
        .filter(|p| riders.contains(&p.entity_id))
        .map(|p| p.profile.uuid)
        .collect();
    for uuid in players {
        let safe = lock(&ctx.vitals).get(&uuid).map_or(3.0, |v| {
            v.effects.value(crate::effects::Attr::SafeFallDistance)
        });
        let damage = (fell - safe).ceil();
        if damage > 0.0 {
            crate::vitals::hurt_by(ctx, uuid, damage as f32, "minecraft:fall", key, None);
        }
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
        Species::Camel | Species::CamelHusk => true,
        Species::Nautilus => m.is_tame(),
        Species::ZombieNautilus => m.is_tame(),
        _ => false,
    };
    if !fits || m.saddled() || m.is_baby() {
        return false;
    }
    m.gear[slot::SADDLE] = Some(mc_entity::mob::Gear {
        item,
        count: 1,
        damage: 0,
        components: Default::default(),
    });
    m.persistent = true;
    let wet = matches!(m.species, Species::Nautilus | Species::ZombieNautilus);
    let sound = if wet {
        "item.nautilus_saddle_underwater_equip"
    } else if m.species == Species::Pig {
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
