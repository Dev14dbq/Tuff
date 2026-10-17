//! Tick logic in vanilla order: block updates (neighbour changed, shape),
//! later scheduled ticks, block events and redstone (ROADMAP Ф3).

pub mod dir;
pub mod facts;
pub mod fluids;
pub mod gravity;
pub mod level;
pub mod pistons;
pub mod redstone;
pub mod shapes;
pub mod support;
pub mod ticks;
pub mod updates;

pub use dir::{Axis, BlockPos, Dir};
pub use facts::BlockRules;
pub use level::Level;
pub use shapes::{Kind, Shapes};
pub use ticks::{ScheduledTicks, TickQueue};
pub use updates::{Ctx, Flags, block_tick, placement_state, set_block, use_block};
