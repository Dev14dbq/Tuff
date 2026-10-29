//! Neutral mobs (bees, wolves, polar bears, iron golems, endermen and
//! zombified piglins) and two game rules, as vanilla runs them:
//! - `universal_anger`: a neutral mob a player hurts forgets whom it was
//!   after and is angry at every player until it picks one, the nearest
//!   survival or adventure player in its follow range. Bees, wolves and
//!   zombified piglins pass that on to every mob of their kind within follow
//!   range sideways and 10 blocks up or down, angry already or not. The
//!   player who hit it is not singled out.
//! - `forgive_dead_players`: when a player dies, the neutral mobs within 32
//!   blocks sideways and 10 up or down of where they died that were angry
//!   at them calm down. With the rule off they stay angry.

use mc_entity::{Entity, Kind, Species};

use crate::context::ServerContext;
use crate::tick::lock;

use super::dist_sq;

/// Vanilla's `NeutralMob`s, plus piglins, whose anger at a dead player is
/// let go the same way.
fn neutral(species: Species) -> bool {
    matches!(
        species,
        Species::Wolf
            | Species::Enderman
            | Species::ZombifiedPiglin
            | Species::Piglin
            | Species::Bee
            | Species::PolarBear
            | Species::IronGolem
    )
}

/// The kinds that call their own kind in when universal anger strikes.
fn alerts_kin(species: Species) -> bool {
    matches!(
        species,
        Species::Bee | Species::Wolf | Species::ZombifiedPiglin
    )
}

/// How long a neutral mob stays angry: 20 to 39 seconds.
fn anger_time(m: &mut mc_entity::Mob) -> i32 {
    400 + m.below(381) as i32
}

/// A player died at `at`. Applied on the next mob tick, which holds the
/// entity list.
pub fn note_death(ctx: &ServerContext, entity_id: i32, at: [f64; 3]) {
    ctx.mobs
        .deaths
        .lock()
        .expect("deaths")
        .push((ctx.dim, entity_id, at));
}

/// Forgives deaths recorded since the last tick, then runs universal anger.
pub fn finish(ctx: &ServerContext, entities: &mut [Entity]) {
    let deaths: Vec<_> = lock(&ctx.mobs.deaths)
        .extract_if(.., |(dim, _, _)| *dim == ctx.dim)
        .collect();
    if ctx.game_rule_bool("forgive_dead_players") {
        for (_, id, at) in deaths {
            forgive(entities, id, at);
        }
    }
    if ctx.game_rule_bool("universal_anger") {
        spread(entities);
        aim(ctx, entities);
    }
    for e in entities.iter_mut() {
        if let Kind::Mob(m) = &mut e.kind {
            m.provoked = false;
        }
    }
}

/// Vanilla tells the neutral mobs in a box around the dead player's block
/// (32 sideways, 10 up and down) and those angry at them stop.
fn forgive(entities: &mut [Entity], dead: i32, at: [f64; 3]) {
    let block = at.map(f64::floor);
    let within = |p: [f64; 3]| {
        p[0] >= block[0] - 32.0
            && p[0] < block[0] + 33.0
            && p[1] >= block[1] - 10.0
            && p[1] < block[1] + 11.0
            && p[2] >= block[2] - 32.0
            && p[2] < block[2] + 33.0
    };
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if !neutral(m.species) || !within(pos) {
            continue;
        }
        let aimed = m.target == Some(dead) || m.revenge.is_some_and(|(id, _)| id == dead);
        if aimed {
            m.anger = 0;
            m.target = None;
            m.revenge = None;
            m.dirty = true;
        }
    }
}

/// A player hurt these: they forget their target and turn on all players,
/// and some bring their kind along.
fn spread(entities: &mut [Entity]) {
    let sparks: Vec<(usize, Species, [f64; 3])> = entities
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let Kind::Mob(m) = &e.kind else {
                return None;
            };
            (m.provoked && neutral(m.species) && m.species != Species::Piglin && !m.is_tame())
                .then_some((i, m.species, e.pos))
        })
        .collect();
    for (i, species, pos) in sparks {
        let range = species.follow_range();
        for (j, e) in entities.iter_mut().enumerate() {
            let near = (e.pos[0] - pos[0].floor() - 0.5).abs() <= range + 0.5
                && (e.pos[1] - pos[1].floor() - 0.5).abs() <= 10.5
                && (e.pos[2] - pos[2].floor() - 0.5).abs() <= range + 0.5;
            if j != i && !(alerts_kin(species) && near) {
                continue;
            }
            let Kind::Mob(m) = &mut e.kind else {
                continue;
            };
            if m.species != species || m.is_tame() || m.is_dead() {
                continue;
            }
            m.anger = anger_time(m);
            m.target = None;
            m.revenge = None;
            m.dirty = true;
        }
    }
}

/// Mobs angry at everyone and after no one yet look for the nearest player
/// (vanilla's target search runs about one tick in five).
fn aim(ctx: &ServerContext, entities: &mut [Entity]) {
    if ctx.difficulty() == 0 {
        return;
    }
    let players = players(ctx);
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if !neutral(m.species)
            || m.species == Species::Piglin
            || m.anger <= 0
            || m.target.is_some()
            || m.is_tame()
            || m.is_dead()
        {
            continue;
        }
        if m.below(5) != 0 {
            continue;
        }
        let range = m.species.follow_range();
        let nearest = players
            .iter()
            .map(|&(id, at)| (dist_sq(pos, at), id))
            .filter(|&(d, _)| d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, id)) = nearest {
            m.target = Some(id);
            m.dirty = true;
        }
    }
}

