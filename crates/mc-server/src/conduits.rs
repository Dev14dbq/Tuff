//! Conduits (minecraft.wiki, "Conduit"): every 40 game ticks the frame of
//! prismarine, prismarine bricks, dark prismarine and sea lanterns is counted
//! and the 3×3×3 around the conduit must be water. Sixteen or more frame
//! blocks make it active. Conduit Power then reaches players in water or rain,
//! and a full frame of 42 blocks also damages one hostile mob.

use mc_entity::mob::Category;
use mc_entity::{Entity, Kind};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use crate::context::{Event, ServerContext};
use crate::effects;
use crate::tick::lock;

/// Ticks between two checks (2 seconds).
pub const PERIOD: i64 = 40;
/// Conduit Power lasts 13 seconds, so it is still there at the next check.
pub const EFFECT_TICKS: i32 = 260;
/// A finished frame attacks this far.
pub const HUNT_RANGE: f64 = 8.0;
/// Magic damage of one attack.
pub const HUNT_DAMAGE: f32 = 4.0;

const FRAME: [&str; 4] = [
    "minecraft:prismarine",
    "minecraft:prismarine_bricks",
    "minecraft:dark_prismarine",
    "minecraft:sea_lantern",
];

/// Blocks of the three 5×5 open squares centred on the conduit, one around
/// each axis. Shared corners are listed once (42 in all).
pub fn frame_offsets() -> Vec<(i32, i32, i32)> {
    let mut out = Vec::new();
    for x in -2..=2 {
        for y in -2..=2 {
            for z in -2..=2 {
                let on = |a: i32, b: i32| a.abs().max(b.abs()) == 2;
                if (x == 0 && on(y, z)) || (y == 0 && on(x, z)) || (z == 0 && on(x, y)) {
                    out.push((x, y, z));
                }
            }
        }
    }
    out
}

/// Spherical range of an active conduit: 16 blocks for every 7 in the frame.
pub fn effect_range(frame: i32) -> i32 {
    frame / 7 * 16
}

fn is_water(blocks: &mc_data::Blocks, state: u32) -> bool {
    blocks.block_name(state) == Some("minecraft:water")
        || blocks.property(state, "waterlogged") == Some("true")
}

/// The block entity at `p`, made when there is none.
fn entity(ctx: &ServerContext, p: Position) -> mc_world::BlockEntity {
    ctx.world
        .block_entity(p, &ctx.data.blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: "minecraft:conduit".to_owned(),
            data: Compound::new(),
        })
}

/// The conduit at `p` (a block entity of its own from now on).
pub fn placed(ctx: &ServerContext, p: Position) {
    if ctx.world.block_entity(p, &ctx.data.blocks).is_none() {
        ctx.world.set_block_entity(entity(ctx, p), &ctx.data.blocks);
    }
}

fn was_active(data: &Compound) -> bool {
    match data.get("active") {
        Some(Tag::Byte(b)) => *b != 0,
        _ => false,
    }
}

/// Source water, flowing water, or a waterlogged block in every cell of the
/// 3×3×3, conduit included.
fn water_around(ctx: &ServerContext, p: Position) -> bool {
    let blocks = &ctx.data.blocks;
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                let q = Position::new(p.x + x, p.y + y, p.z + z);
                let wet = ctx
                    .world
                    .block(q, blocks)
                    .is_some_and(|s| is_water(blocks, s));
                if !wet {
                    return false;
                }
            }
        }
    }
    true
}

/// Frame blocks that count, and how many there are.
fn frame_at(ctx: &ServerContext, p: Position) -> i32 {
    let blocks = &ctx.data.blocks;
    frame_offsets()
        .into_iter()
        .filter(|(x, y, z)| {
            let q = Position::new(p.x + x, p.y + y, p.z + z);
            ctx.world
                .block(q, blocks)
                .is_some_and(|s| blocks.block_name(s).is_some_and(|n| FRAME.contains(&n)))
        })
        .count() as i32
}

