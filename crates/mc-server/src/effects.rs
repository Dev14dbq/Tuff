//! Mob effects on players and mobs (`/effect`, potions, food, beacons...).
//! What the effects are lives in [`mc_entity::effect`]; what they do each
//! tick follows the wiki ("Effect").

use mc_protocol::packets::play::{
    self, AttributeSnapshot, MetadataValue, RemoveMobEffect, SetEntityData, UpdateAttributes,
    UpdateMobEffect,
};

pub use mc_entity::effect::{Attr, Effects, Instance, Kind, Look, kind_of};

use crate::context::{Direct, Event, ServerContext};
use crate::tick::lock;
use crate::vitals::Vitals;

/// The effects' particles and whether they are all ambient.
pub(crate) fn look(ctx: &ServerContext, effects: &Effects) -> (Vec<(i32, Option<i32>)>, bool) {
    let mut particles = Vec::new();
    let mut ambient = true;
    for (name, i) in &effects.active {
        if !i.particles {
            continue;
        }
        ambient &= i.ambient;
        let Some(kind) = kind_of(name) else { continue };
        particles.push(match kind.look {
            Look::Color(rgb) => (
                ctx.data
                    .builtin
                    .id("minecraft:particle_type", "minecraft:entity_effect")
                    .unwrap_or(0),
                Some((0xff00_0000 | rgb) as i32),
            ),
            Look::Particle(p) => (
                ctx.data
                    .builtin
                    .id("minecraft:particle_type", p)
                    .unwrap_or(0),
                None,
            ),
        });
    }
    if effects.is_empty() {
        ambient = false;
    }
    (particles, ambient)
}

/// The entity flags effects set (0x20 invisible, 0x40 glowing).
pub(crate) fn flags(effects: &Effects) -> i8 {
    let has = |n: &str| effects.active.contains_key(n);
    (if has("invisibility") { 0x20 } else { 0 }) | if has("glowing") { 0x40 } else { 0 }
}

/// What the client sees of a player's effects, to compare before and after.
#[derive(Clone, PartialEq)]
struct Seen {
    particles: Vec<(i32, Option<i32>)>,
    ambient: bool,
    flags: i8,
    attributes: Vec<AttributeSnapshot>,
    health: f32,
    absorption: f32,
}

fn seen(ctx: &ServerContext, v: &Vitals) -> Seen {
    let (particles, ambient) = look(ctx, &v.effects);
    Seen {
        particles,
        ambient,
        flags: flags(&v.effects)
            | if v.fire > 0 { 0x01 } else { 0 }
            | if v.gliding { -0x80 } else { 0 },
        attributes: Attr::ALL
            .iter()
            .filter(|a| a.synced())
            .map(|&a| AttributeSnapshot {
                attribute: ctx
                    .data
                    .builtin
                    .id("minecraft:attribute", a.name())
                    .unwrap_or(0),
                base: a.base(),
                modifiers: v
                    .effects
                    .modifiers(a)
                    .into_iter()
                    .chain(v.worn.modifiers(a.name()))
                    .collect(),
            })
            .collect(),
        health: v.health,
        absorption: v.absorption,
    }
}

