//! The noise generator: a chunk's biomes, terrain shape with aquifers, and
//! materials (bedrock, ore veins, surface, deepslate), from the dimension's
//! noise settings (`worldgen/noise_settings/<name>.json`).

use std::collections::HashMap;

use serde_json::Value;

use crate::aquifer::{Aquifer, Fluid, Functions, Global};
use crate::climate::{BiomeList, Climate};
use crate::density::{Builder, Ctx, Df, Source};
use crate::materials::{Column, Materials, Names, zoom_seed, zoomed_cell};
use crate::random::{Positional, RandomSource};

/// Block states the generator places itself.
#[derive(Clone, Copy, Debug)]
pub struct Blocks {
    pub air: u32,
    pub stone: u32,
    pub water: u32,
    pub lava: u32,
}

pub struct NoiseGenerator {
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
    climate: Climate,
    biomes: BiomeList,
    final_density: Df,
    aquifers: Option<Functions>,
    aquifer_random: Positional,
    surface_level: Df,
    materials: Materials,
    blocks: Blocks,
    default_fluid: Fluid,
    zoom: i64,
    /// Biome name → id.
    biome_ids: HashMap<String, u32>,
}

impl std::fmt::Debug for NoiseGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseGenerator")
            .field("min_y", &self.min_y)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// A generated chunk: block states (y, then z, then x) and biome ids per
/// 4×4×4 cell (section by section, y, z, x).
#[derive(Debug)]
pub struct Generated {
    pub blocks: Vec<u32>,
    pub biomes: Vec<u32>,
}

impl NoiseGenerator {
    /// `settings` is the noise settings, `parameters` the biome parameter
    /// report, `biome_ids` the biome registry.
    pub fn new(
        source: &dyn Source,
        seed: i64,
        settings: &Value,
        parameters: &Value,
        names: &dyn Names,
        biome_ids: HashMap<String, u32>,
        blocks: Blocks,
    ) -> Result<Self, String> {
        let legacy = settings["legacy_random_source"].as_bool().unwrap_or(false);
        let mut builder = Builder::new(source, seed, legacy);
        let router = &settings["noise_router"];
        let climate = Climate::from_router(&mut builder, router)?;
        let final_density = builder.build(&router["final_density"])?;
        let aq = &settings["aquifers"];
        let aquifers = if aq.is_object() {
            Some(Functions {
                barrier: builder.build(&aq["barrier"])?,
                floodedness: builder.build(&aq["fluid_level_floodedness"])?,
                spread: builder.build(&aq["fluid_level_spread"])?,
                lava: builder.build(&aq["lava"])?,
                exclusion: builder.build(&aq["exclusion"])?,
                surface: builder.build(&aq["surface_level"])?,
            })
        } else {
            None
        };
        let surface_level = builder.build(&router["chunk_surface_level"])?;
        let aquifer_random = builder
            .factory()
            .from_hash_of("minecraft:aquifer")
            .fork_positional();
        let rule = settings["material_rule"]
            .as_str()
            .unwrap_or("minecraft:overworld");
        let materials = Materials::load(&mut builder, source, rule, names)?;
        let noise = &settings["noise"];
        let default_fluid = match settings["default_fluid"].as_str() {
            Some("minecraft:lava") => Fluid::Lava,
            _ => Fluid::Water,
        };
        Ok(Self {
            min_y: noise["min_y"].as_i64().unwrap_or(-64) as i32,
            height: noise["height"].as_i64().unwrap_or(384) as i32,
            sea_level: settings["sea_level"].as_i64().unwrap_or(63) as i32,
            climate,
            biomes: BiomeList::from_json(parameters),
            final_density,
            aquifers,
            aquifer_random,
            surface_level,
            materials,
            blocks,
            default_fluid,
            zoom: zoom_seed(seed),
            biome_ids,
        })
    }

    /// The biome (id) of a 4×4×4 cell.
    pub fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u32 {
        let name = self.biomes.find(self.climate.sample(qx, qy, qz));
        self.biome_ids.get(name).copied().unwrap_or(0)
    }

