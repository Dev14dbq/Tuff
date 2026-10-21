//! The world as mobs see it, over the server's block tables.

use std::cell::RefCell;
use std::sync::OnceLock;

use mc_entity::Collide;
use mc_entity::nav::{Graze, Liquid, Terrain};
use mc_protocol::Position;

use crate::context::ServerContext;

const WATER: u8 = 1;
const LAVA: u8 = 2;
const HAZARD: u8 = 4;
const CLIMB: u8 = 8;
const PLANT: u8 = 16;
const GRASS_BLOCK: u8 = 32;
const SOLID: u8 = 64;

/// What each block state is to a mob, worked out once from the block tables.
fn flags(ctx: &ServerContext) -> &'static [u8] {
    static TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let blocks = &ctx.data.blocks;
        (0..blocks.state_count())
            .map(|state| {
                let name = blocks.block_name(state).unwrap_or("");
                let name = name.trim_start_matches("minecraft:");
                let mut f = 0;
                if ctx.rules.blocks_motion(state) {
                    f |= SOLID;
                }
                if name == "water" || blocks.property(state, "waterlogged") == Some("true") {
                    f |= WATER;
                }
                if name == "lava" {
                    f |= LAVA;
                }
                if matches!(
                    name,
                    "fire"
                        | "soul_fire"
                        | "cactus"
                        | "magma_block"
                        | "sweet_berry_bush"
                        | "wither_rose"
                        | "powder_snow"
                ) || name.ends_with("campfire") && blocks.property(state, "lit") == Some("true")
                {
                    f |= HAZARD;
                }
                if matches!(
                    name,
                    "ladder" | "vine" | "scaffolding" | "twisting_vines" | "weeping_vines"
                ) {
                    f |= CLIMB;
                }
                if name == "short_grass" {
                    f |= PLANT;
                }
                if name == "grass_block" {
                    f |= GRASS_BLOCK;
                }
                f
            })
            .collect()
    })
}

/// The loaded world, with the blocks looked at so far remembered for the
/// tick (paths ask about the same cells again and again).
pub struct Ground<'a> {
    ctx: &'a ServerContext,
    flags: &'static [u8],
    cache: RefCell<mc_tick::fast::FastMap<(i32, i32, i32), u8>>,
}

impl<'a> Ground<'a> {
    pub fn new(ctx: &'a ServerContext) -> Self {
        Self {
            ctx,
            flags: flags(ctx),
            cache: RefCell::default(),
        }
    }

    fn at(&self, x: i32, y: i32, z: i32) -> u8 {
        if let Some(&f) = self.cache.borrow().get(&(x, y, z)) {
            return f;
        }
        // Unloaded: a wall, so nothing walks out of the world (and mobs do
        // not make the world generate).
        let f = if self.loaded(x, z) {
            match self
                .ctx
                .world
                .block(Position::new(x, y, z), &self.ctx.data.blocks)
            {
                Some(state) => self.flags.get(state as usize).copied().unwrap_or(0),
                None => SOLID,
            }
        } else {
            SOLID
        };
        self.cache.borrow_mut().insert((x, y, z), f);
        f
    }

    /// The chunk of the cell is loaded.
    pub fn loaded(&self, x: i32, z: i32) -> bool {
        self.ctx.world.chunk_loaded(x >> 4, z >> 4)
    }
}

impl Collide for Ground<'_> {
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & SOLID != 0
    }
}

impl Terrain for Ground<'_> {
    fn liquid(&self, x: i32, y: i32, z: i32) -> Liquid {
        let f = self.at(x, y, z);
        if f & LAVA != 0 {
            Liquid::Lava
        } else if f & WATER != 0 {
            Liquid::Water
        } else {
            Liquid::None
        }
    }

    fn hazard(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & HAZARD != 0
    }

    fn sky_light(&self, x: i32, y: i32, z: i32) -> u8 {
        self.ctx.world.sky_light(Position::new(x, y, z))
    }

    fn light(&self, x: i32, y: i32, z: i32) -> u8 {
        self.ctx.world.brightness(Position::new(x, y, z))
    }

    fn climbable(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & CLIMB != 0
    }

    fn grazing(&self, x: i32, y: i32, z: i32) -> Graze {
        if self.at(x, y, z) & PLANT != 0 {
            Graze::Plant
        } else if self.at(x, y - 1, z) & GRASS_BLOCK != 0 {
            Graze::Block
        } else {
            Graze::None
        }
    }
}
