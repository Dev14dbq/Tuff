//! Spears (minecraft.wiki, "Spear", "Lunge"; the `piercing_weapon`,
//! `kinetic_weapon` and `attack_range` components of the 26.3 items).
//!
//! - A jab (the client's `punch`) lands only at a full charge of the blow
//!   and strikes everything on the line of sight between the least and the
//!   most reach, each box grown by the hitbox margin; it is never critical
//!   and sprinting adds no knockback. Lunge then pushes the player on.
//! - A charge (holding use) starts after the spear's delay; from then on
//!   whatever comes onto that line is struck once, if the player and it
//!   close in at 4.6 blocks a second or more, for the spear's multiplier
//!   times that speed. While the charge is young enough and the player fast
//!   enough the blow also knocks back, and earlier still unseats a rider.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use mc_entity::{Entity, Kind};
use serde_json::Value;

use crate::context::{Pose, ServerContext};
use crate::fx;
use crate::tick::lock;

/// A spear's charge: how long each effect lasts and how fast it asks for.
#[derive(Clone, Debug, PartialEq)]
pub struct Kinetic {
    pub delay: i64,
    pub multiplier: f32,
    /// (ticks it lasts, blocks a second needed).
    pub damage: (i64, f64),
    pub knockback: (i64, f64),
    pub dismount: (i64, f64),
    pub sound: Option<String>,
    pub hit_sound: Option<String>,
}

/// A spear as its components describe it.
#[derive(Clone, Debug, PartialEq)]
pub struct Spear {
    /// The jab's sounds (`piercing_weapon`), if it jabs.
    pub jab: Option<(Option<String>, Option<String>)>,
    pub charge: Option<Kinetic>,
    /// Reach: least and most, and in creative.
    pub reach: (f64, f64),
    pub creative_reach: (f64, f64),
    pub margin: f64,
}

fn sound(v: &Value) -> Option<String> {
    v.as_str()
        .or_else(|| v["sound_id"].as_str())
        .map(|s| s.trim_start_matches("minecraft:").to_owned())
}

impl Spear {
    /// From an item's components; `None` unless it jabs or charges.
    pub fn parse(c: &Value) -> Option<Self> {
        let piercing = &c["minecraft:piercing_weapon"];
        let kinetic = &c["minecraft:kinetic_weapon"];
        if piercing.is_null() && kinetic.is_null() {
            return None;
        }
        let cond = |k: &str, speed: &str| {
            let v = &kinetic[k];
            (
                v["max_duration_ticks"].as_i64().unwrap_or(0),
                v[speed].as_f64().unwrap_or(f64::MAX),
            )
        };
        let range = &c["minecraft:attack_range"];
        let f = |k: &str, d: f64| range[k].as_f64().unwrap_or(d);
        Some(Self {
            jab: (!piercing.is_null())
                .then(|| (sound(&piercing["sound"]), sound(&piercing["hit_sound"]))),
            charge: (!kinetic.is_null()).then(|| Kinetic {
                delay: kinetic["delay_ticks"].as_i64().unwrap_or(0),
                multiplier: kinetic["damage_multiplier"].as_f64().unwrap_or(1.0) as f32,
                damage: cond("damage_conditions", "min_relative_speed"),
                knockback: cond("knockback_conditions", "min_speed"),
                dismount: cond("dismount_conditions", "min_speed"),
                sound: sound(&kinetic["sound"]),
                hit_sound: sound(&kinetic["hit_sound"]),
            }),
            reach: (f("min_reach", 0.0), f("max_reach", 3.0)),
            creative_reach: (f("min_creative_reach", 0.0), f("max_creative_reach", 5.0)),
            margin: f("hitbox_margin", 0.0),
        })
    }
}

