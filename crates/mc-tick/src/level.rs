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

    /// Lava flows fast, far and thin here (the dimension's
    /// `gameplay/fast_lava`, the Nether).
    fn fast_lava(&self) -> bool {
        false
    }

    /// Turns the block that was at `p` into a falling block entity.
    fn spawn_falling_block(&mut self, p: BlockPos, state: u32);

    /// Sets a block without telling clients (they play the piston block
    /// event themselves); a block already changed this tick is still sent
    /// as it ends up.
    fn set_hidden(&mut self, p: BlockPos, state: u32) -> bool {
        self.set(p, state)
    }

    /// Queues a block event (pistons) for the end of the tick's block work.
    fn block_event(&mut self, _p: BlockPos, _block: u32, _action: u8, _param: u8) {}

    /// Moving pistons (blocks on their way).
    fn add_moving(&mut self, _p: BlockPos, _moving: crate::pistons::Moving) {}
    fn moving(&self, _p: BlockPos) -> Option<crate::pistons::Moving> {
        None
    }
    fn remove_moving(&mut self, _p: BlockPos) -> Option<crate::pistons::Moving> {
        None
    }
    fn moving_positions(&self) -> Vec<BlockPos> {
        Vec::new()
    }

    /// The light level at `p` (the brighter of sky and block light).
    fn brightness(&self, _p: BlockPos) -> u8 {
        15
    }

    /// The block `state` at `p` broke because nothing holds it any more (it
    /// drops what it would when broken).
    fn destroyed(&mut self, _p: BlockPos, _state: u32) {}

    /// How full the container at `p` is, as a comparator reads it (`None`
    /// if it is not a container).
    fn container_signal(&self, _p: BlockPos) -> Option<u8> {
        None
    }

    /// The signal a comparator at `p` holds (kept in its block entity).
    fn comparator_output(&self, _p: BlockPos) -> u8 {
        0
    }

    fn set_comparator_output(&mut self, _p: BlockPos, _signal: u8) {}

    /// Entities whose boxes touch the box from `min` to `max`: all of them,
    /// and only players and mobs.
    fn entities_in(&self, _min: [f64; 3], _max: [f64; 3]) -> (u32, u32) {
        (0, 0)
    }
}