    pub fn generate(&self, cx: i32, cz: i32) -> Generated {
        let (x0, z0) = (cx * 16, cz * 16);
        let (min_y, height) = (self.min_y, self.height);
        let sections = (height / 16) as usize;
        // Biomes.
        let mut biomes = Vec::with_capacity(sections * 64);
        let mut cell_biomes: HashMap<(i32, i32, i32), u32> = HashMap::new();
        for s in 0..sections as i32 {
            for qy in 0..4 {
                for qz in 0..4 {
                    for qx in 0..4 {
                        let q = (cx * 4 + qx, min_y / 4 + s * 4 + qy, cz * 4 + qz);
                        let b = self.noise_biome(q.0, q.1, q.2);
                        cell_biomes.insert(q, b);
                        biomes.push(b);
                    }
                }
            }
        }
        // Shape, water and lava.
        let mut ctx = Ctx::new(x0, z0, min_y, height);
        let global = Global {
            sea_level: self.sea_level,
            sea: self.default_fluid,
            lava_level: -54,
        };
        let mut aquifer = self
            .aquifers
            .as_ref()
            .map(|f| Aquifer::new(f, global, self.aquifer_random, cx, cz, min_y, height));
        let n = (height * 256) as usize;
        let mut blocks = vec![self.blocks.air; n];
        let at = |y: i32, z: i32, x: i32| (((y - min_y) * 16 + z) * 16 + x) as usize;
        for y in min_y..min_y + height {
            for lz in 0..16 {
                for lx in 0..16 {
                    let (x, z) = (x0 + lx, z0 + lz);
                    let density = self.final_density.eval_in(x, y, z, &mut ctx);
                    let fluid = match &mut aquifer {
                        Some(a) => a.substance(x, y, z, density, &mut ctx),
                        None if density > 0.0 => None,
                        None => Some(global.at(y).at(y)),
                    };
                    blocks[at(y, lz, lx)] = match fluid {
                        None => self.blocks.stone,
                        Some(Fluid::Air) => self.blocks.air,
                        Some(Fluid::Water) => self.blocks.water,
                        Some(Fluid::Lava) => self.blocks.lava,
                    };
                }
            }
        }
        // Heights before materials (WORLD_SURFACE_WG: above the top
        // non-air block).
        let mut heights = [min_y; 256];
        for lz in 0..16 {
            for lx in 0..16 {
                for y in (min_y..min_y + height).rev() {
                    if blocks[at(y, lz, lx)] != self.blocks.air {
                        heights[(lz * 16 + lx) as usize] = y + 1;
                        break;
                    }
                }
            }
        }
        // Materials, column by column from the top.
        let is_stone =
            |s: u32| s != self.blocks.air && s != self.blocks.water && s != self.blocks.lava;
        let mut zoom_cache: HashMap<(i32, i32, i32), u32> = HashMap::new();
        for lz in 0..16 {
            for lx in 0..16 {
                let (x, z) = (x0 + lx, z0 + lz);
                let surface_depth = self.materials.surface_depth(x, z);
                let secondary = self.materials.secondary(x, z);
                let min_surface = self.surface_level.eval_in(x, 0, z, &mut ctx).floor() as i32
                    + surface_depth
                    - 8;
                let top = heights[(lz * 16 + lx) as usize];
                let (mut stone_above, mut water_height, mut below_y) = (0, i32::MIN, i32::MAX);
                for y in (min_y..top.min(min_y + height)).rev() {
                    let state = blocks[at(y, lz, lx)];
                    if state == self.blocks.air {
                        stone_above = 0;
                        water_height = i32::MIN;
                        continue;
                    }
                    if !is_stone(state) {
                        if water_height == i32::MIN {
                            water_height = y + 1;
                        }
                        continue;
                    }
                    if below_y >= y {
                        below_y = i32::MIN;
                        let mut y2 = y - 1;
                        while y2 >= min_y - 1 {
                            if y2 < min_y || !is_stone(blocks[at(y2, lz, lx)]) {
                                below_y = y2 + 1;
                                break;
                            }
                            y2 -= 1;
                        }
                    }
                    stone_above += 1;
                    let stone_below = y - below_y + 1;
                    if state != self.blocks.stone {
                        continue;
                    }
                    let cell = zoomed_cell(self.zoom, x, y, z);
                    let biome = *zoom_cache.entry(cell).or_insert_with(|| {
                        cell_biomes
                            .get(&cell)
                            .copied()
                            .unwrap_or_else(|| self.noise_biome(cell.0, cell.1, cell.2))
                    });
                    let column = Column {
                        x,
                        y,
                        z,
                        min_y,
                        height,
                        stone_above,
                        stone_below,
                        water_height,
                        surface_depth,
                        secondary,
                        min_surface,
                        biome,
                        heights: &heights,
                    };
                    if let Some(new) = self.materials.apply(&column, &mut ctx) {
                        blocks[at(y, lz, lx)] = new;
                    }
                }
            }
        }
        Generated { blocks, biomes }
    }
}
