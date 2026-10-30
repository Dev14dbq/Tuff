//! What a death leaves behind that needs the whole mob list: the effects
//! that act when their carrier is killed (oozing spawns slimes, weaving
//! cobwebs, wind charged a gust) and the players who died, for the neutral
//! mobs to forgive (see [`super::anger`]). Deaths are noted where they
//! happen and settled on the next mob tick of the dimension.

use mc_entity::aftermath;
use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::level::WorldLevel;
use crate::tick::lock;

/// A body killed while it carried effects that act on death.
#[derive(Clone, Debug)]
pub struct Dead {
    /// Entity id (a mob's own slime is never counted against it).
    pub id: i32,
    pub pos: [f64; 3],
    pub width: f64,
    pub height: f64,
    pub player: bool,
    pub effects: Vec<&'static str>,
}

/// Something to settle on the next mob tick.
#[derive(Clone, Debug)]
pub enum Note {
    /// A player died at a place.
    PlayerDied {
        id: i32,
        at: [f64; 3],
    },
    Killed(Dead),
}

const DEATH_EFFECTS: [&str; 3] = ["oozing", "weaving", "wind_charged"];

/// Notes a kill, if the victim carried an effect that acts on it.
pub fn killed(
    ctx: &ServerContext,
    id: i32,
    pos: [f64; 3],
    size: (f64, f64),
    player: bool,
    effects: &mc_entity::effect::Effects,
) {
    let carried: Vec<&'static str> = DEATH_EFFECTS
        .into_iter()
        .filter(|n| effects.active.contains_key(n))
        .collect();
    if carried.is_empty() {
        return;
    }
    push(
        ctx,
        Note::Killed(Dead {
            id,
            pos,
            width: size.0,
            height: size.1,
            player,
            effects: carried,
        }),
    );
}

pub fn push(ctx: &ServerContext, note: Note) {
    lock(&ctx.mobs.deaths).push((ctx.dim, note));
}

/// The notes of this dimension since the last tick.
pub fn take(ctx: &ServerContext) -> Vec<Note> {
    lock(&ctx.mobs.deaths)
        .extract_if(.., |(dim, _)| *dim == ctx.dim)
        .map(|(_, n)| n)
        .collect()
}

/// Settles the kills among `notes`.
pub fn apply(ctx: &ServerContext, level: &mut WorldLevel, entities: &mut [Entity], notes: &[Note]) {
    for note in notes {
        let Note::Killed(d) = note else { continue };
        for effect in &d.effects {
            match *effect {
                "oozing" => oozing(ctx, entities, d),
                "weaving" => weaving(ctx, level, d),
                "wind_charged" => wind(ctx, entities, d),
                _ => {}
            }
        }
    }
}

/// Slimes within two blocks of the body, counted up to `limit`.
fn slimes_near(entities: &[Entity], d: &Dead, limit: usize) -> usize {
    let half = d.width / 2.0 + 2.0;
    entities
        .iter()
        .filter(|e| e.id != d.id && !e.removed)
        .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == Species::Slime))
        .filter(|e| {
            let (w, h) = e.size();
            (e.pos[0] - d.pos[0]).abs() < w / 2.0 + half
                && (e.pos[2] - d.pos[2]).abs() < w / 2.0 + half
                && e.pos[1] < d.pos[1] + d.height + 2.0
                && d.pos[1] - 2.0 < e.pos[1] + h
        })
        .take(limit)
        .count()
}

/// Two slimes of size 2 where it fell, so long as the cramming cap leaves
/// room for them.
fn oozing(ctx: &ServerContext, entities: &[Entity], d: &Dead) {
    let cap = ctx
        .game_rules
        .lock()
        .expect("game rules lock")
        .int("minecraft:max_entity_cramming");
    let nearby = slimes_near(entities, d, aftermath::slime_count_limit(cap));
    let n = aftermath::slimes_to_spawn(cap, nearby as i32, 2);
    for _ in 0..n {
        let mut slime = super::new_mob(ctx, Species::Slime, [d.pos[0], d.pos[1] + 0.5, d.pos[2]]);
        if let Kind::Mob(m) = &mut slime.kind {
            m.set_size(2);
        }
        let _ = ctx.events.send(Event::EntitySpawned(slime.clone()));
        lock(&ctx.entities).push(slime);
    }
}

