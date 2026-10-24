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
    /// Extra damage against aquatic mobs (Impaling).
    pub impaling: f32,
    /// A mace's smash attack: the fall it ends (blocks).
    pub smash: Option<f64>,
    /// How much the target's armor counts for less (Breach: -0.15 a
    /// level).
    pub breach: f32,
    /// The attacker's weapon, for the effects after the hit.
    pub weapon: Option<mc_protocol::packets::play::ItemStack>,
}

impl Blow {
    /// The damage type: a smash is `mace_smash`.
    pub fn kind(&self) -> &'static str {
        if self.smash.is_some() {
            "minecraft:mace_smash"
        } else {
            "minecraft:player_attack"
        }
    }

    pub fn death_key(&self) -> &'static str {
        if self.smash.is_some() {
            "death.attack.mace_smash.player"
        } else {
            "death.attack.player"
        }
    }
}

/// A mace's extra damage for a fall of `fall` blocks (minecraft.wiki,
/// "Mace"): 4 a block for the first 3, 2 for the next 5, 1 after that;
/// Density adds its `smash_damage_per_fallen_block` for each.
pub fn smash_damage(fall: f64, per_block: f32) -> f32 {
    let base = if fall <= 3.0 {
        4.0 * fall
    } else if fall <= 8.0 {
        12.0 + 2.0 * (fall - 3.0)
    } else {
        22.0 + fall - 8.0
    };
    (base + f64::from(per_block) * fall) as f32
}

/// The blow a player swings now; resets the charge and stops a sprint
/// blow's sprint. Sounds come with the hit ([`blow_sound`]).
pub fn swing(ctx: &ServerContext, uuid: u128, pose: Pose) -> Blow {
    swing_with(ctx, uuid, pose, None)
}

