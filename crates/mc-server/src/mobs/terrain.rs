//! The world as mobs see it, over the server's block tables.

use std::cell::RefCell;
use std::sync::OnceLock;

use mc_entity::Collide;
use mc_entity::nav::{Door, Graze, Liquid, Perch, Terrain};
use mc_protocol::Position;

use crate::context::ServerContext;

const WATER: u16 = 1;
const LAVA: u16 = 2;
const HAZARD: u16 = 4;
const CLIMB: u16 = 8;
const PLANT: u16 = 16;
const GRASS_BLOCK: u16 = 32;
const SOLID: u16 = 64;
/// A door opened by hand (not iron), and whether it is open.
const DOOR: u16 = 128;
const DOOR_OPEN: u16 = 256;
const IRON_DOOR: u16 = 512;
/// In `#enderman_holdable`.
const HOLDABLE: u16 = 1024;
const CHEST: u16 = 2048;
const LIT_FURNACE: u16 = 4096;
const BED: u16 = 8192;
/// A full cube that suffocates (not glass, not leaves).
const SMOTHER: u16 = 16384;

/// What each block state is to a mob, worked out once from the block tables.
fn flags(ctx: &ServerContext) -> &'static [u16] {
    static TABLE: OnceLock<Vec<u16>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let blocks = &ctx.data.blocks;
        let holdable = ctx.commands.env.tag("minecraft:block", "minecraft:enderman_holdable");
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
                if name.ends_with("_door") {
                    f |= if name == "iron_door" { IRON_DOOR } else { DOOR };
                    if blocks.property(state, "open") == Some("true") {
                        f |= DOOR_OPEN;
                    }
                }
                if holdable.iter().any(|h| h.trim_start_matches("minecraft:") == name) {
                    f |= HOLDABLE;
                }
                if name == "chest" || name == "trapped_chest" {
                    f |= CHEST;
                }
                if matches!(name, "furnace" | "blast_furnace" | "smoker")
                    && blocks.property(state, "lit") == Some("true")
                {
                    f |= LIT_FURNACE;
                }
                if name.ends_with("_bed") {
                    f |= BED;
                }
                use mc_tick::support::{BOTTOM_FULL, TOP_FULL};
                let full = ctx.shapes.support(state) & (TOP_FULL | BOTTOM_FULL) == TOP_FULL | BOTTOM_FULL;
                if f & SOLID != 0
                    && full
                    && !name.contains("glass")
                    && !name.ends_with("_leaves")
                    && !name.ends_with("_door")
                {
                    f |= SMOTHER;
                }
                f
            })
            .collect()
    })
}

/// Collision boxes of block states that are not full cubes, as the bottom
/// and top in sixteenths (a bottom slab `(0, 8)`, a fence `(0, 24)`);
/// `(0, 0)` for none. After the block shapes on minecraft.wiki.
fn spans(ctx: &ServerContext) -> &'static [(u8, u8)] {
    static TABLE: OnceLock<Vec<(u8, u8)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let blocks = &ctx.data.blocks;
        (0..blocks.state_count())
            .map(|state| {
                if !ctx.rules.blocks_motion(state) {
                    return (0, 0);
                }
                let name = blocks.block_name(state).unwrap_or("");
                let name = name.trim_start_matches("minecraft:");
                let prop = |k| blocks.property(state, k);
                if name.ends_with("_slab") {
                    return match prop("type") {
                        Some("bottom") => (0, 8),
                        Some("top") => (8, 16),
                        _ => (0, 16),
                    };
                }
                if name.ends_with("_fence") || name.ends_with("_wall") {
                    return (0, 24);
                }
                if name.ends_with("_fence_gate") {
                    return if prop("open") == Some("true") { (0, 0) } else { (0, 24) };
                }
                if name.ends_with("_trapdoor") {
                    return match (prop("open"), prop("half")) {
                        (Some("true"), _) => (0, 0),
                        (_, Some("top")) => (13, 16),
                        _ => (0, 3),
                    };
                }
                // Mobs pass through open doors (vanilla's paths do).
                if name.ends_with("_door") && prop("open") == Some("true") {
                    return (0, 0);
                }
                if name.ends_with("carpet") {
                    return (0, 1);
                }
                if name.ends_with("_bed") {
                    return (0, 9);
                }
                match name {
                    "snow" => {
                        let layers: u8 = prop("layers").and_then(|l| l.parse().ok()).unwrap_or(1);
                        (0, (layers - 1) * 2)
                    }
                    "soul_sand" | "mud" | "chest" | "trapped_chest" | "ender_chest" => (0, 14),
                    "farmland" | "dirt_path" | "honey_block" => (0, 15),
                    "enchanting_table" => (0, 12),
                    "end_portal_frame" => (0, 13),
                    "stonecutter" => (0, 9),
                    "daylight_detector" => (0, 6),
                    "cake" => (0, 8),
                    "lily_pad" => (0, 1),
                    "repeater" | "comparator" => (0, 2),
                    _ => (0, 16),
                }
            })
            .collect()
    })
}

/// A collision box's bottom and top, in sixteenths.
type Span = (u8, u8);

/// The loaded world, with the blocks looked at so far remembered for the
/// tick (paths ask about the same cells again and again).
pub struct Ground<'a> {
    ctx: &'a ServerContext,
    flags: &'static [u16],
    spans: &'static [(u8, u8)],
    cache: RefCell<mc_tick::fast::FastMap<(i32, i32, i32), u16>>,
    span_cache: RefCell<mc_tick::fast::FastMap<(i32, i32, i32), Span>>,
}

impl<'a> Ground<'a> {
    pub fn new(ctx: &'a ServerContext) -> Self {
        Self {
            ctx,
            flags: flags(ctx),
            spans: spans(ctx),
            cache: RefCell::default(),
            span_cache: RefCell::default(),
        }
    }

    fn at(&self, x: i32, y: i32, z: i32) -> u16 {
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

    fn span(&self, x: i32, y: i32, z: i32) -> Option<(f64, f64)> {
        if !self.solid(x, y, z) {
            return None;
        }
        let cached = self.span_cache.borrow().get(&(x, y, z)).copied();
        let (lo, hi) = match cached {
            Some(s) => s,
            None => {
                let s = self
                    .ctx
                    .world
                    .block(Position::new(x, y, z), &self.ctx.data.blocks)
                    .and_then(|state| self.spans.get(state as usize).copied())
                    // Unloaded: a wall.
                    .unwrap_or((0, 16));
                self.span_cache.borrow_mut().insert((x, y, z), s);
                s
            }
        };
        (hi > lo).then(|| (f64::from(lo) / 16.0, f64::from(hi) / 16.0))
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

    fn door(&self, x: i32, y: i32, z: i32) -> Option<Door> {
        let f = self.at(x, y, z);
        (f & (DOOR | IRON_DOOR) != 0).then_some(Door {
            by_hand: f & DOOR != 0,
            open: f & DOOR_OPEN != 0,
        })
    }

    fn smothers(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & SMOTHER != 0
    }

    fn holdable(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & HOLDABLE != 0
    }

    fn perch(&self, x: i32, y: i32, z: i32) -> Perch {
        let f = self.at(x, y, z);
        if f & CHEST != 0 {
            Perch::Chest
        } else if f & LIT_FURNACE != 0 {
            Perch::Furnace
        } else if f & BED != 0 {
            Perch::Bed
        } else {
            Perch::None
        }
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
