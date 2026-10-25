//! Arrows, thrown things, fireworks and fishing bobbers.
//!
//! Flight follows the public description (minecraft.wiki, "Arrow",
//! "Snowball", "Ender Pearl", "Potion"): each tick the projectile moves by
//! its velocity, stopping at the first block or entity on the way, then
//! loses speed to drag and gains downward speed to gravity.
//!
//! Other code (dispensers, commands) launches projectiles with [`spawn`] or
//! [`shoot`].

pub mod arrow;
pub mod bullet;
pub mod crossbow;
pub mod eye;
pub mod fangs;
pub mod fireball;
pub mod firework;
pub mod fishing;
pub mod lightning;
pub mod omen;
pub mod potions;
pub mod thrown;
pub mod trident;
pub mod wind;

use std::collections::HashMap;

use mc_entity::extra::{Projectile, Shot};
use mc_entity::{Entity, Kind};

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::hits::{self, Tick};
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// What players are in the middle of with projectiles.
#[derive(Debug, Default)]
pub struct State {
    /// A bow being drawn: world age when it started and the hand.
    pub draws: HashMap<u128, (i64, i32)>,
    /// A cast fishing bobber, by its owner.
    pub bobbers: HashMap<u128, i32>,
    /// Riptide spins, by player.
    pub spins: HashMap<u128, trident::Spin>,
    /// Crossbows being charged: the player's entity id and whether the
    /// start and middle sounds played.
    pub charging: HashMap<u128, (i32, bool, bool)>,
    /// Items a player may not use again before this world age.
    pub cooldowns: HashMap<(u128, &'static str), i64>,
}

/// The direction a player looks along.
pub fn look(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

/// Adds a projectile to the world at `pos` with velocity `vel`; returns its
/// entity id.
pub fn spawn(
    ctx: &ServerContext,
    shot: Shot,
    owner: Option<u128>,
    pos: [f64; 3],
    vel: [f64; 3],
) -> i32 {
    let (yaw, pitch) = fx::facing_of(vel);
    let entity = Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(Projectile {
            shot,
            owner,
            mob: None,
            yaw,
            pitch,
        }),
        pos,
        vel,
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let id = entity.id;
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
    id
}

/// [`spawn`] for a projectile a mob shot (`mob`: its entity id and name).
pub fn spawn_by_mob(
    ctx: &ServerContext,
    shot: Shot,
    mob: (i32, &'static str),
    pos: [f64; 3],
    vel: [f64; 3],
) -> i32 {
    let (yaw, pitch) = fx::facing_of(vel);
    let entity = Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(Projectile {
            shot,
            owner: None,
            mob: Some(mob),
            yaw,
            pitch,
        }),
        pos,
        vel,
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let id = entity.id;
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
    id
}

/// Launches a projectile along `dir` (any length) at `speed`, spread by
/// `inaccuracy` (1.0 for a player, 6.0 for a dispenser).
pub fn shoot(
    ctx: &ServerContext,
    shot: Shot,
    owner: Option<u128>,
    pos: [f64; 3],
    dir: [f64; 3],
    speed: f64,
    inaccuracy: f64,
) -> i32 {
    let len = dir.iter().map(|c| c * c).sum::<f64>().sqrt().max(1e-9);
    let spread = 0.0075 * inaccuracy;
    let vel = dir.map(|c| (c / len + hits::gauss() * spread) * speed);
    let source = owner.map_or_else(Default::default, |u| {
        crate::vibrations::Source::player(ctx, u)
    });
    crate::vibrations::emit(ctx, "projectile_shoot", pos, source, None);
    spawn(ctx, shot, owner, pos, vel)
}

/// Where a player's projectiles start: at the eyes, a little lower.
pub fn muzzle(actor: &Actor) -> [f64; 3] {
    [actor.pose.x, actor.pose.y + 1.62 - 0.1, actor.pose.z]
}

enum Class {
    Arrow,
    Trident,
    Lightning,
    Wind,
    Bullet,
    Fireball,
    Thrown,
    Eye,
    Firework,
    Bobber,
    Cloud,
    Boss,
    Fangs,
    Omen,
}

/// One tick of a projectile.
pub fn tick(t: &mut Tick, e: &mut Entity) {
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let class = match &p.shot {
        Shot::Arrow(a) if a.trident => Class::Trident,
        Shot::Arrow(_) => Class::Arrow,
        Shot::Lightning { .. } => Class::Lightning,
        Shot::WindCharge { .. } => Class::Wind,
        Shot::ShulkerBullet { .. } => Class::Bullet,
        Shot::Fireball { .. } => Class::Fireball,
        Shot::Eye { .. } => Class::Eye,
        Shot::Firework(_) => Class::Firework,
        Shot::Bobber { .. } => Class::Bobber,
        Shot::Cloud { .. } => Class::Cloud,
        Shot::Fangs { .. } => Class::Fangs,
        Shot::ItemSpawner { .. } => Class::Omen,
        Shot::WitherSkull { .. } | Shot::DragonFireball | Shot::Breath { .. } => Class::Boss,
        _ => Class::Thrown,
    };
    match class {
        Class::Arrow => arrow::tick(t, e),
        Class::Trident => trident::tick(t, e),
        Class::Lightning => lightning::tick(t, e),
        Class::Wind => wind::tick(t, e),
        Class::Bullet => bullet::tick(t, e),
        Class::Fireball => fireball::tick(t, e),
        Class::Thrown => thrown::tick(t, e),
        Class::Eye => eye::tick(t, e),
        Class::Firework => firework::tick(t, e),
        Class::Bobber => fishing::tick(t, e),
        Class::Cloud => potions::tick_cloud(t, e),
        Class::Boss => crate::bosses::projectile::tick(t, e),
        Class::Fangs => fangs::tick(t, e),
        Class::Omen => omen::tick(t, e),
    }
    if e.removed {
        return;
    }
    if let Kind::Projectile(p) = &mut e.kind
        && e.vel != [0.0; 3]
    {
        (p.yaw, p.pitch) = fx::facing_of(e.vel);
    }
    if let Kind::Projectile(p) = &e.kind {
        t.out.sync.push(fx::Sync {
            id: e.id,
            pos: e.pos,
            vel: e.vel,
            yaw: p.yaw,
            pitch: p.pitch,
            on_ground: e.on_ground,
        });
    }
}

/// The player pushed away from a hit (the entity's velocity packet).
pub fn knock(ctx: &ServerContext, victim: &hits::Who, from: [f64; 3], strength: f64) {
    let (dx, dz) = (victim.pose.x - from[0], victim.pose.z - from[2]);
    let len = dx.hypot(dz);
    if len < 1e-6 {
        return;
    }
    fx::emit(
        ctx,
        fx::Fx::Motion(mc_protocol::packets::play::SetEntityMotion {
            entity_id: victim.entity_id,
            velocity: [dx / len * strength, strength.min(0.4), dz / len * strength],
        }),
    );
}

/// Hurts the player at `t.players[i]`; returns whether it did damage.
pub fn hurt_player(
    t: &Tick,
    victim: &hits::Who,
    amount: f32,
    kind: &str,
    key: &str,
    owner: Option<u128>,
    with: i32,
) -> bool {
    let mob = t.shooter;
    let attacker = owner
        .and_then(|o| hits::name_of(t.ctx, o))
        .or(mob.map(|(_, name)| name.to_owned()));
    crate::vitals::try_hurt_by_entity(
        t.ctx,
        victim.uuid,
        amount,
        kind,
        key,
        attacker.as_deref(),
        Some(with),
    )
    .is_some()
}

/// A right click in the air with a projectile item: `Some` if the item was
/// one.
pub fn use_item(
    ctx: &ServerContext,
    actor: Actor,
    hand: i32,
    yaw: f32,
    pitch: f32,
) -> Option<Outcome> {
    let held = crate::held::held(ctx, actor.uuid, hand)?;
    let name = held.name.as_str();
    let mut outcome = Outcome::default();
    let consumes = actor.game_mode != 1;
    let aim = |offset: f32| look(yaw, pitch + offset);
    let (shot, speed, offset, sound) = match name {
        "minecraft:bow" => {
            if consumes && crate::held::find(ctx, actor.uuid, is_arrow).is_none() {
                return Some(outcome);
            }
            let now = lock(&ctx.time).age;
            lock(&ctx.shooting).draws.insert(actor.uuid, (now, hand));
            let _ = ctx.events.send(Event::UsingItem {
                uuid: actor.uuid,
                entity_id: actor.entity_id,
                hand: Some(hand),
            });
            return Some(outcome);
        }
        "minecraft:trident" => {
            if !trident::usable(ctx, &actor, &held.stack) {
                return Some(outcome);
            }
            let now = lock(&ctx.time).age;
            lock(&ctx.shooting).draws.insert(actor.uuid, (now, hand));
            let _ = ctx.events.send(Event::UsingItem {
                uuid: actor.uuid,
                entity_id: actor.entity_id,
                hand: Some(hand),
            });
            return Some(outcome);
        }
        "minecraft:wind_charge" => {
            if wind::throw(ctx, &actor, held.slot) {
                outcome.events.push(Event::Swing {
                    uuid: actor.uuid,
                    entity_id: actor.entity_id,
                    times: 1,
                });
            }
            return Some(outcome);
        }
        "minecraft:snowball" => (Shot::Snowball, 1.5, 0.0, "entity.snowball.throw"),
        "minecraft:egg" => (Shot::Egg, 1.5, 0.0, "entity.egg.throw"),
        "minecraft:ender_pearl" => (Shot::Pearl, 1.5, 0.0, "entity.ender_pearl.throw"),
        "minecraft:experience_bottle" => {
            (Shot::Bottle, 0.7, -20.0, "entity.experience_bottle.throw")
        }
        "minecraft:splash_potion" | "minecraft:lingering_potion" => {
            let lingering = name == "minecraft:lingering_potion";
            let contents = potions::contents(held.comps.as_ref());
            (
                Shot::Potion {
                    item: held.stack.item,
                    lingering,
                    color: potions::color(&contents),
                    effects: contents,
                },
                0.5,
                -20.0,
                if lingering {
                    "entity.lingering_potion.throw"
                } else {
                    "entity.splash_potion.throw"
                },
            )
        }
        "minecraft:ender_eye" => return Some(eye::throw(ctx, actor, held.slot)),
        "minecraft:fishing_rod" => return Some(fishing::use_rod(ctx, actor, hand, yaw, pitch)),
        "minecraft:firework_rocket" => {
            // In the air a rocket does something only for a gliding player.
            let gliding = lock(&ctx.vitals)
                .get(&actor.uuid)
                .is_some_and(|v| v.gliding);
            if !gliding {
                return Some(outcome);
            }
            firework::boost(ctx, actor, &held);
            if consumes {
                crate::held::consume(ctx, actor.uuid, held.slot, 1);
            }
            outcome.events.push(Event::Swing {
                uuid: actor.uuid,
                entity_id: actor.entity_id,
                times: 1,
            });
            return Some(outcome);
        }
        "minecraft:crossbow" => {
            crossbow::use_crossbow(ctx, actor, hand, &held);
            return Some(outcome);
        }
        _ => return None,
    };
    let at = muzzle(&actor);
    shoot(ctx, shot, Some(actor.uuid), at, aim(offset), speed, 1.0);
    fx::sound(
        ctx,
        sound,
        fx::category::NEUTRAL,
        at,
        0.5,
        0.4 / (hits::rnd() as f32 * 0.4 + 0.8),
    );
    if consumes {
        crate::held::consume(ctx, actor.uuid, held.slot, 1);
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    Some(outcome)
}

/// A firework rocket used on a block flies up from where it was clicked.
pub fn use_on_block(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    name: &str,
    clicked: mc_protocol::Position,
    cursor: [f32; 3],
) -> bool {
    if name != "minecraft:firework_rocket" {
        return false;
    }
    let Some(held) = crate::held::held(ctx, actor.uuid, hand) else {
        return false;
    };
    let at = [
        f64::from(clicked.x) + f64::from(cursor[0]),
        f64::from(clicked.y) + f64::from(cursor[1]),
        f64::from(clicked.z) + f64::from(cursor[2]),
    ];
    firework::launch(ctx, actor, &held, at, false);
    if actor.game_mode != 1 {
        crate::held::consume(ctx, actor.uuid, held.slot, 1);
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    true
}

pub fn is_arrow(name: &str) -> bool {
    matches!(
        name,
        "minecraft:arrow" | "minecraft:spectral_arrow" | "minecraft:tipped_arrow"
    )
}

/// The player let go of use: a drawn bow shoots.
pub fn release(ctx: &ServerContext, actor: Actor) {
    let Some((start, hand)) = lock(&ctx.shooting).draws.remove(&actor.uuid) else {
        return;
    };
    let _ = ctx.events.send(Event::UsingItem {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        hand: None,
    });
    let now = lock(&ctx.time).age;
    let ticks = (now - start).max(0) as i32;
    match crate::held::held(ctx, actor.uuid, hand).map(|h| h.name) {
        Some(n) if n == "minecraft:trident" => trident::release(ctx, actor, hand, ticks),
        Some(n) if n == "minecraft:crossbow" => crossbow::release(ctx, actor, hand, ticks),
        _ => arrow::fire_bow(ctx, actor, hand, ticks),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mc_entity::extra::Pickup;

    /// A superflat server context, `None` without the game data.
    pub(crate) fn flat() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    fn run(ctx: &ServerContext, e: &mut Entity, ticks: usize) -> hits::Out {
        let mut out = hits::Out::default();
        for _ in 0..ticks {
            let mut t = Tick {
                ctx,
                players: &[],
                targets: &[],
                out: &mut out,
                shooter: None,
            };
            tick(&mut t, e);
            if e.removed {
                break;
            }
        }
        out
    }

    fn arrow_at(ctx: &ServerContext, pos: [f64; 3], vel: [f64; 3]) -> Entity {
        let id = spawn(
            ctx,
            Shot::Arrow(mc_entity::extra::Arrow::new(1)),
            None,
            pos,
            vel,
        );
        let mut entities = lock(&ctx.entities);
        let i = entities.iter().position(|e| e.id == id).expect("spawned");
        entities.swap_remove(i)
    }

    #[test]
    fn an_arrow_arcs_down_and_sticks_in_the_ground() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // The flat world's surface is at y = -60.
        let mut e = arrow_at(&ctx, [0.5, -58.0, 0.5], [1.0, 0.2, 0.0]);
        run(&ctx, &mut e, 200);
        let Kind::Projectile(p) = &e.kind else {
            panic!()
        };
        let Shot::Arrow(a) = &p.shot else { panic!() };
        assert!(a.stuck > 0, "{e:?}");
        assert!(!e.removed);
        assert!((e.pos[1] + 60.0).abs() < 0.01, "{:?}", e.pos);
        assert!(e.pos[0] > 5.0);
        assert_eq!(e.vel, [0.0; 3]);
        assert_eq!(a.pickup, Pickup::Yes);
    }

    fn power_of(ctx: &ServerContext, p: mc_protocol::Position) -> u32 {
        let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
        ctx.shapes
            .property(s, "power")
            .and_then(|v| v.parse().ok())
            .unwrap()
    }

    /// Ticks the world on, the way the tick thread does, `n` times.
    fn pass(ctx: &ServerContext, n: usize) {
        for _ in 0..n {
            lock(&ctx.time).age += 1;
            crate::tick::run_scheduled_ticks(ctx);
        }
    }

    #[test]
    fn a_target_gives_a_signal_by_how_near_the_middle_a_projectile_hit() {
        // Dead centre of the face, and away from it towards the edge.
        let cell = [5, -60, 0];
        assert_eq!(hits::target_strength(cell, 4, [5.0, -59.5, 0.5]), 15);
        assert_eq!(hits::target_strength(cell, 4, [5.0, -59.5, 0.75]), 8);
        assert_eq!(hits::target_strength(cell, 1, [5.5, -59.0, 0.5]), 15);
        assert_eq!(hits::target_strength(cell, 4, [5.0, -59.999, 0.5]), 1);
        assert_eq!(hits::target_strength(cell, 2, [5.1, -59.2, 0.0]), 3);
    }

    #[test]
    fn projectiles_power_a_target_for_eight_ticks_and_arrows_for_twenty() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = mc_protocol::Position::new(5, -60, 0);
        let target = ctx.data.blocks.default_state("minecraft:target").unwrap();
        crate::level::WorldLevel::new(&ctx).set_block(at, target);
        assert_eq!(power_of(&ctx, at), 0);
        let shot = |ctx: &ServerContext, e: &mut Entity| {
            let mut hit = false;
            for _ in 0..10 {
                run(ctx, e, 1);
                if power_of(ctx, at) > 0 {
                    hit = true;
                    break;
                }
            }
            assert!(hit);
        };
        // A snowball straight at the middle.
        let id = spawn(
            &ctx,
            Shot::Snowball,
            None,
            [0.5, -59.5, 0.5],
            [0.9, 0.0, 0.0],
        );
        let mut e = {
            let mut entities = lock(&ctx.entities);
            let i = entities.iter().position(|e| e.id == id).unwrap();
            entities.swap_remove(i)
        };
        shot(&ctx, &mut e);
        assert!(power_of(&ctx, at) > 0);
        pass(&ctx, 7);
        assert!(power_of(&ctx, at) > 0);
        pass(&ctx, 1);
        assert_eq!(power_of(&ctx, at), 0);
        // An arrow keeps it for 20.
        let mut e = arrow_at(&ctx, [0.5, -59.5, 0.5], [1.5, 0.0, 0.0]);
        shot(&ctx, &mut e);
        pass(&ctx, 19);
        assert!(power_of(&ctx, at) > 0);
        pass(&ctx, 1);
        assert_eq!(power_of(&ctx, at), 0);
    }

    #[test]
    fn a_hit_target_powers_dust_and_pulses_an_observer() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let put = |x: i32, name: &str, props: &[(&str, &str)]| {
            let state = blocks.state_id(name, props).unwrap();
            crate::level::WorldLevel::new(&ctx)
                .set_block(mc_protocol::Position::new(x, -60, 0), state);
        };
        put(5, "minecraft:target", &[("power", "0")]);
        put(4, "minecraft:redstone_wire", &[]);
        put(
            6,
            "minecraft:observer",
            &[("facing", "west"), ("powered", "false")],
        );
        let state = |x: i32| {
            let p = mc_protocol::Position::new(x, -60, 0);
            blocks
                .describe(ctx.world.block(p, blocks).unwrap())
                .unwrap()
        };
        assert!(state(4).contains("power=0"), "{}", state(4));
        let strike = hits::Strike::Block {
            cell: [5, -60, 0],
            face: 4,
        };
        hits::hit_block(&ctx, &strike, [5.0, -59.5, 0.5], false);
        assert!(state(4).contains("power=15"), "{}", state(4));
        pass(&ctx, 2);
        assert!(state(6).contains("powered=true"), "{}", state(6));
        pass(&ctx, 6);
        assert!(state(4).contains("power=0"), "{}", state(4));
        assert!(state(6).contains("powered=false"), "{}", state(6));
    }

    #[test]
    fn a_snowball_breaks_on_the_ground() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let id = spawn(
            &ctx,
            Shot::Snowball,
            None,
            [0.5, -58.0, 0.5],
            [0.0, -0.5, 0.0],
        );
        let mut e = {
            let mut entities = lock(&ctx.entities);
            let i = entities.iter().position(|e| e.id == id).unwrap();
            entities.swap_remove(i)
        };
        run(&ctx, &mut e, 20);
        assert!(e.removed);
    }

    #[test]
    fn a_stuck_arrow_falls_when_its_block_goes() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut e = arrow_at(&ctx, [0.5, -55.0, 0.5], [0.0, 0.0, 0.0]);
        if let Kind::Projectile(p) = &mut e.kind
            && let Shot::Arrow(a) = &mut p.shot
        {
            a.stuck = 3;
        }
        run(&ctx, &mut e, 1);
        let Kind::Projectile(p) = &e.kind else {
            panic!()
        };
        let Shot::Arrow(a) = &p.shot else { panic!() };
        // In the air nothing holds it.
        assert_eq!(a.stuck, 0);
        assert!(e.vel[1] < 0.0);
    }

    #[test]
    fn bow_power_rises_with_the_draw() {
        let power = arrow::power;
        assert!(power(2) < 0.1 && power(3) >= 0.1);
        assert!(power(20) >= 1.0 - 1e-9);
        assert!(power(10) > 0.4 && power(10) < 0.5);
    }

    #[test]
    fn velocities_pack_as_the_protocol_does() {
        use mc_protocol::packets::play::put_lpvec3;
        let mut out = Vec::new();
        put_lpvec3(&mut out, [0.0; 3]);
        assert_eq!(out, [0]);
        // Unpack what a client would: scale from the low bits, 15 bits an axis.
        let mut out = Vec::new();
        put_lpvec3(&mut out, [1.5, -0.5, 0.25]);
        assert_eq!(out.len(), 6);
        let bits = u64::from(out[0])
            | u64::from(out[1]) << 8
            | u64::from(u32::from_be_bytes([out[2], out[3], out[4], out[5]])) << 16;
        let scale = (bits & 3) as f64;
        assert_eq!(scale, 2.0);
        let axis = |shift: u32| ((bits >> shift) & 0x7fff) as f64 / 32766.0 * 2.0 - 1.0;
        assert!((axis(3) * scale - 1.5).abs() < 1e-3);
        assert!((axis(18) * scale + 0.5).abs() < 1e-3);
        assert!((axis(33) * scale - 0.25).abs() < 1e-3);
    }

    #[test]
    fn boxes_are_hit_where_the_ray_enters() {
        let b = mc_entity::Aabb {
            min: [1.0, 0.0, 0.0],
            max: [2.0, 1.0, 1.0],
        };
        let f = hits::ray_box([0.0, 0.5, 0.5], [4.0, 0.5, 0.5], &b).unwrap();
        assert!((f - 0.25).abs() < 1e-9);
        assert!(hits::ray_box([0.0, 2.5, 0.5], [4.0, 2.5, 0.5], &b).is_none());
    }

    #[test]
    fn potion_tables_follow_the_wiki() {
        let healing = potions::effects_of("minecraft:strong_healing");
        assert_eq!(healing, vec![("minecraft:instant_health".to_owned(), 1, 1)]);
        let speed = potions::effects_of("long_swiftness");
        assert_eq!(speed, vec![("minecraft:speed".to_owned(), 0, 9600)]);
        assert!(potions::effects_of("water").is_empty());
        assert_eq!(potions::effects_of("strong_turtle_master").len(), 2);
    }
}
