//! Mob effects on players (`/effect`; later potions, beacons...).
//!
//! What each effect looks like and which attributes it changes was observed
//! on vanilla 26.3 (`observe-commands --packets update_mob_effect,
//! update_attributes,set_entity_data` giving every effect at two levels);
//! `attack_damage` is not sent to clients, its modifiers are from the wiki.
//! What the effects do each tick follows the wiki ("Effect").

use std::collections::BTreeMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{
    self, AttributeModifier, AttributeSnapshot, MetadataValue, RemoveMobEffect, SetEntityData,
    UpdateAttributes, UpdateMobEffect,
};

use crate::context::{Direct, Event, ServerContext};
use crate::tick::lock;
use crate::vitals::Vitals;

/// The attributes effects change on a player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Attr {
    AttackDamage,
    AttackSpeed,
    Luck,
    MaxAbsorption,
    MaxHealth,
    MovementSpeed,
    SafeFallDistance,
}

impl Attr {
    const ALL: [Attr; 7] = [
        Attr::AttackDamage,
        Attr::AttackSpeed,
        Attr::Luck,
        Attr::MaxAbsorption,
        Attr::MaxHealth,
        Attr::MovementSpeed,
        Attr::SafeFallDistance,
    ];

    fn name(self) -> &'static str {
        match self {
            Attr::AttackDamage => "minecraft:attack_damage",
            Attr::AttackSpeed => "minecraft:attack_speed",
            Attr::Luck => "minecraft:luck",
            Attr::MaxAbsorption => "minecraft:max_absorption",
            Attr::MaxHealth => "minecraft:max_health",
            Attr::MovementSpeed => "minecraft:movement_speed",
            Attr::SafeFallDistance => "minecraft:safe_fall_distance",
        }
    }

    /// A player's base value (as the client gets it: a float made double).
    fn base(self) -> f64 {
        f64::from(match self {
            Attr::AttackDamage => 1.0f32,
            Attr::AttackSpeed => 4.0,
            Attr::Luck => 0.0,
            Attr::MaxAbsorption => 0.0,
            Attr::MaxHealth => 20.0,
            Attr::MovementSpeed => 0.1,
            Attr::SafeFallDistance => 3.0,
        })
    }

    /// Whether clients are told about it.
    fn synced(self) -> bool {
        self != Attr::AttackDamage
    }
}

/// What an effect's particles look like.
#[derive(Clone, Copy, Debug)]
enum Look {
    /// `entity_effect` in this colour (RGB).
    Color(u32),
    /// A particle type of its own.
    Particle(&'static str),
}

/// One effect.
#[derive(Debug)]
pub struct Kind {
    pub name: &'static str,
    look: Look,
    /// Applies at once (its "seconds" count ticks).
    pub instant: bool,
    /// (attribute, amount per level, operation).
    modifiers: &'static [(Attr, f32, i32)],
}

const fn kind(name: &'static str, look: Look) -> Kind {
    Kind {
        name,
        look,
        instant: false,
        modifiers: &[],
    }
}

use Look::{Color, Particle};

static KINDS: [Kind; 40] = [
    Kind {
        modifiers: &[(Attr::MovementSpeed, 0.2, 2)],
        ..kind("speed", Color(0x33ebff))
    },
    Kind {
        modifiers: &[(Attr::MovementSpeed, -0.15, 2)],
        ..kind("slowness", Color(0x8bafe0))
    },
    Kind {
        modifiers: &[(Attr::AttackSpeed, 0.1, 2)],
        ..kind("haste", Color(0xd9c043))
    },
    Kind {
        modifiers: &[(Attr::AttackSpeed, -0.1, 2)],
        ..kind("mining_fatigue", Color(0x4a4217))
    },
    Kind {
        modifiers: &[(Attr::AttackDamage, 3.0, 0)],
        ..kind("strength", Color(0xffc700))
    },
    Kind {
        instant: true,
        ..kind("instant_health", Color(0xf82423))
    },
    Kind {
        instant: true,
        ..kind("instant_damage", Color(0xa9656a))
    },
    Kind {
        modifiers: &[(Attr::SafeFallDistance, 1.0, 0)],
        ..kind("jump_boost", Color(0xfdff84))
    },
    kind("nausea", Color(0x551d4a)),
    kind("regeneration", Color(0xcd5cab)),
    kind("resistance", Color(0x9146f0)),
    kind("fire_resistance", Color(0xff9900)),
    kind("water_breathing", Color(0x98dac0)),
    kind("invisibility", Color(0xf6f6f6)),
    kind("blindness", Color(0x1f1f23)),
    kind("night_vision", Color(0xc2ff66)),
    kind("hunger", Color(0x587653)),
    Kind {
        modifiers: &[(Attr::AttackDamage, -4.0, 0)],
        ..kind("weakness", Color(0x484d48))
    },
    kind("poison", Color(0x87a363)),
    kind("wither", Color(0x736156)),
    Kind {
        modifiers: &[(Attr::MaxHealth, 4.0, 0)],
        ..kind("health_boost", Color(0xf87d23))
    },
    Kind {
        modifiers: &[(Attr::MaxAbsorption, 4.0, 0)],
        ..kind("absorption", Color(0x2552a5))
    },
    Kind {
        instant: true,
        ..kind("saturation", Color(0xf82423))
    },
    kind("glowing", Color(0x94a061)),
    kind("levitation", Color(0xceffff)),
    Kind {
        modifiers: &[(Attr::Luck, 1.0, 0)],
        ..kind("luck", Color(0x59c106))
    },
    Kind {
        modifiers: &[(Attr::Luck, -1.0, 0)],
        ..kind("unluck", Color(0xc0a44d))
    },
    kind("slow_falling", Color(0xf3cfb9)),
    kind("conduit_power", Color(0x1dc2d1)),
    kind("dolphins_grace", Color(0x88a3be)),
    kind("bad_omen", Color(0x0b6138)),
    kind("hero_of_the_village", Color(0x44ff44)),
    kind("darkness", Color(0x292721)),
    kind("trial_omen", Particle("minecraft:trial_omen")),
    kind("raid_omen", Particle("minecraft:raid_omen")),
    kind("wind_charged", Particle("minecraft:small_gust")),
    kind("weaving", Particle("minecraft:item_cobweb")),
    kind("oozing", Particle("minecraft:item_slime")),
    kind("infested", Particle("minecraft:infested")),
    kind("breath_of_the_nautilus", Color(0x00ffee)),
];

/// The effect named `id` (`minecraft:speed` or `speed`).
pub fn kind_of(id: &str) -> Option<&'static Kind> {
    let name = id.strip_prefix("minecraft:").unwrap_or(id);
    KINDS.iter().find(|k| k.name == name)
}

