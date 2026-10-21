//! Melee and being hit, after the public description of combat
//! (minecraft.wiki, "Damage", "Attack cooldown", "Critical hit", "Sweep
//! attack", "Knockback", "Shield", "Armor"):
//! - a blow's strength grows back after each swing over `20 / attack speed`
//!   ticks; damage is scaled by `0.2 + 0.8 * strength²`, enchantment bonuses
//!   by the strength;
//! - a fully charged blow while falling, not on the ground, not in water and
//!   not sprinting is critical (×1.5);
//! - a fully charged blow while sprinting knocks back one more level and
//!   stops the sprint;
//! - a fully charged sword blow on the ground that is neither critical nor
//!   a sprint blow sweeps: everything else within a block of the target takes
//!   `1 + sweeping ratio × damage`;
//! - everything hurt by an entity is pushed away from it (0.4, less by
//!   knockback resistance);
//! - a shield raised for a quarter of a second blocks hits from the front;
//!   it wears by `1 + damage` from 3 damage up, and an axe disables it for
//!   five seconds;
//! - armor wears by a quarter of the damage (at least 1) per piece.

use std::collections::HashMap;

use mc_protocol::packets::play::SetEntityMotion;

use crate::context::{Direct, Event, Pose, ServerContext};
use crate::fx;
use crate::tick::lock;

/// What a player is doing in a fight.
#[derive(Clone, Debug, Default)]
pub struct Fighter {
    pub sprinting: bool,
    /// World age of the last swing (or change of held item).
    pub last_swing: i64,
    /// The entity that last hurt them and whom they last hit, with when.
    pub hurt_by: Option<(i32, i64)>,
    pub hit: Option<(i32, i64)>,
    /// A shield raised since this world age.
    pub shield: Option<i64>,
    /// The shield is down (an axe hit it) until this world age.
    pub shield_off_until: i64,
}

pub type Fighters = HashMap<u128, Fighter>;

fn now(ctx: &ServerContext) -> i64 {
    lock(&ctx.time).age
}

fn with<R>(ctx: &ServerContext, uuid: u128, f: impl FnOnce(&mut Fighter) -> R) -> R {
    f(lock(&ctx.mobs.fighters).entry(uuid).or_default())
}

pub fn set_sprinting(ctx: &ServerContext, uuid: u128, on: bool) {
    with(ctx, uuid, |f| f.sprinting = on);
}

/// The held item changed: the blow starts charging again.
pub fn reset_strength(ctx: &ServerContext, uuid: u128) {
    let t = now(ctx);
    with(ctx, uuid, |f| f.last_swing = t);
}

/// Starts or stops holding up a shield (`hand` holds it).
pub fn shield(ctx: &ServerContext, uuid: u128, raised: bool) {
    let t = now(ctx);
    with(ctx, uuid, |f| f.shield = raised.then_some(t));
}

/// Whether the item in `hand` is a shield.
pub fn holds_shield(ctx: &ServerContext, uuid: u128, hand: i32) -> bool {
    crate::held::held(ctx, uuid, hand)
        .and_then(|h| ctx.items.get(h.stack.item))
        .is_some_and(|i| i.blocks_attacks)
}

/// Who last hurt the player and whom they hit, within the last 100 ticks
/// (pets go after them).
pub fn recent(ctx: &ServerContext, uuid: u128) -> (Option<i32>, Option<i32>) {
    let t = now(ctx);
    let all = lock(&ctx.mobs.fighters);
    let Some(f) = all.get(&uuid) else {
        return (None, None);
    };
    let fresh = |x: Option<(i32, i64)>| x.filter(|(_, at)| t - at <= 100).map(|(id, _)| id);
    (fresh(f.hurt_by), fresh(f.hit))
}

/// A melee blow, worked out when the player swings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Blow {
    /// Damage before bonuses against the target.
    pub damage: f32,
    /// Strength of the blow (0–1).
    pub strength: f32,
    /// Extra damage against the undead (smite) and arthropods (bane).
    pub undead: f32,
    pub arthropod: f32,
    /// Knockback levels beyond the basic push (sprint, enchantment).
    pub knockback: f64,
    pub crit: bool,
    pub sweep: bool,
    /// Sweeping ratio of the sweep's damage.
    pub sweep_ratio: f32,
    /// Seconds of fire (Fire Aspect).
    pub fire: i32,
    /// Looting level, for what the target drops.
    pub looting: i32,
    /// Seconds an axe disables a shield for.
    pub disables_shield: f32,
    /// Where the attacker stands and which way they face.
    pub from: [f64; 3],
    pub yaw: f32,
}

