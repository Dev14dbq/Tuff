//! Neutral anger and two game rules (minecraft.wiki, "Game rule"):
//! - `universal_anger`: a provoked neutral mob, and the others of its kind
//!   within follow range that are not already angry, go for the nearest
//!   player rather than only the one who provoked them;
//! - `forgive_dead_players`: they drop that anger when the player they were
//!   after dies nearby. With universal anger on, they switch to whoever is
//!   still there.

use mc_entity::{Entity, Kind, Species};

use crate::context::ServerContext;
use crate::tick::lock;

use super::dist_sq;

fn neutral(species: Species) -> bool {
    matches!(
        species,
        Species::Wolf
            | Species::Enderman
            | Species::ZombifiedPiglin
            | Species::Piglin
            | Species::Bee
    )
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

/// Forgives deaths recorded since the last tick, then spreads universal anger.
pub fn finish(ctx: &ServerContext, entities: &mut [Entity]) {
    let deaths: Vec<_> = lock(&ctx.mobs.deaths)
        .extract_if(.., |(dim, _, _)| *dim == ctx.dim)
        .collect();
    for (_, id, at) in deaths {
        forgive(ctx, entities, id, at);
    }
    if ctx.game_rule_bool("universal_anger") {
        share(ctx, entities);
    }
}

fn forgive(ctx: &ServerContext, entities: &mut [Entity], dead: i32, at: [f64; 3]) {
    if !ctx.game_rule_bool("forgive_dead_players") {
        return;
    }
    let universal = ctx.game_rule_bool("universal_anger");
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if !neutral(m.species) || m.anger <= 0 || m.is_tame() {
            continue;
        }
        let aimed = m.target == Some(dead) || m.revenge.is_some_and(|(id, _)| id == dead);
        if !aimed {
            continue;
        }
        let range = m.species.follow_range();
        if dist_sq(pos, at) > range * range {
            continue;
        }
        if universal {
            if let Some(aim) = nearest_player(ctx, pos, Some(dead)) {
                m.target = Some(aim);
                m.revenge = Some((aim, m.anger));
                m.dirty = true;
            }
        } else {
            m.anger = 0;
            m.target = None;
            m.revenge = None;
            m.dirty = true;
        }
    }
}

fn share(ctx: &ServerContext, entities: &mut [Entity]) {
    let sparks: Vec<(usize, Species, [f64; 3])> = entities
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let Kind::Mob(m) = &e.kind else {
                return None;
            };
            (neutral(m.species) && m.anger > 0 && !m.is_tame() && !m.is_dead())
                .then_some((i, m.species, e.pos))
        })
        .collect();
    for (i, species, pos) in sparks {
        let Some(aim) = nearest_player(ctx, pos, None) else {
            continue;
        };
        let range = species.follow_range();
        if let Kind::Mob(m) = &mut entities[i].kind
            && m.target != Some(aim)
        {
            m.target = Some(aim);
            m.dirty = true;
        }
        for (j, e) in entities.iter_mut().enumerate() {
            if j == i || dist_sq(e.pos, pos) > range * range {
                continue;
            }
            let Kind::Mob(m) = &mut e.kind else {
                continue;
            };
            if m.species != species || m.anger > 0 || m.is_tame() || m.is_dead() {
                continue;
            }
            m.anger = 400;
            m.target = Some(aim);
            m.dirty = true;
        }
    }
}

/// The closest living player's entity id, skipping `except`.
fn nearest_player(ctx: &ServerContext, at: [f64; 3], except: Option<i32>) -> Option<i32> {
    let standing: Vec<(u128, i32, [f64; 3])> = {
        let players = lock(&ctx.players);
        players
            .values()
            .filter(|p| p.game_mode != 3 && except != Some(p.entity_id))
            .filter_map(|p| {
                p.pose
                    .map(|pose| (p.profile.uuid, p.entity_id, [pose.x, pose.y, pose.z]))
            })
            .collect()
    };
    let mut best: Option<(f64, i32)> = None;
    for (uuid, id, pos) in standing {
        if lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.dead) {
            continue;
        }
        let d = dist_sq(at, pos);
        if best.is_none_or(|(so_far, _)| d < so_far) {
            best = Some((d, id));
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobs::new_mob;
    use crate::rules::Value;
    use crate::testkit::{flat, join};

    #[test]
    fn universal_anger_spreads_and_a_death_is_forgiven() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, 1, 7000, [0.5, -60.0, 0.5], 0);
        join(&ctx, 2, 7001, [8.0, -60.0, 0.5], 0);
        lock(&ctx.game_rules).set("universal_anger", Value::Bool(true));
        lock(&ctx.game_rules).set("forgive_dead_players", Value::Bool(true));

        let mut provoked = new_mob(&ctx, Species::ZombifiedPiglin, [0.5, -59.0, 0.5]);
        if let Kind::Mob(m) = &mut provoked.kind {
            m.anger = 400;
            m.target = Some(7000);
        }
        let calm = new_mob(&ctx, Species::ZombifiedPiglin, [3.0, -59.0, 0.5]);
        let mut entities = vec![provoked, calm];
        share(&ctx, &mut entities);
        let aimed =
            |e: &Entity| matches!(&e.kind, Kind::Mob(m) if m.target == Some(7000) && m.anger > 0);
        assert!(entities.iter().all(aimed), "both go for the nearer player");

        forgive(&ctx, &mut entities, 7000, [0.5, -60.0, 0.5]);
        assert!(
            entities
                .iter()
                .all(|e| matches!(&e.kind, Kind::Mob(m) if m.target == Some(7001))),
            "universal anger switches to the player still standing"
        );

        lock(&ctx.game_rules).set("universal_anger", Value::Bool(false));
        if let Kind::Mob(m) = &mut entities[0].kind {
            m.target = Some(7001);
            m.anger = 400;
        }
        forgive(&ctx, &mut entities, 7001, [8.0, -60.0, 0.5]);
        assert!(
            matches!(&entities[0].kind, Kind::Mob(m) if m.anger == 0 && m.target.is_none()),
            "without universal anger a death ends it"
        );
    }
}
