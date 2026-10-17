//! What block logic needs from a world.

use crate::dir::BlockPos;

pub trait Level {
    /// The block at `p`; air where nothing is loaded.
    fn block(&self, p: BlockPos) -> u32;

    /// Stores a block without any updates; `false` if it cannot change there
    /// (outside the world, not loaded).
    fn set(&mut self, p: BlockPos, state: u32) -> bool;
}