/// The blow a player swings now; resets the charge and stops a sprint
/// blow's sprint. Sounds come with the hit ([`blow_sound`]).
pub fn swing(ctx: &ServerContext, uuid: u128, pose: Pose) -> Blow {
    let held = crate::held::held(ctx, uuid, 0);
    let info = held.as_ref().and_then(|h| ctx.items.get(h.stack.item));
    let comps = held.as_ref().and_then(|h| h.comps.as_ref());
    let level = |name: &str| crate::held::enchant(comps, name);
    let base = 1.0 + info.map_or(0.0, |i| i.attack_damage);
    let speed = (4.0 + info.map_or(0.0, |i| i.attack_speed)).max(0.1);
    let period = 20.0 / speed;
    let t = now(ctx);
    let (since, sprinting) = with(ctx, uuid, |f| {
        let since = (t - f.last_swing) as f32;
        f.last_swing = t;
        (since, f.sprinting)
    });
    let strength = ((since + 0.5) / period).clamp(0.0, 1.0);
    let full = strength > 0.9;
    let mut damage = base * (0.2 + strength * strength * 0.8);
    let sharp = level("sharpness");
    let bonus = |x: f32| x * strength;
    damage += bonus(if sharp > 0 {
        0.5 * sharp as f32 + 0.5
    } else {
        0.0
    });
    let (fall, in_water) = {
        let v = lock(&ctx.vitals);
        let fall = v.get(&uuid).map_or(0.0, |v| v.fall_distance);
        let wet = crate::hits::in_water(ctx, [pose.x, pose.y, pose.z]);
        (fall, wet)
    };
    let sprint_blow = sprinting && full;
    let crit = full && fall > 0.0 && !pose.on_ground && !in_water && !sprinting;
    if crit {
        damage *= 1.5;
    }
    let sword = held
        .as_ref()
        .is_some_and(|h| ctx.commands.env.tag("minecraft:item", "minecraft:swords").contains(&h.name));
    let sweep = full && !crit && !sprint_blow && pose.on_ground && sword;
    let sweeping = level("sweeping_edge");
    let mut knockback = f64::from(level("knockback"));
    if sprint_blow {
        knockback += 1.0;
        with(ctx, uuid, |f| f.sprinting = false);
    }
    Blow {
        damage,
        strength,
        undead: bonus(2.5 * level("smite") as f32),
        arthropod: bonus(2.5 * level("bane_of_arthropods") as f32),
        knockback,
        crit,
        sweep,
        sweep_ratio: if sweeping > 0 {
            1.0 - 1.0 / (sweeping as f32 + 1.0)
        } else {
            0.0
        },
        fire: level("fire_aspect") * 4,
        looting: level("looting"),
        disables_shield: info.and_then(|i| i.weapon).map_or(0.0, |w| w.1),
        from: [pose.x, pose.y, pose.z],
        yaw: pose.yaw,
    }
}

/// The sounds and particles of a blow that landed (or did nothing).
pub fn blow_sound(ctx: &ServerContext, blow: &Blow, at: [f64; 3], landed: bool) {
    let name = if !landed {
        "entity.player.attack.nodamage"
    } else if blow.crit {
        "entity.player.attack.crit"
    } else if blow.knockback >= 1.0 && blow.strength > 0.9 {
        "entity.player.attack.knockback"
    } else if blow.sweep {
        "entity.player.attack.sweep"
    } else if blow.strength > 0.9 {
        "entity.player.attack.strong"
    } else {
        "entity.player.attack.weak"
    };
    fx::sound(ctx, name, fx::category::PLAYERS, blow.from, 1.0, 1.0);
    if landed && blow.crit {
        fx::particles(ctx, "minecraft:crit", [at[0], at[1] + 1.0, at[2]], 0.4, 0.2, 12);
    }
    if landed && blow.sweep {
        let yaw = f64::from(blow.yaw).to_radians();
        let p = [
            blow.from[0] - yaw.sin(),
            blow.from[1] + 0.9,
            blow.from[2] + yaw.cos(),
        ];
        fx::particles(ctx, "minecraft:sweep_attack", p, 0.0, 0.0, 1);
    }
}

