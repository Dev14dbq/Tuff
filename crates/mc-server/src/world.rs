//! The overworld players are in: a superflat generator, or a world folder saved
//! by vanilla (Anvil region files). One dimension for now.
//!
//! Chunks are loaded on first use and kept in memory with their packet,
//! framed and compressed once for every player; a change to a chunk drops the
//! frame, the next request encodes it again. Each chunk counts the players who have it; chunks nobody has and
//! that have no unsaved changes are dropped by [`World::unload_unused`].

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use mc_data::{Blocks, McData};
use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::{ChunkBlockEntity, LevelChunkWithLight, LightData, heightmap};
use mc_storage::{Decoded, Names, Palettes, RegionStorage, decode_chunk, encode_chunk};
use mc_world::Chunk;
use mc_worldgen::{FlatGenerator, FlatLayer};
use tracing::warn;

use crate::blocks::BlockRules;
use crate::tick::{Clock, WorldTime};

type SyncedIds = HashMap<String, HashMap<String, i32>>;

#[derive(Debug)]
pub struct World {
    pub dimension: String,
    pub dimension_type_id: i32,
    pub min_y: i32,
    pub height: u32,
    pub sea_level: i32,
    pub spawn: (f64, f64, f64),
    pub spawn_yaw: f32,
    pub is_flat: bool,
    /// Shown to players on join.
    pub description: String,
    source: Source,
    chunks: Mutex<HashMap<(i32, i32), Slot, mc_worldgen::density::Fast>>,
    /// Sent where a world folder has no finished chunk: air with full daylight.
    empty: Chunk,
    biome_count: u32,
    /// Biome network id → name, for saving.
    biome_names: Vec<String>,
    data_version: i32,
    block_entity_types: HashMap<String, i32>,
    /// The world folder (`None` for the flat test world).
    folder: Option<PathBuf>,
    /// Opacity and emission of block states.
    light: mc_light::Tables,
    /// Fluid ticks generation left for chunks players already have.
    live_ticks: Mutex<Vec<(mc_tick::BlockPos, u32)>>,
    /// Loaded chunks whose light a new neighbour changed.
    relit: Mutex<Vec<(i32, i32)>>,
    /// New chunks are lit with their neighbours outside the chunks lock;
    /// overlapping 3×3 areas take turns.
    lighting: crate::util::Claims,
    /// `/save-off`.
    no_autosave: std::sync::atomic::AtomicBool,
}

/// What one save did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Saved {
    pub chunks: usize,
    pub failed: usize,
}

#[derive(Debug)]
enum Source {
    /// Every flat chunk is a copy of this one at other coordinates.
    Flat(Box<Chunk>),
    Anvil(Box<Anvil>),
}

#[derive(Debug)]
struct Anvil {
    storage: Mutex<RegionStorage>,
    /// Makes the chunks the folder does not have.
    generation: Option<Generation>,
    biome_ids: HashMap<String, u32>,
    fallback_biome: u32,
    air: u32,
    unresolved_reported: AtomicU32,
}

/// The world's generator, with what turning its output into chunks needs.
struct Generation {
    pipeline: Arc<crate::pipeline::Pipeline>,
    /// Per state: which heightmaps count it (a bit per [`HEIGHTMAPS`] entry).
    counts: Vec<u8>,
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generation").finish_non_exhaustive()
    }
}

/// A chunk in memory.
#[derive(Debug)]
struct Slot {
    /// `None`: the world has no finished chunk here. It is sent empty and
    /// cannot be changed (that would write a chunk vanilla never generated).
    chunk: Option<Box<Chunk>>,
    /// The Chunk Data packet as it goes on the wire (id, compression).
    frame: Option<Arc<[u8]>>,
    /// Counts changes, so a packet made from a copy is kept only if the
    /// chunk did not change meanwhile.
    changes: u64,
    /// Changed since it was loaded or saved.
    dirty: bool,
    /// Players whose client has this chunk.
    viewers: u32,
}

/// The stored heightmaps and the blocks each one counts.
pub const HEIGHTMAPS: [&str; 4] = [
    "WORLD_SURFACE",
    "MOTION_BLOCKING",
    "MOTION_BLOCKING_NO_LEAVES",
    "OCEAN_FLOOR",
];

fn heightmap_counts(name: &str, state: u32, blocks: &Blocks, rules: &BlockRules) -> bool {
    match name {
        "WORLD_SURFACE" => !blocks.is_air(state),
        "MOTION_BLOCKING" => rules.blocks_motion(state) || blocks.is_fluid(state),
        "MOTION_BLOCKING_NO_LEAVES" => {
            (rules.blocks_motion(state) || blocks.is_fluid(state)) && !rules.is_leaves(state)
        }
        _ => rules.blocks_motion(state),
    }
}

struct Dimension {
    name: String,
    type_id: i32,
    min_y: i32,
    height: u32,
}

fn overworld(data: &McData, synced: &SyncedIds) -> Result<Dimension> {
    let name = "minecraft:overworld".to_owned();
    let json = data
        .data_pack()
        .read_entry("minecraft:dimension_type", &name)?;
    Ok(Dimension {
        type_id: synced["minecraft:dimension_type"][&name],
        min_y: json["min_y"].as_i64().context("dimension type min_y")? as i32,
        height: json["height"].as_u64().context("dimension type height")? as u32,
        name,
    })
}

fn state(data: &McData, name: &str) -> Result<u32> {
    data.blocks
        .default_state(name)
        .with_context(|| format!("block {name} not in the blocks report"))
}

/// Full daylight everywhere, no block light. Light covers one section below and
/// one above the world in addition to the world's own sections.
fn full_sky_light(height: u32) -> LightData {
    let sections = height as usize / 16 + 2;
    let all = vec![(1u64 << sections) - 1];
    LightData {
        sky_mask: all.clone(),
        block_mask: Vec::new(),
        empty_sky_mask: Vec::new(),
        empty_block_mask: all,
        sky: vec![vec![0xFF; 2048]; sections],
        block: Vec::new(),
    }
}

/// Index of a biome cell in its section: `x` and `z` in blocks, `local`
/// the block's height above the chunk's bottom.
fn biome_index(x: i32, local: usize, z: i32) -> usize {
    (((local % 16) >> 2) << 4) | ((((z & 15) >> 2) as usize) << 2) | ((x & 15) >> 2) as usize
}

fn biome_names(synced: &SyncedIds) -> Vec<String> {
    let ids = &synced["minecraft:worldgen/biome"];
    let mut names = vec![String::new(); ids.len()];
    for (name, &id) in ids {
        names[id as usize].clone_from(name);
    }
    names
}

fn block_entity_types(data: &McData) -> Result<HashMap<String, i32>> {
    Ok(data
        .builtin
        .get("minecraft:block_entity_type")
        .context("block entity types missing from the registries report")?
        .clone())
}

