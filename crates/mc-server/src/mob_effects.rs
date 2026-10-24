//! Mob effects on mobs (minecraft.wiki, "Effect"): they are given by
//! splash and lingering potions, tipped arrows, wither skulls, bites and
//! `/effect`, act each tick as on players and are saved as
//! `active_effects`:
//! - regeneration heals 1 every 50 ticks (halved per level), poison hurts 1
//!   every 25 but never below 1 health, wither 1 every 40;
//! - instant health heals 4, instant damage hurts 6 (doubled per level);
//!   the undead (`inverted_healing_and_harm`) take them the other way;
//! - speed and slowness change how fast it walks, strength and weakness
//!   its blows, resistance and fire resistance what gets through;
//!   levitation lifts it, slow falling lets it down gently;
//! - invisibility and glowing are entity flags; the effects' particles are
//!   metadata, as for players.
//!
//! Who takes none: the ender dragon and the wither. Undead ignore poison
//! and regeneration, spiders poison, wither skeletons wither, slimes
//! oozing and silverfish infested (the entity type tags where there are
//! some).

use mc_entity::{Entity, Kind, Mob, Species};
use mc_protocol::packets::play::{self, MetadataValue, SetEntityData};

use crate::context::ServerContext;
use crate::effects::{self, Instance, Kind as Effect};
use crate::fx;
use crate::mobs::hurt::Harm;
use crate::tick::lock;

/// Whether the entity type is in a tag of the data pack.
fn tagged(ctx: &ServerContext, species: Species, tag: &str) -> bool {
    ctx.commands
        .env
        .tag("minecraft:entity_type", tag)
        .iter()
        .any(|t| t == species.id())
}

/// Whether a mob of this kind takes the effect at all.
pub fn accepts(ctx: &ServerContext, species: Species, kind: &Effect) -> bool {
    use Species::*;
    if matches!(species, EnderDragon | Wither) {
        return false;
    }
    match kind.name {
        "poison" | "regeneration" if tagged(ctx, species, "minecraft:ignores_poison_and_regen") => {
            false
        }
        "poison" => !matches!(species, Spider | CaveSpider),
        "wither" => species != WitherSkeleton,
        "oozing" => !tagged(ctx, species, "minecraft:immune_to_oozing"),
        "infested" => !tagged(ctx, species, "minecraft:immune_to_infested"),
        _ => true,
    }
}

/// The metadata of a mob's effects: the particles and whether they are
/// all ambient. Nothing for a mob without any unless `always` (when they
/// just ended).
pub fn effect_fields(ctx: &ServerContext, m: &Mob, always: bool) -> Vec<(u8, MetadataValue)> {
    let (particles, ambient) = effects::look(ctx, &m.effects);
    if particles.is_empty() && !always {
        return Vec::new();
    }
    vec![
        (
            play::metadata::EFFECT_PARTICLES,
            MetadataValue::Particles(particles),
        ),
        (
            play::metadata::EFFECT_AMBIENCE,
            MetadataValue::Boolean(ambient),
        ),
    ]
}

/// What clients see of a mob's effects, to compare before and after.
type Seen = (Vec<(i32, Option<i32>)>, bool, i8);

fn seen(ctx: &ServerContext, m: &Mob) -> Seen {
    let (particles, ambient) = effects::look(ctx, &m.effects);
    (particles, ambient, crate::mobs::meta::flags_of(m))
}

/// Tells clients what changed of the mob's effects.
fn show(ctx: &ServerContext, entity_id: i32, m: &Mob, before: &Seen) {
    let after = seen(ctx, m);
    if after == *before {
        return;
    }
    let mut entries = vec![(play::metadata::FLAGS, MetadataValue::Byte(after.2))];
    entries.extend(effect_fields(ctx, m, true));
    fx::emit(ctx, fx::Fx::Data(SetEntityData { entity_id, entries }));
}

/// Gives the mob in `e` an effect (`by`: the player behind it); whether
/// it took. Instant ones act at once.
pub fn give(
    ctx: &ServerContext,
    e: &mut Entity,
    kind: &'static Effect,
    new: Instance,
    by: Option<u128>,
) -> bool {
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.dying.is_some() || m.is_dead() || !accepts(ctx, m.species, kind) {
        return false;
    }
    if kind.instant {
        instant(ctx, e, kind, new.amplifier, 1.0, by);
        return true;
    }
    let before = seen(ctx, m);
    match m.effects.active.get_mut(kind.name) {
        Some(old) => {
            if !old.update(&new) {
                return false;
            }
        }
        None => {
            m.effects.active.insert(kind.name, new);
        }
    }
    show(ctx, e.id, m, &before);
    true
}