/// Players a neutral mob may turn on: alive, here, survival or adventure.
fn players(ctx: &ServerContext) -> Vec<(i32, [f64; 3])> {
    let standing: Vec<(u128, i32, [f64; 3])> = {
        let players = lock(&ctx.players);
        players
            .values()
            .filter(|p| matches!(p.game_mode, 0 | 2) && p.dimension == ctx.world.dimension)
            .filter_map(|p| {
                p.pose
                    .map(|pose| (p.profile.uuid, p.entity_id, [pose.x, pose.y, pose.z]))
            })
            .collect()
    };
    let vitals = lock(&ctx.vitals);
    standing
        .into_iter()
        .filter(|(uuid, _, _)| !vitals.get(uuid).is_some_and(|v| v.dead))
        .map(|(_, id, at)| (id, at))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobs::new_mob;
    use crate::rules::Value;
    use crate::testkit::{flat, join};

    fn mob(e: &Entity) -> &mc_entity::Mob {
        let Kind::Mob(m) = &e.kind else {
            panic!("not a mob")
        };
        m
    }

    #[test]
    fn universal_anger_turns_the_kin_on_the_nearest_player() {
        let Some(ctx) = flat() else {
            return;
        };
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        // The hitter stands further off than a bystander.
        join(&ctx, 1, 7000, [20.5, -60.0, 0.5], 0);
        join(&ctx, 2, 7001, [6.0, -60.0, 0.5], 0);
        join(&ctx, 3, 7002, [2.0, -60.0, 0.5], 1);
        lock(&ctx.game_rules).set("universal_anger", Value::Bool(true));

        let mut hit = new_mob(&ctx, Species::ZombifiedPiglin, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut hit.kind {
            m.anger = 400;
            m.target = Some(7000);
            m.revenge = Some((7000, 100));
            m.provoked = true;
        }
        let kin = new_mob(&ctx, Species::ZombifiedPiglin, [3.0, -60.0, 0.5]);
        let far_kin = new_mob(&ctx, Species::ZombifiedPiglin, [3.0, -45.0, 0.5]);
        let other = new_mob(&ctx, Species::Wolf, [3.0, -60.0, 1.5]);
        let mut entities = vec![hit, kin, far_kin, other];
        finish(&ctx, &mut entities);
        assert!(mob(&entities[0]).anger > 0 && mob(&entities[1]).anger > 0);
        assert_eq!(mob(&entities[2]).anger, 0, "more than 10 blocks below");
        assert_eq!(mob(&entities[3]).anger, 0, "another kind");
        assert!(!mob(&entities[0]).provoked);
        for _ in 0..200 {
            finish(&ctx, &mut entities);
        }
        // Not the hitter, not the creative player: the nearest survival one.
        assert_eq!(mob(&entities[0]).target, Some(7001));
        assert_eq!(mob(&entities[1]).target, Some(7001));
        assert_eq!(mob(&entities[2]).target, None);
    }

    #[test]
    fn an_enderman_does_not_bring_its_kind() {
        let Some(ctx) = flat() else {
            return;
        };
        lock(&ctx.game_rules).set("universal_anger", Value::Bool(true));
        let mut hit = new_mob(&ctx, Species::Enderman, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut hit.kind {
            m.provoked = true;
        }
        let kin = new_mob(&ctx, Species::Enderman, [3.0, -60.0, 0.5]);
        let mut entities = vec![hit, kin];
        finish(&ctx, &mut entities);
        assert!(mob(&entities[0]).anger > 0);
        assert_eq!(mob(&entities[1]).anger, 0);
    }

    #[test]
    fn without_universal_anger_a_hit_changes_nothing_here() {
        let Some(ctx) = flat() else {
            return;
        };
        let mut hit = new_mob(&ctx, Species::ZombifiedPiglin, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut hit.kind {
            m.anger = 400;
            m.target = Some(7000);
            m.provoked = true;
        }
        let kin = new_mob(&ctx, Species::ZombifiedPiglin, [3.0, -60.0, 0.5]);
        let mut entities = vec![hit, kin];
        finish(&ctx, &mut entities);
        assert_eq!(mob(&entities[0]).target, Some(7000));
        assert_eq!(mob(&entities[1]).anger, 0);
    }

    #[test]
    fn forgive_dead_players_calms_those_angry_at_the_dead_nearby() {
        let Some(ctx) = flat() else {
            return;
        };
        let angry = |at: [f64; 3], target: i32| {
            let mut e = new_mob(&ctx, Species::ZombifiedPiglin, at);
            if let Kind::Mob(m) = &mut e.kind {
                m.anger = 400;
                m.target = Some(target);
            }
            e
        };
        let mut entities = vec![
            angry([10.5, -60.0, 0.5], 7000),
            angry([40.5, -60.0, 0.5], 7000),
            angry([10.5, -60.0, 0.5], 7001),
        ];
        // Off by default? No: vanilla's default is on.
        assert!(ctx.game_rule_bool("forgive_dead_players"));
        note_death(&ctx, 7000, [0.5, -60.0, 0.5]);
        finish(&ctx, &mut entities);
        assert_eq!(mob(&entities[0]).anger, 0);
        assert_eq!(mob(&entities[0]).target, None);
        assert!(mob(&entities[1]).anger > 0, "past 32 blocks");
        assert!(mob(&entities[2]).anger > 0, "angry at someone else");

        lock(&ctx.game_rules).set("forgive_dead_players", Value::Bool(false));
        note_death(&ctx, 7001, [0.5, -60.0, 0.5]);
        finish(&ctx, &mut entities);
        assert_eq!(mob(&entities[2]).target, Some(7001), "the rule is off");
    }
}