impl World {
    /// The world folder (`None` for the flat test world).
    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// Classic superflat: bedrock, two dirt, grass.
    pub fn flat(
        data: &McData,
        synced: &SyncedIds,
        rules: &BlockRules,
        light: mc_light::Tables,
    ) -> Result<Self> {
        let dim = overworld(data, synced)?;
        let biomes = &synced["minecraft:worldgen/biome"];
        let generator = FlatGenerator {
            layers: vec![
                FlatLayer {
                    state: state(data, "minecraft:bedrock")?,
                    thickness: 1,
                },
                FlatLayer {
                    state: state(data, "minecraft:dirt")?,
                    thickness: 2,
                },
                FlatLayer {
                    state: state(data, "minecraft:grass_block")?,
                    thickness: 1,
                },
            ],
            biome: biomes["minecraft:plains"] as u32,
            air: state(data, "minecraft:air")?,
        };
        let mut chunk = generator.generate(0, 0, dim.min_y, dim.height);
        light_alone(&mut chunk, &light);
        for name in HEIGHTMAPS {
            let words = chunk.heightmap(|s| heightmap_counts(name, s, &data.blocks, rules));
            chunk.heightmaps.push((name.to_owned(), words));
        }
        let surface_y = generator.surface_y(dim.min_y);
        Ok(Self {
            empty: Chunk::new(0, 0, dim.min_y, dim.height, generator.air, generator.biome),
            source: Source::Flat(Box::new(chunk)),
            chunks: Mutex::new(HashMap::default()),
            biome_count: biomes.len() as u32,
            biome_names: biome_names(synced),
            data_version: data.version.world_version,
            block_entity_types: block_entity_types(data)?,
            folder: None,
            light,
            live_ticks: Mutex::new(Vec::new()),
            relit: Mutex::new(Vec::new()),
            lighting: crate::util::Claims::default(),
            no_autosave: std::sync::atomic::AtomicBool::new(false),
            dimension: dim.name,
            dimension_type_id: dim.type_id,
            min_y: dim.min_y,
            height: dim.height,
            sea_level: surface_y,
            spawn: (0.5, f64::from(surface_y), 0.5),
            spawn_yaw: 0.0,
            is_flat: true,
            description: "a superflat test world".into(),
        })
    }

