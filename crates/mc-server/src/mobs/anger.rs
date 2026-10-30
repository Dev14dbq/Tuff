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
//!
//! Piglins and brutes do it through their brain's memories: a hurt adult
//! turns on the nearest player it can see (line of sight, follow range) and
//! tells the adult piglins within 16 blocks to do the same, all of them
//! remembering for 30 seconds that they are angry at everyone; they forgive
//! a dead player wherever they are.

use mc_entity::{Entity, Kind, Species};

use crate::context::ServerContext;
use crate::tick::lock;

use super::aftermath::Note;
use super::dist_sq;
use super::terrain::Ground;
use mc_entity::mob::{PLAYER_EYE, line_clear};

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
    super::aftermath::push(
        ctx,
        super::aftermath::Note::PlayerDied { id: entity_id, at },
    );
}

/// Forgives deaths recorded since the last tick, then runs universal anger.
pub fn finish(ctx: &ServerContext, entities: &mut [Entity], notes: &[Note]) {
    if ctx.game_rule_bool("forgive_dead_players") {
        for note in notes {
            if let Note::PlayerDied { id, at } = note {
                forgive(entities, *id, *at);
            }
        }
    }
    if ctx.game_rule_bool("universal_anger") {
        spread(entities);
        aim(ctx, entities);
        piglins(ctx, entities);
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
        let piglin = is_piglin(m.species);
        if !(piglin || neutral(m.species) && within(pos)) {
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

/// Piglins and their brutes (vanilla's `AbstractPiglin`).
fn is_piglin(species: Species) -> bool {
    matches!(species, Species::Piglin | Species::PiglinBrute)
}

/// How long a piglin stays angry at whom it was told to be, and angry at
/// every player: 30 seconds (600 ticks of brain memory).
const PIGLIN_ANGER: i32 = 600;

/// A player a piglin may turn on.
struct Standing {
    uuid: u128,
    id: i32,
    at: [f64; 3],
}

/// Under `universal_anger` a piglin a player hurt (not a baby, which runs
/// from them) turns on the nearest player it can see, falling back on the
/// one who hurt it, and tells every adult piglin and brute within 16 blocks
/// to do the same with their own nearest player in sight. A piglin with
/// the universal memory and nobody to be angry at goes for the nearest
/// player in sight as well.
fn piglins(ctx: &ServerContext, entities: &mut [Entity]) {
    if ctx.difficulty() == 0
        || !entities
            .iter()
            .any(|e| matches!(&e.kind, Kind::Mob(m) if is_piglin(m.species)))
    {
        return;
    }
    let ground = Ground::new(ctx);
    let players = standing(ctx);
    // The nearest player in the piglin's sight and follow range.
    let nearest = |m: &mc_entity::Mob, pos: [f64; 3]| -> Option<i32> {
        let eye = [pos[0], pos[1] + m.eye_height(), pos[2]];
        let range = m.species.follow_range();
        players
            .iter()
            .map(|p| (dist_sq(pos, p.at), p))
            .filter(|&(d, p)| {
                d <= range * range
                    && line_clear(&ground, eye, [p.at[0], p.at[1] + PLAYER_EYE, p.at[2]])
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p.id)
    };
    let angry_at = |m: &mut mc_entity::Mob, id: i32| {
        m.target = Some(id);
        m.revenge = None;
        m.anger = PIGLIN_ANGER;
        m.universal = PIGLIN_ANGER;
        m.dirty = true;
    };
    let hit: Vec<usize> = entities
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match &e.kind {
            Kind::Mob(m) if is_piglin(m.species) && m.provoked && !m.is_baby() && !m.is_dead() => {
                Some(i)
            }
            _ => None,
        })
        .collect();
    for i in hit {
        let pos = entities[i].pos;
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        // Whoever hurt it, if it cannot see anyone.
        let hitter = m
            .hurt_by_player
            .and_then(|(uuid, _)| players.iter().find(|p| p.uuid == uuid))
            .map(|p| p.id);
        if let Some(id) = nearest(m, pos).or(hitter) {
            angry_at(m, id);
        }
        for (j, other) in entities.iter_mut().enumerate() {
            let near = (other.pos[0] - pos[0]).abs() <= 16.0
                && (other.pos[1] - pos[1]).abs() <= 16.0
                && (other.pos[2] - pos[2]).abs() <= 16.0;
            let opos = other.pos;
            let Kind::Mob(o) = &mut other.kind else {
                continue;
            };
            if j == i || !near || !is_piglin(o.species) || o.is_baby() || o.is_dead() {
                continue;
            }
            if let Some(id) = nearest(o, opos) {
                angry_at(o, id);
            }
        }
    }
    // The memory of being angry at everyone makes for a target.
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if !is_piglin(m.species)
            || m.universal <= 0
            || m.target.is_some()
            || m.is_baby()
            || m.is_dead()
            || m.below(5) != 0
        {
            continue;
        }
        if let Some(id) = nearest(m, pos) {
            m.target = Some(id);
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
    standing(ctx).into_iter().map(|p| (p.id, p.at)).collect()
}

fn standing(ctx: &ServerContext) -> Vec<Standing> {
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
        .map(|(uuid, id, at)| Standing { uuid, id, at })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobs::new_mob;
    use crate::rules::Value;
    use crate::testkit::{flat, join};

    fn settle(ctx: &ServerContext, entities: &mut [Entity]) {
        let notes = crate::mobs::aftermath::take(ctx);
        finish(ctx, entities, &notes);
    }

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
        settle(&ctx, &mut entities);
        assert!(mob(&entities[0]).anger > 0 && mob(&entities[1]).anger > 0);
        assert_eq!(mob(&entities[2]).anger, 0, "more than 10 blocks below");
        assert_eq!(mob(&entities[3]).anger, 0, "another kind");
        assert!(!mob(&entities[0]).provoked);
        for _ in 0..200 {
            settle(&ctx, &mut entities);
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
        settle(&ctx, &mut entities);
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
        settle(&ctx, &mut entities);
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
        settle(&ctx, &mut entities);
        assert_eq!(mob(&entities[0]).anger, 0);
        assert_eq!(mob(&entities[0]).target, None);
        assert!(mob(&entities[1]).anger > 0, "past 32 blocks");
        assert!(mob(&entities[2]).anger > 0, "angry at someone else");

        lock(&ctx.game_rules).set("forgive_dead_players", Value::Bool(false));
        note_death(&ctx, 7001, [0.5, -60.0, 0.5]);
        settle(&ctx, &mut entities);
        assert_eq!(mob(&entities[2]).target, Some(7001), "the rule is off");
    }
    fn piglin(ctx: &ServerContext, at: [f64; 3]) -> Entity {
        let mut e = new_mob(ctx, Species::Piglin, at);
        if let Kind::Mob(m) = &mut e.kind {
            m.set_baby(false);
        }
        e
    }

    fn universal_world() -> Option<std::sync::Arc<ServerContext>> {
        let ctx = flat()?;
        ctx.difficulty
            .store(2, std::sync::atomic::Ordering::Relaxed);
        lock(&ctx.game_rules).set("universal_anger", Value::Bool(true));
        Some(ctx)
    }

    #[test]
    fn a_hurt_piglin_turns_on_the_nearest_player_it_sees_and_calls_its_neighbours() {
        let Some(ctx) = universal_world() else {
            panic!("no game data");
        };
        // The hitter is far off; a bystander is nearer.
        join(&ctx, 1, 7000, [14.5, -60.0, 0.5], 0);
        join(&ctx, 2, 7001, [4.5, -60.0, 0.5], 0);
        let mut hit = piglin(&ctx, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut hit.kind {
            m.provoked = true;
            m.hurt_by_player = Some((1, 100));
        }
        let near = piglin(&ctx, [0.5, -60.0, 8.5]);
        let far = piglin(&ctx, [0.5, -60.0, 30.5]);
        let mut baby = piglin(&ctx, [0.5, -60.0, 4.5]);
        if let Kind::Mob(m) = &mut baby.kind {
            m.set_baby(true);
        }
        let brute = new_mob(&ctx, Species::PiglinBrute, [0.5, -60.0, -6.5]);
        let mut entities = vec![hit, near, far, baby, brute];
        settle(&ctx, &mut entities);
        let hit = mob(&entities[0]);
        assert_eq!(hit.target, Some(7001));
        assert_eq!(hit.anger, 600);
        assert!(hit.universal > 0 && !hit.provoked);
        assert!(mob(&entities[1]).target.is_some(), "neighbour told");
        assert!(mob(&entities[1]).universal > 0);
        assert_eq!(mob(&entities[2]).target, None, "too far off");
        assert_eq!(mob(&entities[3]).target, None, "a baby");
        assert!(mob(&entities[4]).target.is_some(), "a brute is told too");
    }

    #[test]
    fn a_piglin_does_not_turn_on_players_it_cannot_see() {
        let Some(ctx) = universal_world() else {
            panic!("no game data");
        };
        join(&ctx, 1, 7000, [10.5, -60.0, 0.5], 0);
        join(&ctx, 2, 7001, [-4.5, -60.0, 0.5], 0);
        // A wall between the piglin and the bystander on one side.
        for y in -60..=-58 {
            for z in -3..=3 {
                crate::testkit::set(&ctx, [-2, y, z], "minecraft:stone");
            }
        }
        let mut hit = piglin(&ctx, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut hit.kind {
            m.provoked = true;
            m.hurt_by_player = Some((1, 100));
        }
        let mut entities = vec![hit];
        settle(&ctx, &mut entities);
        // The bystander is nearer but hidden: the hitter it is.
        assert_eq!(mob(&entities[0]).target, Some(7000));

        // With the memory and no target, only a visible player draws it.
        let mut calm = piglin(&ctx, [0.5, -60.0, 0.5]);
        if let Kind::Mob(m) = &mut calm.kind {
            m.universal = 300;
        }
        let mut entities = vec![calm];
        for _ in 0..200 {
            settle(&ctx, &mut entities);
        }
        assert_eq!(mob(&entities[0]).target, Some(7000));
    }

    #[test]
    fn without_the_memory_a_calm_piglin_leaves_players_be() {
        let Some(ctx) = universal_world() else {
            panic!("no game data");
        };
        join(&ctx, 1, 7000, [4.5, -60.0, 0.5], 0);
        let mut entities = vec![piglin(&ctx, [0.5, -60.0, 0.5])];
        for _ in 0..200 {
            settle(&ctx, &mut entities);
        }
        assert_eq!(mob(&entities[0]).target, None);
    }

    #[test]
    fn piglins_forgive_a_dead_player_wherever_they_are() {
        let Some(ctx) = universal_world() else {
            panic!("no game data");
        };
        let angry = |at: [f64; 3]| {
            let mut e = piglin(&ctx, at);
            if let Kind::Mob(m) = &mut e.kind {
                m.anger = 400;
                m.target = Some(7000);
            }
            e
        };
        let mut entities = vec![angry([0.5, -60.0, 0.5]), angry([200.5, -60.0, 0.5])];
        note_death(&ctx, 7000, [0.5, -60.0, 0.5]);
        settle(&ctx, &mut entities);
        assert_eq!(mob(&entities[0]).target, None);
        assert_eq!(mob(&entities[1]).target, None);
        assert_eq!(mob(&entities[1]).anger, 0);
    }
}