/// Two or three cobwebs on solid ground around the body.
fn weaving(ctx: &ServerContext, level: &mut WorldLevel, d: &Dead) {
    if !d.player && !ctx.game_rule_bool("mob_griefing") {
        return;
    }
    let Some(web) = ctx.data.blocks.default_state("minecraft:cobweb") else {
        return;
    };
    let origin = d.pos.map(|v| v.floor() as i32);
    let offset = || (crate::util::random_u64() % 3) as i32 - 1;
    let candidates: Vec<[i32; 3]> = (0..15)
        .map(|_| {
            [
                origin[0] + offset(),
                origin[1] + offset(),
                origin[2] + offset(),
            ]
        })
        .collect();
    let count = 2 + (crate::util::random_u64() % 2) as usize;
    let state_at = |c: [i32; 3]| {
        ctx.world
            .block(Position::new(c[0], c[1], c[2]), &ctx.data.blocks)
    };
    let cells = aftermath::cobweb_cells(&candidates, count, |c| {
        let here = state_at(c);
        let under = state_at([c[0], c[1] - 1, c[2]]);
        matches!((here, under), (Some(h), Some(u))
            if (ctx.shapes.is_air(h) || ctx.rules.is_replaceable(h))
                && ctx.shapes.support(u) & mc_tick::support::TOP_FULL != 0)
    });
    for c in cells {
        level.set_block(Position::new(c[0], c[1], c[2]), web);
        fx::level_event(
            ctx,
            3018,
            [
                f64::from(c[0]) + 0.5,
                f64::from(c[1]) + 0.5,
                f64::from(c[2]) + 0.5,
            ],
            0,
        );
    }
}

/// A burst of wind of 3 to 5 from the middle of the body.
fn wind(ctx: &ServerContext, entities: &mut [Entity], d: &Dead) {
    let strength = aftermath::gust_strength(crate::util::random_f64() as f32);
    crate::projectiles::wind::burst(
        ctx,
        entities,
        &crate::projectiles::wind::Gust {
            center: [d.pos[0], d.pos[1] + d.height / 2.0, d.pos[2]],
            radius: strength,
            knockback: 1.0,
            sound: "entity.breeze.wind_burst",
            triggers: true,
        },
    );
}

/// A hurt victim with the infested effect lets one or two silverfish out,
/// one time in ten, from the middle of its body.
pub fn infested(ctx: &ServerContext, pos: [f64; 3], height: f64, yaw: f32, pitch: f32) {
    if crate::util::random_f64() as f32 > aftermath::INFESTED_CHANCE {
        return;
    }
    let count = 1 + crate::util::random_u64() % 2;
    let look = crate::projectiles::look(yaw, pitch);
    for _ in 0..count {
        let angle = (crate::util::random_f64() as f32 - 0.5) * std::f32::consts::PI;
        let mut fish = super::new_mob(
            ctx,
            Species::Silverfish,
            [pos[0], pos[1] + height / 2.0, pos[2]],
        );
        fish.vel = aftermath::silverfish_push(look, angle);
        if let Kind::Mob(m) = &mut fish.kind {
            m.yaw = (crate::util::random_f64() * 360.0) as f32;
        }
        let _ = ctx.events.send(Event::EntitySpawned(fish.clone()));
        fx::sound(
            ctx,
            "entity.silverfish.hurt",
            fx::category::HOSTILE,
            fish.pos,
            1.0,
            1.0,
        );
        lock(&ctx.entities).push(fish);
    }
}