/// Where the player looks: a unit vector.
fn look(pose: &Pose) -> [f64; 3] {
    let (yaw, pitch) = (
        f64::from(pose.yaw).to_radians(),
        f64::from(pose.pitch).to_radians(),
    );
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

/// Whether the segment from `a` to `b` passes through the box.
fn crosses(min: [f64; 3], max: [f64; 3], a: [f64; 3], b: [f64; 3]) -> bool {
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for i in 0..3 {
        let d = b[i] - a[i];
        if d.abs() < 1e-12 {
            if a[i] < min[i] || a[i] > max[i] {
                return false;
            }
            continue;
        }
        let (mut lo, mut hi) = ((min[i] - a[i]) / d, (max[i] - a[i]) / d);
        if lo > hi {
            std::mem::swap(&mut lo, &mut hi);
        }
        t0 = t0.max(lo);
        t1 = t1.min(hi);
        if t0 > t1 {
            return false;
        }
    }
    true
}

/// The reach segment from the eyes.
fn segment(pose: &Pose, reach: (f64, f64)) -> ([f64; 3], [f64; 3]) {
    let eye = [pose.x, pose.y + 1.62, pose.z];
    let d = look(pose);
    let at = |r: f64| [eye[0] + d[0] * r, eye[1] + d[1] * r, eye[2] + d[2] * r];
    (at(reach.0), at(reach.1))
}

/// The ids of the mobs and other living entities the segment crosses.
fn struck(entities: &[Entity], pose: &Pose, reach: (f64, f64), margin: f64, me: i32) -> Vec<i32> {
    let (a, b) = segment(pose, reach);
    entities
        .iter()
        .filter(|e| !e.removed && e.id != me && matches!(e.kind, Kind::Mob(_) | Kind::Merchant(_)))
        .filter(|e| {
            let bx = e.aabb();
            let grow = |v: [f64; 3], s: f64| [v[0] + s, v[1] + s, v[2] + s];
            crosses(grow(bx.min, -margin), grow(bx.max, margin), a, b)
        })
        .map(|e| e.id)
        .collect()
}

/// Other players the segment crosses: (uuid, entity id, feet).
fn struck_players(
    ctx: &ServerContext,
    pose: &Pose,
    reach: (f64, f64),
    margin: f64,
    me: u128,
) -> Vec<(u128, i32, Pose)> {
    let (a, b) = segment(pose, reach);
    lock(&ctx.players)
        .values()
        .filter(|p| p.profile.uuid != me && p.dimension == ctx.world.dimension && p.game_mode != 3)
        .filter_map(|p| Some((p.profile.uuid, p.entity_id, p.pose?)))
        .filter(|(_, _, at)| {
            let min = [at.x - 0.3 - margin, at.y - margin, at.z - 0.3 - margin];
            let max = [
                at.x + 0.3 + margin,
                at.y + 1.8 + margin,
                at.z + 0.3 + margin,
            ];
            crosses(min, max, a, b)
        })
        .collect()
}

fn held_spear(ctx: &ServerContext, uuid: u128, hand: i32) -> Option<(crate::held::Held, Spear)> {
    let h = crate::held::held(ctx, uuid, hand)?;
    let spear = ctx.items.get(h.stack.item)?.spear.clone()?;
    Some((h, spear))
}

fn reach_of(spear: &Spear, game_mode: i32) -> (f64, f64) {
    if game_mode == 1 {
        spear.creative_reach
    } else {
        spear.reach
    }
}

/// The mob-side attacker for a player.
fn by_of(ctx: &ServerContext, actor: &crate::interact::Actor) -> crate::mobs::Actor {
    crate::mobs::Actor {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        name: crate::hits::name_of(ctx, actor.uuid).unwrap_or_default(),
        pos: [actor.pose.x, actor.pose.y, actor.pose.z],
        game_mode: actor.game_mode,
        sneaking: actor.sneaking,
    }
}

/// A jab: the client punched with what it holds. Whether it was a spear.
pub fn jab(ctx: &ServerContext, actor: &crate::interact::Actor) -> bool {
    let Some((held, spear)) = held_spear(ctx, actor.uuid, 0) else {
        return false;
    };
    let Some((jab_sound, _)) = &spear.jab else {
        return false;
    };
    let blow = crate::combat::jab(ctx, actor.uuid, actor.pose);
    // A spear jabs only at a full charge.
    if blow.strength < 1.0 {
        return true;
    }
    let at = [actor.pose.x, actor.pose.y, actor.pose.z];
    if let Some(s) = jab_sound {
        fx::sound(ctx, s, fx::category::PLAYERS, at, 1.0, 1.0);
    }
    let reach = reach_of(&spear, actor.game_mode);
    let mobs = struck(
        &lock(&ctx.entities),
        &actor.pose,
        reach,
        spear.margin,
        actor.entity_id,
    );
    let by = by_of(ctx, actor);
    for id in mobs {
        ctx.mobs.hit(ctx.dim, id, by.clone(), blow.clone());
    }
    for (victim, _, _) in struck_players(ctx, &actor.pose, reach, spear.margin, actor.uuid) {
        crate::combat::blow_hits_player(ctx, actor, &by.name, victim, blow.clone(), true);
    }
    lunge(ctx, actor, &held);
    true
}

/// Lunge (minecraft.wiki, "Lunge"): a jab pushes the player on along where
/// they look, 0.458 a level, at the cost of 4 exhaustion a level and a
/// point of wear; not riding, gliding or in water, nor with 6 food or less
/// (but in creative).
fn lunge(ctx: &ServerContext, actor: &crate::interact::Actor, held: &crate::held::Held) {
    let level = crate::held::enchant(held.comps.as_ref(), "lunge");
    if level <= 0 {
        return;
    }
    let riding = crate::stats::riding(ctx, actor.entity_id);
    let (food, gliding) = lock(&ctx.vitals)
        .get(&actor.uuid)
        .map_or((0, false), |v| (v.food, v.gliding));
    let wet = crate::hits::in_water(ctx, [actor.pose.x, actor.pose.y, actor.pose.z]);
    if riding || gliding || wet || actor.game_mode != 1 && food < 7 {
        return;
    }
    let d = look(&actor.pose);
    let flat = (d[0] * d[0] + d[2] * d[2]).sqrt().max(1e-9);
    let m = 0.458 * f64::from(level);
    crate::combat::push_player(
        ctx,
        actor.uuid,
        actor.entity_id,
        [d[0] / flat * m, 0.0, d[2] / flat * m],
    );
    if actor.game_mode != 1 {
        if let Some(v) = lock(&ctx.vitals).get_mut(&actor.uuid) {
            v.exhaust(4.0 * level as f32);
        }
        crate::held::wear(ctx, actor.uuid, held.slot, 1);
    }
    let n = 1 + (crate::util::random_u64() % 3);
    fx::sound(
        ctx,
        &format!("item.spear.lunge_{n}"),
        fx::category::PLAYERS,
        [actor.pose.x, actor.pose.y, actor.pose.z],
        1.0,
        1.0,
    );
}

/// A charge under way.
#[derive(Debug)]
struct Charge {
    entity_id: i32,
    hand: i32,
    game_mode: i32,
    started: i64,
    /// Where the player was a tick ago.
    last: Option<[f64; 3]>,
    hit: HashSet<i32>,
    hit_players: HashSet<u128>,
    /// How many mobs this charge struck.
    mobs: i32,
}

static CHARGES: Mutex<Option<HashMap<u128, Charge>>> = Mutex::new(None);

fn charges<R>(f: impl FnOnce(&mut HashMap<u128, Charge>) -> R) -> R {
    f(CHARGES
        .lock()
        .expect("charges lock")
        .get_or_insert_with(Default::default))
}

/// The player started using what `hand` holds: a spear lowers to charge.
/// Whether it was one.
pub fn start(ctx: &ServerContext, uuid: u128, entity_id: i32, hand: i32, game_mode: i32) -> bool {
    let Some((_, spear)) = held_spear(ctx, uuid, hand) else {
        return false;
    };
    let Some(k) = &spear.charge else {
        return false;
    };
    let started = lock(&ctx.time).age;
    if let Some(s) = &k.sound
        && let Some(at) = lock(&ctx.players).get(&uuid).and_then(|p| p.pose)
    {
        fx::sound(ctx, s, fx::category::PLAYERS, [at.x, at.y, at.z], 1.0, 1.0);
    }
    let _ = ctx.events.send(crate::context::Event::UsingItem {
        uuid,
        entity_id,
        hand: Some(hand),
    });
    charges(|c| {
        c.insert(
            uuid,
            Charge {
                entity_id,
                hand,
                game_mode,
                started,
                last: None,
                hit: HashSet::new(),
                hit_players: HashSet::new(),
                mobs: 0,
            },
        )
    });
    true
}

/// The player let go of use: the entity id of a player who was charging.
pub fn stop(uuid: u128) -> Option<i32> {
    charges(|c| c.remove(&uuid)).map(|c| c.entity_id)
}

/// Speeds in blocks a second along `dir`.
fn along(v: [f64; 3], dir: [f64; 3]) -> f64 {
    (v[0] * dir[0] + v[1] * dir[1] + v[2] * dir[2]) * 20.0
}

fn speed(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() * 20.0
}

/// One tick of every charge.
pub fn tick(ctx: &ServerContext) {
    let uuids: Vec<u128> = charges(|c| c.keys().copied().collect());
    for uuid in uuids {
        let here = ctx.of_player(uuid);
        charge_tick(&here, uuid);
    }
}

fn charge_tick(ctx: &ServerContext, uuid: u128) {
    let Some(pose) = lock(&ctx.players).get(&uuid).and_then(|p| p.pose) else {
        stop(uuid);
        return;
    };
    let Some((entity_id, hand, game_mode, started, last)) = charges(|c| {
        let ch = c.get_mut(&uuid)?;
        let last = ch.last.replace([pose.x, pose.y, pose.z]);
        Some((ch.entity_id, ch.hand, ch.game_mode, ch.started, last))
    }) else {
        return;
    };
    let Some((held, spear)) = held_spear(ctx, uuid, hand) else {
        stop(uuid);
        return;
    };
    let Some(k) = spear.charge.clone() else {
        stop(uuid);
        return;
    };
    let age = lock(&ctx.time).age - started - k.delay;
    let Some(last) = last else {
        return;
    };
    if age < 0 || age > k.damage.0 {
        return;
    }
    let v = [pose.x - last[0], pose.y - last[1], pose.z - last[2]];
    let dir = look(&pose);
    let own = speed(v);
    let reach = reach_of(&spear, game_mode);
    let by = by_of(
        ctx,
        &crate::interact::Actor {
            uuid,
            entity_id,
            game_mode,
            pose,
            sneaking: false,
        },
    );
    let mut struck_now = 0;
    let mut unseated = Vec::new();
    {
        let mut entities = lock(&ctx.entities);
        let ids = struck(&entities, &pose, reach, spear.margin, entity_id);
        for id in ids {
            if charges(|c| c.get(&uuid).is_some_and(|ch| ch.hit.contains(&id))) {
                continue;
            }
            let Some(e) = entities.iter_mut().find(|e| e.id == id) else {
                continue;
            };
            let closing = along(v, dir) - along(e.vel, dir);
            if closing < k.damage.1 {
                continue;
            }
            charges(|c| c.get_mut(&uuid).map(|ch| ch.hit.insert(id)));
            let knock = age <= k.knockback.0 && own >= k.knockback.1;
            let unseat = age <= k.dismount.0 && own >= k.dismount.1;
            let mut h =
                crate::mobs::hurt::Harm::new(k.multiplier * closing as f32, "minecraft:spear");
            h.player = Some(uuid);
            h.by = Some(entity_id);
            h.killer = Some("minecraft:player".into());
            h.weapon = Some(crate::loot::Tool {
                item: held.name.clone(),
                enchants: crate::mobs::hurt::enchant_list(held.comps.as_ref()),
            });
            if knock {
                h.from = Some([pose.x, pose.y, pose.z]);
            }
            if crate::mobs::hurt::harm(ctx, e, &h) {
                if matches!(e.kind, Kind::Mob(_)) {
                    struck_now += 1;
                }
                if unseat {
                    unseated.push(id);
                }
                if let Some(s) = &k.hit_sound {
                    fx::sound(ctx, s, fx::category::PLAYERS, e.pos, 1.0, 1.0);
                }
                if game_mode != 1 {
                    crate::held::wear(ctx, uuid, held.slot, 1);
                }
            }
        }
        // Those struck off what they ride (a mount with only them on it).
        for id in unseated {
            if let Some(mount) = entities
                .iter_mut()
                .find(|m| matches!(&m.kind, Kind::Mob(mob) if mob.riders == [id]))
            {
                crate::mobs::ride::eject(ctx, mount);
            }
        }
    }
    for (victim, _, at) in struck_players(ctx, &pose, reach, spear.margin, uuid) {
        if charges(|c| {
            c.get(&uuid)
                .is_some_and(|ch| ch.hit_players.contains(&victim))
        }) {
            continue;
        }
        // The other player's motion is not known here: theirs counts as
        // still.
        let _ = at;
        let closing = along(v, dir);
        if closing < k.damage.1 {
            continue;
        }
        let blow = crate::combat::Blow {
            damage: k.multiplier * closing as f32,
            strength: 1.0,
            from: [pose.x, pose.y, pose.z],
            yaw: pose.yaw,
            spear: true,
            ..Default::default()
        };
        let knock = age <= k.knockback.0 && own >= k.knockback.1;
        charges(|c| c.get_mut(&uuid).map(|ch| ch.hit_players.insert(victim)));
        crate::combat::blow_hits_player(
            ctx,
            &crate::interact::Actor {
                uuid,
                entity_id,
                game_mode,
                pose,
                sneaking: false,
            },
            &by.name,
            victim,
            blow,
            knock,
        );
    }
    if struck_now > 0 {
        let total = charges(|c| {
            c.get_mut(&uuid).map_or(0, |ch| {
                ch.mobs += struck_now;
                ch.mobs
            })
        });
        crate::criteria::spear_mobs(ctx, uuid, total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spears_of_26_3_read_from_their_components() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../cache/mcdata/26.3/reports/minecraft/components/item/iron_spear.json");
        let Ok(text) = std::fs::read_to_string(dir) else {
            return;
        };
        let v: Value = serde_json::from_str(&text).unwrap();
        let s = Spear::parse(&v["components"]).unwrap();
        assert_eq!(s.reach, (2.0, 4.5));
        assert_eq!(s.margin, 0.125);
        let k = s.charge.unwrap();
        assert_eq!((k.delay, k.multiplier), (12, 0.95));
        assert_eq!(k.damage, (225, 4.6));
        assert_eq!(k.knockback, (135, 5.1));
        assert_eq!(k.dismount, (50, 11.0));
        assert!(s.jab.is_some());
    }

    #[test]
    fn a_charge_strikes_what_it_runs_into_by_how_fast() {
        use crate::testkit;
        const ME: u128 = 0x5bea;
        let Some(ctx) = testkit::flat() else { return };
        testkit::join(&ctx, ME, 42, [0.5, -60.0, 0.5], 0);
        crate::commands::load_progress(&ctx, ME, &[], &crate::players::BookState::default());
        testkit::wear(&ctx, ME, 36, testkit::stack(&ctx, "iron_spear", &[]));
        let zombie = crate::mobs::new_mob(&ctx, mc_entity::mob::Species::Zombie, [0.5, -60.0, 4.5]);
        let (zid, before) = match &zombie.kind {
            Kind::Mob(m) => (zombie.id, m.health),
            _ => unreachable!(),
        };
        lock(&ctx.entities).push(zombie);
        assert!(start(&ctx, ME, 42, 0, 0));
        // Past the delay, running at it at 10 blocks a second.
        lock(&ctx.time).age += 13;
        charge_tick(&ctx, ME);
        testkit::place(&ctx, ME, [0.5, -60.0, 1.0], true);
        lock(&ctx.time).age += 1;
        charge_tick(&ctx, ME);
        let health = |ctx: &ServerContext| {
            lock(&ctx.entities)
                .iter()
                .find(|e| e.id == zid)
                .and_then(|e| match &e.kind {
                    Kind::Mob(m) => Some(m.health),
                    _ => None,
                })
                .unwrap()
        };
        let after = health(&ctx);
        // 0.95 × 10 before the zombie's own armor (2 points) takes a little.
        let lost = before - after;
        assert!(lost > 9.0 && lost <= 9.5, "{before} -> {after}");
        // Each is struck once a charge.
        testkit::place(&ctx, ME, [0.5, -60.0, 1.5], true);
        lock(&ctx.time).age += 1;
        charge_tick(&ctx, ME);
        assert_eq!(health(&ctx), after);
        stop(ME);
    }

    #[test]
    fn a_line_of_sight_strikes_what_it_passes_through() {
        let pose = Pose {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        };
        // Looking along +z at eye height: a box 3 blocks ahead is struck,
        // one a block ahead (too near) or 6 ahead (too far) is not.
        let (a, b) = segment(&pose, (2.0, 4.5));
        let at = |z: f64| ([-0.3, 1.0, z - 0.3], [0.3, 2.0, z + 0.3]);
        let hit = |z: f64| {
            let (min, max) = at(z);
            crosses(min, max, a, b)
        };
        assert!(hit(3.0));
        assert!(!hit(1.0));
        assert!(!hit(6.0));
    }
}
