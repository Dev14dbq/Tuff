//! Mob effects: what each one is, how one instance takes in another, and
//! the set a player or mob has, saved as `active_effects`.
//!
//! What each effect looks like and which attributes it changes was observed
//! on vanilla 26.3 (`observe-commands --packets update_mob_effect,
//! update_attributes,set_entity_data` giving every effect at two levels);
//! `attack_damage` is not sent to clients, its modifiers are from the wiki.

use std::collections::BTreeMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::AttributeModifier;

/// The attributes effects change.
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
    pub const ALL: [Attr; 7] = [
        Attr::AttackDamage,
        Attr::AttackSpeed,
        Attr::Luck,
        Attr::MaxAbsorption,
        Attr::MaxHealth,
        Attr::MovementSpeed,
        Attr::SafeFallDistance,
    ];

    pub fn name(self) -> &'static str {
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
    pub fn base(self) -> f64 {
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
    pub fn synced(self) -> bool {
        self != Attr::AttackDamage
    }
}

/// What an effect's particles look like.
#[derive(Clone, Copy, Debug)]
pub enum Look {
    /// `entity_effect` in this colour (RGB).
    Color(u32),
    /// A particle type of its own.
    Particle(&'static str),
}

/// One effect.
#[derive(Debug)]
pub struct Kind {
    pub name: &'static str,
    pub look: Look,
    /// Applies at once (its "seconds" count ticks).
    pub instant: bool,
    /// (attribute, amount per level, operation).
    pub modifiers: &'static [(Attr, f32, i32)],
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
    pub fn id(&self) -> String {
        format!("minecraft:{}", self.name)
    }

    /// The colour of its particles (RGB).
    pub fn color(&self) -> u32 {
        match self.look {
            Look::Color(c) => c,
            Look::Particle(_) => 0xffffff,
        }
    }

    /// The translation key of its name.
    pub fn key(&self) -> String {
        format!("effect.minecraft.{}", self.name)
    }
}

/// One effect a player or mob has.
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

    pub fn infinite(&self) -> bool {
        self.duration == -1
    }

    fn shorter_than(&self, other: &Instance) -> bool {
        !self.infinite() && (self.duration < other.duration || other.infinite())
    }

    /// Takes in `other` of the same effect, as vanilla does; whether
    /// anything changed.
    pub fn update(&mut self, other: &Instance) -> bool {
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

    pub fn flags(&self, blend: bool) -> u8 {
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

/// The effects a player or mob has, by effect name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effects {
    pub active: BTreeMap<&'static str, Instance>,
}

impl Effects {
    /// Whether the effect named `name` (no namespace) is active.
    pub fn has(&self, name: &str) -> bool {
        self.active.contains_key(name)
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    pub fn get(&self, kind: &Kind) -> Option<&Instance> {
        self.active.get(kind.name)
    }

    pub fn modifiers(&self, attr: Attr) -> Vec<AttributeModifier> {
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

    /// The modifiers the effects put on an attribute (by registry name).
    pub fn modifiers_named(&self, name: &str) -> Vec<AttributeModifier> {
        Attr::ALL
            .iter()
            .find(|a| a.name() == name)
            .map(|&a| self.modifiers(a))
            .unwrap_or_default()
    }

    /// An attribute's value with the effects' modifiers.
    pub fn value(&self, attr: Attr) -> f64 {
        self.value_from(attr, attr.base())
    }

    /// An attribute's value from `base` (a mob's own) with the effects'
    /// modifiers.
    pub fn value_from(&self, attr: Attr, base: f64) -> f64 {
        let mods = self.modifiers(attr);
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
}
