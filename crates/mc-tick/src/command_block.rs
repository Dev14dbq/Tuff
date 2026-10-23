//! Command blocks: power starts impulse and repeating ones; what they run
//! is up to the world ([`Level::command_block_tick`]).

use crate::Ctx;
use crate::dir::BlockPos;
use crate::level::Level;
use crate::redstone;

/// The three kinds, by block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `chain_command_block`: runs when the one behind it ran.
    Chain,
    /// `repeating_command_block`: every tick while it may.
    Repeating,
    /// `command_block`: once each time it is started.
    Impulse,
}

pub fn mode(ctx: Ctx, state: u32) -> Option<Mode> {
    match ctx.shapes.block_name(state)? {
        "minecraft:command_block" => Some(Mode::Impulse),
        "minecraft:chain_command_block" => Some(Mode::Chain),
        "minecraft:repeating_command_block" => Some(Mode::Repeating),
        _ => None,
    }
}

/// A neighbour changed: the block notes its power, and an impulse or
/// repeating block that needs redstone starts a tick later when the power
/// comes.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if ctx.shapes.block_name(state) == Some("minecraft:structure_block") {
        let powered = redstone::received(level, ctx, p, true) > 0;
        level.structure_block_power(p, powered);
        return;
    }
    let Some(mode) = mode(ctx, state) else {
        return;
    };
    let powered = redstone::received(level, ctx, p, true) > 0;
    let Some((was, auto)) = level.command_block_power(p, powered) else {
        return;
    };
    if powered && !was && !auto && mode != Mode::Chain {
        level.schedule_block_tick(p, ctx.shapes.block_index(state), 1, 0);
    }
}

/// A due tick of a command block.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) == kind && mode(ctx, state).is_some() {
        level.command_block_tick(p);
    }
}
