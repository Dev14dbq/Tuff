//! Tick logic in vanilla order: block updates (neighbour changed, shape),
//! later scheduled ticks, block events and redstone (ROADMAP Ф3).

pub mod cauldron;
pub mod command_block;
pub mod composter;
pub mod dir;
pub mod facts;
pub mod fast;
pub mod fire;
pub mod fluids;
pub mod gravity;
pub mod hopper;
pub mod leaves;
pub mod level;
pub mod pistons;
pub mod portal;
pub mod rails;
pub mod random;
pub mod redstone;
pub mod rng;
pub mod shapes;
pub mod sponge;
pub mod support;
pub mod ticks;
pub mod tripwire;
pub mod updates;

pub use dir::{Axis, BlockPos, Dir};
pub use facts::BlockRules;
pub use level::Level;
pub use shapes::{Kind, Shapes};
pub use ticks::{ScheduledTicks, TickQueue};
pub use updates::{Ctx, Flags, block_tick, placement_state, set_block, use_block};