/// Packets for what changed between `before` and `after` (the colours are
/// sent again whenever an effect changed, as vanilla does).
fn send_changes(
    ctx: &ServerContext,
    uuid: u128,
    entity_id: i32,
    before: &Seen,
    after: &Seen,
    v: &Vitals,
    refresh: Option<&Kind>,
) {
    let mut entries = Vec::new();
    if after.health != before.health {
        entries.push((play::metadata::HEALTH, MetadataValue::Float(after.health)));
    }
    // Vanilla's coloured particles never compare equal: sent again.
    if after.particles != before.particles || after.particles.iter().any(|p| p.1.is_some()) {
        entries.push((
            play::metadata::EFFECT_PARTICLES,
            MetadataValue::Particles(after.particles.clone()),
        ));
    }
    if after.ambient != before.ambient {
        entries.push((
            play::metadata::EFFECT_AMBIENCE,
            MetadataValue::Boolean(after.ambient),
        ));
    }
    if after.flags != before.flags {
        entries.push((play::metadata::FLAGS, MetadataValue::Byte(after.flags)));
    }
    if after.absorption != before.absorption {
        entries.push((
            play::metadata::PLAYER_ABSORPTION,
            MetadataValue::Float(after.absorption),
        ));
    }
    entries.sort_by_key(|e| e.0);
    if !entries.is_empty() {
        let _ = ctx.events.send(Event::EntityData {
            uuid,
            packet: SetEntityData { entity_id, entries },
        });
    }
    let attributes: Vec<AttributeSnapshot> = after
        .attributes
        .iter()
        .zip(&before.attributes)
        .zip(Attr::ALL.iter().filter(|a| a.synced()))
        .filter(|((a, b), attr)| {
            a != b || refresh.is_some_and(|k| k.modifiers.iter().any(|m| m.0 == **attr))
        })
        .map(|((a, _), _)| a.clone())
        .collect();
    if !attributes.is_empty() {
        let _ = ctx.events.send(Event::Attributes {
            uuid,
            packet: UpdateAttributes {
                entity_id,
                attributes,
            },
        });
    }
    if after.health != before.health {
        let _ = ctx.events.send(Event::Health {
            uuid,
            health: v.packet(),
        });
    }
}

fn effect_id(ctx: &ServerContext, kind: &Kind) -> i32 {
    ctx.data
        .builtin
        .id("minecraft:mob_effect", &kind.id())
        .unwrap_or(0)
}

fn entity_id(ctx: &ServerContext, uuid: u128) -> Option<i32> {
    lock(&ctx.players).get(&uuid).map(|p| p.entity_id)
}

/// Keeps health and absorption within what the attributes allow now.
fn clamp(v: &mut Vitals) {
    v.health = v.health.min(v.effects.value(Attr::MaxHealth) as f32);
    v.absorption = v
        .absorption
        .min(v.effects.value(Attr::MaxAbsorption) as f32);
}

/// Gives a player an effect (`/effect give`); whether it took.
pub fn give(ctx: &ServerContext, uuid: u128, kind: &'static Kind, new: Instance) -> bool {
    let Some(entity_id) = entity_id(ctx, uuid) else {
        return false;
    };
    let mut all = lock(&ctx.vitals);
    let Some(v) = all.get_mut(&uuid) else {
        return false;
    };
    let before = seen(ctx, v);
    let had = v.effects.active.contains_key(kind.name);
    let (added, packet) = match v.effects.active.get_mut(kind.name) {
        Some(old) => {
            let amplifier = old.amplifier;
            if !old.update(&new) {
                return false;
            }
            (old.amplifier != amplifier, old.clone())
        }
        None => {
            v.effects.active.insert(kind.name, new.clone());
            (true, new)
        }
    };
    // Only effects new to the player blend in.
    let blend = !had;
    if added && kind.name == "absorption" {
        v.absorption = v.absorption.max(4.0 * (packet.amplifier + 1) as f32);
    }
    clamp(v);
    let _ = ctx.events.send(Event::Direct {
        uuid,
        packet: Direct::MobEffect(UpdateMobEffect {
            entity_id,
            effect: effect_id(ctx, kind),
            amplifier: packet.amplifier,
            duration: packet.duration,
            flags: packet.flags(blend),
        }),
    });
    let after = seen(ctx, v);
    send_changes(ctx, uuid, entity_id, &before, &after, v, Some(kind));
    true
}

fn removed(ctx: &ServerContext, uuid: u128, entity_id: i32, kind: &Kind) {
    let _ = ctx.events.send(Event::Direct {
        uuid,
        packet: Direct::RemoveMobEffect(RemoveMobEffect {
            entity_id,
            effect: effect_id(ctx, kind),
        }),
    });
}

/// Changes a player's vitals with `f` and tells clients what they see
/// change of it (flags, attributes, health).
pub fn change<R>(ctx: &ServerContext, uuid: u128, f: impl FnOnce(&mut Vitals) -> R) -> Option<R> {
    let entity_id = entity_id(ctx, uuid)?;
    let mut all = lock(&ctx.vitals);
    let v = all.get_mut(&uuid)?;
    let before = seen(ctx, v);
    let r = f(v);
    let after = seen(ctx, v);
    send_changes(ctx, uuid, entity_id, &before, &after, v, None);
    Some(r)
}