impl Kind {
    fn id(&self) -> String {
        format!("minecraft:{}", self.name)
    }

    /// The translation key of its name.
    pub fn key(&self) -> String {
        format!("effect.minecraft.{}", self.name)
    }
}

/// One effect a player has.
#[derive(Clone, Debug, PartialEq)]
pub struct Instance {
    pub amplifier: i32,
    /// Ticks left; -1 for ever.
    pub duration: i32,
    pub ambient: bool,
    pub particles: bool,
    pub icon: bool,
    /// A weaker but longer one, back when this one ends.
    pub hidden: Option<Box<Instance>>,
}

impl Instance {
    pub fn new(amplifier: i32, duration: i32, particles: bool) -> Self {
        Self {
            amplifier,
            duration,
            ambient: false,
            particles,
            icon: particles,
            hidden: None,
        }
    }

    fn infinite(&self) -> bool {
        self.duration == -1
    }

    fn shorter_than(&self, other: &Instance) -> bool {
        !self.infinite() && (self.duration < other.duration || other.infinite())
    }

    /// Takes in `other` of the same effect, as vanilla does; whether
    /// anything changed.
    fn update(&mut self, other: &Instance) -> bool {
        let mut changed = false;
        if other.amplifier > self.amplifier {
            if other.shorter_than(self) {
                let mut old = self.clone();
                old.hidden = self.hidden.take();
                self.hidden = Some(Box::new(old));
            }
            self.amplifier = other.amplifier;
            self.duration = other.duration;
            changed = true;
        } else if self.shorter_than(other) {
            if other.amplifier == self.amplifier {
                self.duration = other.duration;
                changed = true;
            } else if let Some(hidden) = &mut self.hidden {
                hidden.update(other);
            } else {
                let mut h = other.clone();
                h.hidden = None;
                self.hidden = Some(Box::new(h));
            }
        }
        if !other.ambient && self.ambient || changed {
            self.ambient = other.ambient;
            changed = true;
        }
        if other.particles != self.particles {
            self.particles = other.particles;
            changed = true;
        }
        if other.icon != self.icon {
            self.icon = other.icon;
            changed = true;
        }
        changed
    }

    fn flags(&self, blend: bool) -> u8 {
        u8::from(self.ambient)
            | u8::from(self.particles) << 1
            | u8::from(self.icon) << 2
            | u8::from(blend) << 3
    }

    fn to_nbt(&self, id: &str) -> Compound {
        let mut c = Compound::new();
        c.insert("id", Tag::from(id));
        c.insert("amplifier", Tag::Byte(self.amplifier as i8));
        c.insert("duration", Tag::Int(self.duration));
        c.insert("ambient", Tag::Byte(i8::from(self.ambient)));
        c.insert("show_particles", Tag::Byte(i8::from(self.particles)));
        c.insert("show_icon", Tag::Byte(i8::from(self.icon)));
        if let Some(h) = &self.hidden {
            c.insert("hidden_effect", Tag::Compound(h.to_nbt(id)));
        }
        c
    }