    /// A world folder saved by vanilla 26.3 (`level.dat`, region files).
    /// Chunks it does not have are generated from its seed. A folder without
    /// `level.dat` becomes a new world (`seed`, or a random one).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        dir: &Path,
        data: &McData,
        synced: &SyncedIds,
        light: mc_light::Tables,
        rules: &BlockRules,
        seed: Option<i64>,
        shapes: mc_tick::Shapes,
        new: crate::generation::NewWorld,
    ) -> Result<Self> {
        let dim = overworld(data, synced)?;
        let biome_ids: HashMap<String, u32> = synced["minecraft:worldgen/biome"]
            .iter()
            .map(|(k, &v)| (k.clone(), v as u32))
            .collect();
        let mut generator = None;
        if !dir.join("level.dat").is_file() {
            let seed = seed.unwrap_or_else(|| crate::util::random_u64() as i64);
            tracing::debug!("creating a new world in {} (seed {seed})", dir.display());
            let g = crate::generation::overworld(data, seed, &biome_ids)?;
            let spawn = crate::generation::find_spawn(&g, &data.blocks);
            crate::generation::create(dir, data, seed, spawn, new)?;
            generator = Some((g, seed));
        } else if let Some(seed) = crate::generation::seed_of(dir) {
            generator = Some((crate::generation::overworld(data, seed, &biome_ids)?, seed));
            tracing::debug!("new chunks are generated from seed {seed}");
        }
        let generation = match generator {
            Some((generator, seed)) => {
                let features = crate::generation::features(data, shapes, &biome_ids, &generator)?;
                let air = state(data, "minecraft:air")?;
                let plains = biome_ids.get("minecraft:plains").copied().unwrap_or(0);
                let pipeline = Arc::new(crate::pipeline::Pipeline::new(
                    generator, features, seed, air, plains,
                ));
                let early = Arc::clone(&pipeline);
                std::thread::spawn(move || early.prepare());
                Some(pipeline)
            }
            None => None,
        };
        let generation = generation.map(|pipeline| Generation {
            pipeline,
            counts: (0..data.blocks.state_count())
                .map(|s| {
                    HEIGHTMAPS.iter().enumerate().fold(0u8, |bits, (i, name)| {
                        if heightmap_counts(name, s, &data.blocks, rules) {
                            bits | (1 << i)
                        } else {
                            bits
                        }
                    })
                })
                .collect(),
        });
        // 26.x keeps dimensions under dimensions/<namespace>/<name>/.
        let regions = [
            dir.join("dimensions/minecraft/overworld/region"),
            dir.join("region"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
        .with_context(|| format!("no overworld region folder in {}", dir.display()))?;
        let (spawn, spawn_yaw) = read_spawn(&dir.join("level.dat"))
            .with_context(|| format!("reading {}", dir.join("level.dat").display()))?;

        let biomes = &synced["minecraft:worldgen/biome"];
        let air = state(data, "minecraft:air")?;
        let fallback_biome = biomes["minecraft:plains"] as u32;
        let name = dir.file_name().map_or_else(
            || dir.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        tracing::debug!("world {} from {}", name, regions.display());
        Ok(Self {
            source: Source::Anvil(Box::new(Anvil {
                storage: Mutex::new(RegionStorage::new(regions)),
                generation,
                biome_ids: biomes.iter().map(|(k, &v)| (k.clone(), v as u32)).collect(),
                fallback_biome,
                air,
                unresolved_reported: AtomicU32::new(0),
            })),
            chunks: Mutex::new(HashMap::default()),
            empty: Chunk::new(0, 0, dim.min_y, dim.height, air, fallback_biome),
            biome_count: biomes.len() as u32,
            biome_names: biome_names(synced),
            data_version: data.version.world_version,
            block_entity_types: block_entity_types(data)?,
            folder: Some(dir.to_owned()),
            light,
            live_ticks: Mutex::new(Vec::new()),
            relit: Mutex::new(Vec::new()),
            lighting: crate::util::Claims::default(),
            no_autosave: std::sync::atomic::AtomicBool::new(false),
            dimension: dim.name,
            dimension_type_id: dim.type_id,
            min_y: dim.min_y,
            height: dim.height,
            sea_level: 63,
            spawn,
            spawn_yaw,
            is_flat: false,
            description: format!("the world \"{name}\""),
        })
    }

    /// Runs `f` on the chunk at `x, z`, loading it first if needed. Loading
    /// happens outside the chunk map lock.
    fn with_slot<R>(&self, x: i32, z: i32, blocks: &Blocks, f: impl FnOnce(&mut Slot) -> R) -> R {
        if let Some(slot) = self.chunks.lock().expect("chunks lock").get_mut(&(x, z)) {
            return f(slot);
        }
        let (chunk, generated) = self.load(x, z, blocks);
        let new_slot = |chunk| Slot {
            chunk,
            frame: None,
            changes: 0,
            // A generated chunk is saved like a changed one.
            dirty: generated,
            viewers: 0,
        };
        if !generated {
            let mut chunks = self.chunks.lock().expect("chunks lock");
            // Someone else may have loaded it meanwhile; theirs wins.
            let slot = chunks.entry((x, z)).or_insert_with(|| new_slot(chunk));
            return f(slot);
        }
        // Lit together with the chunks around it: light crosses the border
        // both ways, and neighbours whose light changed are sent again. The
        // work is done on copies; what changed is put back after.
        let area: Vec<(i32, i32)> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (x + dx, z + dz)))
            .collect();
        let claim = self.lighting.claim(area.clone());
        let mut local: HashMap<(i32, i32), Slot, mc_worldgen::density::Fast> = HashMap::default();
        {
            let mut chunks = self.chunks.lock().expect("chunks lock");
            if let Some(slot) = chunks.get_mut(&(x, z)) {
                return f(slot);
            }
            for &p in &area {
                if let Some(slot) = chunks.get(&p) {
                    local.insert(
                        p,
                        Slot {
                            chunk: slot.chunk.clone(),
                            frame: None,
                            changes: slot.changes,
                            dirty: false,
                            viewers: 0,
                        },
                    );
                }
            }
        }
        let versions: HashMap<(i32, i32), u64> =
            local.iter().map(|(p, s)| (*p, s.changes)).collect();
        local.insert((x, z), new_slot(chunk));
        let mut view = LightView {
            chunks: &mut local,
            min_y: self.min_y,
            top: self.min_y + self.height as i32,
            touched: Vec::new(),
            grid: None,
        };
        mc_light::light_region(
            &mut view,
            &self.light,
            (x * 16, z * 16),
            (x * 16 + 16, z * 16 + 16),
            self.min_y,
        );
        let mut touched = view.touched;
        touched.sort_unstable();
        touched.dedup();
        let mut chunks = self.chunks.lock().expect("chunks lock");
        if chunks.contains_key(&(x, z)) {
            drop(claim);
            return f(chunks.get_mut(&(x, z)).expect("present"));
        }
        let conflict = touched.iter().any(|c| {
            *c != (x, z)
                && versions
                    .get(c)
                    .is_none_or(|&v| chunks.get(c).is_none_or(|s| s.changes != v))
        });
        let mut slot = local.remove(&(x, z)).expect("inserted above");
        slot.changes = 0;
        chunks.insert((x, z), slot);
        let relit: Vec<(i32, i32)> = if conflict {
            // A neighbour changed meanwhile: light again, on the real thing.
            let mut view = LightView {
                chunks: &mut chunks,
                min_y: self.min_y,
                top: self.min_y + self.height as i32,
                touched: Vec::new(),
                grid: None,
            };
            mc_light::light_region(
                &mut view,
                &self.light,
                (x * 16, z * 16),
                (x * 16 + 16, z * 16 + 16),
                self.min_y,
            );
            view.touched.into_iter().filter(|&c| c != (x, z)).collect()
        } else {
            for c in touched.iter().filter(|&&c| c != (x, z)) {
                if let (Some(from), Some(to)) = (local.get_mut(c), chunks.get_mut(c))
                    && let (Some(lit), Some(chunk)) = (from.chunk.take(), to.chunk.as_mut())
                {
                    chunk.light = lit.light;
                    to.frame = None;
                    to.changes += 1;
                    to.dirty |= from.dirty;
                }
            }
            touched.into_iter().filter(|&c| c != (x, z)).collect()
        };
        if !relit.is_empty() {
            self.relit.lock().expect("relit lock").extend(relit);
        }
        drop(claim);
        let slot = chunks.get_mut(&(x, z)).expect("inserted above");
        f(slot)
    }

    /// The chunk, and whether it was generated just now.
    fn load(&self, x: i32, z: i32, blocks: &Blocks) -> (Option<Box<Chunk>>, bool) {
        match &self.source {
            Source::Flat(template) => {
                let mut chunk = template.as_ref().clone();
                (chunk.x, chunk.z) = (x, z);
                (Some(Box::new(chunk)), false)
            }
            Source::Anvil(anvil) => match anvil.load(x, z, blocks, self.min_y, self.height) {
                Ok(Some(chunk)) => (Some(chunk), false),
                Ok(None) => match &anvil.generation {
                    Some(g) => (Some(self.generate(g, x, z, anvil, blocks)), true),
                    None => (None, false),
                },
                Err(err) => {
                    warn!("chunk {x},{z}: {err:#}; sending it empty");
                    (None, false)
                }
            },
        }
    }

    /// A new chunk from the generator, lit, with heightmaps.
    fn generate(
        &self,
        g: &Generation,
        x: i32,
        z: i32,
        anvil: &Anvil,
        blocks: &Blocks,
    ) -> Box<Chunk> {
        let view = Generated {
            world: self,
            anvil,
            blocks,
        };
        let done = g.pipeline.finish(x, z, &view);
        if !done.live_ticks.is_empty() {
            self.live_ticks
                .lock()
                .expect("live ticks lock")
                .extend(done.live_ticks);
        }
        let mut chunk = done.chunk;
        for (i, name) in HEIGHTMAPS.iter().enumerate() {
            let words =
                chunk.heightmap(|s| g.counts.get(s as usize).is_some_and(|b| b & (1 << i) != 0));
            chunk.heightmaps.push(((*name).to_owned(), words));
        }
        chunk
    }

    /// Structures starting within `radius` chunks of block `x, z`: name and
    /// the start chunk's corner (with the structure's middle height), nearest
    /// first.
    pub fn starts_within(
        &self,
        x: i32,
        z: i32,
        radius: i32,
        name: Option<&str>,
    ) -> Vec<(String, (i32, i32, i32))> {
        let Source::Anvil(anvil) = &self.source else {
            return Vec::new();
        };
        let Some(g) = &anvil.generation else {
            return Vec::new();
        };
        g.pipeline
            .starts_within(x >> 4, z >> 4, radius, name)
            .into_iter()
            .map(|(n, c, p)| (n, (c.0 * 16, p.y, c.1 * 16)))
            .collect()
    }

    /// The biome the generator gives a 4×4×4 cell, as `/locate biome`
    /// samples it; `None` where the world has no generator of ours.
    pub fn noise_biome(&self, qx: i32, qy: i32, qz: i32) -> Option<&str> {
        let id = match &self.source {
            Source::Flat(_) => return Some("minecraft:plains"),
            Source::Anvil(anvil) => anvil
                .generation
                .as_ref()?
                .pipeline
                .generator
                .noise_biome(qx, qy, qz),
        };
        self.biome_names.get(id as usize).map(String::as_str)
    }

    /// What makes the world's new chunks (and knows its structures).
    pub(crate) fn pipeline(&self) -> Option<&crate::pipeline::Pipeline> {
        match &self.source {
            Source::Anvil(anvil) => anvil.generation.as_ref().map(|g| &*g.pipeline),
            Source::Flat(_) => None,
        }
    }

    /// Chunks whose light a new neighbour changed since the last call.
    pub fn take_relit(&self) -> Vec<(i32, i32)> {
        let mut list = std::mem::take(&mut *self.relit.lock().expect("relit lock"));
        list.sort_unstable();
        list.dedup();
        list
    }

    /// Fluid ticks generation left for chunks players already have.
    pub fn take_live_ticks(&self) -> Vec<(mc_tick::BlockPos, u32)> {
        std::mem::take(&mut *self.live_ticks.lock().expect("live ticks lock"))
    }

    /// The framed Chunk Data packet for a player who now has the chunk (pair
    /// with [`World::unwatch`]). `frame` turns a packet into wire bytes; it
    /// runs once per change of the chunk, not once per player.
    pub fn watch(
        &self,
        x: i32,
        z: i32,
        blocks: &Blocks,
        frame: &dyn Fn(&LevelChunkWithLight) -> Vec<u8>,
    ) -> Arc<[u8]> {
        // Encoding and compressing happen without the lock, on a copy.
        let copy = self.with_slot(x, z, blocks, |slot| {
            slot.viewers += 1;
            match &slot.frame {
                Some(frame) => Err(frame.clone()),
                None => Ok((slot.chunk.clone(), slot.changes)),
            }
        });
        let (chunk, changes) = match copy {
            Err(frame) => return frame,
            Ok(copy) => copy,
        };
        let packet = match &chunk {
            Some(chunk) => self.encode(chunk, blocks),
            None => {
                let mut empty = self.empty.clone();
                (empty.x, empty.z) = (x, z);
                self.encode(&empty, blocks)
            }
        };
        let bytes: Arc<[u8]> = frame(&packet).into();
        if let Some(slot) = self.chunks.lock().expect("chunks lock").get_mut(&(x, z))
            && slot.frame.is_none()
            && slot.changes == changes
        {
            slot.frame = Some(bytes.clone());
        }
        bytes
    }

    /// A player's client no longer has the chunk.
    pub fn unwatch(&self, x: i32, z: i32) {
        if let Some(slot) = self.chunks.lock().expect("chunks lock").get_mut(&(x, z)) {
            slot.viewers = slot.viewers.saturating_sub(1);
        }
    }

    /// Drops chunks no player has and that have nothing to save. Returns how
    /// many were dropped.
    pub fn unload_unused(&self) -> usize {
        let mut chunks = self.chunks.lock().expect("chunks lock");
        let before = chunks.len();
        chunks.retain(|_, slot| slot.viewers > 0 || slot.dirty);
        before - chunks.len()
    }

    /// Whether the chunk holding `p` is in memory (vanilla's "loaded").
    pub fn is_loaded(&self, p: Position) -> bool {
        self.chunks
            .lock()
            .expect("chunks lock")
            .get(&(p.x >> 4, p.z >> 4))
            .is_some_and(|s| s.chunk.is_some())
    }

    /// Chunks in memory.
    pub fn loaded_chunks(&self) -> usize {
        self.chunks.lock().expect("chunks lock").len()
    }

    /// The block at `p`; `None` outside the world's height or where the world
    /// has no chunk.
    pub fn block(&self, p: Position, blocks: &Blocks) -> Option<u32> {
        self.with_slot(p.x >> 4, p.z >> 4, blocks, |slot| {
            let chunk = slot.chunk.as_ref()?;
            chunk
                .contains_y(p.y)
                .then(|| chunk.block((p.x & 15) as usize, p.y, (p.z & 15) as usize))
        })
    }

    /// Fixes light after the block at `p` changed. Returns the chunks whose
    /// light changed (their packets are dropped and they are marked changed).
    pub fn relight(&self, p: Position) -> Vec<(i32, i32)> {
        let mut chunks = self.chunks.lock().expect("chunks lock");
        let mut view = LightView {
            chunks: &mut chunks,
            min_y: self.min_y,
            top: self.min_y + self.height as i32,
            touched: Vec::new(),
            grid: None,
        };
        mc_light::block_changed(&mut view, &self.light, (p.x, p.y, p.z));
        view.touched
    }

    /// Fixes light after the blocks at `ps` changed from the states given,
    /// all in one pass. Returns the chunks whose light changed.
    pub fn relight_many(&self, ps: &[(Position, u32)]) -> Vec<(i32, i32)> {
        if ps.is_empty() {
            return Vec::new();
        }
        // Light moves at most 15 blocks from a change: one chunk around.
        let min = ps.iter().fold((i32::MAX, i32::MAX), |m, (p, _)| (m.0.min((p.x >> 4) - 1), m.1.min((p.z >> 4) - 1)));
        let max = ps.iter().fold((i32::MIN, i32::MIN), |m, (p, _)| (m.0.max((p.x >> 4) + 1), m.1.max((p.z >> 4) + 1)));
        let mut chunks = self.chunks.lock().expect("chunks lock");
        let top = self.min_y + self.height as i32;
        let mut view = LightView::around(&mut chunks, self.min_y, top, min, max);
        let ps: Vec<(mc_light::Pos, u32)> = ps.iter().map(|&(p, old)| ((p.x, p.y, p.z), old)).collect();
        mc_light::blocks_changed(&mut view, &self.light, &ps);
        view.finish()
    }

    /// The brighter of sky and block light at `p` (15 where nothing is
    /// loaded).
    pub fn brightness(&self, p: Position) -> u8 {
        use mc_light::LightWorld as _;
        let mut chunks = self.chunks.lock().expect("chunks lock");
        let view = LightView {
            chunks: &mut chunks,
            min_y: self.min_y,
            top: self.min_y + self.height as i32,
            touched: Vec::new(),
            grid: None,
        };
        let at = (p.x, p.y, p.z);
        view.light(mc_light::Kind::Sky, at)
            .max(view.light(mc_light::Kind::Block, at))
    }

    /// Block light alone at `p`.
    pub fn block_light(&self, p: Position) -> u8 {
        self.light_of(mc_light::Kind::Block, p)
    }

    /// Sky light alone at `p` (15 open to the sky).
    pub fn sky_light(&self, p: Position) -> u8 {
        self.light_of(mc_light::Kind::Sky, p)
    }

    fn light_of(&self, kind: mc_light::Kind, p: Position) -> u8 {
        use mc_light::LightWorld as _;
        let mut chunks = self.chunks.lock().expect("chunks lock");
        let view = LightView {
            chunks: &mut chunks,
            min_y: self.min_y,
            top: self.min_y + self.height as i32,
            touched: Vec::new(),
            grid: None,
        };
        view.light(kind, (p.x, p.y, p.z))
    }

    /// How much light `state` stops.
    pub fn light_block(&self, state: u32) -> u8 {
        self.light.opacity(state)
    }

    /// Whether changing `a` to `b` leaves light as it is.
    pub fn same_light(&self, a: u32, b: u32) -> bool {
        self.light.same_light(a, b)
    }

    /// Random tick picks for one game tick (minecraft.wiki, "Tick" →
    /// "Random tick"): in every section of `chunks` whose palette holds a
    /// state `ticks` cares about, `speed` positions drawn from vanilla's
    /// level-wide generator (`lcg`); the ones holding such a state come back.
    /// Sections without one cost a palette scan and nothing else.
    pub fn random_tick_picks(
        &self,
        chunks: &[(i32, i32)],
        speed: u32,
        lcg: &mut i32,
        ticks: impl Fn(u32) -> bool,
    ) -> Vec<(Position, u32)> {
        let mut picks = Vec::new();
        let all = self.chunks.lock().expect("chunks lock");
        for &(cx, cz) in chunks {
            // Only chunks with all eight neighbours in memory: what ticks
            // looks a few blocks around and must not load anything.
            let surrounded = (-1..=1).all(|dx| (-1..=1).all(|dz| all.contains_key(&(cx + dx, cz + dz))));
            if !surrounded {
                continue;
            }
            let Some(chunk) = all.get(&(cx, cz)).and_then(|slot| slot.chunk.as_ref()) else {
                continue;
            };
            for (i, section) in chunk.sections.iter().enumerate() {
                if !section.blocks.may_contain(&ticks) {
                    continue;
                }
                let base_y = chunk.min_y + 16 * i as i32;
                for _ in 0..speed {
                    *lcg = lcg.wrapping_mul(3).wrapping_add(1_013_904_223);
                    let r = *lcg >> 2;
                    let (x, y, z) = ((r & 15) as usize, ((r >> 16) & 15) as usize, ((r >> 8) & 15) as usize);
                    let state = section.blocks.get(mc_world::Section::index(x, y, z));
                    if ticks(state) {
                        picks.push((Position::new(cx * 16 + x as i32, base_y + y as i32, cz * 16 + z as i32), state));
                    }
                }
            }
        }
        picks
    }

    /// Whether the chunk is in memory (nothing is loaded to answer).
    pub fn chunk_loaded(&self, x: i32, z: i32) -> bool {
        self.chunks.lock().expect("chunks lock").contains_key(&(x, z))
    }

    /// Light Data of a chunk as it is now (for Update Light).
    pub fn light_data(&self, x: i32, z: i32) -> Option<LightData> {
        let chunks = self.chunks.lock().expect("chunks lock");
        let chunk = chunks.get(&(x, z))?.chunk.as_ref()?;
        Some(stored_light(chunk))
    }

    /// The block entity at `p` (chest contents, sign text...).
    pub fn block_entity(&self, p: Position, blocks: &Blocks) -> Option<mc_world::BlockEntity> {
        self.with_slot(p.x >> 4, p.z >> 4, blocks, |slot| {
            slot.chunk
                .as_ref()?
                .block_entities
                .iter()
                .find(|be| (be.x, be.y, be.z) == (p.x, p.y, p.z))
                .cloned()
        })
    }

    /// The `minecraft:block_entity_type` registry id of a block entity id.
    pub fn block_entity_kind(&self, id: &str) -> Option<i32> {
        self.block_entity_types.get(id).copied()
    }

    /// Stores a block entity (replacing the one at its position).
    pub fn set_block_entity(&self, be: mc_world::BlockEntity, blocks: &Blocks) {
        self.with_slot(be.x >> 4, be.z >> 4, blocks, |slot| {
            let Some(chunk) = slot.chunk.as_mut() else {
                return;
            };
            chunk
                .block_entities
                .retain(|other| (other.x, other.y, other.z) != (be.x, be.y, be.z));
            chunk.block_entities.push(be);
            slot.frame = None;
            slot.changes += 1;
            slot.dirty = true;
        });
    }

    /// Sets one block, keeping heightmaps and block entities consistent.
    /// Returns the previous state, or `None` if the block cannot change there.
    pub fn set_block(
        &self,
        p: Position,
        state: u32,
        blocks: &Blocks,
        rules: &BlockRules,
    ) -> Option<u32> {
        self.with_slot(p.x >> 4, p.z >> 4, blocks, |slot| {
            let chunk = slot.chunk.as_mut()?;
            if !chunk.contains_y(p.y) {
                return None;
            }
            let (x, z) = ((p.x & 15) as usize, (p.z & 15) as usize);
            let old = chunk.set_block(x, p.y, z, state);
            if old == state {
                return Some(old);
            }
            for name in HEIGHTMAPS {
                chunk.update_heightmap(name, x, p.y, z, state, |s| {
                    heightmap_counts(name, s, blocks, rules)
                });
            }
            if blocks.block_name(old) != blocks.block_name(state) {
                chunk
                    .block_entities
                    .retain(|be| (be.x, be.y, be.z) != (p.x, p.y, p.z));
            }
            slot.frame = None;
            slot.changes += 1;
            slot.dirty = true;
            Some(old)
        })
    }

    /// The biome network id of a biome name.
    pub fn biome_id(&self, name: &str) -> Option<u32> {
        self.biome_names
            .iter()
            .position(|n| n == name)
            .map(|i| i as u32)
    }

    pub fn biome_name(&self, id: u32) -> Option<&str> {
        self.biome_names.get(id as usize).map(String::as_str)
    }

    /// The biome stored for the 4×4×4 cell holding `p`; `None` where no
    /// chunk is or outside the world's height.
    pub fn biome(&self, p: Position, blocks: &Blocks) -> Option<u32> {
        self.with_slot(p.x >> 4, p.z >> 4, blocks, |slot| {
            let chunk = slot.chunk.as_ref()?;
            if !chunk.contains_y(p.y) {
                return None;
            }
            let local = (p.y - chunk.min_y) as usize;
            let i = biome_index(p.x, local, p.z);
            Some(chunk.sections[local / 16].biomes.get(i))
        })
    }

    /// Sets the biome of the cells from `min` to `max` (cell coordinates,
    /// inclusive) in chunk `(cx, cz)` where `replace` allows; how many
    /// changed.
    pub fn fill_biome(
        &self,
        (cx, cz): (i32, i32),
        min: [i32; 3],
        max: [i32; 3],
        biome: u32,
        replace: &dyn Fn(u32) -> bool,
        blocks: &Blocks,
    ) -> usize {
        self.with_slot(cx, cz, blocks, |slot| {
            let Some(chunk) = slot.chunk.as_mut() else {
                return 0;
            };
            let mut changed = 0;
            let bottom = chunk.min_y >> 2;
            let cells = chunk.sections.len() as i32 * 4;
            for qy in min[1].max(bottom)..=max[1].min(bottom + cells - 1) {
                let local = ((qy - bottom) * 4) as usize;
                for qz in min[2].max(cz * 4)..=max[2].min(cz * 4 + 3) {
                    for qx in min[0].max(cx * 4)..=max[0].min(cx * 4 + 3) {
                        let i = biome_index(qx * 4, local, qz * 4);
                        let section = &mut chunk.sections[local / 16].biomes;
                        let old = section.get(i);
                        if old != biome && replace(old) {
                            section.set(i, biome);
                            changed += 1;
                        }
                    }
                }
            }
            if changed > 0 {
                // Vanilla refills every section into a fresh copy, whose
                // palette starts with the old one's first entry.
                for section in &mut chunk.sections {
                    let b = &section.biomes;
                    section.biomes = mc_world::PalettedContainer::from_values_after(
                        mc_world::BIOMES,
                        Some(b.first()),
                        &b.values(),
                    );
                }
                slot.frame = None;
                slot.changes += 1;
                slot.dirty = true;
            }
            changed
        })
    }

    /// Every section's biomes as the Chunks Biomes packet carries them.
    pub fn biome_data(&self, cx: i32, cz: i32, blocks: &Blocks) -> Option<Vec<u8>> {
        let bits = mc_world::bits_needed((self.biome_names.len() as u32).saturating_sub(1));
        self.with_slot(cx, cz, blocks, |slot| {
            let chunk = slot.chunk.as_ref()?;
            let mut out = Vec::new();
            for s in &chunk.sections {
                s.biomes.write_network(bits, &mut out);
            }
            Some(out)
        })
    }

    /// Writes every changed chunk to its region file (world folders only; a
    /// flat test world lives in memory). Changed chunks are saved with
    /// `isLightOn` cleared: their stored light is stale, vanilla relights them.
    pub fn save(&self, blocks: &Blocks) -> Saved {
        let Source::Anvil(anvil) = &self.source else {
            return Saved::default();
        };
        let dirty: Vec<Chunk> = self
            .chunks
            .lock()
            .expect("chunks lock")
            .values_mut()
            .filter(|slot| slot.dirty)
            .filter_map(|slot| {
                slot.dirty = false;
                slot.chunk.as_deref().cloned()
            })
            .collect();
        let names = Names {
            blocks,
            biomes: &self.biome_names,
            data_version: self.data_version,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);
        let mut saved = Saved::default();
        // Chunks still being generated go out as `minecraft:features`.
        let protos = anvil
            .generation
            .as_ref()
            .map(|g| g.pipeline.unsaved())
            .unwrap_or_default();
        let mut storage = anvil.storage.lock().expect("region storage");
        for chunk in protos {
            let result = (|| -> Result<()> {
                let Tag::Compound(mut root) = encode_chunk(&chunk, None, &names, true)? else {
                    bail!("encoded chunk is not a compound");
                };
                root.insert("Status", Tag::String("minecraft:features".into()));
                let mut bytes = Vec::new();
                mc_nbt::write_named("", &Tag::Compound(root), &mut bytes)?;
                storage.write_chunk(chunk.x, chunk.z, &bytes, now)?;
                Ok(())
            })();
            if let Err(err) = result {
                warn!(
                    "saving chunk {},{} (still generating): {err:#}",
                    chunk.x, chunk.z
                );
            }
        }
        for chunk in dirty {
            let result = (|| -> Result<()> {
                let stored = storage.read_chunk(chunk.x, chunk.z)?;
                let tag = encode_chunk(&chunk, stored.as_ref(), &names, true)?;
                let mut bytes = Vec::new();
                mc_nbt::write_named("", &tag, &mut bytes)?;
                storage.write_chunk(chunk.x, chunk.z, &bytes, now)?;
                Ok(())
            })();
            match result {
                Ok(()) => saved.chunks += 1,
                Err(err) => {
                    warn!("saving chunk {},{}: {err:#}", chunk.x, chunk.z);
                    saved.failed += 1;
                    // Try again next time.
                    if let Some(slot) = self
                        .chunks
                        .lock()
                        .expect("chunks lock")
                        .get_mut(&(chunk.x, chunk.z))
                    {
                        slot.dirty = true;
                    }
                }
            }
        }
        saved
    }

    /// The world's age (`Time` in level.dat) and clocks
    /// (`data/minecraft/world_clocks.dat`, 26.x). Clocks stop when the
    /// `minecraft:advance_time` game rule is off. A flat world starts at 0.
    pub fn load_time(&self, synced: &SyncedIds) -> Result<WorldTime> {
        let registry = synced
            .get("minecraft:world_clock")
            .context("no minecraft:world_clock registry")?;
        let mut clocks: Vec<Clock> = registry
            .iter()
            .map(|(name, &id)| Clock {
                name: name.clone(),
                id,
                ticks: 0,
                running: true,
                rate: 1.0,
                fraction: 0.0,
            })
            .collect();
        clocks.sort_by_key(|c| c.id);
        let Some(dir) = &self.folder else {
            return Ok(WorldTime { age: 0, clocks });
        };
        let level = read_gz(&dir.join("level.dat"))?;
        let age = match level.get("Data") {
            Some(Tag::Compound(data)) => match data.get("Time") {
                Some(Tag::Long(t)) => *t,
                _ => 0,
            },
            _ => 0,
        };
        let stored = read_gz(&dir.join(CLOCKS_FILE)).ok();
        let rules = read_gz(&dir.join(GAME_RULES_FILE)).ok();
        let advance = match rules.as_ref().and_then(|r| r.get("data")) {
            Some(Tag::Compound(data)) => {
                !matches!(data.get("minecraft:advance_time"), Some(Tag::Byte(0)))
            }
            _ => true,
        };
        for clock in &mut clocks {
            clock.running = advance;
            clock.ticks = match stored.as_ref().and_then(|s| s.get("data")) {
                Some(Tag::Compound(data)) => match data.get(&clock.name) {
                    Some(Tag::Compound(c)) => match c.get("total_ticks") {
                        Some(Tag::Long(t)) => *t,
                        _ => age,
                    },
                    _ => age,
                },
                _ => age,
            };
        }
        Ok(WorldTime { age, clocks })
    }

    /// Writes the age into level.dat (keeping the previous file as
    /// level.dat_old, as vanilla does) and the clocks into world_clocks.dat.
    pub fn save_time(&self, time: &WorldTime) -> Result<()> {
        let Some(dir) = &self.folder else {
            return Ok(());
        };
        let level_path = dir.join("level.dat");
        let mut level = read_gz(&level_path)?;
        if let Some(Tag::Compound(data)) = level.get_mut("Data") {
            data.insert("Time", Tag::Long(time.age));
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            data.insert("LastPlayed", Tag::Long(now));
        }
        std::fs::copy(&level_path, dir.join("level.dat_old"))?;
        write_gz(&level_path, &level)?;

        let clocks_path = dir.join(CLOCKS_FILE);
        let mut stored = read_gz(&clocks_path).unwrap_or_else(|_| {
            let mut root = mc_nbt::Compound::new();
            root.insert("DataVersion", Tag::Int(self.data_version));
            root
        });
        let mut data = match stored.remove("data") {
            Some(Tag::Compound(data)) => data,
            _ => mc_nbt::Compound::new(),
        };
        for clock in &time.clocks {
            let mut entry = match data.remove(&clock.name) {
                Some(Tag::Compound(entry)) => entry,
                _ => mc_nbt::Compound::new(),
            };
            entry.insert("total_ticks", Tag::Long(clock.ticks));
            data.insert(clock.name.as_str(), Tag::Compound(entry));
        }
        stored.insert("data", Tag::Compound(data));
        if let Some(parent) = clocks_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_gz(&clocks_path, &stored)
    }

    /// Game mode for new players: level.dat `GameType` (creative on the flat
    /// test world).
    pub fn default_game_mode(&self) -> i32 {
        let Some(dir) = &self.folder else {
            return 1;
        };
        match read_gz(&dir.join("level.dat"))
            .ok()
            .and_then(|root| match root.get("Data") {
                Some(Tag::Compound(data)) => data.get("GameType").cloned(),
                _ => None,
            }) {
            Some(Tag::Int(mode)) => mode,
            _ => 0,
        }
    }

    /// 0 peaceful, 1 easy, 2 normal, 3 hard (level.dat
    /// `difficulty_settings.difficulty`); easy for the flat test world.
    pub fn difficulty(&self) -> i32 {
        let Some(dir) = &self.folder else {
            return 1;
        };
        let name = read_gz(&dir.join("level.dat"))
            .ok()
            .and_then(|root| match root.get("Data") {
                Some(Tag::Compound(data)) => match data.get("difficulty_settings") {
                    Some(Tag::Compound(d)) => match d.get("difficulty") {
                        Some(Tag::String(s)) => Some(s.clone()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            });
        match name.as_deref() {
            Some("peaceful") => 0,
            Some("normal") => 2,
            Some("hard") => 3,
            _ => 1,
        }
    }

    /// Changes level.dat's `Data` (keeping the previous file as
    /// level.dat_old).
    pub fn update_level(&self, change: impl FnOnce(&mut mc_nbt::Compound)) -> Result<()> {
        let Some(dir) = &self.folder else {
            return Ok(());
        };
        let path = dir.join("level.dat");
        let mut level = read_gz(&path)?;
        if let Some(Tag::Compound(data)) = level.get_mut("Data") {
            change(data);
        }
        let _ = std::fs::copy(&path, dir.join("level.dat_old"));
        write_gz(&path, &level)
    }

    /// Turns automatic saving on or off (`/save-on`, `/save-off`);
    /// whether it was on.
    pub fn autosave(&self, on: bool) -> bool {
        !self
            .no_autosave
            .swap(!on, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn autosaving(&self) -> bool {
        !self.no_autosave.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether changes are written anywhere.
    pub fn is_saved(&self) -> bool {
        matches!(self.source, Source::Anvil(_))
    }

    fn encode(&self, chunk: &Chunk, blocks: &Blocks) -> LevelChunkWithLight {
        let heightmaps = chunk
            .heightmaps
            .iter()
            .filter_map(|(name, words)| {
                let kind = match name.as_str() {
                    "WORLD_SURFACE" => heightmap::WORLD_SURFACE,
                    "MOTION_BLOCKING" => heightmap::MOTION_BLOCKING,
                    "MOTION_BLOCKING_NO_LEAVES" => heightmap::MOTION_BLOCKING_NO_LEAVES,
                    _ => return None, // server-only heightmaps
                };
                Some((kind, words.clone()))
            })
            .collect();
        let block_entities = chunk
            .block_entities
            .iter()
            .filter_map(|be| {
                Some(ChunkBlockEntity {
                    packed_xz: (((be.x & 15) << 4) | (be.z & 15)) as u8,
                    y: be.y as i16,
                    kind: *self.block_entity_types.get(&be.id)?,
                    data: client_data(&be.id, &be.data).map(Tag::Compound),
                })
            })
            .collect();
        LevelChunkWithLight {
            x: chunk.x,
            z: chunk.z,
            heightmaps,
            data: chunk.write_sections(blocks, self.biome_count),
            block_entities,
            light: if chunk.light.is_empty() {
                full_sky_light(self.height)
            } else {
                stored_light(chunk)
            },
        }
    }
}

/// The world as the generation pipeline sees it.
struct Generated<'a> {
    world: &'a World,
    anvil: &'a Anvil,
    blocks: &'a Blocks,
}

impl crate::pipeline::Finished for Generated<'_> {
    fn is_finished(&self, x: i32, z: i32) -> bool {
        if let Some(slot) = self.world.chunks.lock().expect("chunks lock").get(&(x, z)) {
            return slot.chunk.is_some();
        }
        self.anvil.has_full(x, z)
    }

    fn stored_proto(&self, x: i32, z: i32) -> Option<Chunk> {
        let raw = self
            .anvil
            .storage
            .lock()
            .expect("region storage")
            .read_raw(x, z)
            .ok()??;
        let tag = raw.parse().ok()?;
        let biome = |name: &str| self.anvil.biome_ids.get(name).copied();
        let palettes = Palettes {
            blocks: self.blocks,
            biome: &biome,
            air: self.anvil.air,
            fallback_biome: self.anvil.fallback_biome,
            unresolved: Default::default(),
        };
        let (status, chunk) =
            mc_storage::decode_with_status(&tag, self.world.min_y, self.world.height, &palettes)
                .ok()?;
        (status == "minecraft:features").then_some(*chunk)
    }

    fn block(&self, p: mc_tick::BlockPos) -> Option<u32> {
        let chunks = self.world.chunks.lock().expect("chunks lock");
        let chunk = chunks.get(&(p.x >> 4, p.z >> 4))?.chunk.as_ref()?;
        chunk
            .contains_y(p.y)
            .then(|| chunk.block((p.x & 15) as usize, p.y, (p.z & 15) as usize))
    }
}

impl Anvil {
    /// Whether the folder has a finished chunk here (without decoding it).
    fn has_full(&self, x: i32, z: i32) -> bool {
        let raw = self.storage.lock().expect("region storage").read_raw(x, z);
        let Ok(Some(raw)) = raw else {
            return false;
        };
        match raw.parse() {
            Ok(Tag::Compound(root)) => match root.get("Status") {
                Some(Tag::String(s)) => s == "minecraft:full" || s == "full",
                _ => false,
            },
            _ => false,
        }
    }

    /// `None` if the world has no finished chunk here.
    fn load(
        &self,
        x: i32,
        z: i32,
        blocks: &Blocks,
        min_y: i32,
        height: u32,
    ) -> Result<Option<Box<Chunk>>> {
        // Only the file read holds the lock; decompression and decoding do not.
        let raw = self
            .storage
            .lock()
            .expect("region storage")
            .read_raw(x, z)?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let tag = raw.parse()?;
        let biome = |name: &str| self.biome_ids.get(name).copied();
        let palettes = Palettes {
            blocks,
            biome: &biome,
            air: self.air,
            fallback_biome: self.fallback_biome,
            unresolved: Default::default(),
        };
        let chunk = match decode_chunk(&tag, min_y, height, &palettes)? {
            Decoded::Chunk(chunk) => chunk,
            Decoded::NotFull(_) => return Ok(None),
        };
        let unresolved = palettes.unresolved.get();
        if unresolved > 0 && self.unresolved_reported.fetch_add(1, Ordering::Relaxed) < 5 {
            warn!("chunk {x},{z}: {unresolved} unknown blocks or biomes replaced");
        }
        Ok(Some(chunk))
    }
}

/// What the client gets for a block entity in a chunk packet: only what it
/// needs to draw it, never container contents.
///
/// Observed against vanilla 26.3 (`cargo xtask chunk-diff`): chests get no data
/// at all, spawners get their data without `SpawnPotentials` and `components`.
/// The other rules are assumptions until checked the same way.
pub(crate) fn client_data(id: &str, data: &mc_nbt::Compound) -> Option<mc_nbt::Compound> {
    const CONTAINERS: &[&str] = &[
        "minecraft:barrel",
        "minecraft:blast_furnace",
        "minecraft:brewing_stand",
        "minecraft:chest",
        "minecraft:crafter",
        "minecraft:dispenser",
        "minecraft:dropper",
        "minecraft:ender_chest",
        "minecraft:furnace",
        "minecraft:hopper",
        "minecraft:shulker_box",
        "minecraft:smoker",
        "minecraft:trapped_chest",
    ];
    if CONTAINERS.contains(&id) {
        return None;
    }
    let mut data = data.clone();
    for key in [
        "Items",
        "LootTable",
        "LootTableSeed",
        "components",
        "SpawnPotentials",
    ] {
        data.remove(key);
    }
    (!data.is_empty()).then_some(data)
}

/// Lights one chunk by itself (nothing around it): for the flat template.
fn light_alone(chunk: &mut Chunk, tables: &mc_light::Tables) {
    let (x, z) = (chunk.x, chunk.z);
    let min_y = chunk.min_y;
    let top = min_y + chunk.height() as i32;
    let mut slot = HashMap::default();
    slot.insert(
        (x, z),
        Slot {
            chunk: Some(Box::new(chunk.clone())),
            frame: None,
            changes: 0,
            dirty: false,
            viewers: 0,
        },
    );
    let mut view = LightView {
        chunks: &mut slot,
        min_y,
        top,
        touched: Vec::new(),
        grid: None,
    };
    mc_light::light_region(
        &mut view,
        tables,
        (x * 16, z * 16),
        (x * 16 + 16, z * 16 + 16),
        min_y,
    );
    if let Some(lit) = slot.remove(&(x, z)).and_then(|s| s.chunk) {
        chunk.light = lit.light;
    }
}

/// The loaded chunks as the light engine sees them.
struct LightView<'a> {
    chunks: &'a mut HashMap<(i32, i32), Slot, mc_worldgen::density::Fast>,
    min_y: i32,
    top: i32,
    touched: Vec<(i32, i32)>,
    /// The slots of a rectangle of chunks, taken out of the map while the
    /// view lives, so light flooding through it finds a chunk by index
    /// instead of by hash. They go back when the view is dropped.
    grid: Option<Grid>,
}

struct Grid {
    x0: i32,
    z0: i32,
    w: i32,
    d: i32,
    slots: Vec<Option<Slot>>,
}

impl<'a> LightView<'a> {
    /// A view whose lookups inside the chunks `min..=max` go through a grid.
    fn around(
        chunks: &'a mut HashMap<(i32, i32), Slot, mc_worldgen::density::Fast>,
        min_y: i32,
        top: i32,
        min: (i32, i32),
        max: (i32, i32),
    ) -> Self {
        let (w, d) = (max.0 - min.0 + 1, max.1 - min.1 + 1);
        let grid = (w > 0 && d > 0 && w * d <= 4096).then(|| {
            let mut slots = Vec::with_capacity((w * d) as usize);
            for x in 0..w {
                for z in 0..d {
                    slots.push(chunks.remove(&(min.0 + x, min.1 + z)));
                }
            }
            Grid {
                x0: min.0,
                z0: min.1,
                w,
                d,
                slots,
            }
        });
        Self {
            chunks,
            min_y,
            top,
            touched: Vec::new(),
            grid,
        }
    }

    /// Puts the grid's chunks back and returns the chunks whose light
    /// changed.
    fn finish(mut self) -> Vec<(i32, i32)> {
        if let Some(g) = self.grid.take() {
            let mut slots = g.slots.into_iter();
            for x in 0..g.w {
                for z in 0..g.d {
                    if let Some(Some(slot)) = slots.next() {
                        self.chunks.insert((g.x0 + x, g.z0 + z), slot);
                    }
                }
            }
        }
        self.touched
    }

    /// Where the grid keeps a chunk, if it covers it.
    fn cell(&self, key: (i32, i32)) -> Option<usize> {
        let g = self.grid.as_ref()?;
        let (x, z) = (key.0 - g.x0, key.1 - g.z0);
        (x >= 0 && z >= 0 && x < g.w && z < g.d).then_some((x * g.d + z) as usize)
    }

    fn slot(&self, key: (i32, i32)) -> Option<&Slot> {
        match self.cell(key) {
            Some(i) => self.grid.as_ref()?.slots[i].as_ref(),
            None => self.chunks.get(&key),
        }
    }

    fn slot_mut(&mut self, key: (i32, i32)) -> Option<&mut Slot> {
        match self.cell(key) {
            Some(i) => self.grid.as_mut()?.slots[i].as_mut(),
            None => self.chunks.get_mut(&key),
        }
    }

    fn chunk(&self, p: mc_light::Pos) -> Option<&Chunk> {
        if p.1 < self.min_y || p.1 >= self.top {
            return None;
        }
        self.slot((p.0 >> 4, p.2 >> 4))?.chunk.as_deref()
    }
}

/// A section's stored light value. A missing sky array reads as the bottom
/// layer of the nearest stored array above it (open sky if there is none),
/// as clients read it; missing block light is dark.
fn stored_level(chunk: &Chunk, kind: mc_light::Kind, section: usize, index: usize) -> u8 {
    let value = |a: &[u8], index: usize| {
        let byte = a[index / 2];
        if index.is_multiple_of(2) {
            byte & 15
        } else {
            byte >> 4
        }
    };
    match kind {
        mc_light::Kind::Block => chunk
            .light
            .get(section)
            .and_then(|l| l.block.as_ref())
            .map_or(0, |a| value(a, index)),
        mc_light::Kind::Sky => {
            if let Some(a) = chunk.light.get(section).and_then(|l| l.sky.as_ref()) {
                return value(a, index);
            }
            chunk
                .light
                .iter()
                .skip(section + 1)
                .find_map(|l| l.sky.as_ref())
                .map_or(15, |a| value(a, index & 0xFF))
        }
    }
}

impl mc_light::LightWorld for LightView<'_> {
    fn state(&self, p: mc_light::Pos) -> Option<u32> {
        let chunk = self.chunk(p)?;
        Some(chunk.block((p.0 & 15) as usize, p.1, (p.2 & 15) as usize))
    }

    fn light(&self, kind: mc_light::Kind, p: mc_light::Pos) -> u8 {
        if p.1 >= self.top {
            return if kind == mc_light::Kind::Sky { 15 } else { 0 };
        }
        let Some(chunk) = self.chunk(p) else { return 0 };
        let local = (p.1 - self.min_y) as usize;
        let index = ((local & 15) << 8) | (((p.2 & 15) as usize) << 4) | (p.0 & 15) as usize;
        stored_level(chunk, kind, local / 16, index)
    }

    fn top(&self) -> i32 {
        self.top
    }

    fn set_light(&mut self, kind: mc_light::Kind, p: mc_light::Pos, level: u8) {
        if p.1 < self.min_y || p.1 >= self.top {
            return;
        }
        let key = (p.0 >> 4, p.2 >> 4);
        let min_y = self.min_y;
        let mut touched = std::mem::take(&mut self.touched);
        if let Some(slot) = self.slot_mut(key) {
            set_slot_light(slot, &mut touched, key, min_y, kind, p, level);
        }
        self.touched = touched;
    }
}

/// Sets one light value in a chunk's slot, making its array real first.
fn set_slot_light(
    slot: &mut Slot,
    touched: &mut Vec<(i32, i32)>,
    key: (i32, i32),
    min_y: i32,
    kind: mc_light::Kind,
    p: mc_light::Pos,
    level: u8,
) {
    {
        let Some(chunk) = slot.chunk.as_mut() else {
            return;
        };
        let local = (p.1 - min_y) as usize;
        let section = local / 16;
        let index = ((local & 15) << 8) | (((p.2 & 15) as usize) << 4) | (p.0 & 15) as usize;
        if chunk.light.len() < chunk.sections.len() {
            chunk
                .light
                .resize(chunk.sections.len(), mc_world::SectionLight::default());
        }
        if stored_level(chunk, kind, section, index) == level {
            return;
        }
        // Make the array real, with the values it stood for.
        let missing = match kind {
            mc_light::Kind::Sky => chunk.light[section].sky.is_none(),
            mc_light::Kind::Block => chunk.light[section].block.is_none(),
        };
        if missing && kind == mc_light::Kind::Sky {
            // Sections below without sky light read it through this one;
            // they keep what they read now, so a later change here does
            // not silently change them too.
            let mut below = Vec::new();
            let mut t = section;
            while t > 0 {
                t -= 1;
                if chunk.light[t].sky.is_some() {
                    break;
                }
                below.push(t);
            }
            let arrays: Vec<(usize, Vec<u8>)> = below
                .into_iter()
                .map(|t| {
                    let a = (0..2048)
                        .map(|i| {
                            stored_level(chunk, kind, t, 2 * i)
                                | (stored_level(chunk, kind, t, 2 * i + 1) << 4)
                        })
                        .collect();
                    (t, a)
                })
                .collect();
            for (t, a) in arrays {
                chunk.light[t].sky = Some(a);
            }
        }
        if missing {
            let array = (0..2048)
                .map(|i| {
                    stored_level(chunk, kind, section, 2 * i)
                        | (stored_level(chunk, kind, section, 2 * i + 1) << 4)
                })
                .collect();
            match kind {
                mc_light::Kind::Sky => chunk.light[section].sky = Some(array),
                mc_light::Kind::Block => chunk.light[section].block = Some(array),
            }
        }
        let array = match kind {
            mc_light::Kind::Sky => chunk.light[section].sky.as_mut(),
            mc_light::Kind::Block => chunk.light[section].block.as_mut(),
        }
        .expect("made above");
        let byte = &mut array[index / 2];
        *byte = if index.is_multiple_of(2) {
            (*byte & 0xF0) | level
        } else {
            (*byte & 0x0F) | (level << 4)
        };
        slot.frame = None;
        slot.changes += 1;
        slot.dirty = true;
        // Most writes land in the chunk written last.
        if touched.last() != Some(&key) && !touched.contains(&key) {
            touched.push(key);
        }
    }
}

/// Light as stored: a section's array is sent when the world has one. Like
/// vanilla, sections without stored light are left out of all masks.
fn stored_light(chunk: &Chunk) -> LightData {
    let mut light = LightData::default();
    let words = (chunk.light.len() + 2).div_ceil(64);
    let (mut sky_mask, mut block_mask) = (vec![0u64; words], vec![0u64; words]);
    for (i, section) in chunk.light.iter().enumerate() {
        let bit = i + 1; // bit 0 is the section below the world
        if let Some(sky) = &section.sky {
            sky_mask[bit / 64] |= 1 << (bit % 64);
            light.sky.push(sky.clone());
        }
        if let Some(block) = &section.block {
            block_mask[bit / 64] |= 1 << (bit % 64);
            light.block.push(block.clone());
        }
    }
    light.sky_mask = sky_mask;
    light.block_mask = block_mask;
    light
}

const CLOCKS_FILE: &str = "data/minecraft/world_clocks.dat";
const GAME_RULES_FILE: &str = "data/minecraft/game_rules.dat";

/// A gzip-compressed NBT file (level.dat, data/*.dat).
pub(crate) fn read_nbt_gz(path: &Path) -> Result<mc_nbt::Compound> {
    read_gz(path)
}

pub(crate) fn write_nbt_gz(path: &Path, root: &mc_nbt::Compound) -> Result<()> {
    write_gz(path, root)
}

pub(crate) fn read_nbt_gz_compound(path: &Path) -> Result<mc_nbt::Compound> {
    read_gz(path)
}

fn read_gz(path: &Path) -> Result<mc_nbt::Compound> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut bytes)?;
    match mc_nbt::read_named(&bytes)? {
        (_, Tag::Compound(root), _) => Ok(root),
        _ => bail!("{}: root is not a compound", path.display()),
    }
}

