//! What block logic needs from a world.

use crate::dir::{BlockPos, Dir};

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

    /// Whether fire may light a nether portal here (the overworld and the
    /// Nether).
    fn allows_nether_portal(&self) -> bool {
        true
    }

    /// Lava flows fast, far and thin here (the dimension's
    /// `gameplay/fast_lava`, the Nether).
    fn fast_lava(&self) -> bool {
        false
    }

    /// Whether two sources of `fluid` make a new one between them (game rules
    /// `water_source_conversion`, `lava_source_conversion`).
    fn source_conversion(&self, fluid: crate::fluids::Fluid) -> bool {
        fluid == crate::fluids::Fluid::Water
    }

    /// Whether vines spread (game rule `spread_vines`).
    fn spread_vines(&self) -> bool {
        true
    }

    /// Water cannot stay here (the Nether): wet sponges dry at once.
    fn ultrawarm(&self) -> bool {
        false
    }

    /// A wet sponge dried at `p`: it hisses and steams.
    fn sponge_dried(&mut self, _p: BlockPos) {}

    /// A block sound (`block.copper_bulb.turn_on`...) at `p`.
    fn sound(&mut self, _name: &str, _p: BlockPos) {}

    /// A world event (sound and particles clients play) at `p`.
    fn level_event(&mut self, _event: i32, _p: BlockPos, _data: i32) {}

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

    /// Block light alone at `p` (torches, lava; not the sky).
    fn block_light(&self, _p: BlockPos) -> u8 {
        0
    }

    /// How much light `state` takes away passing through it (0-15).
    fn light_block(&self, _state: u32) -> u8 {
        0
    }

    /// Whether rain falls on `p` (raining, and open to the sky).
    fn raining_at(&self, _p: BlockPos) -> bool {
        false
    }

    /// Whether fire at `p` may age and spread (a player is near enough,
    /// `fire_spread_radius_around_player`).
    fn fire_spreads_at(&self, _p: BlockPos) -> bool {
        true
    }

    /// The block `state` at `p` broke because nothing holds it any more (it
    /// drops what it would when broken).
    fn destroyed(&mut self, _p: BlockPos, _state: u32) {}

    /// How full the container at `p` is, as a comparator reads it (`None`
    /// if it is not a container).
    fn container_signal(&self, _p: BlockPos) -> Option<u8> {
        None
    }

    /// What a comparator reads from the block entity at `p` when the block
    /// state alone does not tell (chiseled bookshelves, lecterns,
    /// jukeboxes); `None` to read it as a container.
    fn block_entity_signal(&self, _p: BlockPos) -> Option<u8> {
        None
    }

    /// How many players have the container at `p` open.
    fn viewers(&self, _p: BlockPos) -> u8 {
        0
    }

    /// The signal a comparator at `p` holds (kept in its block entity).
    fn comparator_output(&self, _p: BlockPos) -> u8 {
        0
    }

    fn set_comparator_output(&mut self, _p: BlockPos, _signal: u8) {}

    /// Grows the sapling `state` at `p` into a tree where the world has
    /// the tree features; `false` if it did not grow.
    fn grow_tree(&mut self, _p: BlockPos, _state: u32) -> bool {
        false
    }

    /// Lights the TNT at `p`: primed TNT appears there (the caller removes
    /// the block). `false` if the game forbids it.
    fn prime_tnt(&mut self, _p: BlockPos) -> bool {
        false
    }

    /// Entities whose boxes touch the box from `min` to `max`: all of them,
    /// and only players and mobs.
    fn entities_in(&self, _min: [f64; 3], _max: [f64; 3]) -> (u32, u32) {
        (0, 0)
    }

    /// Minecarts whose boxes touch the block at `p`.
    fn minecarts_at(&self, _p: BlockPos) -> u32 {
        0
    }

    /// How full a minecart holding items (chest, hopper) in the block at
    /// `p` is, as a comparator reads it; `None` if there is none.
    fn minecart_container_signal(&self, _p: BlockPos) -> Option<u8> {
        None
    }

    /// Records a redstone torch toggle at `p` (when `add`) and tells whether
    /// it has toggled too often lately (8 times within 60 ticks): then it
    /// burns out.
    fn torch_burnout(&mut self, _p: BlockPos, _add: bool) -> bool {
        false
    }

    /// A burned-out torch hisses (sound and smoke).
    fn torch_fizz(&mut self, _p: BlockPos) {}

    /// Moves one item from the container at `from` (through its face `from_side`)
    /// into the container at `to` (through its face `to_side`), into an
    /// existing stack first. `false` if either is not a container or nothing
    /// could move.
    fn container_transfer(
        &mut self,
        _from: BlockPos,
        _from_side: Dir,
        _to: BlockPos,
        _to_side: Dir,
    ) -> bool {
        false
    }

    /// The hopper's remaining rest in ticks (its block entity).
    fn hopper_cooldown(&self, _p: BlockPos) -> i32 {
        0
    }

    fn set_hopper_cooldown(&mut self, _p: BlockPos, _ticks: i32) {}

    /// Stores whether the command block at `p` is powered; returns whether
    /// it was before and whether it is always active (`None` without its
    /// block entity).
    fn command_block_power(&mut self, _p: BlockPos, _powered: bool) -> Option<(bool, bool)> {
        None
    }

    /// The structure block at `p` is powered (or not): a save or load block
    /// starts when the power comes.
    fn structure_block_power(&mut self, _p: BlockPos, _powered: bool) {}
    /// A test block's power changed.
    fn test_block_power(&mut self, _p: BlockPos, _powered: bool) {}

    /// The due tick of the command block at `p`: it runs.
    fn command_block_tick(&mut self, _p: BlockPos) {}

    /// The hopper at `p` picks up an item entity from the space above it.
    fn hopper_pickup(&mut self, _p: BlockPos) -> bool {
        false
    }

    /// A dispenser or dropper at `p` facing `facing` fires.
    fn dispense(&mut self, _p: BlockPos, _facing: Dir, _dropper: bool) {}

    /// A crafter at `p` was triggered: it crafts what its grid holds.
    fn craft(&mut self, _p: BlockPos) {}
}
