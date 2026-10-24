//! A thrown eye of ender (minecraft.wiki, "Eye of Ender"): it flies about
//! twelve blocks towards the nearest stronghold, through anything in the
//! way, rises when the stronghold is further than that and sinks when it is
//! nearer, hovers, and after four seconds either drops as an item (four
//! times in five) or shatters.

use mc_entity::extra::Shot;
use mc_entity::{Entity, Kind};

use super::spawn;
use crate::commands::places::nearest_structure;
use crate::context::ServerContext;
use crate::fx;
use crate::hits::{self, Tick};
use crate::interact::{Actor, Outcome};

/// Ticks an eye flies for.
pub const LIFE: u32 = 80;
/// How far a throw reaches towards a stronghold, in blocks.
const REACH: f64 = 12.0;
/// How far above the thrower an eye rises when the stronghold is further
/// than [`REACH`].
const RISE: f64 = 8.0;
/// Fastest an eye moves along an axis, in blocks per tick.
const SPEED: f64 = 0.5;
/// The share of the way to its mark an eye covers each tick.
const EASE: f64 = 0.12;

/// Where an eye thrown from `from` flies to for a stronghold at `stronghold`:
/// 12 blocks along the way and 8 up if it is further than that, the
/// stronghold itself if it is not.
pub fn mark(from: [f64; 3], stronghold: [f64; 3]) -> [f64; 3] {
    let (dx, dz) = (stronghold[0] - from[0], stronghold[2] - from[2]);
    let dist = dx.hypot(dz);
    if dist > REACH {
        [
            from[0] + dx / dist * REACH,
            from[1] + RISE,
            from[2] + dz / dist * REACH,
        ]
    } else {
        stronghold
    }
}

/// A right click in the air with an eye of ender: it flies off towards the
/// nearest stronghold, if there is one in this world.
pub fn throw(ctx: &ServerContext, actor: Actor, slot: usize) -> Outcome {
    let mut outcome = Outcome::default();
    let (x, y, z) = (
        actor.pose.x.floor() as i32,
        actor.pose.y.floor() as i32,
        actor.pose.z.floor() as i32,
    );
    let Some((_, at)) = nearest_structure(ctx, &["minecraft:stronghold".to_owned()], (x, y, z))
    else {
        return outcome;
    };
    // From the chest.
    let from = [actor.pose.x, actor.pose.y + 0.9, actor.pose.z];
    let target = mark(
        from,
        [
            f64::from(at.0) + 0.5,
            f64::from(at.1),
            f64::from(at.2) + 0.5,
        ],
    );
    let survive = hits::rnd() < 0.8;
    spawn(
        ctx,
        Shot::Eye { target, survive },
        Some(actor.uuid),
        from,
        [0.0; 3],
    );
    fx::sound(
        ctx,
        "entity.ender_eye.launch",
        fx::category::NEUTRAL,
        from,
        1.0,
        1.0,
    );
    if actor.game_mode != 1 {
        crate::held::consume(ctx, actor.uuid, slot, 1);
    }
    outcome.events.push(crate::context::Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    outcome
}

/// One tick of an eye in flight.
pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::Eye { target, survive } = p.shot else {
        return;
    };
    let ctx = t.ctx;
    e.age += 1;
    if e.age > LIFE {
        end(ctx, e.pos, survive);
        e.removed = true;
        return;
    }
    // It glides to its mark and stays there.
    for (i, goal) in target.iter().enumerate() {
        e.vel[i] = ((goal - e.pos[i]) * EASE).clamp(-SPEED, SPEED);
        e.pos[i] += e.vel[i];
    }
    fx::particles(ctx, "minecraft:portal", e.pos, 0.25, 0.1, 2);
}

