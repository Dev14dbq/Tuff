//! The noise generator: a chunk's biomes, terrain shape with aquifers, and
//! materials (bedrock, ore veins, surface, deepslate), from the dimension's
//! noise settings (`worldgen/noise_settings/<name>.json`).

use std::collections::HashMap;

use serde_json::Value;

use crate::aquifer::{Aquifer, Fluid, Functions, Global};
use crate::carver::{Carver, Target, Trig};
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

/// Where a dimension's biomes come from (the generator's `biome_source`).
#[derive(Clone, Debug)]
pub enum BiomeSource {
    /// `multi_noise`: the climate's nearest biome in the parameter list
    /// (`reports/biome_parameters/<preset>.json`).
    MultiNoise(Value),
    /// `the_end`: the central island, then highlands, midlands, barrens or
    /// small islands by the island density around each section.
    TheEnd,
}

/// The three dimensions of a normal world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dimension {
    Overworld,
    Nether,
    End,
}

impl Dimension {
    /// The dimension's id (`minecraft:the_nether`).
    pub fn id(self) -> &'static str {
        match self {
            Dimension::Overworld => "minecraft:overworld",
            Dimension::Nether => "minecraft:the_nether",
            Dimension::End => "minecraft:the_end",
        }
    }

    /// Its noise settings (`worldgen/noise_settings`).
    pub fn settings(self) -> &'static str {
        match self {
            Dimension::Overworld => "minecraft:overworld",
            Dimension::Nether => "minecraft:nether",
            Dimension::End => "minecraft:end",
        }
    }

    /// The multi-noise preset whose parameter list it uses, if any.
    pub fn preset(self) -> Option<&'static str> {
        match self {
            Dimension::Overworld => Some("overworld"),
            Dimension::Nether => Some("nether"),
            Dimension::End => None,
        }
    }

    /// `overworld`, `nether`, `end` (also `the_nether`, `the_end` and
    /// full ids).
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim_start_matches("minecraft:") {
            "overworld" => Some(Dimension::Overworld),
            "nether" | "the_nether" => Some(Dimension::Nether),
            "end" | "the_end" => Some(Dimension::End),
            _ => None,
        }
    }
}

enum Biomes {
    List(BiomeList),
    End,
}

pub struct NoiseGenerator {
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
    climate: Climate,
    biomes: Biomes,
    final_density: Df,
    aquifers: Option<Functions>,
    aquifer_random: Positional,
    surface_level: Df,
    materials: Materials,
    blocks: Blocks,
    /// Carvers cutting after the materials leave it.
    bedrock: Option<u32>,
    default_fluid: Fluid,
    zoom: i64,
    /// Biome name → id.
    biome_ids: HashMap<String, u32>,
    seed: i64,
    /// The carvers of each biome (id).
    carvers: HashMap<u32, Vec<Carver>>,
    trig: Trig,
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
    /// `settings` is the noise settings, `biome_ids` the biome registry.
    /// `blocks.stone` is replaced by the settings' default block.
    pub fn new(
        source: &dyn Source,
        seed: i64,
        settings: &Value,
        biome_source: &BiomeSource,
        names: &dyn Names,
        biome_ids: HashMap<String, u32>,
        mut blocks: Blocks,
    ) -> Result<Self, String> {
        if let Some(state) = names.state(&settings["default_block"]) {
            blocks.stone = state;
        }
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
            Some("minecraft:air") => Fluid::Air,
            _ => Fluid::Water,
        };
        let (min_y, height) = (
            noise["min_y"].as_i64().unwrap_or(-64) as i32,
            noise["height"].as_i64().unwrap_or(384) as i32,
        );
        let mut carvers = HashMap::new();
        for (name, &id) in &biome_ids {
            let Some(biome) = source.read("biome", name) else {
                continue;
            };
            let ids: Vec<&str> = match &biome["carvers"] {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            let list: Vec<Carver> = ids
                .iter()
                .filter_map(|c| source.read("carver", c))
                .filter_map(|v| Carver::parse(&v, min_y, height))
                .collect();
            carvers.insert(id, list);
        }
        Ok(Self {
            min_y,
            height,
            sea_level: settings["sea_level"].as_i64().unwrap_or(63) as i32,
            climate,
            biomes: match biome_source {
                BiomeSource::MultiNoise(parameters) => {
                    Biomes::List(BiomeList::from_json(parameters))
                }
                BiomeSource::TheEnd => Biomes::End,
            },
            final_density,
            aquifers,
            aquifer_random,
            surface_level,
            materials,
            blocks,
            bedrock: names.state(&Value::String("minecraft:bedrock".into())),
            default_fluid,
            zoom: zoom_seed(seed),
            biome_ids,
            seed,
            carvers,
            trig: Trig::default(),
        })
    }