fn block_dist(a: Position, b: Position) -> i64 {
    let d = |p: i32, q: i32| i64::from(p - q);
    let (x, y, z) = (d(a.x, b.x), d(a.y, b.y), d(a.z, b.z));
    x * x + y * y + z * z
}

fn touching_water(ctx: &ServerContext, at: [f64; 3]) -> bool {
    let blocks = &ctx.data.blocks;
    let (x, z) = (at[0].floor() as i32, at[2].floor() as i32);
    let y0 = at[1].floor() as i32;
    (y0..y0 + 2).any(|y| {
        ctx.world
            .block(Position::new(x, y, z), blocks)
            .is_some_and(|s| is_water(blocks, s))
    })
}

fn wet(ctx: &ServerContext, at: [f64; 3]) -> bool {
    touching_water(ctx, at) || crate::worn::rain_at(ctx, at)
}

fn give_power(ctx: &ServerContext, p: Position, frame: i32) {
    let range = effect_range(frame);
    if range <= 0 {
        return;
    }
    let limit = i64::from(range) * i64::from(range);
    let near: Vec<u128> = lock(&ctx.players)
        .values()
        .filter(|pl| pl.dimension == ctx.world.dimension)
        .filter(|pl| pl.game_mode != 3)
        .filter(|pl| {
            pl.pose.is_some_and(|pose| {
                let feet = Position::new(
                    pose.x.floor() as i32,
                    pose.y.floor() as i32,
                    pose.z.floor() as i32,
                );
                block_dist(p, feet) < limit && wet(ctx, [pose.x, pose.y, pose.z])
            })
        })
        .map(|pl| pl.profile.uuid)
        .collect();
    let Some(kind) = effects::kind_of("minecraft:conduit_power") else {
        return;
    };
    for uuid in near {
        let mut instance = effects::Instance::new(0, EFFECT_TICKS, true);
        instance.ambient = true;
        effects::give(ctx, uuid, kind, instance);
    }
}

struct Prey {
    id: i32,
    uuid: u128,
    pos: [f64; 3],
}

fn prey_ok(ctx: &ServerContext, e: &Entity, origin: [f64; 3]) -> bool {
    if e.removed {
        return false;
    }
    let Kind::Mob(m) = &e.kind else {
        return false;
    };
    if m.species.category() != Category::Monster || m.health <= 0.0 || m.is_dead() {
        return false;
    }
    let (dx, dy, dz) = (
        e.pos[0] - origin[0],
        e.pos[1] - origin[1],
        e.pos[2] - origin[2],
    );
    dx * dx + dy * dy + dz * dz <= HUNT_RANGE * HUNT_RANGE && wet(ctx, e.pos)
}

/// The mob already chosen, if it is still in the water and in range;
/// otherwise the nearest hostile mob that is.
fn choose_prey(ctx: &ServerContext, origin: [f64; 3], current: Option<u128>) -> Option<Prey> {
    let entities = lock(&ctx.entities);
    let still = current.and_then(|uuid| {
        entities
            .iter()
            .find(|e| e.uuid == uuid && prey_ok(ctx, e, origin))
    });
    let best = still.or_else(|| {
        entities
            .iter()
            .filter(|e| prey_ok(ctx, e, origin))
            .min_by(|a, b| {
                let d = |e: &Entity| {
                    let (dx, dy, dz) = (
                        e.pos[0] - origin[0],
                        e.pos[1] - origin[1],
                        e.pos[2] - origin[2],
                    );
                    dx * dx + dy * dy + dz * dz
                };
                d(a).total_cmp(&d(b))
            })
    })?;
    Some(Prey {
        id: best.id,
        uuid: best.uuid,
        pos: best.pos,
    })
}