/// Wears the weapon in the main hand after a hit (not in creative).
pub fn wear_weapon(ctx: &ServerContext, uuid: u128, game_mode: i32) {
    if game_mode == 1 {
        return;
    }
    let Some(h) = crate::held::held(ctx, uuid, 0) else {
        return;
    };
    if let Some((wear, _)) = ctx.items.get(h.stack.item).and_then(|i| i.weapon)
        && wear > 0
    {
        crate::held::wear(ctx, uuid, h.slot, wear);
    }
}

/// Notes that the player hit `target` (their pets join in).
pub fn note_hit(ctx: &ServerContext, uuid: u128, target: i32) {
    let t = now(ctx);
    with(ctx, uuid, |f| f.hit = Some((target, t)));
}

/// Pushes a player with this velocity: they move themselves, so they are
/// told, and so is everyone who sees them.
pub fn push_player(ctx: &ServerContext, uuid: u128, entity_id: i32, velocity: [f64; 3]) {
    let packet = SetEntityMotion {
        entity_id,
        velocity,
    };
    let _ = ctx.events.send(Event::Direct {
        uuid,
        packet: Direct::Motion(packet.clone()),
    });
    fx::emit(ctx, fx::Fx::Motion(packet));
}

/// A knockback push of `strength` away from `from`, as vanilla gives it:
/// half the old speed plus the push, up to 0.4 upwards when on the ground.
pub fn knockback(vel: [f64; 3], pos: [f64; 3], from: [f64; 3], strength: f64, on_ground: bool) -> [f64; 3] {
    let (mut dx, mut dz) = (from[0] - pos[0], from[2] - pos[2]);
    let len = dx.hypot(dz);
    if len < 1e-4 {
        // Straight on top: a random side.
        let a = crate::hits::rnd() * std::f64::consts::TAU;
        (dx, dz) = (a.cos(), a.sin());
    } else {
        dx /= len;
        dz /= len;
    }
    if strength <= 0.0 {
        return vel;
    }
    [
        vel[0] / 2.0 - dx * strength,
        if on_ground {
            (vel[1] / 2.0 + strength).min(0.4)
        } else {
            vel[1]
        },
        vel[2] / 2.0 - dz * strength,
    ]
}

/// A hit on a player from an entity.
#[derive(Clone, Debug)]
pub struct Hit<'a> {
    pub amount: f32,
    pub kind: &'a str,
    pub death_key: &'a str,
    /// The attacker's name for the death message.
    pub attacker: Option<&'a str>,
    /// The attacker's entity id.
    pub by: Option<i32>,
    /// Where the hit came from (pushes away from it; a shield must face it).
    pub from: Option<[f64; 3]>,
    /// Knockback beyond the basic 0.4.
    pub knockback: f64,
    /// Seconds an axe disables a shield for.
    pub disables_shield: f32,
}

/// Total knockback resistance a player wears (netherite).
fn worn_resistance(ctx: &ServerContext, uuid: u128) -> f64 {
    let inv = lock(&ctx.inventories);
    let Some(h) = inv.get(&uuid) else {
        return 0.0;
    };
    h.slots[5..9]
        .iter()
        .flatten()
        .filter_map(|s| ctx.items.get(s.item))
        .map(|i| f64::from(i.knockback_resistance))
        .sum::<f64>()
        .min(1.0)
}

/// Whether a raised shield faces `from`.
fn blocks(ctx: &ServerContext, uuid: u128, pose: Pose, from: [f64; 3], t: i64) -> Option<usize> {
    let (since, off) = {
        let all = lock(&ctx.mobs.fighters);
        let f = all.get(&uuid)?;
        (f.shield?, f.shield_off_until)
    };
    if t - since < 5 || t < off {
        return None;
    }
    let hand = [1, 0]
        .into_iter()
        .find(|&h| holds_shield(ctx, uuid, h))?;
    let look = crate::projectiles::look(pose.yaw, 0.0);
    let (dx, dz) = (from[0] - pose.x, from[2] - pose.z);
    if look[0] * dx + look[2] * dz <= 0.0 {
        return None;
    }
    crate::held::held(ctx, uuid, hand).map(|h| h.slot)
}

