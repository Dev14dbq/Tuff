//! Block entities that tick or hold items: hoppers and droppers moving
//! items, furnaces, brewing stands, campfires, crafters and jukeboxes.
//!
//! The rules live here. The host (the server today, the level crate later)
//! hands them the world through the traits below, one for each thing they
//! need: the item registry, block states and block-entity data, containers,
//! item entities on the ground, recipes, playing jukebox songs, and a sink for
//! what others see. Block states are read with the plain `mc_data::Blocks`
//! table passed in alongside.

pub mod brewing;
pub mod campfire;
pub mod crafter;
pub mod furnace;
pub mod jukebox;
pub mod transfer;

use mc_nbt::{Compound, Tag};
pub use mc_protocol::components::ItemStack;

use crate::dir::BlockPos;

/// The slots of a container, slot 0 first; an empty slot is `None`.
pub type Slots = Vec<Option<ItemStack>>;

/// The item registry: what vanilla's `Item` and its default components say,
/// and the saved form of item lists (`ContainerHelper`'s `Items`).
pub trait Items {
    /// How many fit in one slot (`max_stack_size`).
    fn max_stack(&self, item: i32) -> i32;
    /// Uses before a tool breaks (`max_damage`); 0 if it does not wear.
    fn max_damage(&self, item: i32) -> i32;
    fn name(&self, item: i32) -> Option<&str>;
    fn id(&self, name: &str) -> Option<i32>;
    /// The `Items` list of a block entity's data, `size` slots.
    fn read_items(&self, data: &Compound, size: usize) -> Slots;
    /// `slots` as an `Items` list.
    fn write_items(&self, slots: &[Option<ItemStack>]) -> Tag;
}

/// Block states and block-entity data as saved (vanilla's `BlockEntity`
/// `loadAdditional`/`saveAdditional` view of it).
pub trait Blocks {
    /// The block at `p`; `None` while its chunk is not loaded.
    fn block(&self, p: BlockPos) -> Option<u32>;
    /// The saved data of the block entity at `p`.
    fn block_entity(&self, p: BlockPos) -> Option<Compound>;
    /// Looks at the saved data of the block entity at `p` without copying
    /// it; `false` if there is none. `f` must not reach back into the host.
    fn read_block_entity(&self, p: BlockPos, f: &mut dyn FnMut(&Compound)) -> bool {
        match self.block_entity(p) {
            Some(data) => {
                f(&data);
                true
            }
            None => false,
        }
    }
    /// Changes the data of the block entity at `p` and stores it. With
    /// `create`, a missing one is made with that id; without, nothing
    /// happens when there is none. `edit` must not reach back into the host.
    fn edit_block_entity(
        &mut self,
        p: BlockPos,
        create: Option<&str>,
        edit: &mut dyn FnMut(&mut Compound),
    );
}

/// What block entities do that players see or that reaches the rest of the
/// world, applied at once (vanilla's `Level` calls from a block entity's
/// `serverTick`).
pub trait Effects {
    /// A world event (`levelEvent`: sounds and particles clients make up)
    /// at the corner of `p`.
    fn level_event(&mut self, p: BlockPos, event: i32, data: i32);
    /// Particles at `at` (`sendParticles`).
    fn particles(&mut self, name: &str, at: [f64; 3], spread: f32, speed: f32, count: i32);
    /// An item entity with `stack` appears at `at` (`Containers.dropItemStack`
    /// and the like).
    fn spawn_stack(&mut self, at: [f64; 3], stack: &ItemStack);
    /// Sets the block at `p` with all its updates (`setBlock` with flag 3);
    /// players see the change at once.
    fn set_block(&mut self, p: BlockPos, state: u32);
    /// The redstone output of a block changed: the neighbours of each of `at`
    /// look again (`updateNeighborsAt`), and players see what changed.
    fn update_neighbors(&mut self, at: &[BlockPos]);
    /// The data a window shows changed (`ContainerData`: a furnace's burn
    /// and cook times, a brewing stand's brew time and fuel).
    fn window_values(&mut self, p: BlockPos, values: &[i16]);
    /// The block entity's client-visible data changed
    /// (`sendBlockUpdated`): clients redraw it.
    fn show_block_entity(&mut self, p: BlockPos, id: &str, data: &Compound);
    /// Players within `radius` of `centre` saw a crafter craft `recipe`
    /// out of `used` (the `crafter_recipe_crafted` trigger).
    fn crafter_crafted(&mut self, centre: [f64; 3], radius: f64, recipe: &str, used: &[ItemStack]);
    /// The next number from the world's random source.
    fn random(&mut self) -> u64;
}

/// The data pack's recipes and fuels.
pub trait Recipes {
    /// The cooking recipe of type `kind` (`minecraft:smelting`...) that
    /// takes `item`.
    fn cooking(&self, kind: &str, item: i32) -> Option<Cooking>;
    /// What a 3×3 crafting grid makes now, with the recipe's name.
    fn craft(&self, grid: &[Option<ItemStack>]) -> Option<(ItemStack, String)>;
    /// Furnace fuel burn times.
    fn fuels(&self) -> &furnace::Fuels;
    /// Brewing mixes and brewing fuels.
    fn mixes(&self) -> &brewing::Mixes;
    /// Uses and speed of a stack's own `brewing_fuel` component.
    fn brewing_fuel(&self, component: &Compound) -> (i32, f32);
}

/// One cooking recipe (`AbstractCookingRecipe`).
#[derive(Clone, Debug)]
pub struct Cooking {
    /// The recipe's registry name.
    pub name: String,
    pub result: ItemStack,
    /// Ticks at normal speed.
    pub time: i32,
}

/// One less of `stack`; `None` when it was the last.
fn less_one(stack: &ItemStack) -> Option<ItemStack> {
    (stack.count > 1).then(|| stack.with_count(stack.count - 1))
}