/// Gives the mob effects from a potion, arrow or cloud at a fraction of
/// their strength: instant ones by that much, the others that much as
/// long (a splash's only if over a second is left).
pub fn dose(
    ctx: &ServerContext,
    e: &mut Entity,
    list: &[mc_entity::extra::EffectSpec],
    strength: f64,
    splash: bool,
    by: Option<u128>,
) {
    for (name, amp, dur) in list {
        let Some(kind) = effects::kind_of(name) else {
            continue;
        };
        if kind.instant {
            if matches!(&e.kind, Kind::Mob(m) if accepts(ctx, m.species, kind)) {
                instant(ctx, e, kind, *amp, strength, by);
            }
        } else {
            let ticks = (strength * f64::from(*dur) + 0.5) as i32;
            if ticks > 20 || !splash && ticks > 0 {
                give(ctx, e, kind, Instance::new(*amp, ticks, true), by);
            }
        }
    }
}

/// Takes effects from a mob: one, or all with `None`; whether any went.
pub fn remove(ctx: &ServerContext, e: &mut Entity, kind: Option<&Effect>) -> bool {
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    let before = seen(ctx, m);
    let gone = match kind {
        Some(k) => m.effects.active.remove(k.name).is_some(),
        None => !std::mem::take(&mut m.effects.active).is_empty(),
    };
    show(ctx, e.id, m, &before);
    gone
}

/// Heals a living mob up to its maximum health.
pub fn heal(ctx: &ServerContext, e: &mut Entity, amount: f32) {
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.dying.is_some() || m.is_dead() {
        return;
    }
    let health = (m.health + amount).min(m.max_health);
    if health != m.health {
        m.health = health;
        fx::emit(
            ctx,
            fx::Fx::Data(SetEntityData {
                entity_id: e.id,
                entries: vec![(play::metadata::HEALTH, MetadataValue::Float(health))],
            }),
        );
    }
}

/// Magic damage to a mob (`kind` its type), from a player if `by`.
fn hurt(ctx: &ServerContext, e: &mut Entity, amount: f32, kind: &str, by: Option<u128>) {
    let mut h = Harm::new(amount, kind);
    if let Some(p) = by {
        h.player = Some(p);
        h.by = lock(&ctx.players).get(&p).map(|p| p.entity_id);
        h.killer = Some("minecraft:player".into());
    }
    crate::mobs::hurt::harm(ctx, e, &h);
}

/// An instant effect acts on a mob, `strength` of it: health heals 4 and
/// damage hurts 6, doubled with each level, the other way round for the
/// undead.
pub fn instant(
    ctx: &ServerContext,
    e: &mut Entity,
    kind: &Effect,
    amplifier: i32,
    strength: f64,
    by: Option<u128>,
) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let inverted = tagged(ctx, m.species, "minecraft:inverted_healing_and_harm");
    let amp = amplifier.clamp(0, 28);
    let heals = match kind.name {
        "instant_health" => !inverted,
        "instant_damage" => inverted,
        _ => return,
    };
    if heals {
        heal(ctx, e, (f64::from(4 << amp) * strength) as f32);
    } else if by.is_some() {
        let amount = (strength * f64::from(6 << amp) + 0.5) as i32;
        hurt(ctx, e, amount as f32, "minecraft:indirect_magic", by);
    } else {
        let amount = (strength * f64::from(6 << amp) + 0.5) as i32;
        hurt(ctx, e, amount as f32, "minecraft:magic", None);
    }
}