    /// Leaves out the carvers (to compare with a world made without them).
    pub fn clear_carvers(&mut self) {
        self.carvers.clear();
    }

    /// The biome (id) of a 4×4×4 cell.
    pub fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> u32 {
        let name = match &self.biomes {
            Biomes::List(list) => list.find(self.climate.sample(qx, qy, qz)),
            Biomes::End => self.end_biome(qx, qy, qz),
        };
        self.biome_ids.get(name).copied().unwrap_or(0)
    }

    /// [`NoiseGenerator::noise_biome`] with a chunk's caches.
    fn noise_biome_in(&self, qx: i32, qy: i32, qz: i32, ctx: &mut Ctx) -> u32 {
        let name = match &self.biomes {
            Biomes::List(list) => list.find(self.climate.sample_in(qx, qy, qz, ctx)),
            Biomes::End => self.end_biome(qx, qy, qz),
        };
        self.biome_ids.get(name).copied().unwrap_or(0)
    }

    /// The End's biome: the island density (the router's erosion) at the
    /// middle of the cell's section, the central island within 64 sections.
    fn end_biome(&self, qx: i32, qy: i32, qz: i32) -> &'static str {
        let (sx, sz) = ((qx << 2) >> 4, (qz << 2) >> 4);
        if i64::from(sx) * i64::from(sx) + i64::from(sz) * i64::from(sz) <= 4096 {
            return "minecraft:the_end";
        }
        let h = self
            .climate
            .erosion
            .eval((sx * 2 + 1) * 8, qy << 2, (sz * 2 + 1) * 8);
        if h > 0.25 {
            "minecraft:end_highlands"
        } else if h >= -0.0625 {
            "minecraft:end_midlands"
        } else if h < -0.21875 {
            "minecraft:small_end_islands"
        } else {
            "minecraft:end_barrens"
        }
    }

    pub fn generate(&self, cx: i32, cz: i32) -> Generated {
        self.generate_with(cx, cz, &|_, _, _| 0.0)
    }

    /// Heights of columns from the density alone (no chunk generated):
    /// what structures stand on before the terrain exists.
    pub fn columns(&self) -> ColumnSampler<'_> {
        ColumnSampler {
            g: self,
            ctxs: HashMap::new(),
        }
    }

    /// A chunk with `beard` added to the terrain density (the
    /// `beardifier`: terrain made to fit structures).
    pub fn generate_with(
        &self,
        cx: i32,
        cz: i32,
        beard: &dyn Fn(i32, i32, i32) -> f64,
    ) -> Generated {
        let (x0, z0) = (cx * 16, cz * 16);
        let (min_y, height) = (self.min_y, self.height);
        let sections = (height / 16) as usize;
        // Biomes.
        let mut ctx = Ctx::new(x0, z0, min_y, height);
        let mut biomes = Vec::with_capacity(sections * 64);
        let mut cell_biomes: HashMap<(i32, i32, i32), u32> = HashMap::new();
        for s in 0..sections as i32 {
            for qy in 0..4 {
                for qz in 0..4 {
                    for qx in 0..4 {
                        let q = (cx * 4 + qx, min_y / 4 + s * 4 + qy, cz * 4 + qz);
                        let b = self.noise_biome_in(q.0, q.1, q.2, &mut ctx);
                        cell_biomes.insert(q, b);
                        biomes.push(b);
                    }
                }
            }
        }
        // Shape, water and lava.
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
                    let density = self.final_density.eval_in(x, y, z, &mut ctx) + beard(x, y, z);
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
        // Carvers cut the raw terrain; the aquifer fills what they cut.
        // Without aquifers (the Nether) they cut after the materials, as
        // carvers always did: floors stay bare, bedrock stays.
        let late = self.aquifers.is_none();
        if !late {
            self.carve(cx, cz, &mut blocks, &mut aquifer, &mut ctx, global, false);
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
                        cell_biomes.get(&cell).copied().unwrap_or_else(|| {
                            self.noise_biome_in(cell.0, cell.1, cell.2, &mut ctx)
                        })
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
        if late {
            self.carve(cx, cz, &mut blocks, &mut aquifer, &mut ctx, global, true);
        }
        Generated { blocks, biomes }
    }

    /// Runs the carvers of the chunk's biome. Before the materials they cut
    /// the default block only; after them anything solid but bedrock.
    #[allow(clippy::too_many_arguments)]
    fn carve(
        &self,
        cx: i32,
        cz: i32,
        blocks: &mut [u32],
        aquifer: &mut Option<Aquifer>,
        ctx: &mut Ctx,
        global: Global,
        after_materials: bool,
    ) {
        let (x0, z0) = (cx * 16, cz * 16);
        let (min_y, height) = (self.min_y, self.height);
        let at = |y: i32, z: i32, x: i32| (((y - min_y) * 16 + z) * 16 + x) as usize;
        let biome = self.noise_biome((cx - 8) * 4, 0, (cz - 8) * 4);
        let Some(list) = self.carvers.get(&biome).filter(|l| !l.is_empty()) else {
            return;
        };
        let fill = self.blocks;
        let carvable = |s: u32| {
            if after_materials {
                s != fill.air && s != fill.water && s != fill.lava && Some(s) != self.bedrock
            } else {
                s == fill.stone
            }
        };
        let mut carve = |x: i32, y: i32, z: i32| {
            let i = at(y, z - z0, x - x0);
            if !carvable(blocks[i]) {
                return false;
            }
            let fluid = match aquifer {
                Some(a) => a.substance(x, y, z, 0.0, ctx),
                None => Some(global.at(y).at(y)),
            };
            let Some(fluid) = fluid else {
                return false;
            };
            blocks[i] = match fluid {
                Fluid::Air => fill.air,
                Fluid::Water => fill.water,
                Fluid::Lava => fill.lava,
            };
            true
        };
        let mut target = Target {
            cx,
            cz,
            min_y,
            height,
            trig: &self.trig,
            carve: &mut carve,
            mask: vec![false; (height * 256) as usize],
        };
        crate::carver::carve(self.seed, &mut target, &|_, _| list.clone());
    }
}

