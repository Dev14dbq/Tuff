//! Mobs changing blocks (minecraft.wiki, "Zombie", "Enderman", "Door"):
//! zombies crack and break wooden doors, villagers open and close them,
//! endermen pick blocks up and put them down.

use mc_entity::mob::slot;
use mc_entity::nav::{Cell, Terrain};
use mc_entity::{Collide, Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play;

use super::Ground;
use crate::context::ServerContext;
use crate::fx;
use crate::level::{WorldLevel, to_pos};

fn pos(c: Cell) -> Position {
    Position::new(c[0], c[1], c[2])
}

fn centre(c: Cell) -> [f64; 3] {
    [
        f64::from(c[0]) + 0.5,
        f64::from(c[1]) + 0.5,
        f64::from(c[2]) + 0.5,
    ]
}

/// The cracks a zombie leaves in a door (`stage` -1 takes them away).
pub fn crack(ctx: &ServerContext, mob: i32, cell: Cell, stage: i32) {
    fx::emit(
        ctx,
        fx::Fx::Cracks(play::BlockDestruction {
            entity_id: mob,
            position: pos(cell),
            stage: stage.clamp(-1, 127) as i8,
        }),
    );
}

/// The door shakes: "zombie attacks wooden door".
pub fn bash(ctx: &ServerContext, cell: Cell) {
    fx::level_event(ctx, 1019, centre(cell), 0);
}

/// The other half of the door at `cell`, if it has one.
fn other_half(ctx: &ServerContext, cell: Cell, state: u32) -> Cell {
    match ctx.data.blocks.property(state, "half") {
        Some("upper") => [cell[0], cell[1] - 1, cell[2]],
        _ => [cell[0], cell[1] + 1, cell[2]],
    }
}

/// A zombie breaks the door: both halves go, with the crash and the bits.
pub fn break_door(ctx: &ServerContext, level: &mut WorldLevel, mob: i32, cell: Cell) {
    let Some(state) = ctx.world.block(pos(cell), &ctx.data.blocks) else {
        return;
    };
    let name = ctx.data.blocks.block_name(state).unwrap_or("");
    if !name.ends_with("_door") || name == "minecraft:iron_door" {
        return;
    }
    let other = other_half(ctx, cell, state);
    let air = ctx.shapes.air();
    if ctx
        .world
        .block(pos(other), &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        == Some(name)
    {
        level.set_block(pos(other), air);
    }
    level.set_block(pos(cell), air);
    crack(ctx, mob, cell, -1);
    fx::level_event(ctx, 1021, centre(cell), 0);
    fx::level_event(ctx, play::level_event::BLOCK_BROKEN, centre(cell), state as i32);
}

/// The sound set of a door ("wooden_door", "copper_door", ...).
pub fn door_sounds(name: &str) -> &'static str {
    let n = name.trim_start_matches("minecraft:");
    if n.contains("copper") {
        "copper_door"
    } else if n.starts_with("bamboo") {
        "bamboo_wood_door"
    } else if n.starts_with("cherry") {
        "cherry_wood_door"
    } else if n.starts_with("crimson") || n.starts_with("warped") {
        "nether_wood_door"
    } else {
        "wooden_door"
    }
}

/// A villager opens or closes the door at `cell` (the other half follows).
pub fn open_door(ctx: &ServerContext, level: &mut WorldLevel, cell: Cell, open: bool) {
    let Some(state) = ctx.world.block(pos(cell), &ctx.data.blocks) else {
        return;
    };
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(state).unwrap_or("").to_owned();
    if !name.ends_with("_door") || name == "minecraft:iron_door" {
        return;
    }
    let value = if open { "true" } else { "false" };
    if blocks.property(state, "open") == Some(value) {
        return;
    }
    let Some(to) = blocks.with_property(state, "open", value) else {
        return;
    };
    let logic = level.logic();
    mc_tick::updates::set_block(level, logic, to_pos(pos(cell)), to, mc_tick::updates::Flags::ALL);
    {
        let mut watch = crate::tick::lock(&ctx.door_watch);
        watch.retain(|c| *c != cell);
        if open {
            watch.push(cell);
        }
    }
    let sound = format!(
        "block.{}.{}",
        door_sounds(&name),
        if open { "open" } else { "close" }
    );
    let pitch = crate::hits::rnd() as f32 * 0.1 + 0.9;
    fx::sound(ctx, &sound, fx::category::BLOCKS, centre(cell), 1.0, pitch);
}

/// The metadata of a villager asleep in the bed at `bed` (or up again).
pub fn sleep_data(id: i32, bed: Option<Cell>) -> play::SetEntityData {
    play::SetEntityData {
        entity_id: id,
        entries: vec![
            (
                play::metadata::POSE,
                play::MetadataValue::Pose(if bed.is_some() { 2 } else { 0 }),
            ),
            (
                play::metadata::SLEEPING_POS,
                play::MetadataValue::OptPosition(bed.map(pos)),
            ),
        ],
    }
}

/// Sets `occupied` of the bed whose head is at `cell` and of its foot.
fn occupy(ctx: &ServerContext, level: &mut WorldLevel, cell: Cell, taken: bool) {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(pos(cell), blocks) else {
        return;
    };
    let value = if taken { "true" } else { "false" };
    let Some(facing) = blocks.property(state, "facing") else {
        return;
    };
    let (dx, dz) = match facing {
        "north" => (0, 1),
        "south" => (0, -1),
        "west" => (1, 0),
        _ => (-1, 0),
    };
    let foot = [cell[0] + dx, cell[1], cell[2] + dz];
    let logic = level.logic();
    for c in [cell, foot] {
        let Some(s) = ctx.world.block(pos(c), blocks) else {
            continue;
        };
        if blocks.property(s, "occupied") == Some(value) {
            continue;
        }
        if let Some(to) = blocks.with_property(s, "occupied", value) {
            mc_tick::updates::set_block(level, logic, to_pos(pos(c)), to, mc_tick::updates::Flags::ALL);
        }
    }
}

/// A villager lies down in the bed at `cell`.
pub fn lie_down(ctx: &ServerContext, level: &mut WorldLevel, e: &mut Entity, cell: Cell) {
    occupy(ctx, level, cell, true);
    e.pos = mc_entity::villager::bed_spot(cell);
    e.vel = [0.0; 3];
    e.on_ground = true;
    {
        let mut watch = crate::tick::lock(&ctx.bed_watch);
        watch.retain(|(c, _)| *c != cell);
        watch.push((cell, e.id));
    }
    fx::emit(ctx, fx::Fx::Data(sleep_data(e.id, Some(cell))));
}

/// A villager gets up from the bed at `cell`.
pub fn get_up(ctx: &ServerContext, level: &mut WorldLevel, id: i32, cell: Cell) {
    occupy(ctx, level, cell, false);
    crate::tick::lock(&ctx.bed_watch).retain(|(c, _)| *c != cell);
    fx::emit(ctx, fx::Fx::Data(sleep_data(id, None)));
}

/// Once a second: beds whose sleeper is gone (killed, unloaded) are free.
pub fn free_beds(ctx: &ServerContext, level: &mut WorldLevel, entities: &[Entity]) {
    let watched: Vec<([i32; 3], i32)> = crate::tick::lock(&ctx.bed_watch).clone();
    for (cell, id) in watched {
        let asleep = entities.iter().any(|e| {
            e.id == id
                && !e.removed
                && matches!(&e.kind, Kind::Merchant(m) if m.walk.asleep && m.walk.bed == Some(cell))
        });
        if !asleep {
            occupy(ctx, level, cell, false);
            crate::tick::lock(&ctx.bed_watch).retain(|(c, _)| *c != cell);
        }
    }
}

/// Once a second: doors a villager opened shut again when nothing stands
/// by them (the villager may be gone, or far off by now).
pub fn close_idle(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &[Entity],
    players: &[mc_entity::mob::Target],
) {
    let watched: Vec<Cell> = crate::tick::lock(&ctx.door_watch).clone();
    for cell in watched {
        let near = |p: [f64; 3]| {
            (p[0] - f64::from(cell[0]) - 0.5).abs() < 1.6
                && (p[2] - f64::from(cell[2]) - 0.5).abs() < 1.6
                && p[1] > f64::from(cell[1]) - 2.0
                && p[1] < f64::from(cell[1]) + 2.0
        };
        let busy = entities
            .iter()
            .any(|e| !e.removed && matches!(e.kind, Kind::Mob(_) | Kind::Merchant(_)) && near(e.pos))
            || players.iter().any(|p| p.present && near(p.pos));
        let is_open = ctx
            .world
            .block(pos(cell), &ctx.data.blocks)
            .is_some_and(|s| ctx.data.blocks.property(s, "open") == Some("true"));
        if !is_open {
            crate::tick::lock(&ctx.door_watch).retain(|c| *c != cell);
        } else if !busy {
            open_door(ctx, level, cell, false);
        }
    }
}

/// An enderman takes the block at `cell` into its hands (as the block's
/// plain default state).
pub fn take(ctx: &ServerContext, level: &mut WorldLevel, e: &mut Entity, cell: Cell) {
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.carried.is_some() {
        return;
    }
    let ground = Ground::new(ctx);
    if !ground.holdable(cell[0], cell[1], cell[2]) {
        return;
    }
    let Some(state) = ctx.world.block(pos(cell), &ctx.data.blocks) else {
        return;
    };
    let Some(plain) = ctx
        .data
        .blocks
        .block_name(state)
        .and_then(|n| ctx.data.blocks.default_state(n))
    else {
        return;
    };
    level.set_block(pos(cell), ctx.shapes.air());
    m.carried = Some(plain);
    m.dirty = true;
}

/// Whether anything with a body (mob, item, player) is in the cell.
fn occupied(entities: &[Entity], players: &[mc_entity::mob::Target], cell: Cell) -> bool {
    let (lo, hi) = (
        [f64::from(cell[0]), f64::from(cell[1]), f64::from(cell[2])],
        [
            f64::from(cell[0] + 1),
            f64::from(cell[1] + 1),
            f64::from(cell[2] + 1),
        ],
    );
    let hits = |p: [f64; 3], w: f64, h: f64| {
        p[0] + w / 2.0 > lo[0]
            && p[0] - w / 2.0 < hi[0]
            && p[2] + w / 2.0 > lo[2]
            && p[2] - w / 2.0 < hi[2]
            && p[1] + h > lo[1]
            && p[1] < hi[1]
    };
    entities
        .iter()
        .any(|e| !e.removed && matches!(e.kind, Kind::Mob(_) | Kind::Merchant(_)) && {
            let (w, h) = e.size();
            hits(e.pos, w, h)
        })
        || players.iter().any(|p| p.present && hits(p.pos, 0.6, 1.8))
}

/// An enderman sets its block down at `cell` if the spot is free air on
/// a full block (not bedrock) and the block can stay there.
pub fn place(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    i: usize,
    cell: Cell,
    players: &[mc_entity::mob::Target],
) {
    let Kind::Mob(m) = &entities[i].kind else {
        return;
    };
    let Some(carried) = m.carried else {
        return;
    };
    let blocks = &ctx.data.blocks;
    let air = |c: Cell| {
        ctx.world
            .block(pos(c), blocks)
            .and_then(|s| blocks.block_name(s))
            .is_some_and(|n| matches!(n, "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"))
    };
    let below = [cell[0], cell[1] - 1, cell[2]];
    if !air(cell) || air(below) {
        return;
    }
    let below_state = ctx.world.block(pos(below), blocks);
    if below_state.and_then(|s| blocks.block_name(s)) == Some("minecraft:bedrock") {
        return;
    }
    let ground = Ground::new(ctx);
    if ground.span(below[0], below[1], below[2]) != Some((0.0, 1.0)) {
        return;
    }
    if !mc_tick::support::can_survive(level, level.logic(), to_pos(pos(cell)), carried) {
        return;
    }
    if occupied(entities, players, cell) {
        return;
    }
    drop(ground);
    level.set_block(pos(cell), carried);
    if let Kind::Mob(m) = &mut entities[i].kind {
        m.carried = None;
        m.dirty = true;
    }
    let _ = slot::MAINHAND;
}
