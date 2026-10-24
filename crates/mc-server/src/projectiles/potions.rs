//! What the potions in this game do: the effects of each kind, and a
//! thrown potion breaking.
//!
//! Durations follow minecraft.wiki ("Potion"): a base potion lasts 3
//! minutes (1:30 for slowness, weakness, slow falling; 45 s for poison and
//! 22.5 s for regeneration), extended 8 minutes (4 for the short ones), and
//! the strong ones are shorter.

use mc_entity::extra::EffectSpec;
use mc_entity::extra::{Projectile, Shot};
use mc_entity::{Entity, Kind};
use mc_nbt::Tag;

use crate::effects::{self, Instance};
use crate::fx;
use crate::hits::{Tick, Who};

const fn s(secs: i32) -> i32 {
    secs * 20
}

/// Effects of a potion by its name (`minecraft:strong_healing`).
pub fn effects_of(name: &str) -> Vec<EffectSpec> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let (grade, base) = if let Some(b) = name.strip_prefix("long_") {
        (1, b)
    } else if let Some(b) = name.strip_prefix("strong_") {
        (2, b)
    } else {
        (0, name)
    };
    // (effect, amplifier of the strong one, normal, long, strong ticks)
    let one = |effect: &str, amp: i32, normal: i32, long: i32, strong: i32| {
        let ticks = match grade {
            0 => normal,
            1 => long,
            _ => strong,
        };
        vec![(
            format!("minecraft:{effect}"),
            if grade == 2 { amp } else { 0 },
            ticks,
        )]
    };
    match base {
        "night_vision" => one("night_vision", 0, s(180), s(480), 0),
        "invisibility" => one("invisibility", 0, s(180), s(480), 0),
        "leaping" => one("jump_boost", 1, s(180), s(480), s(90)),
        "fire_resistance" => one("fire_resistance", 0, s(180), s(480), 0),
        "swiftness" => one("speed", 1, s(180), s(480), s(90)),
        "slowness" => one("slowness", 3, s(90), s(240), s(20)),
        "water_breathing" => one("water_breathing", 0, s(180), s(480), 0),
        "healing" => one("instant_health", 1, 1, 1, 1),
        "harming" => one("instant_damage", 1, 1, 1, 1),
        "poison" => one("poison", 1, s(45), s(90), s(21) + 12),
        "regeneration" => one("regeneration", 1, s(45), s(90), s(22)),
        "strength" => one("strength", 1, s(180), s(480), s(90)),
        "weakness" => one("weakness", 0, s(90), s(240), 0),
        "luck" => one("luck", 0, s(300), 0, 0),
        "slow_falling" => one("slow_falling", 0, s(90), s(240), 0),
        "wind_charged" => one("wind_charged", 0, s(180), 0, 0),
        "weaving" => one("weaving", 0, s(180), 0, 0),
        "oozing" => one("oozing", 0, s(180), 0, 0),
        "infested" => one("infested", 0, s(180), 0, 0),
        "turtle_master" => {
            let (slow, res, ticks) = match grade {
                0 => (3, 2, s(20)),
                1 => (3, 2, s(40)),
                _ => (5, 3, s(20)),
            };
            vec![
                ("minecraft:slowness".into(), slow, ticks),
                ("minecraft:resistance".into(), res, ticks),
            ]
        }
        _ => Vec::new(),
    }
}

/// The effects a stack carries: those of its `potion_contents` (the potion
/// and any custom effects).
pub fn contents(comps: Option<&Tag>) -> Vec<EffectSpec> {
    let Some(Tag::Compound(c)) = comps else {
        return Vec::new();
    };
    let Some(Tag::Compound(contents)) = c.get("minecraft:potion_contents") else {
        return Vec::new();
    };
    let mut out = match contents.get("potion") {
        Some(Tag::String(name)) => effects_of(name),
        _ => Vec::new(),
    };
    if let Some(Tag::List(custom)) = contents.get("custom_effects") {
        for e in custom {
            let Tag::Compound(e) = e else { continue };
            let Some(Tag::String(id)) = e.get("id") else {
                continue;
            };
            let int = |k: &str, d: i32| match e.get(k) {
                Some(Tag::Int(v)) => *v,
                Some(Tag::Byte(v)) => i32::from(*v),
                _ => d,
            };
            out.push((id.clone(), int("amplifier", 0), int("duration", 600)));
        }
    }
    out
}

/// The colour of a potion's particles: the mean of its effects' colours.
pub fn color(effects: &[EffectSpec]) -> i32 {
    if effects.is_empty() {
        // Water.
        return 0x385dc6;
    }
    let (mut r, mut g, mut b) = (0, 0, 0);
    for (name, _, _) in effects {
        let c = effects::kind_of(name).map_or(0x385dc6, |k| k.color() as i32);
        r += (c >> 16) & 255;
        g += (c >> 8) & 255;
        b += c & 255;
    }
    let n = effects.len() as i32;
    (r / n) << 16 | (g / n) << 8 | (b / n)
}

