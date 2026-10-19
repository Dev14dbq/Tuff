//! Changing the world for commands: blocks with their block entities, as
//! a player would see them change.

use mc_protocol::Position;

use super::Output;
use crate::context::{Event, ServerContext};
use crate::level::WorldLevel;

pub fn in_world(ctx: &ServerContext, p: Position) -> bool {
    p.y >= ctx.world.min_y && p.y < ctx.world.min_y + ctx.world.height as i32
}

/// Block entity id of a block (most are named like the block).
fn block_entity_id(name: &str) -> String {
    let short = name.trim_start_matches("minecraft:");
    let id = if short.ends_with("_hanging_sign") {
        "hanging_sign"
    } else if short.ends_with("_sign") {
        "sign"
    } else if short.ends_with("_bed") {
        "bed"
    } else if short.ends_with("_banner") {
        "banner"
    } else if short.ends_with("shulker_box") {
        "shulker_box"
    } else if short.ends_with("_skull") || short.ends_with("_head") {
        "skull"
    } else {
        short
    };
    format!("minecraft:{id}")
}

/// Merges command data into the block entity at `p`.
pub fn put_block_data(ctx: &ServerContext, p: Position, state: u32, data: mc_nbt::Compound) {
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(state).unwrap_or("");
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: block_entity_id(name),
            data: mc_nbt::Compound::new(),
        });
    for (key, value) in data.iter() {
        be.data.insert(key, value.clone());
    }
    ctx.world.set_block_entity(be, blocks);
    ctx.containers
        .lock()
        .expect("containers lock")
        .remove(&(p.x, p.y, p.z));
    ctx.container_updates
        .lock()
        .expect("container updates lock")
        .push(p);
}

/// Applies changes with block updates and announces everything that changed
/// to everyone (nobody predicted it). Returns how many of `changes` took.
pub fn apply(
    ctx: &ServerContext,
    changes: Vec<(Position, u32)>,
    destroy: bool,
    out: &mut Output,
) -> usize {
    let blocks = &ctx.data.blocks;
    let mut level = WorldLevel::new(ctx);
    let mut count = 0;
    for (p, state) in changes {
        let old = ctx.world.block(p, blocks);
        let state = mc_tick::placement_state(&level, level.logic(), crate::level::to_pos(p), state);
        if level.set_block(p, state) {
            crate::containers::placed(ctx, p, state);
            count += 1;
            if destroy && let Some(old) = old.filter(|&o| !blocks.is_air(o)) {
                out.events.push(Event::BlockBroken {
                    by: 0,
                    position: p,
                    state: old,
                });
            }
        }
    }
    let changed = level.into_changes();
    if !changed.is_empty() {
        out.events.push(Event::BlocksChanged {
            by: 0,
            changes: changed,
        });
    }
    count
}