/// Writes next to `path` and renames over it, so a crash leaves either the
/// old or the new file.
fn write_gz(path: &Path, root: &mc_nbt::Compound) -> Result<()> {
    use std::io::Write as _;
    let mut bytes = Vec::new();
    mc_nbt::write_named("", &Tag::Compound(root.clone()), &mut bytes)?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&bytes)?;
    let compressed = encoder.finish()?;
    let tmp = path.with_extension("dat_tmp");
    std::fs::write(&tmp, compressed)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Spawn point from level.dat: `Data.spawn` (26.x) or `Data.SpawnX/Y/Z` (older).
fn read_spawn(path: &Path) -> Result<((f64, f64, f64), f32)> {
    let root = read_gz(path)?;
    let Some(Tag::Compound(data)) = root.get("Data") else {
        bail!("level.dat has no Data");
    };
    if let Some(Tag::Compound(spawn)) = data.get("spawn")
        && let Some(Tag::IntArray(pos)) = spawn.get("pos")
        && let [x, y, z] = pos[..]
    {
        let yaw = match spawn.get("yaw") {
            Some(Tag::Float(yaw)) => *yaw,
            _ => 0.0,
        };
        return Ok(((f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5), yaw));
    }
    let int = |key: &str| match data.get(key) {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    };
    match (int("SpawnX"), int("SpawnY"), int("SpawnZ")) {
        (Some(x), Some(y), Some(z)) => {
            Ok(((f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5), 0.0))
        }
        _ => bail!("level.dat has no spawn position"),
    }
}