fn play(ctx: &ServerContext, name: &str, at: [f64; 3], volume: f32) {
    crate::fx::sound(ctx, name, crate::fx::category::BLOCKS, at, volume, 1.0);
}

fn show_target(ctx: &ServerContext, p: Position, data: &Compound) {
    let Some(kind) = ctx.world.block_entity_kind("minecraft:conduit") else {
        return;
    };
    let mut shown = Compound::new();
    if let Some(target) = data.get("Target") {
        shown.insert("Target", target.clone());
    }
    let _ = ctx.events.send(Event::BlockEntity {
        position: p,
        kind,
        data: shown,
    });
}

fn store(ctx: &ServerContext, p: Position, active: bool, target: Option<u128>) {
    let mut be = entity(ctx, p);
    be.data.insert("active", Tag::Byte(i8::from(active)));
    match target {
        Some(uuid) => {
            be.data.insert("Target", crate::persist::uuid_tag(uuid));
        }
        None => {
            be.data.remove("Target");
        }
    }
    ctx.world.set_block_entity(be.clone(), &ctx.data.blocks);
    show_target(ctx, p, &be.data);
}

/// One game tick. Every 40 ticks each conduit rechecks its frame.
pub fn tick(ctx: &ServerContext, count: i64) {
    if count % PERIOD != 0 {
        return;
    }
    for be in ctx.world.block_entities_of("minecraft:conduit") {
        let p = Position::new(be.x, be.y, be.z);
        let still = ctx
            .world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            == Some("minecraft:conduit");
        if !still {
            continue;
        }
        let before = was_active(&be.data);
        let previous = crate::persist::uuid_of(be.data.get("Target"));
        let frame = if water_around(ctx, p) {
            frame_at(ctx, p)
        } else {
            0
        };
        let active = frame >= 16;
        let centre = [
            f64::from(p.x) + 0.5,
            f64::from(p.y) + 0.5,
            f64::from(p.z) + 0.5,
        ];
        if active != before {
            play(
                ctx,
                if active {
                    "block.conduit.activate"
                } else {
                    "block.conduit.deactivate"
                },
                centre,
                if active { 0.9 } else { 0.7 },
            );
        }
        if active && count % (PERIOD * 2) == 0 {
            play(ctx, "block.conduit.ambient", centre, 1.0);
        }
        let prey = if frame == 42 {
            choose_prey(ctx, centre, previous)
        } else {
            None
        };
        if active {
            give_power(ctx, p, frame);
        }
        if let Some(prey) = &prey {
            ctx.mobs
                .harm(ctx.dim, prey.id, HUNT_DAMAGE, "minecraft:magic", None);
            play(ctx, "block.conduit.attack.target", prey.pos, 1.0);
        }
        let uuid = prey.as_ref().map(|pr| pr.uuid);
        if active != before || uuid != previous {
            store(ctx, p, active, uuid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Event;
    use crate::fx::Fx;
    use mc_entity::Species;

    #[test]
    fn the_frame_is_three_rings_of_forty_two_blocks() {
        let cells = frame_offsets();
        assert_eq!(cells.len(), 42);
        assert!(cells.contains(&(2, 0, 0)));
        assert!(cells.contains(&(0, -2, 2)));
        assert!(!cells.contains(&(2, 2, 2)));
        assert!(!cells.contains(&(1, 0, 0)));
        assert_eq!(effect_range(16), 32);
        assert_eq!(effect_range(21), 48);
        assert_eq!(effect_range(28), 64);
        assert_eq!(effect_range(35), 80);
        assert_eq!(effect_range(42), 96);
        // 15 is still two groups of seven; activation itself waits for 16.
        assert_eq!(effect_range(15), 32);
    }

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        crate::testkit::flat()
    }

    fn put(ctx: &ServerContext, name: &str, at: Position, props: &[(&str, &str)]) {
        let state = ctx
            .data
            .blocks
            .state_id(name, props)
            .or_else(|| ctx.data.blocks.default_state(name))
            .unwrap_or(0);
        ctx.world.set_block(at, state, &ctx.data.blocks, &ctx.rules);
    }

    /// Water in the 3×3×3 and `blocks` of the frame (the rest stay empty).
    fn build(ctx: &ServerContext, at: Position, blocks: i32) {
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let q = Position::new(at.x + x, at.y + y, at.z + z);
                    if (x, y, z) == (0, 0, 0) {
                        put(ctx, "minecraft:conduit", q, &[("waterlogged", "true")]);
                        placed(ctx, q);
                    } else {
                        put(ctx, "minecraft:water", q, &[]);
                    }
                }
            }
        }
        let kinds = [
            "minecraft:prismarine",
            "minecraft:prismarine_bricks",
            "minecraft:dark_prismarine",
            "minecraft:sea_lantern",
        ];
        for (i, (x, y, z)) in frame_offsets().into_iter().enumerate() {
            if i as i32 >= blocks {
                break;
            }
            put(
                ctx,
                kinds[i % kinds.len()],
                Position::new(at.x + x, at.y + y, at.z + z),
                &[],
            );
        }
    }

    fn powered(ctx: &ServerContext, uuid: u128) -> Option<(i32, bool)> {
        let vitals = lock(&ctx.vitals);
        let kind = effects::kind_of("minecraft:conduit_power")?;
        vitals
            .get(&uuid)
            .and_then(|v| v.effects.get(kind))
            .map(|e| (e.duration, e.ambient))
    }

    fn sound_names(events: &mut crate::context::EventReceiver) -> Vec<(String, f32)> {
        let mut out = Vec::new();
        while let Some(ev) = events.try_changed() {
            if let Event::Fx(Fx::Sound(s)) = ev {
                out.push((s.name, s.volume));
            }
        }
        out
    }

    #[test]
    fn sixteen_blocks_give_conduit_power_inside_the_sphere() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(100, 20, 100);
        crate::testkit::join(&ctx, 1, 10, [131.5, 20.0, 100.5], 0);
        crate::testkit::join(&ctx, 2, 11, [132.5, 20.0, 100.5], 0);
        crate::testkit::join(&ctx, 3, 12, [105.5, 20.0, 100.5], 0);
        crate::testkit::join(&ctx, 4, 13, [103.5, 20.0, 100.5], 3);
        build(&ctx, at, 16);
        put(&ctx, "minecraft:water", Position::new(131, 20, 100), &[]);
        put(&ctx, "minecraft:water", Position::new(132, 20, 100), &[]);
        put(&ctx, "minecraft:water", Position::new(103, 20, 100), &[]);
        let mut events = ctx.events.subscribe();
        tick(&ctx, 40);
        assert_eq!(powered(&ctx, 1), Some((EFFECT_TICKS, true)));
        assert_eq!(powered(&ctx, 2), None);
        assert_eq!(powered(&ctx, 3), None);
        assert_eq!(powered(&ctx, 4), None);
        let names = sound_names(&mut events);
        assert!(
            names
                .iter()
                .any(|(n, v)| n == "minecraft:block.conduit.activate" && *v == 0.9)
        );
        assert!(!names.iter().any(|(n, _)| n.contains("ambient")));
        // Still active: the ambient sound, and no second activate.
        tick(&ctx, 80);
        let names = sound_names(&mut events);
        assert!(
            names
                .iter()
                .any(|(n, v)| n == "minecraft:block.conduit.ambient" && *v == 1.0)
        );
        assert!(!names.iter().any(|(n, _)| n.contains("activate")));
        // The water goes. Power already given runs out on its own.
        put(
            &ctx,
            "minecraft:air",
            Position::new(at.x + 1, at.y, at.z),
            &[],
        );
        tick(&ctx, 120);
        assert_eq!(powered(&ctx, 1), Some((EFFECT_TICKS, true)));
        let names = sound_names(&mut events);
        assert!(
            names
                .iter()
                .any(|(n, v)| n == "minecraft:block.conduit.deactivate" && *v == 0.7)
        );
    }

    #[test]
    fn rain_counts_as_being_wet() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, -58, 0);
        crate::testkit::join(&ctx, 1, 10, [20.5, -60.0, 0.5], 0);
        build(&ctx, at, 16);
        lock(&ctx.weather).raining = true;
        tick(&ctx, 40);
        assert_eq!(powered(&ctx, 1), Some((EFFECT_TICKS, true)));
    }

    #[test]
    fn a_full_frame_attacks_the_nearest_wet_hostile_mob() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, 20, 0);
        crate::testkit::join(&ctx, 1, 10, [0.5, 20.0, 8.0], 0);
        build(&ctx, at, 42);
        let spawn = |species, pos: [f64; 3], water: bool| {
            if water {
                put(
                    &ctx,
                    "minecraft:water",
                    Position::new(
                        pos[0].floor() as i32,
                        pos[1].floor() as i32,
                        pos[2].floor() as i32,
                    ),
                    &[],
                );
            }
            let mob = crate::mobs::new_mob(&ctx, species, pos);
            let (id, uuid, health) = match &mob.kind {
                Kind::Mob(m) => (mob.id, mob.uuid, m.health),
                _ => unreachable!(),
            };
            crate::trading::spawn(&ctx, mob);
            (id, uuid, health)
        };
        let (near, near_uuid, near_hp) = spawn(Species::Drowned, [3.5, 20.0, 0.5], true);
        let (far, _, far_hp) = spawn(Species::Drowned, [7.5, 20.0, 0.5], true);
        // Outside the frame, closer than the far drowned, and dry.
        let (dry, _, dry_hp) = spawn(Species::Zombie, [0.5, 20.0, 4.5], false);
        let (cow, _, cow_hp) = spawn(Species::Cow, [1.5, 20.0, 0.5], true);
        let mut events = ctx.events.subscribe();
        tick(&ctx, 40);
        let target = |ctx: &ServerContext| {
            ctx.world
                .block_entity(at, &ctx.data.blocks)
                .and_then(|be| crate::persist::uuid_of(be.data.get("Target")))
        };
        assert_eq!(target(&ctx), Some(near_uuid));
        assert!(
            sound_names(&mut events)
                .iter()
                .any(|(n, v)| n == "minecraft:block.conduit.attack.target" && *v == 1.0)
        );
        // It keeps that mob while the mob has not moved.
        tick(&ctx, 80);
        assert_eq!(target(&ctx), Some(near_uuid));
        let mut level = crate::level::WorldLevel::new(&ctx);
        crate::mobs::tick(&ctx, &mut level);
        let health = |id: i32| {
            lock(&ctx.entities)
                .iter()
                .find(|e| e.id == id)
                .map(|e| match &e.kind {
                    Kind::Mob(m) => m.health,
                    _ => 0.0,
                })
        };
        assert!(health(near).unwrap() < near_hp);
        assert_eq!(health(far), Some(far_hp));
        assert_eq!(health(dry), Some(dry_hp));
        assert_eq!(health(cow), Some(cow_hp));
    }

    #[test]
    fn a_slab_does_not_count_toward_the_frame() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, 40, 0);
        crate::testkit::join(&ctx, 1, 10, [6.5, 40.0, 0.5], 0);
        build(&ctx, at, 16);
        let (x, y, z) = frame_offsets()[0];
        put(
            &ctx,
            "minecraft:prismarine_slab",
            Position::new(at.x + x, at.y + y, at.z + z),
            &[("type", "bottom"), ("waterlogged", "false")],
        );
        put(&ctx, "minecraft:water", Position::new(6, 40, 0), &[]);
        tick(&ctx, 40);
        assert_eq!(powered(&ctx, 1), None);
    }
}