/// The end of an eye's flight.
fn end(ctx: &ServerContext, at: [f64; 3], survive: bool) {
    fx::sound(
        ctx,
        "entity.ender_eye.death",
        fx::category::NEUTRAL,
        at,
        1.3,
        1.0,
    );
    if survive {
        if let Some(item) = ctx.items.id("minecraft:ender_eye") {
            crate::tick::spawn_item_at(ctx, at, item, 1);
        }
    } else {
        // The eye breaks in a burst of its own particles.
        fx::level_event(ctx, 2003, at, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;
    use crate::tick::lock;

    #[test]
    fn a_far_stronghold_is_twelve_blocks_off_and_eight_up() {
        let m = mark([0.0, 64.0, 0.0], [30.0, 0.0, 40.0]);
        assert!(
            (m[0] - 7.2).abs() < 1e-9 && (m[2] - 9.6).abs() < 1e-9,
            "{m:?}"
        );
        assert_eq!(m[1], 72.0);
        // A near one is the mark itself, below the thrower.
        assert_eq!(mark([0.0, 64.0, 0.0], [3.0, 0.0, 4.0]), [3.0, 0.0, 4.0]);
    }

    fn eye(ctx: &ServerContext, target: [f64; 3], survive: bool) -> Entity {
        let id = spawn(
            ctx,
            Shot::Eye { target, survive },
            None,
            [0.5, -58.0, 0.5],
            [0.0; 3],
        );
        let mut entities = lock(&ctx.entities);
        let i = entities.iter().position(|e| e.id == id).unwrap();
        entities.swap_remove(i)
    }

    fn fly(ctx: &ServerContext, e: &mut Entity, ticks: usize) {
        let mut out = hits::Out::default();
        for _ in 0..ticks {
            let mut t = Tick {
                ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            crate::projectiles::tick(&mut t, e);
            if e.removed {
                break;
            }
        }
    }

    fn items_near(ctx: &ServerContext) -> usize {
        lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(e.kind, Kind::Item { .. }))
            .count()
    }

    #[test]
    fn an_eye_flies_to_its_mark_hovers_and_drops_as_an_item() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mark = [7.7, -50.0, 9.6];
        let mut e = eye(&ctx, mark, true);
        fly(&ctx, &mut e, 60);
        assert!(!e.removed);
        for (pos, goal) in e.pos.iter().zip(mark) {
            assert!((pos - goal).abs() < 0.2, "{:?}", e.pos);
        }
        // Hovering.
        assert!(e.vel.iter().all(|v| v.abs() < 0.05), "{:?}", e.vel);
        let before = items_near(&ctx);
        fly(&ctx, &mut e, 40);
        assert!(e.removed);
        assert_eq!(items_near(&ctx), before + 1);
    }

    #[test]
    fn an_eye_that_shatters_leaves_no_item() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = eye(&ctx, [3.0, -55.0, 3.0], false);
        let before = items_near(&ctx);
        fly(&ctx, &mut e, 100);
        assert!(e.removed);
        assert_eq!(e.age, LIFE + 1);
        assert_eq!(items_near(&ctx), before);
    }

    #[test]
    fn with_no_stronghold_in_the_world_an_eye_stays_in_the_hand() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let u = 0x9301;
        crate::testkit::join(&ctx, u, 9401, [0.5, -60.0, 0.5], 0);
        crate::testkit::wear(&ctx, u, 36, crate::testkit::stack(&ctx, "ender_eye", &[]));
        let pose = lock(&ctx.players).get(&u).unwrap().pose.unwrap();
        let actor = Actor {
            uuid: u,
            entity_id: 9401,
            game_mode: 0,
            pose,
            sneaking: false,
        };
        assert!(crate::projectiles::use_item(&ctx, actor, 0, 0.0, 0.0).is_some());
        assert!(lock(&ctx.inventories).get(&u).unwrap().slots[36].is_some());
        assert!(
            !lock(&ctx.entities).iter().any(
                |e| matches!(&e.kind, Kind::Projectile(p) if matches!(p.shot, Shot::Eye { .. }))
            )
        );
    }

    #[test]
    fn an_eye_sinks_towards_a_stronghold_below() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = eye(&ctx, [2.5, -62.0, 1.5], true);
        fly(&ctx, &mut e, 20);
        assert!(e.pos[1] < -60.0, "{:?}", e.pos);
    }
}