/// Takes effects from a player: one, or all with `None`; whether any went.
pub fn remove(ctx: &ServerContext, uuid: u128, kind: Option<&Kind>) -> bool {
    let Some(entity_id) = entity_id(ctx, uuid) else {
        return false;
    };
    let mut all = lock(&ctx.vitals);
    let Some(v) = all.get_mut(&uuid) else {
        return false;
    };
    let before = seen(ctx, v);
    let gone: Vec<&'static str> = match kind {
        Some(k) => v
            .effects
            .active
            .remove_entry(k.name)
            .map(|(n, _)| n)
            .into_iter()
            .collect(),
        None => std::mem::take(&mut v.effects.active).into_keys().collect(),
    };
    if gone.is_empty() {
        return false;
    }
    for name in gone {
        if let Some(k) = kind_of(name) {
            removed(ctx, uuid, entity_id, k);
        }
    }
    clamp(v);
    let after = seen(ctx, v);
    send_changes(ctx, uuid, entity_id, &before, &after, v, None);
    true
}

/// Damage types the effects cause or stop (from the data pack's tags).
const FIRE: [&str; 8] = [
    "minecraft:in_fire",
    "minecraft:campfire",
    "minecraft:on_fire",
    "minecraft:lava",
    "minecraft:hot_floor",
    "minecraft:sulfur_cube_hot",
    "minecraft:unattributed_fireball",
    "minecraft:fireball",
];
const BYPASSES_RESISTANCE: [&str; 3] = [
    "minecraft:out_of_world",
    "minecraft:generic_kill",
    // `bypasses_effects`
    "minecraft:starve",
];

/// Whether fire resistance keeps this damage off entirely.
pub fn fire_proof(effects: &Effects, damage_type: &str) -> bool {
    effects.active.contains_key("fire_resistance") && FIRE.contains(&damage_type)
}

/// Damage after fire resistance and resistance.
pub fn reduce_damage(effects: &Effects, amount: f32, damage_type: &str) -> f32 {
    if fire_proof(effects, damage_type) {
        return 0.0;
    }
    match effects.active.get("resistance") {
        Some(r) if !BYPASSES_RESISTANCE.contains(&damage_type) => {
            let level = (r.amplifier + 1) * 5;
            (amount * (25 - level).max(0) as f32 / 25.0).max(0.0)
        }
        _ => amount,
    }
}

/// What an effect does this tick.
enum Act {
    Heal(f32),
    Hurt(f32, &'static str, &'static str),
}