/// [`swing`] with the base damage set (a riptide spin hits for 8 instead
/// of the attack damage).
pub fn swing_with(ctx: &ServerContext, uuid: u128, pose: Pose, base: Option<f32>) -> Blow {
    let held = crate::held::held(ctx, uuid, 0);
    let info = held.as_ref().and_then(|h| ctx.items.get(h.stack.item));
    let comps = held.as_ref().and_then(|h| h.comps.as_ref());
    let level = |name: &str| crate::held::enchant(comps, name);
    // The attributes, with the held item's modifiers and the effects'.
    let base = base.unwrap_or(crate::commands::gear::value(ctx, uuid, "attack_damage") as f32);
    let speed = (crate::commands::gear::value(ctx, uuid, "attack_speed") as f32).max(0.1);
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
    let enchanted = bonus(if sharp > 0 {
        0.5 * sharp as f32 + 0.5
    } else {
        0.0
    });
    let (fall, in_water, gliding) = {
        let v = lock(&ctx.vitals);
        let fall = v.get(&uuid).map_or(0.0, |v| v.fall_distance);
        let gliding = v.get(&uuid).is_some_and(|v| v.gliding);
        let wet = crate::hits::in_water(ctx, [pose.x, pose.y, pose.z]);
        (fall, wet, gliding)
    };
    // A mace falling more than 1.5 blocks smashes.
    let mace = held.as_ref().is_some_and(|h| h.name == "minecraft:mace");
    let smash = (mace && fall > 1.5 && !gliding).then_some(fall);
    let weapon = held.as_ref().map(|h| h.stack.clone());
    let none = crate::enchant_effects::Situation::default();
    if let (Some(fall), Some(w)) = (smash, &weapon) {
        let per = crate::enchant_effects::modify(
            ctx,
            "minecraft:smash_damage_per_fallen_block",
            w,
            &none,
            0.0,
        );
        damage += smash_damage(fall, per);
    }
    let breach = weapon.as_ref().map_or(0.0, |w| {
        crate::enchant_effects::modify(ctx, "minecraft:armor_effectiveness", w, &none, 0.0)
    });
    let sprint_blow = sprinting && full;
    let crit = full && fall > 0.0 && !pose.on_ground && !in_water && !sprinting;
    if crit {
        damage *= 1.5;
    }
    // Enchantment damage is not made critical.
    damage += enchanted;
    let sword = held.as_ref().is_some_and(|h| {
        ctx.commands
            .env
            .tag("minecraft:item", "minecraft:swords")
            .contains(&h.name)
    });
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
        impaling: bonus(2.5 * level("impaling") as f32),
        smash,
        breach,
        weapon,
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
        fx::particles(
            ctx,
            "minecraft:crit",
            [at[0], at[1] + 1.0, at[2]],
            0.4,
            0.2,
            12,
        );
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
pub fn knockback(
    vel: [f64; 3],
    pos: [f64; 3],
    from: [f64; 3],
    strength: f64,
    on_ground: bool,
) -> [f64; 3] {
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

/// A player's knockback resistance (netherite armor).
fn worn_resistance(ctx: &ServerContext, uuid: u128) -> f64 {
    crate::commands::gear::value(ctx, uuid, "knockback_resistance")
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
    let hand = [1, 0].into_iter().find(|&h| holds_shield(ctx, uuid, h))?;
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
            fx::sound(
                ctx,
                "item.shield.break",
                fx::category::PLAYERS,
                at,
                0.8,
                0.8 + crate::hits::rnd() as f32 * 0.4,
            );
        } else {
            fx::status(ctx, entity_id, 29);
            fx::sound(
                ctx,
                "item.shield.block",
                fx::category::PLAYERS,
                at,
                1.0,
                0.8 + crate::hits::rnd() as f32 * 0.4,
            );
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
    if let Some(by) = hit.by {
        thorns(ctx, uuid, by, mode);
    }
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
        let v = knockback(
            [0.0; 3],
            [pose.x, pose.y, pose.z],
            from,
            strength,
            pose.on_ground,
        );
        push_player(ctx, uuid, entity_id, v);
    }
    Some(died)
}

/// Hurts the entity with this id, a player or a mob, by `amount` of
/// `kind`; `by` is the player behind it.
pub fn hurt_entity(ctx: &ServerContext, entity_id: i32, amount: f32, kind: &str, by: Option<u128>) {
    let player = lock(&ctx.players)
        .values()
        .find(|p| p.entity_id == entity_id && p.dimension == ctx.world.dimension)
        .map(|p| p.profile.uuid);
    match player {
        Some(uuid) => {
            let (name, direct) = by
                .and_then(|b| {
                    lock(&ctx.players)
                        .get(&b)
                        .map(|p| (p.profile.name.clone(), p.entity_id))
                })
                .unzip();
            let key = format!("death.attack.{}", kind.trim_start_matches("minecraft:"));
            crate::vitals::try_hurt_by_entity(
                ctx,
                uuid,
                amount,
                kind,
                &key,
                name.as_deref(),
                direct,
            );
        }
        None => ctx.mobs.harm(ctx.dim, entity_id, amount, kind, by),
    }
}

/// What follows a blow that landed: a mace's smash stops the attacker in
/// the air (they fall only below it), sounds and pushes everything within
/// 3.5 blocks of the target up and away (twice as hard after 5 blocks);
/// then the weapon's `post_attack` effects on the attacker (Wind Burst).
/// `entities` are the world's other entities.
#[allow(clippy::too_many_arguments)]
pub fn after_blow(
    ctx: &ServerContext,
    attacker: u128,
    blow: &Blow,
    target: i32,
    target_pos: [f64; 3],
    target_on_ground: bool,
    entities: &mut [mc_entity::Entity],
) {
    let Some((entity_id, flying)) = lock(&ctx.players)
        .get(&attacker)
        .map(|p| (p.entity_id, p.flying))
    else {
        return;
    };
    let fall = blow.smash.unwrap_or_else(|| {
        lock(&ctx.vitals)
            .get(&attacker)
            .map_or(0.0, |v| v.fall_distance)
    });
    // The weapon's effects on its wielder, while the fall still counts.
    if let Some(w) = &blow.weapon {
        let mover = crate::enchant_effects::Mover {
            fall_distance: fall,
            flying,
            ..Default::default()
        };
        let s = crate::enchant_effects::Situation {
            this: Some("minecraft:player"),
            attacker: Some("minecraft:player"),
            direct_attacker: Some("minecraft:player"),
            mover: Some(&mover),
            ..Default::default()
        };
        let gear = [(0usize, "mainhand", w.clone())];
        for p in crate::enchant_effects::post_attack(ctx, &gear, "attacker", "attacker", &s) {
            if let Some(e) = &p.explode {
                let value = |k: &str| crate::enchant_effects::value(&e[k], p.level);
                let sound = e["sound"]
                    .as_str()
                    .unwrap_or("minecraft:entity.wind_charge.wind_burst");
                let gust = crate::projectiles::wind::Gust {
                    center: blow.from,
                    radius: value("radius"),
                    knockback: value("knockback_multiplier"),
                    sound: if sound.ends_with("wind_burst") && sound.contains("breeze") {
                        "entity.breeze.wind_burst"
                    } else {
                        "entity.wind_charge.wind_burst"
                    },
                    triggers: e["block_interaction"] == "trigger",
                };
                crate::projectiles::wind::burst(ctx, entities, &gust);
            }
        }
    }
    let Some(fall) = blow.smash else {
        return;
    };
    push_player(ctx, attacker, entity_id, [0.0, 0.01, 0.0]);
    if let Some(v) = lock(&ctx.vitals).get_mut(&attacker) {
        v.impulse_y = Some(blow.from[1]);
        v.fall_distance = 0.0;
    }
    let sound = if !target_on_ground {
        "item.mace.smash_air"
    } else if fall > 5.0 {
        "item.mace.smash_ground_heavy"
    } else {
        "item.mace.smash_ground"
    };
    fx::sound(ctx, sound, fx::category::PLAYERS, blow.from, 1.0, 1.0);
    fx::level_event(
        ctx,
        2013,
        [target_pos[0], target_pos[1] - 0.2, target_pos[2]],
        750,
    );
    let heavy = if fall > 5.0 { 2.0 } else { 1.0 };
    let power = |at: [f64; 3], resist: f64| -> Option<[f64; 3]> {
        let d = [
            at[0] - target_pos[0],
            at[1] - target_pos[1],
            at[2] - target_pos[2],
        ];
        let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if !(1e-9..=3.5).contains(&dist) {
            return None;
        }
        let k = (3.5 - dist) * 0.7 * heavy * (1.0 - resist);
        Some([d[0] / dist * k, 0.7, d[2] / dist * k])
    };
    for e in entities.iter_mut() {
        if e.id == target || e.removed {
            continue;
        }
        let mc_entity::Kind::Mob(m) = &e.kind else {
            continue;
        };
        if m.dying.is_some() {
            continue;
        }
        let resist = crate::mobs::hurt::armor_resistance(ctx, m);
        if let Some(v) = power(e.pos, resist) {
            for (a, b) in e.vel.iter_mut().zip(v) {
                *a += b;
            }
            e.on_ground = false;
        }
    }
    let players: Vec<(u128, i32, Pose)> = lock(&ctx.players)
        .values()
        .filter(|p| p.game_mode != 3 && p.dimension == ctx.world.dimension)
        .filter(|p| p.profile.uuid != attacker && p.entity_id != target)
        .filter_map(|p| Some((p.profile.uuid, p.entity_id, p.pose?)))
        .collect();
    for (uuid, id, pose) in players {
        if let Some(v) = power([pose.x, pose.y, pose.z], worn_resistance(ctx, uuid)) {
            push_player(ctx, uuid, id, v);
        }
    }
}

/// Thorns: the victim's armor hurts whoever hit them, and wears for it.
fn thorns(ctx: &ServerContext, victim: u128, attacker: i32, mode: i32) {
    let gear = crate::enchant_effects::gear(ctx, victim);
    let s = crate::enchant_effects::Situation {
        this: Some("minecraft:player"),
        ..Default::default()
    };
    for p in crate::enchant_effects::post_attack(ctx, &gear, "victim", "attacker", &s) {
        if let Some((kind, amount)) = &p.damage {
            hurt_entity(ctx, attacker, *amount, kind, Some(victim));
        }
        if p.wear > 0 && mode != 1 {
            crate::held::wear(ctx, victim, p.slot, p.wear);
        }
    }
}

/// A player hits another player in melee.
pub fn player_hits_player(
    ctx: &ServerContext,
    attacker: &crate::interact::Actor,
    name: &str,
    victim: u128,
) {
    player_hits_player_with(ctx, attacker, name, victim, None);
}

/// [`player_hits_player`] with the blow's base damage set (riptide).
pub fn player_hits_player_with(
    ctx: &ServerContext,
    attacker: &crate::interact::Actor,
    name: &str,
    victim: u128,
    base: Option<f32>,
) {
    let blow = swing_with(ctx, attacker.uuid, attacker.pose, base);
    let Some(at) = lock(&ctx.players).get(&victim).and_then(|p| p.pose) else {
        return;
    };
    let victim_id = lock(&ctx.players).get(&victim).map_or(0, |p| p.entity_id);
    let hit = Hit {
        amount: blow.damage,
        kind: blow.kind(),
        death_key: blow.death_key(),
        attacker: Some(name),
        by: Some(attacker.entity_id),
        from: Some(blow.from),
        knockback: blow.knockback,
        disables_shield: blow.disables_shield,
    };
    let landed =
        crate::damage::with_breach(blow.breach, || hurt_player(ctx, victim, &hit)).is_some();
    blow_sound(ctx, &blow, [at.x, at.y, at.z], landed);
    if landed {
        let mut entities = lock(&ctx.entities);
        after_blow(
            ctx,
            attacker.uuid,
            &blow,
            victim_id,
            [at.x, at.y, at.z],
            at.on_ground,
            &mut entities,
        );
    }
    if landed {
        if blow.fire > 0 {
            crate::worn::ignite(ctx, victim, blow.fire * 20);
        }
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

    #[test]
    fn mace_smash_grows_with_the_fall() {
        assert_eq!(smash_damage(1.5, 0.0), 6.0);
        assert_eq!(smash_damage(3.0, 0.0), 12.0);
        assert_eq!(smash_damage(8.0, 0.0), 22.0);
        assert_eq!(smash_damage(10.0, 0.0), 24.0);
        // Density V: 2.5 more a block.
        assert_eq!(smash_damage(10.0, 2.5), 49.0);
    }

    #[test]
    fn breach_lets_more_through_armor() {
        let plain = crate::damage::armor_reduce(10.0, 20.0, 0.0, "minecraft:mace_smash");
        let breached = crate::damage::with_breach(-0.6, || {
            crate::damage::armor_reduce(10.0, 20.0, 0.0, "minecraft:mace_smash")
        });
        // 15 points stop 60%; Breach IV takes all of that away.
        assert!((plain - 4.0).abs() < 1e-5, "{plain}");
        assert!((breached - 10.0).abs() < 1e-5, "{breached}");
        // Back to normal afterwards.
        assert_eq!(
            crate::damage::armor_reduce(10.0, 20.0, 0.0, "minecraft:mace_smash"),
            plain
        );
    }

    #[test]
    fn a_falling_mace_smashes_and_pushes_those_around() {
        use crate::testkit::{flat, join, stack, wear};
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let u = 0xa001;
        join(&ctx, u, 9301, [0.5, -60.0, 0.5], 0);
        wear(
            &ctx,
            u,
            36,
            stack(
                &ctx,
                "mace",
                &[("density", 2), ("breach", 4), ("wind_burst", 1)],
            ),
        );
        lock(&ctx.time).age = 1000;
        lock(&ctx.vitals).get_mut(&u).unwrap().fall_distance = 5.0;
        let pose = lock(&ctx.players).get(&u).unwrap().pose.unwrap();
        let blow = swing(&ctx, u, pose);
        assert_eq!(blow.smash, Some(5.0));
        assert_eq!(blow.kind(), "minecraft:mace_smash");
        assert!((blow.breach + 0.6).abs() < 1e-6);
        // 1 + 5, 12 + 4 for 5 blocks, Density II 1 a block.
        assert!((blow.damage - 27.0).abs() < 1e-4, "{}", blow.damage);
        // Not falling: no smash.
        lock(&ctx.vitals).get_mut(&u).unwrap().fall_distance = 1.0;
        assert_eq!(swing(&ctx, u, pose).smash, None);

        let mut near =
            crate::mobs::new_mob(&ctx, mc_entity::mob::Species::Zombie, [3.5, -60.0, 0.5]);
        near.on_ground = true;
        let mut far =
            crate::mobs::new_mob(&ctx, mc_entity::mob::Species::Zombie, [9.5, -60.0, 0.5]);
        far.on_ground = true;
        let mut entities = vec![near, far];
        let mut events = ctx.events.subscribe();
        after_blow(&ctx, u, &blow, -5, [1.5, -60.0, 0.5], true, &mut entities);
        assert!(
            entities[0].vel[1] >= 0.7 && entities[0].vel[0] > 0.0,
            "{:?}",
            entities[0].vel
        );
        assert_eq!(entities[1].vel, [0.0; 3]);
        let v = lock(&ctx.vitals).get(&u).cloned().unwrap();
        assert_eq!((v.fall_distance, v.impulse_y), (0.0, Some(-60.0)));
        // Wind Burst I throws the wielder up 1.2 times an explosion's push.
        let mut up = None;
        while let Some(e) = events.try_changed() {
            if let crate::context::Event::Explosion {
                knockback, radius, ..
            } = e
            {
                assert_eq!(radius, 3.5);
                up = knockback.iter().find(|k| k.0 == u).map(|k| k.1);
            }
        }
        let up = up.expect("a wind burst");
        assert!((up[1] - 1.2).abs() < 1e-6 && up[0].abs() < 1e-9, "{up:?}");
    }
}
