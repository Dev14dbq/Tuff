//! Silverfish and infested blocks (minecraft.wiki, "Silverfish", "Infested
//! Block"): a silverfish at peace hides in a stone, cobblestone, stone
//! bricks or deepslate block next to it, which turns infested; a hurt one
//! calls the others out of the infested blocks within ten blocks sideways
//! and five up or down; and a player who breaks an infested block without
//! Silk Touch lets one out.

use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;
use mc_protocol::packets::play;

use super::new_mob;
use crate::context::ServerContext;
use crate::fx;
use crate::level::WorldLevel;

/// The blocks (without namespace) that have an infested form.
const HOSTS: [&str; 7] = [
    "stone",
    "cobblestone",
    "stone_bricks",
    "mossy_stone_bricks",
    "cracked_stone_bricks",
    "chiseled_stone_bricks",
    "deepslate",
];

/// Blocks around a calling silverfish: sideways and up or down.
const CALL_SIDE: i32 = 10;
const CALL_UP: i32 = 5;

/// Whether a block (name without namespace) can be infested.
pub fn is_host(name: &str) -> bool {
    HOSTS.contains(&name)
}

fn pos(c: [i32; 3]) -> Position {
    Position::new(c[0], c[1], c[2])
}

/// The block a state is the infested form of, with the same axis.
pub fn host_of(ctx: &ServerContext, state: u32) -> Option<u32> {
    let blocks = &ctx.data.blocks;
    let name = blocks
        .block_name(state)?
        .strip_prefix("minecraft:infested_")?;
    let host = blocks.default_state(&format!("minecraft:{name}"))?;
    match blocks.property(state, "axis") {
        Some(axis) => blocks.with_property(host, "axis", axis),
        None => Some(host),
    }
}

/// The infested form of a block, if it has one.
pub fn infested_of(ctx: &ServerContext, state: u32) -> Option<u32> {
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(state)?.strip_prefix("minecraft:")?;
    if !is_host(name) {
        return None;
    }
    let infested = blocks.default_state(&format!("minecraft:infested_{name}"))?;
    match blocks.property(state, "axis") {
        Some(axis) => blocks.with_property(infested, "axis", axis),
        None => Some(infested),
    }
}

fn silverfish_at(ctx: &ServerContext, cell: [i32; 3]) -> Entity {
    let at = [
        f64::from(cell[0]) + 0.5,
        f64::from(cell[1]),
        f64::from(cell[2]) + 0.5,
    ];
    let mut e = new_mob(ctx, Species::Silverfish, at);
    if let Kind::Mob(m) = &mut e.kind {
        m.yaw = (crate::util::random_u64() % 360) as f32;
        m.head_yaw = m.yaw;
    }
    e
}

/// An infested block was broken (by a player without Silk Touch, or by a
/// blast): a silverfish comes out of it, if blocks drop things.
pub fn emerge(ctx: &ServerContext, at: Position, state: u32) {
    if host_of(ctx, state).is_none() || !ctx.game_rule_bool("block_drops") {
        return;
    }
    let e = silverfish_at(ctx, [at.x, at.y, at.z]);
    crate::tick::spawn_entity(ctx, e);
}

/// The silverfish at `i` goes into the block at `cell`, which turns
/// infested; the silverfish is gone.
pub fn hide(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    i: usize,
    cell: [i32; 3],
) {
    if !ctx.game_rule_bool("minecraft:mob_griefing") {
        return;
    }
    let Some(state) = ctx.world.block(pos(cell), &ctx.data.blocks) else {
        return;
    };
    let Some(infested) = infested_of(ctx, state) else {
        return;
    };
    level.set_block(pos(cell), infested);
    fx::status(ctx, entities[i].id, 20);
    entities[i].removed = true;
}

/// 0, 1, -1, 2, -2, ... up to `n` and back: nearest first.
fn outward(n: i32) -> impl Iterator<Item = i32> {
    std::iter::once(0).chain((1..=n).flat_map(|k| [k, -k]))
}

/// The silverfish at `i` calls the others: the infested blocks around it,
/// nearest first, are broken (letting one out each) or, without mob
/// griefing, turned back to plain stone; each time there is an even chance
/// it stops. The silverfish that came out are put in `fresh`.
pub fn wake_friends(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &[Entity],
    i: usize,
    fresh: &mut Vec<Entity>,
) {
    let griefing = ctx.game_rule_bool("minecraft:mob_griefing");
    let drops = ctx.game_rule_bool("block_drops");
    let at = [
        entities[i].pos[0].floor() as i32,
        entities[i].pos[1].floor() as i32,
        entities[i].pos[2].floor() as i32,
    ];
    for dy in outward(CALL_UP) {
        for dx in outward(CALL_SIDE) {
            for dz in outward(CALL_SIDE) {
                let cell = [at[0] + dx, at[1] + dy, at[2] + dz];
                if !ctx.world.chunk_loaded(cell[0] >> 4, cell[2] >> 4) {
                    continue;
                }
                let Some(state) = ctx.world.block(pos(cell), &ctx.data.blocks) else {
                    continue;
                };
                let Some(host) = host_of(ctx, state) else {
                    continue;
                };
                if griefing {
                    level.set_block(pos(cell), ctx.shapes.air());
                    fx::level_event(
                        ctx,
                        play::level_event::BLOCK_BROKEN,
                        [
                            f64::from(cell[0]) + 0.5,
                            f64::from(cell[1]) + 0.5,
                            f64::from(cell[2]) + 0.5,
                        ],
                        state as i32,
                    );
                    if drops {
                        fresh.push(silverfish_at(ctx, cell));
                    }
                } else {
                    level.set_block(pos(cell), host);
                }
                if crate::util::random_u64() & 1 == 0 {
                    return;
                }
            }
        }
    }
}

/// Tells a silverfish it was hurt by something: it will call the others
/// in a second (the call is not made again while one is on its way).
pub fn notify_hurt(m: &mut mc_entity::Mob) {
    if m.species == Species::Silverfish && m.quirks.call_friends == 0 {
        m.quirks.call_friends = 20;
    }
}