/// One tick of every player's effects: they act, run down and end.
pub fn tick(ctx: &ServerContext, tick_count: i64) {
    let uuids: Vec<(u128, i32)> = lock(&ctx.players)
        .values()
        .map(|p| (p.profile.uuid, p.entity_id))
        .collect();
    for (uuid, entity_id) in uuids {
        let mut acts = Vec::new();
        {
            let mut all = lock(&ctx.vitals);
            let Some(v) = all.get_mut(&uuid) else {
                continue;
            };
            if v.effects.is_empty() || v.dead {
                continue;
            }
            let before = seen(ctx, v);
            let food = (v.food, v.saturation);
            let mut ended = Vec::new();
            let mut changed = Vec::new();
            let names: Vec<&'static str> = v.effects.active.keys().copied().collect();
            for name in names {
                let Some(kind) = kind_of(name) else { continue };
                let i = v.effects.active[name].clone();
                let t = if i.infinite() {
                    tick_count as i32
                } else {
                    i.duration
                };
                let every = |n: i32| {
                    let k = n >> i.amplifier.min(31);
                    k <= 0 || t % k == 0
                };
                let level = (i.amplifier + 1) as f32;
                let keep = match name {
                    "regeneration" if every(50) => {
                        acts.push(Act::Heal(1.0));
                        true
                    }
                    "poison" if every(25) => {
                        if v.health > 1.0 {
                            acts.push(Act::Hurt(1.0, "minecraft:magic", "death.attack.magic"));
                        }
                        true
                    }
                    "wither" if every(40) => {
                        acts.push(Act::Hurt(1.0, "minecraft:wither", "death.attack.wither"));
                        true
                    }
                    "hunger" => {
                        v.exhaust(0.005 * level);
                        true
                    }
                    "saturation" if i.duration >= 1 || i.infinite() => {
                        let n = i.amplifier + 1;
                        v.food = (v.food + n).min(20);
                        v.saturation = (v.saturation + n as f32).min(v.food as f32);
                        true
                    }
                    "instant_health" if i.duration >= 1 || i.infinite() => {
                        acts.push(Act::Heal((4 << i.amplifier.min(28)) as f32));
                        true
                    }
                    "instant_damage" if i.duration >= 1 || i.infinite() => {
                        acts.push(Act::Hurt(
                            (6 << i.amplifier.min(28)) as f32,
                            "minecraft:magic",
                            "death.attack.magic",
                        ));
                        true
                    }
                    "absorption" => v.absorption > 0.0,
                    _ => true,
                };
                let entry = v.effects.active.get_mut(name).expect("listed");
                if !keep {
                    ended.push(kind);
                    continue;
                }
                if entry.infinite() {
                    continue;
                }
                entry.duration -= 1;
                if entry.duration <= 0 {
                    match entry.hidden.take() {
                        Some(h) => {
                            *entry = *h;
                            changed.push((kind, entry.clone()));
                        }
                        None => ended.push(kind),
                    }
                }
            }
            for kind in &ended {
                v.effects.active.remove(kind.name);
                removed(ctx, uuid, entity_id, kind);
            }
            for (kind, i) in &changed {
                let _ = ctx.events.send(Event::Direct {
                    uuid,
                    packet: Direct::MobEffect(UpdateMobEffect {
                        entity_id,
                        effect: effect_id(ctx, kind),
                        amplifier: i.amplifier,
                        duration: i.duration,
                        flags: i.flags(false),
                    }),
                });
            }
            if !ended.is_empty() || !changed.is_empty() {
                clamp(v);
                let after = seen(ctx, v);
                send_changes(ctx, uuid, entity_id, &before, &after, v, None);
            }
            // Vanilla tells of food only when it or saturation's being zero
            // changes.
            if v.food != food.0 || (v.saturation == 0.0) != (food.1 == 0.0) {
                let _ = ctx.events.send(Event::Health {
                    uuid,
                    health: v.packet(),
                });
            }
        }
        for act in acts {
            match act {
                Act::Heal(n) => heal(ctx, uuid, n),
                Act::Hurt(n, kind, key) => {
                    crate::vitals::hurt(ctx, uuid, n, kind, key);
                }
            }
        }
    }
}

/// An effect as data packs and components write one (`{id, amplifier,
/// duration, ambient, show_particles, show_icon}`; numbers or booleans).
pub fn instance_of(v: &serde_json::Value) -> Option<(&'static Kind, Instance)> {
    let kind = kind_of(v["id"].as_str()?)?;
    let flag = |name: &str, default: bool| match &v[name] {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_i64().is_some_and(|n| n != 0),
        _ => default,
    };
    let particles = flag("show_particles", true);
    Some((
        kind,
        Instance {
            amplifier: v["amplifier"].as_i64().unwrap_or(0) as i32,
            duration: v["duration"].as_i64().unwrap_or(0) as i32,
            ambient: flag("ambient", false),
            particles,
            icon: flag("show_icon", particles),
            hidden: None,
        },
    ))
}