/// What an effect does to a mob this tick.
enum Act {
    Heal(f32),
    Hurt(f32, &'static str),
    Instant(&'static Effect, i32),
}

/// One tick of a mob's effects: they act, run down and end.
pub fn tick(ctx: &ServerContext, e: &mut Entity, now: i64) {
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.effects.is_empty() || m.dying.is_some() {
        return;
    }
    let before = seen(ctx, m);
    let mut acts = Vec::new();
    let mut ended = Vec::new();
    let names: Vec<&'static str> = m.effects.active.keys().copied().collect();
    for name in names {
        let Some(kind) = effects::kind_of(name) else {
            continue;
        };
        let i = m.effects.active[name].clone();
        let t = if i.infinite() { now as i32 } else { i.duration };
        let every = |n: i32| {
            let k = n >> i.amplifier.min(31);
            k <= 0 || t % k == 0
        };
        match name {
            "regeneration" if every(50) => acts.push(Act::Heal(1.0)),
            "poison" if every(25) && m.health > 1.0 => {
                acts.push(Act::Hurt(1.0, "minecraft:magic"));
            }
            "wither" if every(40) => acts.push(Act::Hurt(1.0, "minecraft:wither")),
            "instant_health" | "instant_damage" if i.duration >= 1 || i.infinite() => {
                acts.push(Act::Instant(kind, i.amplifier));
            }
            _ => {}
        }
        let entry = m.effects.active.get_mut(name).expect("listed");
        if entry.infinite() {
            continue;
        }
        entry.duration -= 1;
        if entry.duration <= 0 {
            match entry.hidden.take() {
                Some(h) => *entry = *h,
                None => ended.push(name),
            }
        }
    }
    for name in ended {
        m.effects.active.remove(name);
    }
    show(ctx, e.id, m, &before);
    for act in acts {
        match act {
            Act::Heal(n) => heal(ctx, e, n),
            Act::Hurt(n, kind) => hurt(ctx, e, n, kind, None),
            Act::Instant(kind, amp) => instant(ctx, e, kind, amp, 1.0, None),
        }
    }
}

/// Runs `f` on the mob with this UUID, with the world's entities taken
/// out while it does (what it hurts may die and leave things).
pub fn with_mob<R>(ctx: &ServerContext, uuid: u128, f: impl FnOnce(&mut Entity) -> R) -> Option<R> {
    let mut entities = std::mem::take(&mut *lock(&ctx.entities));
    let r = entities
        .iter_mut()
        .find(|e| e.uuid == uuid && matches!(e.kind, Kind::Mob(_)))
        .map(f);
    let mut stored = lock(&ctx.entities);
    entities.append(&mut stored);
    *stored = entities;
    r
}

/// The names of a mob's equipment slots, by index.
const SLOTS: [&str; 8] = [
    "mainhand", "offhand", "feet", "legs", "chest", "head", "body", "saddle",
];

/// An attribute of a mob (`armor`) from its own `base`, with the
/// modifiers of what it wears and holds (each item in the slot it is for)
/// and of its effects.
pub fn attribute(ctx: &ServerContext, m: &Mob, name: &str, base: f64) -> f64 {
    let full = format!("minecraft:{}", name.trim_start_matches("minecraft:"));
    let mut mods = m.effects.modifiers_named(&full);
    for (s, g) in m.gear.iter().enumerate() {
        let Some(g) = g else { continue };
        let stack = mc_protocol::packets::play::ItemStack {
            damage: g.damage,
            components: g.components.clone(),
            ..mc_protocol::packets::play::ItemStack::new(g.item, g.count)
        };
        mods.extend(
            ctx.items
                .slot_modifiers(&stack, SLOTS[s])
                .into_iter()
                .filter(|(a, _)| *a == full)
                .map(|(_, m)| m),
        );
    }
    let add: f64 = mods
        .iter()
        .filter(|m| m.operation == 0)
        .map(|m| m.amount)
        .sum();
    let v = base + add;
    let mut x = v;
    for m in mods.iter().filter(|m| m.operation == 1) {
        x += v * m.amount;
    }
    for m in mods.iter().filter(|m| m.operation == 2) {
        x *= 1.0 + m.amount;
    }
    x
}

/// The `active_effects` a mob is saved with.
pub fn to_nbt(m: &Mob) -> Option<mc_nbt::Tag> {
    (!m.effects.is_empty()).then(|| m.effects.to_nbt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{flat, join};

    fn mob(ctx: &ServerContext, species: Species) -> Entity {
        let mut e = crate::mobs::new_mob(ctx, species, [3.5, -60.0, 0.5]);
        e.on_ground = true;
        e
    }

    fn health(e: &Entity) -> f32 {
        match &e.kind {
            Kind::Mob(m) => m.health,
            _ => 0.0,
        }
    }

    fn effect(e: &Entity, name: &str) -> Option<Instance> {
        match &e.kind {
            Kind::Mob(m) => m.effects.active.get(name).cloned(),
            _ => None,
        }
    }

    #[test]
    fn effects_tick_on_mobs_and_the_undead_take_them_the_other_way() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, 0xe1, 910, [0.5, -60.0, 0.5], 0);
        let kind = |n: &str| effects::kind_of(n).unwrap();
        // Poison: 1 every 25 ticks, never the last point.
        let mut cow = mob(&ctx, Species::Cow);
        assert!(give(
            &ctx,
            &mut cow,
            kind("poison"),
            Instance::new(0, 100, true),
            None
        ));
        for t in 0..100 {
            tick(&ctx, &mut cow, t);
            if let Kind::Mob(m) = &mut cow.kind {
                m.invulnerable = 0;
            }
        }
        assert_eq!(health(&cow), 10.0 - 4.0);
        assert!(effect(&cow, "poison").is_none());
        // Healing heals the living and hurts the undead.
        dose(
            &ctx,
            &mut cow,
            &[("minecraft:instant_health".into(), 0, 1)],
            1.0,
            true,
            None,
        );
        assert_eq!(health(&cow), 10.0);
        let mut zombie = mob(&ctx, Species::Zombie);
        dose(
            &ctx,
            &mut zombie,
            &[("minecraft:instant_health".into(), 0, 1)],
            1.0,
            true,
            None,
        );
        assert_eq!(health(&zombie), 20.0 - 6.0);
        dose(
            &ctx,
            &mut zombie,
            &[("minecraft:instant_damage".into(), 0, 1)],
            1.0,
            true,
            None,
        );
        assert_eq!(health(&zombie), 18.0);
        // The undead take no poison or regeneration; spiders no poison.
        assert!(!give(
            &ctx,
            &mut zombie,
            kind("poison"),
            Instance::new(0, 100, true),
            None
        ));
        assert!(!give(
            &ctx,
            &mut zombie,
            kind("regeneration"),
            Instance::new(0, 100, true),
            None
        ));
        let mut spider = mob(&ctx, Species::Spider);
        assert!(!give(
            &ctx,
            &mut spider,
            kind("poison"),
            Instance::new(0, 100, true),
            None
        ));
        let mut dragon = mob(&ctx, Species::EnderDragon);
        assert!(!give(
            &ctx,
            &mut dragon,
            kind("speed"),
            Instance::new(0, 100, true),
            None
        ));
        // Glowing and invisibility are flags; particles are shown.
        assert!(give(
            &ctx,
            &mut zombie,
            kind("glowing"),
            Instance::new(0, 100, true),
            None
        ));
        assert!(give(
            &ctx,
            &mut zombie,
            kind("invisibility"),
            Instance::new(0, 100, true),
            None
        ));
        let Kind::Mob(m) = &zombie.kind else { panic!() };
        assert_eq!(crate::mobs::meta::flags_of(m), 0x60);
        assert_eq!(effect_fields(&ctx, m, false).len(), 2);
        // Speed makes it quicker, weakness weaker.
        assert!(give(
            &ctx,
            &mut zombie,
            kind("speed"),
            Instance::new(1, 100, true),
            None
        ));
        let Kind::Mob(m) = &zombie.kind else { panic!() };
        assert!((m.movement_speed() - m.speed_attr * 1.4).abs() < 1e-6);
    }

    #[test]
    fn a_splash_doses_the_mobs_near_by_distance() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = |x: f64| mc_entity::Aabb {
            min: [x - 0.3, -60.0, 0.2],
            max: [x + 0.3, -58.0, 0.8],
        };
        let targets = [
            crate::hits::Target {
                id: 1,
                aabb: at(0.5),
                kind: Some("minecraft:zombie"),
            },
            crate::hits::Target {
                id: 2,
                aabb: at(2.5),
                kind: Some("minecraft:cow"),
            },
            crate::hits::Target {
                id: 3,
                aabb: at(9.5),
                kind: Some("minecraft:cow"),
            },
            // Not a mob: an item frame.
            crate::hits::Target {
                id: 4,
                aabb: at(0.5),
                kind: None,
            },
        ];
        let mut out = crate::hits::Out::default();
        let mut t = crate::hits::Tick {
            ctx: &ctx,
            players: &[],
            targets: &targets,
            out: &mut out,
            shooter: None,
        };
        let poison = crate::projectiles::potions::effects_of("poison");
        crate::projectiles::potions::shatter(
            &mut t,
            [0.5, -60.0, 0.5],
            &poison,
            0,
            false,
            None,
            Some(1),
            Some(7),
        );
        let doses: Vec<(i32, f64)> = out.doses.iter().map(|d| (d.target, d.strength)).collect();
        assert_eq!(doses, vec![(1, 1.0), (2, 0.5)]);
        assert!(out.doses.iter().all(|d| d.splash && d.by == Some(7)));
        // Given: half as long for the cow two blocks off.
        let mut cow = mob(&ctx, Species::Cow);
        let d = &out.doses[1];
        dose(&ctx, &mut cow, &d.effects, d.strength, d.splash, d.by);
        assert_eq!(effect(&cow, "poison").map(|i| i.duration), Some(450));
    }

    #[test]
    fn a_mobs_effects_are_saved_and_read_back() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut cow = mob(&ctx, Species::Cow);
        let speed = effects::kind_of("speed").unwrap();
        let mut i = Instance::new(1, 300, false);
        i.ambient = true;
        assert!(give(&ctx, &mut cow, speed, i.clone(), None));
        let Kind::Mob(m) = &cow.kind else { panic!() };
        let mut c = mc_nbt::Compound::new();
        crate::mobs::nbt::write(&ctx, m, &mut c);
        assert!(c.get("active_effects").is_some());
        let mut back = mob(&ctx, Species::Cow);
        let Kind::Mob(b) = &mut back.kind else {
            panic!()
        };
        crate::mobs::nbt::apply(&ctx, b, &c);
        assert_eq!(b.effects.get(speed), Some(&i));
    }
}
