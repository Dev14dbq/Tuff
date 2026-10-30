//! Portals: standing in a nether portal for four seconds or stepping into an
//! end portal sends a player to another dimension (minecraft.wiki, "Nether
//! portal", "End portal"). The nether portal on the other side is looked for
//! within 16 blocks of the scaled spot (128 when going to the overworld) and
//! made if there is none.

use std::collections::HashMap;
use std::sync::Arc;

use mc_protocol::Position;
use mc_tick::{Axis, BlockPos, Level as _};

use crate::context::{Event, Pose, ServerContext};
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

/// Ticks a survival player waits in a nether portal by default
/// (`players_nether_portal_default_delay`).
#[cfg(test)]
const WAIT: u32 = 80;
/// Ticks a player must be out of every portal before one works again (also
/// after coming through one). Other entities wait 15 seconds, the same
/// length as the portal ticket on the far side.
pub const COOLDOWN: u32 = 10;
/// Ticks before a non-player entity can use a portal again.
const ENTITY_COOLDOWN: u32 = 300;
/// Where a player entering the End appears.
pub const END_SPAWN: (f64, f64, f64) = (100.5, 49.0, 0.5);

/// Players standing in portals, and how long. Other entities that just
/// came through keep a cooldown so they do not bounce straight back.
#[derive(Debug, Default)]
pub struct Waiting {
    players: HashMap<u128, Wait>,
    /// Entity id → ticks left before a portal will take it again.
    arrived: HashMap<i32, u32>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Wait {
    ticks: u32,
    cooldown: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Nether,
    End,
}

/// The portal a player at `pose` stands in.
fn portal_at(ctx: &ServerContext, pose: Pose) -> Option<Kind> {
    let (x, y, z) = (
        pose.x.floor() as i32,
        pose.y.floor() as i32,
        pose.z.floor() as i32,
    );
    let name = |y: i32| {
        ctx.world
            .block(Position::new(x, y, z), &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
    };
    match (name(y), name(y + 1)) {
        (Some("minecraft:nether_portal"), _) | (_, Some("minecraft:nether_portal")) => {
            Some(Kind::Nether)
        }
        (Some("minecraft:end_portal"), _) => Some(Kind::End),
        _ => None,
    }
}

/// One tick for every player: counting up in a portal, down outside, and
/// sending those who waited long enough.
pub fn tick(ctx: &ServerContext) {
    let players: Vec<(u128, i32, Pose, String)> = lock(&ctx.players)
        .values()
        .filter_map(|p| Some((p.profile.uuid, p.game_mode, p.pose?, p.dimension.clone())))
        .collect();
    let (creative_wait, survival_wait, to_nether) = {
        let rules = lock(&ctx.game_rules);
        (
            rules.int("players_nether_portal_creative_delay").max(0) as u32,
            rules.int("players_nether_portal_default_delay").max(0) as u32,
            rules.bool("allow_entering_nether_using_portals"),
        )
    };
    let mut jobs = Vec::new();
    {
        let mut waiting = lock(&ctx.portals);
        waiting
            .players
            .retain(|uuid, _| players.iter().any(|p| p.0 == *uuid));
        for (uuid, game_mode, pose, dimension) in players {
            let Some(here) = ctx.in_dimension(&dimension) else {
                continue;
            };
            let dead = lock(&ctx.vitals).get(&uuid).is_some_and(|v| v.dead);
            let wait = waiting.players.entry(uuid).or_default();
            wait.cooldown = wait.cooldown.saturating_sub(1);
            let kind = if dead { None } else { portal_at(&here, pose) };
            match kind {
                None => wait.ticks = wait.ticks.saturating_sub(4),
                // Still in the portal they came out of: the wait starts
                // once they have left it.
                Some(_) if wait.cooldown > 0 => wait.cooldown = COOLDOWN,
                Some(Kind::End) => jobs.push((uuid, here, Kind::End, pose)),
                // `allow_entering_nether_using_portals` off: no way in,
                // but the way back still works.
                Some(Kind::Nether) if !to_nether && dimension != "minecraft:the_nether" => {}
                Some(Kind::Nether) => {
                    wait.ticks += 1;
                    let need = if matches!(game_mode, 1 | 3) {
                        creative_wait
                    } else {
                        survival_wait
                    }
                    .max(1);
                    if wait.ticks >= need {
                        jobs.push((uuid, here, Kind::Nether, pose));
                    }
                }
            }
            if jobs.last().is_some_and(|j| j.0 == uuid) {
                wait.ticks = 0;
                wait.cooldown = COOLDOWN;
            }
        }
    }
    for (uuid, here, kind, pose) in jobs {
        if let Some((dimension, pose)) = destination(&here, uuid, kind, pose) {
            if let Some(dest) = ctx.in_dimension(&dimension) {
                crate::tickets::portal(&dest, pose.x, pose.z);
            }
            let _ = ctx.events.send(Event::ChangeDimension {
                uuid,
                dimension,
                pose,
                after_death: false,
            });
        }
    }
    ferry_entities(ctx);
}

/// An entity that is not a player walks into a portal and comes out the
/// other side on the same tick. A portal ticket there keeps the chunk
/// simulating, so it keeps moving with nobody nearby. Riders go with the
/// vehicle. The cooldown is stored on the destination.
fn ferry_entities(ctx: &ServerContext) {
    {
        let mut waiting = lock(&ctx.portals);
        waiting.arrived.retain(|_, left| {
            *left = left.saturating_sub(1);
            *left > 0
        });
    }
    let cooling: Vec<i32> = lock(&ctx.portals).arrived.keys().copied().collect();
    let players: Vec<i32> = lock(&ctx.players).values().map(|p| p.entity_id).collect();
    let to_nether = lock(&ctx.game_rules).bool("allow_entering_nether_using_portals");
    let overworld = ctx.dim == 0;
    let mut carried = Vec::new();
    let mut trips = Vec::new();
    {
        let entities = lock(&ctx.entities);
        for e in entities.iter() {
            carried.extend(passengers(e));
        }
        for e in entities.iter() {
            if e.removed
                || cooling.contains(&e.id)
                || players.contains(&e.id)
                || carried.contains(&e.id)
            {
                continue;
            }
            let pose = pose_of(e.pos);
            let Some(kind) = portal_at(ctx, pose) else {
                continue;
            };
            if kind == Kind::Nether && !to_nether && overworld {
                continue;
            }
            trips.push((e.id, kind, pose));
        }
    }
    for (id, kind, pose) in trips {
        let Some((dimension, dest_pose)) = entity_destination(ctx, kind, pose) else {
            continue;
        };
        let Some(dest) = ctx.in_dimension(&dimension) else {
            continue;
        };
        let mut moving = take_with_riders(ctx, id);
        if moving.is_empty() {
            continue;
        }
        let origin = moving[0].pos;
        let (dx, dy, dz) = (
            dest_pose.x - origin[0],
            dest_pose.y - origin[1],
            dest_pose.z - origin[2],
        );
        let mut ids = Vec::new();
        for e in &mut moving {
            e.pos[0] += dx;
            e.pos[1] += dy;
            e.pos[2] += dz;
            e.vel = [0.0; 3];
            ids.push(e.id);
            let _ = dest.events.send(Event::EntitySpawned(e.clone()));
        }
        lock(&dest.entities).extend(moving);
        {
            let mut waiting = lock(&dest.portals);
            for id in &ids {
                waiting.arrived.insert(*id, ENTITY_COOLDOWN);
            }
        }
        crate::tickets::portal(&dest, dest_pose.x, dest_pose.z);
        let _ = ctx.events.send(Event::EntitiesRemoved(ids));
    }
}

fn pose_of(pos: [f64; 3]) -> Pose {
    Pose {
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw: 0.0,
        pitch: 0.0,
        on_ground: false,
    }
}

fn passengers(e: &mc_entity::Entity) -> Vec<i32> {
    match &e.kind {
        mc_entity::Kind::Mob(m) => m.riders.clone(),
        mc_entity::Kind::Vehicle(v) => v.riders.clone(),
        _ => Vec::new(),
    }
}

/// The vehicle and whoever is riding it, vehicle first.
fn take_with_riders(ctx: &ServerContext, id: i32) -> Vec<mc_entity::Entity> {
    let mut all = lock(&ctx.entities);
    let Some(pos) = all.iter().position(|e| e.id == id) else {
        return Vec::new();
    };
    let vehicle = all.remove(pos);
    let riders = passengers(&vehicle);
    let mut out = vec![vehicle];
    all.retain(|e| {
        if riders.contains(&e.id) {
            out.push(e.clone());
            false
        } else {
            true
        }
    });
    out
}

/// Where an entity (not a player) comes out. Leaving the End uses the
/// world spawn, not a bed.
fn entity_destination(from: &ServerContext, kind: Kind, pose: Pose) -> Option<(String, Pose)> {
    if kind == Kind::End && from.dim == 2 {
        let r = lock(&from.spawn).clone();
        return Some((
            r.dimension,
            Pose {
                x: f64::from(r.pos.0) + 0.5,
                y: f64::from(r.pos.1),
                z: f64::from(r.pos.2) + 0.5,
                yaw: r.yaw,
                pitch: r.pitch,
                on_ground: false,
            },
        ));
    }
    destination(from, 0, kind, pose)
}

/// Where a portal of `kind` in `from` leads for a player at `pose`.
pub fn destination(
    from: &ServerContext,
    uuid: u128,
    kind: Kind,
    pose: Pose,
) -> Option<(String, Pose)> {
    match kind {
        Kind::Nether => {
            let to = match from.dim {
                0 => 1,
                1 => 0,
                _ => return None,
            };
            let dest = from.dimension(to)?;
            let axis = portal_axis(from, pose).unwrap_or(Axis::X);
            let pose = through_nether_portal(&dest, pose, axis, to == 1)?;
            Some((dest.dimension_name().to_owned(), pose))
        }
        Kind::End => {
            if from.dim == 2 {
                Some(leave_end(from, uuid))
            } else {
                let dest = from.dimension(2)?;
                build_end_platform(&dest);
                let (x, y, z) = END_SPAWN;
                Some((
                    dest.dimension_name().to_owned(),
                    Pose {
                        x,
                        y,
                        z,
                        yaw: 90.0,
                        pitch: 0.0,
                        on_ground: false,
                    },
                ))
            }
        }
    }
}

/// The axis of the portal block a player stands in.
fn portal_axis(ctx: &ServerContext, pose: Pose) -> Option<Axis> {
    let (x, y, z) = (
        pose.x.floor() as i32,
        pose.y.floor() as i32,
        pose.z.floor() as i32,
    );
    [y, y + 1].into_iter().find_map(|y| {
        let state = ctx.world.block(Position::new(x, y, z), &ctx.data.blocks)?;
        if ctx.data.blocks.block_name(state) != Some("minecraft:nether_portal") {
            return None;
        }
        Some(if ctx.data.blocks.property(state, "axis") == Some("z") {
            Axis::Z
        } else {
            Axis::X
        })
    })
}

/// Where a player leaving the End arrives: their respawn point, else the
/// world spawn.
pub fn leave_end(ctx: &ServerContext, uuid: u128) -> (String, Pose) {
    let own = lock(&ctx.players)
        .get(&uuid)
        .and_then(|p| p.extra.respawn.clone());
    let r = own.unwrap_or_else(|| lock(&ctx.spawn).clone());
    (
        r.dimension.clone(),
        Pose {
            x: f64::from(r.pos.0) + 0.5,
            y: f64::from(r.pos.1),
            z: f64::from(r.pos.2) + 0.5,
            yaw: r.yaw,
            pitch: r.pitch,
            on_ground: false,
        },
    )
}

/// Obsidian under 5×5 blocks with room above, as vanilla remakes it every
/// time someone enters the End.
fn build_end_platform(end: &ServerContext) {
    let Some(obsidian) = end.data.blocks.default_state("minecraft:obsidian") else {
        return;
    };
    let air = end.rules.air;
    let mut level = WorldLevel::new(end);
    for dx in -2..=2 {
        for dz in -2..=2 {
            let (x, z) = (100 + dx, dz);
            level.set(BlockPos::new(x, 48, z), obsidian);
            for y in 49..=51 {
                level.set(BlockPos::new(x, y, z), air);
            }
        }
    }
    let changes = level.into_changes();
    let _ = end.events.send(Event::BlocksChanged { by: 0, changes });
}

/// The pose on the other side of a nether portal that a player at `pose`
/// (in a portal along `axis`) comes out at in `dest`.
fn through_nether_portal(
    dest: &Arc<ServerContext>,
    pose: Pose,
    axis: Axis,
    nether: bool,
) -> Option<Pose> {
    let scale = if nether { 1.0 / 8.0 } else { 8.0 };
    let (min_y, top) = (
        dest.world.min_y,
        dest.world.min_y + dest.world.height as i32,
    );
    let (x, z) = {
        let b = lock(&dest.border);
        let half = (b.size / 2.0 - 1.0).max(0.0);
        (
            (pose.x * scale).clamp(b.center_x - half, b.center_x + half),
            (pose.z * scale).clamp(b.center_z - half, b.center_z + half),
        )
    };
    let target = BlockPos::new(
        x.floor() as i32,
        (pose.y.floor() as i32).clamp(min_y, top - 1),
        z.floor() as i32,
    );
    let radius = if nether { 16 } else { 128 };
    let yaw_turn = |found: Axis| if found == axis { 0.0 } else { 90.0 };
    if let Some(shape) = find_portal(dest, target, radius) {
        let (cx, cz) = shape_center(&shape);
        return Some(Pose {
            x: cx,
            y: f64::from(shape.bottom_left.y),
            z: cz,
            yaw: pose.yaw + yaw_turn(shape.axis),
            pitch: pose.pitch,
            on_ground: false,
        });
    }
    let (cx, y, cz) = make_portal(dest, target, axis, nether);
    Some(Pose {
        x: cx,
        y: f64::from(y),
        z: cz,
        yaw: pose.yaw,
        pitch: pose.pitch,
        on_ground: false,
    })
}

/// The middle of the bottom row inside a portal.
fn shape_center(shape: &mc_tick::portal::Shape) -> (f64, f64) {
    let half = f64::from(shape.width - 1) / 2.0;
    let bl = shape.bottom_left;
    if shape.axis == Axis::X {
        (f64::from(bl.x) + 0.5 - half, f64::from(bl.z) + 0.5)
    } else {
        (f64::from(bl.x) + 0.5, f64::from(bl.z) + 0.5 + half)
    }
}

/// The nearest portal within `radius` blocks of `target` in a world,
/// among the chunks it has.
fn find_portal(
    dest: &ServerContext,
    target: BlockPos,
    radius: i32,
) -> Option<mc_tick::portal::Shape> {
    let blocks = &dest.data.blocks;
    let is_portal = |s: u32| blocks.block_name(s) == Some("minecraft:nether_portal");
    let mut best: Option<(i64, i32, BlockPos)> = None;
    for cz in (target.z - radius) >> 4..=(target.z + radius) >> 4 {
        for cx in (target.x - radius) >> 4..=(target.x + radius) >> 4 {
            for p in dest.world.find_in_existing(cx, cz, blocks, &is_portal) {
                let (dx, dz) = (i64::from(p.x - target.x), i64::from(p.z - target.z));
                if dx.abs() > i64::from(radius) || dz.abs() > i64::from(radius) {
                    continue;
                }
                let dy = i64::from(p.y - target.y);
                let key = (dx * dx + dy * dy + dz * dz, p.y);
                if best.is_none_or(|b| key < (b.0, b.1)) {
                    best = Some((key.0, key.1, to_pos(p)));
                }
            }
        }
    }
    let (_, _, at) = best?;
    let level = WorldLevel::new(dest);
    let state = level.block(at);
    let axis = if blocks.property(state, "axis") == Some("z") {
        Axis::Z
    } else {
        Axis::X
    };
    mc_tick::portal::find(&level, level.logic(), at, axis)
}

/// Whether the blocks a portal needs at `(x, y, z)` are free: solid ground
/// under a platform that is air up to three blocks above.
fn fits(level: &WorldLevel, dest: &ServerContext, x: i32, y: i32, z: i32, axis: Axis) -> bool {
    let blocks = &dest.data.blocks;
    let (ax, az) = if axis == Axis::X { (1, 0) } else { (0, 1) };
    for a in -1..=2 {
        for p in -1..=1 {
            let (px, pz) = (x + ax * a + az * p, z + az * a + ax * p);
            let below = level.block(BlockPos::new(px, y - 1, pz));
            if !solid(dest, below) {
                return false;
            }
            for k in 0..=2 {
                let state = level.block(BlockPos::new(px, y + k, pz));
                if !(blocks.is_air(state) || replaceable(dest, state)) {
                    return false;
                }
            }
        }
    }
    true
}

fn solid(dest: &ServerContext, state: u32) -> bool {
    !dest.data.blocks.is_air(state)
        && !dest.data.blocks.is_fluid(state)
        && dest.rules.blocks_motion(state)
}

fn replaceable(dest: &ServerContext, state: u32) -> bool {
    !dest.data.blocks.is_fluid(state) && !dest.rules.blocks_motion(state)
}

/// Builds a portal near `target` and returns where a player appears in it
/// (x, y, z).
fn make_portal(
    dest: &Arc<ServerContext>,
    target: BlockPos,
    axis: Axis,
    nether: bool,
) -> (f64, i32, f64) {
    let (min_y, top) = (
        dest.world.min_y,
        // The Nether's roof is bedrock: keep below it.
        if nether {
            dest.world.min_y + 128
        } else {
            dest.world.min_y + dest.world.height as i32
        },
    );
    let mut level = WorldLevel::new(dest);
    // Columns by distance from the target.
    let mut columns: Vec<(i32, i32)> = (-16..=16)
        .flat_map(|dz| (-16..=16).map(move |dx| (dx, dz)))
        .collect();
    columns.sort_by_key(|&(dx, dz)| dx * dx + dz * dz);
    let mut spot = None;
    'search: for (dx, dz) in columns {
        let (x, z) = (target.x + dx, target.z + dz);
        let mut found: Option<i32> = None;
        for y in (min_y + 1..top - 3).rev() {
            let here = level.block(BlockPos::new(x, y, z));
            let below = level.block(BlockPos::new(x, y - 1, z));
            if dest.data.blocks.is_air(here)
                && solid(dest, below)
                && fits(&level, dest, x, y, z, axis)
                && found.is_none_or(|f| (y - target.y).abs() < (f - target.y).abs())
            {
                found = Some(y);
            }
        }
        if let Some(y) = found {
            spot = Some((x, y, z));
            break 'search;
        }
    }
    let (x, y, z) = spot.unwrap_or((
        target.x,
        target.y.clamp(min_y + 70, top - 10).max(min_y + 2),
        target.z,
    ));
    build_portal(&mut level, dest, x, y, z, axis);
    let changes = level.into_changes();
    let _ = dest.events.send(Event::BlocksChanged { by: 0, changes });
    if axis == Axis::X {
        (f64::from(x) + 1.0, y, f64::from(z) + 0.5)
    } else {
        (f64::from(x) + 0.5, y, f64::from(z) + 1.0)
    }
}

/// The frame, its platform and the portal, with `(x, y, z)` the lowest
/// inside block on the low side.
fn build_portal(level: &mut WorldLevel, dest: &ServerContext, x: i32, y: i32, z: i32, axis: Axis) {
    let blocks = &dest.data.blocks;
    let obsidian = blocks.default_state("minecraft:obsidian").unwrap_or(0);
    let value = if axis == Axis::X { "x" } else { "z" };
    let portal = dest
        .shapes
        .state("minecraft:nether_portal", &[("axis", value)])
        .unwrap_or(0);
    let air = dest.rules.air;
    let (ax, az) = if axis == Axis::X { (1, 0) } else { (0, 1) };
    for a in -1..=2 {
        for p in -1..=1 {
            let (px, pz) = (x + ax * a + az * p, z + az * a + ax * p);
            // The platform, and room above it.
            level.set(BlockPos::new(px, y - 1, pz), obsidian);
            if p != 0 {
                for k in 0..=2 {
                    level.set(BlockPos::new(px, y + k, pz), air);
                }
            }
        }
        let (px, pz) = (x + ax * a, z + az * a);
        for k in 0..=3 {
            let edge = a == -1 || a == 2 || k == 3;
            let state = if edge { obsidian } else { portal };
            level.set(BlockPos::new(px, y + k, pz), state);
        }
    }
}

/// Eyes of ender in the frame: when the twelve frame blocks around a spot
/// all hold one, the 3×3 inside becomes an end portal. `frame` is the
/// block that just got its eye. Returns whether a portal formed.
pub fn light_end_portal(level: &mut WorldLevel, frame: BlockPos) -> bool {
    use mc_tick::Dir;
    let ctx = level.context();
    let blocks = &ctx.data.blocks;
    let Some(portal) = blocks.default_state("minecraft:end_portal") else {
        return false;
    };
    let ring_ok = |center: BlockPos| -> bool {
        for dx in -2..=2i32 {
            for dz in -2..=2i32 {
                if dx.abs().max(dz.abs()) != 2 || dx.abs() == 2 && dz.abs() == 2 {
                    continue;
                }
                let at = BlockPos::new(center.x + dx, center.y, center.z + dz);
                let state = level.block(at);
                if blocks.block_name(state) != Some("minecraft:end_portal_frame")
                    || blocks.property(state, "eye") != Some("true")
                {
                    return false;
                }
                // The frame faces the middle.
                let facing = if dx == -2 {
                    Dir::East
                } else if dx == 2 {
                    Dir::West
                } else if dz == -2 {
                    Dir::South
                } else {
                    Dir::North
                };
                if blocks.property(state, "facing") != Some(facing.name()) {
                    return false;
                }
            }
        }
        true
    };
    for dx in -2..=2i32 {
        for dz in -2..=2i32 {
            if dx.abs().max(dz.abs()) != 2 || dx.abs() == 2 && dz.abs() == 2 {
                continue;
            }
            let center = BlockPos::new(frame.x - dx, frame.y, frame.z - dz);
            if !ring_ok(center) {
                continue;
            }
            for ox in -1..=1 {
                for oz in -1..=1 {
                    level.set(
                        BlockPos::new(center.x + ox, center.y, center.z + oz),
                        portal,
                    );
                }
            }
            crate::fx::heard(
                ctx,
                1038,
                [
                    f64::from(center.x),
                    f64::from(center.y),
                    f64::from(center.z),
                ],
            );
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::OnlinePlayer;

    fn context() -> Option<Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    fn name(ctx: &ServerContext, x: i32, y: i32, z: i32) -> String {
        ctx.world
            .block(Position::new(x, y, z), &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            .unwrap_or("?")
            .to_owned()
    }

    fn put(ctx: &ServerContext, x: i32, y: i32, z: i32, block: &str) {
        let state = ctx.data.blocks.default_state(block).unwrap();
        WorldLevel::new(ctx).set_block(Position::new(x, y, z), state);
    }

    /// A 4×5 obsidian frame in the x–y plane at z = 0, inside x 0..1,
    /// y -60..-58 (on the flat world's grass at y -61).
    fn frame(ctx: &ServerContext) {
        for x in -1..=2 {
            for y in -61..=-57 {
                if x == -1 || x == 2 || y == -61 || y == -57 {
                    put(ctx, x, y, 0, "minecraft:obsidian");
                }
            }
        }
    }

    fn light(ctx: &ServerContext, x: i32, y: i32, z: i32) {
        let fire = ctx.data.blocks.default_state("minecraft:fire").unwrap();
        let mut level = WorldLevel::new(ctx);
        let logic = level.logic();
        mc_tick::set_block(
            &mut level,
            logic,
            BlockPos::new(x, y, z),
            fire,
            mc_tick::Flags::ALL,
        );
    }

    fn player(ctx: &ServerContext, pose: Pose, game_mode: i32) -> u128 {
        let uuid = 7u128;
        ctx.players.lock().unwrap().insert(
            uuid,
            OnlinePlayer {
                profile: mc_protocol::packets::GameProfile {
                    uuid,
                    name: "Steve".into(),
                    properties: Vec::new(),
                },
                game_mode,
                entity_id: 1,
                skin_parts: 0,
                pose: Some(pose),
                dimension: ctx.dimension_name().to_owned(),
                main_hand: None,
                flying: false,
                address: None,
                extra: Default::default(),
            },
        );
        ctx.vitals.lock().unwrap().insert(uuid, Default::default());
        uuid
    }

    fn pose(x: f64, y: f64, z: f64) -> Pose {
        Pose {
            x,
            y,
            z,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        }
    }

    #[test]
    fn fire_in_a_frame_makes_a_portal_and_a_broken_frame_unmakes_it() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        frame(&ctx);
        light(&ctx, 0, -60, 0);
        for (x, y) in [(0, -60), (1, -60), (0, -59), (1, -58)] {
            assert_eq!(name(&ctx, x, y, 0), "minecraft:nether_portal", "{x} {y}");
        }
        assert_eq!(name(&ctx, 0, -60, 1), "minecraft:air");
        // Break a side of the frame: everything inside goes.
        put(&ctx, 2, -59, 0, "minecraft:air");
        for (x, y) in [(0, -60), (1, -60), (0, -59), (1, -58)] {
            assert_eq!(name(&ctx, x, y, 0), "minecraft:air", "{x} {y}");
        }
    }

    #[test]
    fn a_frame_too_small_makes_no_portal() {
        let Some(ctx) = context() else {
            return;
        };
        // Inside 2 wide but only 2 tall.
        for x in -1..=2 {
            for y in -61..=-58 {
                if x == -1 || x == 2 || y == -61 || y == -58 {
                    put(&ctx, x + 10, y, 5, "minecraft:obsidian");
                }
            }
        }
        light(&ctx, 10, -60, 5);
        assert_eq!(name(&ctx, 10, -60, 5), "minecraft:fire");
    }

    #[test]
    fn twelve_eyes_make_an_end_portal() {
        let Some(ctx) = context() else {
            return;
        };
        let place = |x: i32, z: i32, facing: &str, eye: &str| {
            let state = ctx
                .data
                .blocks
                .state_id(
                    "minecraft:end_portal_frame",
                    &[("eye", eye), ("facing", facing)],
                )
                .unwrap();
            WorldLevel::new(&ctx).set_block(Position::new(x, -60, z), state);
        };
        let ring: Vec<(i32, i32, &str)> = (-1..=1)
            .flat_map(|i| {
                [
                    (i, -2, "south"),
                    (i, 2, "north"),
                    (-2, i, "east"),
                    (2, i, "west"),
                ]
            })
            .collect();
        for &(x, z, f) in &ring {
            place(x, z, f, "true");
        }
        let (x, z, f) = ring[0];
        place(x, z, f, "false");
        assert_eq!(name(&ctx, 0, -60, 0), "minecraft:air");
        let mut level = WorldLevel::new(&ctx);
        level.set(
            BlockPos::new(x, -60, z),
            ctx.data
                .blocks
                .state_id(
                    "minecraft:end_portal_frame",
                    &[("eye", "true"), ("facing", f)],
                )
                .unwrap(),
        );
        assert!(light_end_portal(&mut level, BlockPos::new(x, -60, z)));
        drop(level);
        for dx in -1..=1 {
            for dz in -1..=1 {
                assert_eq!(name(&ctx, dx, -60, dz), "minecraft:end_portal");
            }
        }
    }

    #[test]
    fn waiting_four_seconds_in_a_portal_sends_you_to_the_nether_at_an_eighth() {
        let Some(ctx) = context() else {
            return;
        };
        // Far out so the flat world's frame is in nobody's way.
        for x in -1..=2 {
            for y in -61..=-57 {
                if x == -1 || x == 2 || y == -61 || y == -57 {
                    put(&ctx, 79 + x, y, 800, "minecraft:obsidian");
                }
            }
        }
        light(&ctx, 79, -60, 800);
        assert_eq!(name(&ctx, 79, -60, 800), "minecraft:nether_portal");
        let uuid = player(&ctx, pose(79.5, -60.0, 800.5), 0);
        let mut events = ctx.events.subscribe();
        for _ in 0..WAIT - 1 {
            tick(&ctx);
        }
        assert!(events.try_changed().is_none(), "not yet");
        tick(&ctx);
        let Some(Event::ChangeDimension {
            uuid: who,
            dimension,
            pose,
            ..
        }) = events.try_changed()
        else {
            panic!("no dimension change after {WAIT} ticks");
        };
        assert_eq!((who, dimension.as_str()), (uuid, "minecraft:the_nether"));
        // 79.5 / 8 and 800.5 / 8 in the nether; a portal was made there.
        assert!(
            (pose.x - 10.0).abs() <= 17.0 && (pose.z - 100.0).abs() <= 17.0,
            "{pose:?}"
        );
        let nether = ctx.in_dimension("minecraft:the_nether").unwrap();
        let (bx, by, bz) = (
            pose.x.floor() as i32,
            pose.y.floor() as i32,
            pose.z.floor() as i32,
        );
        let found = [(0, 0), (-1, 0), (0, -1), (1, 0), (0, 1)]
            .iter()
            .any(|(dx, dz)| name(&nether, bx + dx, by, bz + dz) == "minecraft:nether_portal");
        assert!(found, "portal at {pose:?}");

        // And back: the same portal is found, not a new one.
        let (dimension, back) = destination(&nether, uuid, Kind::Nether, pose).expect("a way back");
        assert_eq!(dimension, "minecraft:overworld");
        assert!(
            (back.x - 79.5).abs() < 10.0 && (back.z - 800.5).abs() < 10.0,
            "{back:?}"
        );
    }

    #[test]
    fn an_item_steps_through_a_portal_into_the_nether() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        frame(&ctx);
        light(&ctx, 0, -60, 0);
        let id = ctx.next_entity_id();
        lock(&ctx.entities).push(mc_entity::Entity {
            id,
            uuid: 9,
            kind: mc_entity::Kind::FallingBlock { state: 1 },
            pos: [0.5, -60.0, 0.5],
            vel: [0.2, 0.0, 0.0],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        });
        tick(&ctx);
        assert!(lock(&ctx.entities).is_empty(), "it left the overworld");
        let nether = ctx.dimension(1).expect("nether");
        let there = lock(&nether.entities);
        assert_eq!(there.len(), 1);
        assert_eq!(there[0].id, id);
        assert_eq!(there[0].vel, [0.0; 3]);
        assert!(
            there[0].pos[0].abs() < 20.0 && there[0].pos[2].abs() < 20.0,
            "came out near the scaled spot {:?}",
            there[0].pos
        );
        assert_eq!(
            lock(&nether.portals).arrived.get(&id),
            Some(&ENTITY_COOLDOWN)
        );
        drop(there);
        // The cooldown holds: another tick does not send it back.
        tick(&nether);
        assert_eq!(lock(&nether.entities).len(), 1);
        assert!(lock(&ctx.entities).is_empty());
    }

    #[test]
    fn creative_players_go_through_at_once_and_cooldown_stops_a_loop() {
        let Some(ctx) = context() else {
            return;
        };
        for x in -1..=2 {
            for y in -61..=-57 {
                if x == -1 || x == 2 || y == -61 || y == -57 {
                    put(&ctx, 40 + x, y, 40, "minecraft:obsidian");
                }
            }
        }
        light(&ctx, 40, -60, 40);
        player(&ctx, pose(40.5, -60.0, 40.5), 1);
        let mut events = ctx.events.subscribe();
        tick(&ctx);
        assert!(matches!(
            events.try_changed(),
            Some(Event::ChangeDimension { .. })
        ));
        // Still standing in it: nothing more until they step out.
        for _ in 0..50 {
            tick(&ctx);
        }
        assert!(events.try_changed().is_none());
    }

    #[test]
    fn end_portals_lead_to_a_platform_and_back_to_the_spawn() {
        let Some(ctx) = context() else {
            return;
        };
        put(&ctx, 5, -61, 5, "minecraft:end_portal");
        let uuid = player(&ctx, pose(5.5, -61.0, 5.5), 0);
        let (dimension, to) = destination(&ctx, uuid, Kind::End, pose(5.5, -61.0, 5.5)).unwrap();
        assert_eq!(dimension, "minecraft:the_end");
        assert_eq!((to.x, to.y, to.z), END_SPAWN);
        let end = ctx.in_dimension("minecraft:the_end").unwrap();
        assert_eq!(name(&end, 100, 48, 0), "minecraft:obsidian");
        assert_eq!(name(&end, 102, 48, -2), "minecraft:obsidian");
        assert_eq!(name(&end, 100, 49, 0), "minecraft:air");
        // From the End: the world spawn.
        let (dimension, back) = destination(&end, uuid, Kind::End, to).unwrap();
        assert_eq!(dimension, "minecraft:overworld");
        let spawn = ctx.spawn.lock().unwrap().pos;
        assert_eq!(back.x.floor() as i32, spawn.0);
    }

    #[test]
    fn execute_in_runs_in_that_dimension() {
        let Some(ctx) = context() else {
            return;
        };
        let mut control = crate::tick::TickControl::default();
        let out = crate::commands::execute(
            &ctx,
            &mut control,
            &crate::commands::Source::Console,
            "execute in minecraft:the_nether run setblock 3 10 3 minecraft:netherrack",
        );
        assert!(out.lines.is_empty(), "{:?}", out.lines);
        let nether = ctx.in_dimension("minecraft:the_nether").unwrap();
        assert_eq!(name(&nether, 3, 10, 3), "minecraft:netherrack");
        assert_eq!(name(&ctx, 3, 10, 3), "minecraft:air");
        // The change is announced to the nether, not to the overworld.
        assert!(out.events.iter().any(|e| matches!(e, Event::In(1, _))));
    }

    /// A real world folder with a seed (generated, so slow).
    fn seeded(tag: &str) -> Option<(Arc<ServerContext>, std::path::PathBuf)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let folder = std::env::temp_dir().join(format!("mc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let mut config = crate::Config::new(dir);
        config.world = Some(folder.clone());
        config.seed = Some(12345);
        Some((ServerContext::new(config, data).ok()?, folder))
    }

    #[test]
    fn generated_chunks_have_every_section_of_the_dimension() {
        let Some((ctx, folder)) = seeded("sections") else {
            return;
        };
        for index in 0..3 {
            let d = ctx.dimension(index).unwrap();
            let want = (d.world.height / 16) as usize;
            for (x, z) in [(0, 0), (5, -3)] {
                let got = d.world.section_count(x, z, &ctx.data.blocks);
                assert_eq!(got, Some(want), "{} chunk {x},{z}", d.dimension_name());
            }
        }
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn portals_survive_saving_and_reopening() {
        let Some((ctx, folder)) = seeded("portal-save") else {
            return;
        };
        let nether = ctx.dimension(1).unwrap();
        let state = nether
            .shapes
            .state("minecraft:nether_portal", &[("axis", "z")])
            .unwrap();
        let at = Position::new(3, 70, 3);
        WorldLevel::new(&nether).set_block(at, state);
        let overworld_state = ctx
            .shapes
            .state("minecraft:nether_portal", &[("axis", "x")])
            .unwrap();
        WorldLevel::new(&ctx).set_block(Position::new(3, 70, 3), overworld_state);
        crate::save(&ctx);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).unwrap();
        let mut config = crate::Config::new(dir);
        config.world = Some(folder.clone());
        let again = ServerContext::new(config, data).unwrap();
        let blocks = &again.data.blocks;
        assert_eq!(again.world.block(at, blocks), Some(overworld_state));
        let nether = again.dimension(1).unwrap();
        assert_eq!(nether.world.block(at, blocks), Some(state));
        let _ = std::fs::remove_dir_all(folder);
    }
}