/// Column heights from the density, one evaluation context per chunk.
pub struct ColumnSampler<'a> {
    g: &'a NoiseGenerator,
    ctxs: HashMap<(i32, i32), Ctx>,
}

impl std::fmt::Debug for ColumnSampler<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColumnSampler").finish_non_exhaustive()
    }
}

impl ColumnSampler<'_> {
    /// The first free y above the column's top block: solid ground, or the
    /// sea for `WORLD_SURFACE_WG` (`ocean_floor` false).
    pub fn height(&mut self, x: i32, z: i32, ocean_floor: bool) -> i32 {
        let g = self.g;
        let key = (x >> 4, z >> 4);
        let ctx = self
            .ctxs
            .entry(key)
            .or_insert_with(|| Ctx::new(key.0 * 16, key.1 * 16, g.min_y, g.height));
        let mut y = g.min_y + g.height - 1;
        while y >= g.min_y {
            if g.final_density.eval_in(x, y, z, ctx) > 0.0 || (!ocean_floor && y < g.sea_level) {
                return y + 1;
            }
            y -= 1;
        }
        g.min_y
    }

    /// Whether the bare terrain has something at a block: stone, or for
    /// `ocean_floor` false also the sea (what is not air).
    /// With aquifers (whose caves this does not follow) it is the column's
    /// height.
    pub fn occupied(&mut self, x: i32, y: i32, z: i32, ocean_floor: bool) -> bool {
        let g = self.g;
        if g.aquifers.is_some() {
            return y < self.height(x, z, ocean_floor);
        }
        let key = (x >> 4, z >> 4);
        let ctx = self
            .ctxs
            .entry(key)
            .or_insert_with(|| Ctx::new(key.0 * 16, key.1 * 16, g.min_y, g.height));
        if g.final_density.eval_in(x, y, z, ctx) > 0.0 {
            return true;
        }
        let global = Global {
            sea_level: g.sea_level,
            sea: g.default_fluid,
            lava_level: -54,
        };
        !ocean_floor && global.at(y).at(y) != Fluid::Air
    }

    /// The biome (id) of a 4×4×4 cell.
    pub fn biome(&self, qx: i32, qy: i32, qz: i32) -> u32 {
        self.g.noise_biome(qx, qy, qz)
    }
}