/// A splash potion `owner` threw breaks at `at` (`direct` is the player
/// and `direct_mob` the mob it hit, who get the full strength). Lingering
/// ones leave a cloud instead.
#[allow(clippy::too_many_arguments)]
pub fn shatter(
    t: &mut Tick,
    at: [f64; 3],
    effects_list: &[EffectSpec],
    color_rgb: i32,
    lingering: bool,
    direct: Option<&Who>,
    direct_mob: Option<i32>,
    owner: Option<u128>,
) {
    let ctx = t.ctx;
    // Particles and the sound of breaking glass: world event 2002 (3 for
    // an instant potion).
    let instant = effects_list
        .iter()
        .any(|(n, _, _)| effects::kind_of(n).is_some_and(|k| k.instant));
    fx::level_event(ctx, if instant { 2007 } else { 2002 }, at, color_rgb);
    if lingering {
        cloud(t, at, effects_list, color_rgb);
        return;
    }
    // Everyone within 4 blocks across and 2 up and down, weaker with
    // distance.
    for who in t.players {
        if !who.solid() {
            continue;
        }
        let d2 = (who.pose.x - at[0]).powi(2)
            + (who.pose.y + 0.9 - at[1]).powi(2)
            + (who.pose.z - at[2]).powi(2);
        let near = (who.pose.x - at[0]).abs() <= 4.3
            && (who.pose.y - at[1]).abs() <= 3.0
            && (who.pose.z - at[2]).abs() <= 4.3
            && d2 < 16.0 + 4.0;
        let hit = direct.is_some_and(|d| d.uuid == who.uuid);
        if !near && !hit {
            continue;
        }
        let strength = if hit { 1.0 } else { 1.0 - d2.sqrt() / 4.0 };
        if strength <= 0.0 {
            continue;
        }
        apply(t.ctx, who.uuid, effects_list, strength, true);
    }
    // And the mobs within 4 blocks of it (feet to where it broke).
    let (by, direct_mob) = (owner, direct_mob);
    for target in t.targets.iter().filter(|g| g.kind.is_some()) {
        let b = target.aabb;
        let feet = [
            (b.min[0] + b.max[0]) / 2.0,
            b.min[1],
            (b.min[2] + b.max[2]) / 2.0,
        ];
        let d2 = (0..3).map(|a| (feet[a] - at[a]).powi(2)).sum::<f64>();
        let hit = direct_mob == Some(target.id);
        if d2 >= 16.0 && !hit {
            continue;
        }
        let strength = if hit { 1.0 } else { 1.0 - d2.sqrt() / 4.0 };
        t.out.doses.push(crate::hits::Dose {
            target: target.id,
            effects: effects_list.to_vec(),
            strength,
            splash: true,
            by,
        });
    }
}

/// Gives a player the effects at a fraction of their strength: instant
/// ones act by that much, the others last that much as long (a splash's
/// only if over a second is left).
pub fn apply(
    ctx: &crate::context::ServerContext,
    uuid: u128,
    list: &[EffectSpec],
    strength: f64,
    splash: bool,
) {
    for (name, amp, dur) in list {
        let Some(kind) = effects::kind_of(name) else {
            continue;
        };
        if kind.instant {
            effects::instant(ctx, uuid, kind, *amp, strength);
        } else {
            let ticks = (strength * f64::from(*dur) + 0.5) as i32;
            if ticks > 20 || !splash && ticks > 0 {
                effects::give(ctx, uuid, kind, Instance::new(*amp, ticks, true));
            }
        }
    }
}

/// A lingering potion leaves a cloud: 3 blocks wide, 30 seconds.
fn cloud(t: &mut Tick, at: [f64; 3], list: &[EffectSpec], color: i32) {
    t.out.spawn.push(Entity {
        id: t.ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(Projectile {
            shot: Shot::Cloud {
                effects: list.to_vec(),
                color,
                radius: 3.0,
                left: 600,
            },
            owner: None,
            mob: None,
            yaw: 0.0,
            pitch: 0.0,
        }),
        pos: at,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    });
}

/// A cloud shrinks as it lasts; every second it gives the effects (a
/// quarter as long) to players standing in it, and each time loses half a
/// block of radius.
pub fn tick_cloud(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &mut e.kind else {
        return;
    };
    let Shot::Cloud {
        effects,
        radius,
        left,
        ..
    } = &mut p.shot
    else {
        return;
    };
    e.age += 1;
    *left -= 1;
    *radius -= 0.005;
    if *left <= 0 || *radius <= 0.5 {
        e.removed = true;
        return;
    }
    // Takes ten ticks to start and then acts once a second.
    if e.age < 10 || !e.age.is_multiple_of(20) {
        return;
    }
    let mut used = false;
    let quarter: Vec<EffectSpec> = effects
        .iter()
        .map(|(n, a, d)| (n.clone(), *a, d / 4))
        .collect();
    for target in t.targets.iter().filter(|g| g.kind.is_some()) {
        let b = target.aabb;
        let (dx, dz) = (
            (b.min[0] + b.max[0]) / 2.0 - e.pos[0],
            (b.min[2] + b.max[2]) / 2.0 - e.pos[2],
        );
        if dx * dx + dz * dz <= f64::from(*radius).powi(2) && (b.min[1] - e.pos[1]).abs() < 2.0 {
            t.out.doses.push(crate::hits::Dose {
                target: target.id,
                effects: quarter.clone(),
                strength: 1.0,
                splash: false,
                by: None,
            });
            used = true;
        }
    }
    for who in t.players {
        let (dx, dz) = (who.pose.x - e.pos[0], who.pose.z - e.pos[2]);
        if who.solid()
            && dx * dx + dz * dz <= f64::from(*radius).powi(2)
            && (who.pose.y - e.pos[1]).abs() < 2.0
        {
            apply(t.ctx, who.uuid, &quarter, 1.0, false);
            used = true;
        }
    }
    if used {
        *radius -= 0.5;
    }
    let size = *radius;
    fx::particles(
        t.ctx,
        "minecraft:entity_effect",
        [e.pos[0], e.pos[1] + 0.1, e.pos[2]],
        size / 2.0,
        0.0,
        6,
    );
}