/// An instant effect acts on a player at once, `strength` of it (1 when
/// drunk, less for a splash further off): health 4, damage 6, doubled with
/// each level; saturation feeds.
pub fn instant(ctx: &ServerContext, uuid: u128, kind: &Kind, amplifier: i32, strength: f64) {
    let amp = amplifier.clamp(0, 28);
    match kind.name {
        "instant_health" => heal(ctx, uuid, (f64::from(4 << amp) * strength) as f32),
        "instant_damage" => {
            let amount = (strength * f64::from(6 << amp) + 0.5) as i32;
            crate::vitals::hurt(
                ctx,
                uuid,
                amount as f32,
                "minecraft:magic",
                "death.attack.magic",
            );
        }
        "saturation" => {
            let packet = lock(&ctx.vitals).get_mut(&uuid).map(|v| {
                let n = amplifier + 1;
                v.food = (v.food + n).min(20);
                v.saturation = (v.saturation + n as f32).min(v.food as f32);
                v.packet()
            });
            if let Some(health) = packet {
                let _ = ctx.events.send(Event::Health { uuid, health });
            }
        }
        _ => {}
    }
}

/// Heals a living player up to their maximum health.
pub fn heal(ctx: &ServerContext, uuid: u128, amount: f32) {
    let Some(entity_id) = entity_id(ctx, uuid) else {
        return;
    };
    let mut all = lock(&ctx.vitals);
    let Some(v) = all.get_mut(&uuid) else {
        return;
    };
    if v.dead {
        return;
    }
    let max = v.effects.value(Attr::MaxHealth) as f32;
    let health = (v.health + amount).min(max);
    if health != v.health {
        v.health = health;
        let _ = ctx.events.send(Event::Health {
            uuid,
            health: v.packet(),
        });
        let _ = ctx.events.send(Event::EntityData {
            uuid,
            packet: SetEntityData {
                entity_id,
                entries: vec![(play::metadata::HEALTH, MetadataValue::Float(health))],
            },
        });
    }
}

/// What a joining player is told of the effects they have.
pub fn join_packets(
    ctx: &ServerContext,
    v: &Vitals,
    entity_id: i32,
) -> (
    Vec<UpdateMobEffect>,
    Option<SetEntityData>,
    Option<UpdateAttributes>,
) {
    let effects = v
        .effects
        .active
        .iter()
        .filter_map(|(name, i)| {
            let kind = kind_of(name)?;
            Some(UpdateMobEffect {
                entity_id,
                effect: effect_id(ctx, kind),
                amplifier: i.amplifier,
                duration: i.duration,
                flags: i.flags(false),
            })
        })
        .collect();
    let mut empty = v.clone();
    empty.effects = Effects::default();
    empty.absorption = 0.0;
    empty.fire = 0;
    empty.gliding = false;
    empty.worn = crate::worn::Worn::default();
    let before = seen(ctx, &empty);
    let after = seen(ctx, v);
    let mut entries = Vec::new();
    if !after.particles.is_empty() {
        entries.push((
            play::metadata::EFFECT_PARTICLES,
            MetadataValue::Particles(after.particles.clone()),
        ));
        entries.push((
            play::metadata::EFFECT_AMBIENCE,
            MetadataValue::Boolean(after.ambient),
        ));
    }
    if after.flags != 0 {
        entries.push((play::metadata::FLAGS, MetadataValue::Byte(after.flags)));
    }
    if after.absorption > 0.0 {
        entries.push((
            play::metadata::PLAYER_ABSORPTION,
            MetadataValue::Float(after.absorption),
        ));
    }
    entries.sort_by_key(|e| e.0);
    let attributes: Vec<AttributeSnapshot> = after
        .attributes
        .iter()
        .zip(&before.attributes)
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.clone())
        .collect();
    (
        effects,
        (!entries.is_empty()).then_some(SetEntityData { entity_id, entries }),
        (!attributes.is_empty()).then_some(UpdateAttributes {
            entity_id,
            attributes,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resistance_and_fire_resistance() {
        let mut e = Effects::default();
        e.active.insert("resistance", Instance::new(1, 200, true));
        assert_eq!(reduce_damage(&e, 10.0, "minecraft:magic"), 6.0);
        assert_eq!(reduce_damage(&e, 10.0, "minecraft:out_of_world"), 10.0);
        e.active
            .insert("fire_resistance", Instance::new(0, 200, true));
        assert_eq!(reduce_damage(&e, 10.0, "minecraft:lava"), 0.0);
    }
}
