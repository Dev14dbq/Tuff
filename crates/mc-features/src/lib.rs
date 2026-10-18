//! World generation features and structures from the data pack: the step
//! after terrain where trees, plants, ores, lakes and springs are put into
//! a chunk (minecraft.wiki, "Feature", "Placed feature", "Configured
//! feature").
//!
//! A chunk is decorated in a [`Region`] of its 3×3 chunk neighbourhood, so
//! a tree near the edge reaches into the next chunk. Features run step by
//! step in one order for the whole world: the biomes' lists merged (each
//! biome's order is kept), and each feature's random source is seeded from
//! the chunk and the feature's place in that order.

pub mod blocks;
pub mod feature;
mod misc;
pub mod placement;
pub mod predicate;
pub mod rand;
pub mod registry;
pub mod state;
pub mod structure;
pub mod tree;
pub mod value;

pub use blocks::{Facts, Fluid};
pub use registry::Features;
pub use value::Heights;

use mc_tick::Dir;

/// A block position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct P {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl P {
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn add(self, o: P) -> P {
        P::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }

    pub fn offset(self, dx: i32, dy: i32, dz: i32) -> P {
        P::new(self.x + dx, self.y + dy, self.z + dz)
    }

    pub fn rel(self, d: Dir) -> P {
        let (dx, dy, dz) = d.offset();
        self.offset(dx, dy, dz)
    }

    pub fn rel_n(self, d: Dir, n: i32) -> P {
        let (dx, dy, dz) = d.offset();
        self.offset(dx * n, dy * n, dz * n)
    }

    pub fn above(self, n: i32) -> P {
        self.offset(0, n, 0)
    }

    pub fn below(self, n: i32) -> P {
        self.offset(0, -n, 0)
    }

    pub fn with_y(self, y: i32) -> P {
        P::new(self.x, y, self.z)
    }
}

/// The heightmaps of a chunk being generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Heightmap {
    WorldSurfaceWg,
    WorldSurface,
    OceanFloorWg,
    OceanFloor,
    MotionBlocking,
    MotionBlockingNoLeaves,
}

impl Heightmap {
    pub const ALL: [Heightmap; 6] = [
        Heightmap::WorldSurfaceWg,
        Heightmap::WorldSurface,
        Heightmap::OceanFloorWg,
        Heightmap::OceanFloor,
        Heightmap::MotionBlocking,
        Heightmap::MotionBlockingNoLeaves,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "WORLD_SURFACE_WG" => Heightmap::WorldSurfaceWg,
            "WORLD_SURFACE" => Heightmap::WorldSurface,
            "OCEAN_FLOOR_WG" => Heightmap::OceanFloorWg,
            "OCEAN_FLOOR" => Heightmap::OceanFloor,
            "MOTION_BLOCKING" => Heightmap::MotionBlocking,
            "MOTION_BLOCKING_NO_LEAVES" => Heightmap::MotionBlockingNoLeaves,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Heightmap::WorldSurfaceWg => "WORLD_SURFACE_WG",
            Heightmap::WorldSurface => "WORLD_SURFACE",
            Heightmap::OceanFloorWg => "OCEAN_FLOOR_WG",
            Heightmap::OceanFloor => "OCEAN_FLOOR",
            Heightmap::MotionBlocking => "MOTION_BLOCKING",
            Heightmap::MotionBlockingNoLeaves => "MOTION_BLOCKING_NO_LEAVES",
        }
    }

    /// Whether the heightmap stops at `state`.
    pub fn counts(self, facts: &Facts, state: u32) -> bool {
        match self {
            Heightmap::WorldSurfaceWg | Heightmap::WorldSurface => !facts.is_air(state),
            Heightmap::OceanFloorWg | Heightmap::OceanFloor => facts.blocks_motion(state),
            Heightmap::MotionBlocking => {
                facts.blocks_motion(state) || !facts.fluid(state).is_none()
            }
            Heightmap::MotionBlockingNoLeaves => {
                (facts.blocks_motion(state) || !facts.fluid(state).is_none())
                    && !facts.is_leaves(state)
            }
        }
    }
}

/// The blocks a feature sees and changes: the chunk being decorated and
/// its neighbours.
pub trait Region {
    /// Air outside the region.
    fn block(&self, p: P) -> u32;
    /// Whether the block was set (outside the writable area it is not).
    fn set(&mut self, p: P, state: u32) -> bool;
    /// The first free y above the heightmap's top block.
    fn height(&self, map: Heightmap, x: i32, z: i32) -> i32;
    /// The biome (id) at a block, as the game zooms biomes.
    fn biome(&self, p: P) -> u32;
    fn heights(&self) -> Heights;
    fn set_block_entity(&mut self, p: P, nbt: mc_nbt::Compound);
    /// Asks for a tick of the fluid or block at `p` once the chunk is done.
    fn schedule_tick(&mut self, p: P);
    fn seed(&self) -> i64;
}