    fn from_nbt(c: &Compound) -> Option<(&'static Kind, Instance)> {
        let flag = |name: &str, default: bool| match c.get(name) {
            Some(Tag::Byte(b)) => *b != 0,
            _ => default,
        };
        let Some(Tag::String(id)) = c.get("id") else {
            return None;
        };
        let kind = kind_of(id)?;
        let particles = flag("show_particles", true);
        let instance = Instance {
            amplifier: match c.get("amplifier") {
                Some(Tag::Byte(a)) => i32::from(*a as u8),
                _ => 0,
            },
            duration: match c.get("duration") {
                Some(Tag::Int(d)) => *d,
                _ => 0,
            },
            ambient: flag("ambient", false),
            particles,
            icon: flag("show_icon", particles),
            hidden: match c.get("hidden_effect") {
                Some(Tag::Compound(h)) => Instance::from_nbt(h).map(|(_, i)| Box::new(i)),
                _ => None,
            },
        };
        Some((kind, instance))
    }
}

/// A player's effects, by effect name.
#[derive(Clone, Debug, Default)]
pub struct Effects {
    active: BTreeMap<&'static str, Instance>,
}

impl Effects {
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    pub fn get(&self, kind: &Kind) -> Option<&Instance> {
        self.active.get(kind.name)
    }

    fn modifiers(&self, attr: Attr) -> Vec<AttributeModifier> {
        self.active
            .iter()
            .filter_map(|(name, i)| Some((kind_of(name)?, i)))
            .flat_map(|(k, i)| {
                k.modifiers.iter().filter(move |m| m.0 == attr).map(
                    move |&(_, amount, operation)| AttributeModifier {
                        id: format!("minecraft:effect.{}", k.name),
                        amount: f64::from(amount * (i.amplifier + 1) as f32),
                        operation,
                    },
                )
            })
            .collect()
    }

    /// An attribute's value with the effects' modifiers.
    pub fn value(&self, attr: Attr) -> f64 {
        let mods = self.modifiers(attr);
        let base = attr.base();
        let add: f64 = mods
            .iter()
            .filter(|m| m.operation == 0)
            .map(|m| m.amount)
            .sum();
        let v = base + add;
        let mul: f64 = mods
            .iter()
            .filter(|m| m.operation == 1)
            .map(|m| m.amount)
            .sum();
        let mut v = v + (base + add) * mul;
        for m in mods.iter().filter(|m| m.operation == 2) {
            v *= 1.0 + m.amount;
        }
        v
    }

    /// The saved form (`active_effects`).
    pub fn to_nbt(&self) -> Tag {
        Tag::List(
            self.active
                .iter()
                .map(|(name, i)| Tag::Compound(i.to_nbt(&format!("minecraft:{name}"))))
                .collect(),
        )
    }

    pub fn from_nbt(tag: Option<&Tag>) -> Self {
        let mut effects = Effects::default();
        if let Some(Tag::List(list)) = tag {
            for entry in list {
                if let Tag::Compound(c) = entry
                    && let Some((kind, i)) = Instance::from_nbt(c)
                {
                    effects.active.insert(kind.name, i);
                }
            }
        }
        effects
    }
}

/// The effects' particles and whether they are all ambient.
fn look(ctx: &ServerContext, effects: &Effects) -> (Vec<(i32, Option<i32>)>, bool) {
    let mut particles = Vec::new();
    let mut ambient = true;
    for (name, i) in &effects.active {
        if !i.particles {
            continue;
        }
        ambient &= i.ambient;
        let Some(kind) = kind_of(name) else { continue };
        particles.push(match kind.look {
            Color(rgb) => (
                ctx.data
                    .builtin
                    .id("minecraft:particle_type", "minecraft:entity_effect")
                    .unwrap_or(0),
                Some((0xff00_0000 | rgb) as i32),
            ),
            Particle(p) => (
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
fn flags(effects: &Effects) -> i8 {
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
        flags: flags(&v.effects),
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
                modifiers: v.effects.modifiers(a),
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
    fn update_keeps_the_stronger_or_longer() {
        let mut speed = Instance::new(1, 200, true);
        assert!(!speed.update(&Instance::new(0, 100, true)));
        assert!(speed.update(&Instance::new(1, 400, true)));
        assert_eq!(speed.duration, 400);
        assert!(!speed.update(&Instance::new(1, 100, true)));
        // Stronger but shorter: the old one waits underneath.
        assert!(speed.update(&Instance::new(2, 100, true)));
        assert_eq!(speed.hidden.as_ref().map(|h| h.amplifier), Some(1));
        // Hiding particles is a change.
        assert!(speed.update(&Instance::new(0, 10, false)));
        assert!(!speed.particles && !speed.icon);
    }

    #[test]
    fn modifiers_scale_with_the_level() {
        let mut e = Effects::default();
        e.active.insert("speed", Instance::new(1, 200, true));
        e.active.insert("health_boost", Instance::new(1, 200, true));
        assert_eq!(
            e.modifiers(Attr::MovementSpeed)[0].amount,
            f64::from(0.4f32)
        );
        assert_eq!(e.value(Attr::MaxHealth), 28.0);
        assert!((e.value(Attr::MovementSpeed) - 0.14).abs() < 1e-6);
    }

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
