//! The world as mobs see it, over the server's block tables.

use std::cell::RefCell;
use std::sync::OnceLock;

use mc_entity::Collide;
use mc_entity::nav::{Door, Feature, Graze, Liquid, Perch, Terrain};
use mc_protocol::Position;

use crate::context::ServerContext;

const WATER: u32 = 1;
const LAVA: u32 = 2;
const HAZARD: u32 = 4;
const CLIMB: u32 = 8;
const PLANT: u32 = 16;
const GRASS_BLOCK: u32 = 32;
const SOLID: u32 = 64;
/// A door opened by hand (not iron), and whether it is open.
const DOOR: u32 = 128;
const DOOR_OPEN: u32 = 256;
const IRON_DOOR: u32 = 512;
/// In `#enderman_holdable`.
const HOLDABLE: u32 = 1024;
const CHEST: u32 = 2048;
const LIT_FURNACE: u32 = 4096;
const BED: u32 = 8192;
/// The head of a bed nobody lies in.
const BED_FREE: u32 = 32768;
/// A full cube that suffocates (not glass, not leaves).
const SMOTHER: u32 = 16384;
/// A block silverfish hide in.
const INFESTABLE: u32 = 65536;

/// What each block state is to a mob, worked out once from the block tables.
fn flags(ctx: &ServerContext) -> &'static [u32] {
    static TABLE: OnceLock<Vec<u32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let blocks = &ctx.data.blocks;
        let holdable = ctx
            .commands
            .env
            .tag("minecraft:block", "minecraft:enderman_holdable");
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
                if holdable
                    .iter()
                    .any(|h| h.trim_start_matches("minecraft:") == name)
                {
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
                    if blocks.property(state, "part") == Some("head")
                        && blocks.property(state, "occupied") == Some("false")
                    {
                        f |= BED_FREE;
                    }
                }
                if super::infest::is_host(name) {
                    f |= INFESTABLE;
                }
                use mc_tick::support::{BOTTOM_FULL, TOP_FULL};
                let full =
                    ctx.shapes.support(state) & (TOP_FULL | BOTTOM_FULL) == TOP_FULL | BOTTOM_FULL;
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
                    return if prop("open") == Some("true") {
                        (0, 0)
                    } else {
                        (0, 24)
                    };
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
    flags: &'static [u32],
    spans: &'static [(u8, u8)],
    cache: RefCell<mc_tick::fast::FastMap<(i32, i32, i32), u32>>,
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

    fn at(&self, x: i32, y: i32, z: i32) -> u32 {
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

    fn name_at(&self, x: i32, y: i32, z: i32) -> Option<&str> {
        if !self.loaded(x, z) {
            return None;
        }
        let state = self
            .ctx
            .world
            .block(Position::new(x, y, z), &self.ctx.data.blocks)?;
        self.ctx.data.blocks.block_name(state)
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

    fn free_bed(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & BED_FREE != 0
    }

    fn snow_golem_melts(&self, x: i32, y: i32, z: i32) -> bool {
        crate::environment::flag(
            self.ctx,
            Some([x, y, z]),
            crate::environment::SNOW_GOLEM_MELTS,
            false,
        )
    }

    fn infestable(&self, x: i32, y: i32, z: i32) -> bool {
        self.at(x, y, z) & INFESTABLE != 0
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

    fn feature(&self, x: i32, y: i32, z: i32, kind: Feature) -> bool {
        let Some(name) = self.name_at(x, y, z) else {
            return false;
        };
        match kind {
            Feature::PowderSnow => name == "minecraft:powder_snow",
            Feature::Bamboo => name == "minecraft:bamboo" || name == "minecraft:bamboo_sapling",
            Feature::Cake => name == "minecraft:cake",
            Feature::Moss => name == "minecraft:moss_block",
            Feature::HornSnap => block_in_tag(self.ctx, "minecraft:snaps_goat_horn", name),
            Feature::Diggable => block_in_tag(self.ctx, "minecraft:sniffer_diggable_block", name),
            Feature::Repellent => block_in_tag(self.ctx, "minecraft:hoglin_repellents", name),
            // Aquatic (Claude)
            Feature::Sand => block_in_tag(self.ctx, "minecraft:sand", name),
        }
    }
}

fn block_in_tag(ctx: &ServerContext, tag: &'static str, name: &str) -> bool {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<&'static str, HashSet<String>>>> = OnceLock::new();
    let mut cache = crate::tick::lock(CACHE.get_or_init(Default::default));
    let set = cache.entry(tag).or_insert_with(|| {
        ctx.commands
            .env
            .tag("minecraft:block", tag)
            .into_iter()
            .collect()
    });
    set.contains(name)
}