/// Hurts a player hit by an entity: the shield, the damage (see
/// [`crate::vitals::try_hurt_by_entity`]), worn armor and knockback.
/// `None` when nothing happened, else whether they died.
pub fn hurt_player(ctx: &ServerContext, uuid: u128, hit: &Hit) -> Option<bool> {
    let (entity_id, pose, mode) = {
        let players = lock(&ctx.players);
        let p = players.get(&uuid)?;
        (p.entity_id, p.pose?, p.game_mode)
    };
    let t = now(ctx);
    if let Some(from) = hit.from
        && matches!(mode, 0 | 2)
        && let Some(slot) = blocks(ctx, uuid, pose, from, t)
    {
        let at = [pose.x, pose.y, pose.z];
        if hit.disables_shield > 0.0 {
            with(ctx, uuid, |f| {
                f.shield_off_until = t + (hit.disables_shield * 20.0) as i64;
                f.shield = None;
            });
            fx::status(ctx, entity_id, 30);
            fx::sound(ctx, "item.shield.break", fx::category::PLAYERS, at, 0.8, 0.8 + crate::hits::rnd() as f32 * 0.4);
        } else {
            fx::status(ctx, entity_id, 29);
            fx::sound(ctx, "item.shield.block", fx::category::PLAYERS, at, 1.0, 0.8 + crate::hits::rnd() as f32 * 0.4);
        }
        if hit.amount >= 3.0 {
            crate::held::wear(ctx, uuid, slot, 1 + hit.amount.floor() as i32);
        }
        return Some(false);
    }
    let died = crate::vitals::try_hurt_by_entity(
        ctx,
        uuid,
        hit.amount,
        hit.kind,
        hit.death_key,
        hit.attacker,
        hit.by,
    )?;
    if !crate::damage::bypasses_armor(hit.kind) && mode != 1 {
        let wear = ((hit.amount / 4.0).floor() as i32).max(1);
        let worn: Vec<usize> = {
            let inv = lock(&ctx.inventories);
            inv.get(&uuid).map_or(Vec::new(), |h| {
                (5..9)
                    .filter(|&s| {
                        h.slots[s]
                            .as_ref()
                            .and_then(|st| ctx.items.get(st.item))
                            .is_some_and(|i| i.armor > 0.0)
                    })
                    .collect()
            })
        };
        for slot in worn {
            crate::held::wear(ctx, uuid, slot, wear);
        }
    }
    if let Some(by) = hit.by {
        with(ctx, uuid, |f| f.hurt_by = Some((by, t)));
    }
    if let Some(from) = hit.from
        && !died
    {
        let strength = (0.4 + hit.knockback * 0.5) * (1.0 - worn_resistance(ctx, uuid));
        let v = knockback([0.0; 3], [pose.x, pose.y, pose.z], from, strength, pose.on_ground);
        push_player(ctx, uuid, entity_id, v);
    }
    Some(died)
}

/// A player hits another player in melee.
pub fn player_hits_player(
    ctx: &ServerContext,
    attacker: &crate::interact::Actor,
    name: &str,
    victim: u128,
) {
    let blow = swing(ctx, attacker.uuid, attacker.pose);
    let Some(at) = lock(&ctx.players).get(&victim).and_then(|p| p.pose) else {
        return;
    };
    let victim_id = lock(&ctx.players).get(&victim).map_or(0, |p| p.entity_id);
    let hit = Hit {
        amount: blow.damage,
        kind: "minecraft:player_attack",
        death_key: "death.attack.player",
        attacker: Some(name),
        by: Some(attacker.entity_id),
        from: Some(blow.from),
        knockback: blow.knockback,
        disables_shield: blow.disables_shield,
    };
    let landed = hurt_player(ctx, victim, &hit).is_some();
    blow_sound(ctx, &blow, [at.x, at.y, at.z], landed);
    if landed {
        note_hit(ctx, attacker.uuid, victim_id);
        wear_weapon(ctx, attacker.uuid, attacker.game_mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knockback_pushes_away_and_up() {
        let v = knockback([0.0; 3], [0.0, 0.0, 0.0], [-1.0, 0.0, 0.0], 0.4, true);
        assert!(v[0] > 0.39 && v[1] == 0.4 && v[2].abs() < 1e-9, "{v:?}");
        let air = knockback([0.0, -0.3, 0.0], [0.0; 3], [0.0, 0.0, 1.0], 0.4, false);
        assert_eq!(air[1], -0.3);
        assert!(air[2] < 0.0);
    }
}
