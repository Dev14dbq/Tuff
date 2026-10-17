//! What block logic needs from a world.

use crate::dir::BlockPos;

pub trait Level {
    /// The block at `p`; air where nothing is loaded.
    fn block(&self, p: BlockPos) -> u32;

    /// Stores a block without any updates; `false` if it cannot change there
    /// (outside the world, not loaded).
    fn set(&mut self, p: BlockPos, state: u32) -> bool;

    /// Game time (world age) in ticks.
    fn game_time(&self) -> i64;

    /// Asks for a fluid tick at `p` after `delay` ticks; `fluid` is a
    /// [`crate::fluids::Fluid`] id.
    fn schedule_fluid_tick(&mut self, p: BlockPos, fluid: u32, delay: i64);

    /// Asks for a block tick at `p` after `delay` ticks.
    fn schedule_block_tick(&mut self, p: BlockPos, block: u32, delay: i64, priority: i32);

    /// Whether a block tick for `block` at `p` is waiting.
    fn is_block_tick_scheduled(&self, p: BlockPos, block: u32) -> bool;

    /// Whether `p` is inside the world's height.
    fn in_world(&self, p: BlockPos) -> bool;

    /// Turns the block that was at `p` into a falling block entity.
    fn spawn_falling_block(&mut self, p: BlockPos, state: u32);
}
