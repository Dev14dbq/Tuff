//! The overworld players are in: a superflat generator, or a world folder saved
//! by vanilla (Anvil region files). One dimension for now.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, bail};
use mc_data::{Blocks, McData};
use mc_nbt::Tag;
use mc_protocol::packets::play::{ChunkBlockEntity, LevelChunkWithLight, LightData, heightmap};
use mc_storage::{Decoded, Palettes, RegionStorage, decode_chunk};
use mc_world::Chunk;
use mc_worldgen::{FlatGenerator, FlatLayer};
use tracing::{info, warn};

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
}

#[derive(Debug)]
enum Source {
    /// Every flat chunk is identical except for its coordinates.
    Flat(ChunkTemplate),
    Anvil(Box<Anvil>),
}

#[derive(Debug, Clone)]
struct ChunkTemplate {
    sections: Vec<u8>,
    heightmaps: Vec<(i32, Vec<i64>)>,
    light: LightData,
}

impl ChunkTemplate {
    fn packet(&self, x: i32, z: i32) -> LevelChunkWithLight {
        LevelChunkWithLight {
            x,
            z,
            heightmaps: self.heightmaps.clone(),
            data: self.sections.clone(),
            block_entities: Vec::new(),
            light: self.light.clone(),
        }
    }
}

#[derive(Debug)]
struct Anvil {
    storage: Mutex<RegionStorage>,
    /// Encoded chunks; a world folder is read-only for now (ROADMAP Ф2).
    cache: Mutex<HashMap<(i32, i32), Arc<LevelChunkWithLight>>>,
    /// Sent where the world has no finished chunk: air with full daylight.
    empty: ChunkTemplate,
    biome_ids: HashMap<String, u32>,
    biome_count: u32,
    fallback_biome: u32,
    air: u32,
    block_entity_types: HashMap<String, i32>,
    unresolved_reported: AtomicU32,
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

fn template(chunk: &Chunk, blocks: &Blocks, biome_count: u32, height: u32) -> ChunkTemplate {
    let surface = chunk.heightmap(|s| !blocks.is_air(s));
    ChunkTemplate {
        sections: chunk.write_sections(blocks, biome_count),
        heightmaps: vec![
            (heightmap::WORLD_SURFACE, surface.clone()),
            (heightmap::MOTION_BLOCKING, surface.clone()),
            (heightmap::MOTION_BLOCKING_NO_LEAVES, surface),
        ],
        light: full_sky_light(height),
    }
}

impl World {
    /// Classic superflat: bedrock, two dirt, grass.
    pub fn flat(data: &McData, synced: &SyncedIds) -> Result<Self> {
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
        let chunk = generator.generate(0, 0, dim.min_y, dim.height);
        let surface_y = generator.surface_y(dim.min_y);
        Ok(Self {
            source: Source::Flat(template(
                &chunk,
                &data.blocks,
                biomes.len() as u32,
                dim.height,
            )),
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
    pub fn open(dir: &Path, data: &McData, synced: &SyncedIds) -> Result<Self> {
        let dim = overworld(data, synced)?;
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
        let empty_chunk = Chunk::new(0, 0, dim.min_y, dim.height, air, fallback_biome);
        let block_entity_types = data
            .builtin
            .get("minecraft:block_entity_type")
            .context("block entity types missing from the registries report")?
            .clone();
        let name = dir.file_name().map_or_else(
            || dir.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        info!("world {} from {}", name, regions.display());
        Ok(Self {
            source: Source::Anvil(Box::new(Anvil {
                storage: Mutex::new(RegionStorage::new(regions)),
                cache: Mutex::new(HashMap::new()),
                empty: template(&empty_chunk, &data.blocks, biomes.len() as u32, dim.height),
                biome_ids: biomes.iter().map(|(k, &v)| (k.clone(), v as u32)).collect(),
                biome_count: biomes.len() as u32,
                fallback_biome,
                air,
                block_entity_types,
                unresolved_reported: AtomicU32::new(0),
            })),
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

    pub fn chunk_packet(&self, x: i32, z: i32, blocks: &Blocks) -> Arc<LevelChunkWithLight> {
        match &self.source {
            Source::Flat(template) => Arc::new(template.packet(x, z)),
            Source::Anvil(anvil) => anvil.chunk_packet(x, z, blocks, self.min_y, self.height),
        }
    }
}

impl Anvil {
    fn chunk_packet(
        &self,
        x: i32,
        z: i32,
        blocks: &Blocks,
        min_y: i32,
        height: u32,
    ) -> Arc<LevelChunkWithLight> {
        if let Some(packet) = self.cache.lock().expect("chunk cache").get(&(x, z)) {
            return packet.clone();
        }
        let packet = Arc::new(match self.load(x, z, blocks, min_y, height) {
            Ok(Some(packet)) => packet,
            Ok(None) => self.empty.packet(x, z),
            Err(err) => {
                warn!("chunk {x},{z}: {err:#}; sending it empty");
                self.empty.packet(x, z)
            }
        });
        self.cache
            .lock()
            .expect("chunk cache")
            .insert((x, z), packet.clone());
        packet
    }

    /// `None` if the world has no finished chunk here.
    fn load(
        &self,
        x: i32,
        z: i32,
        blocks: &Blocks,
        min_y: i32,
        height: u32,
    ) -> Result<Option<LevelChunkWithLight>> {
        let Some(tag) = self
            .storage
            .lock()
            .expect("region storage")
            .read_chunk(x, z)?
        else {
            return Ok(None);
        };
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
        Ok(Some(self.encode(&chunk, blocks)))
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
            light: stored_light(chunk),
        }
    }
}

/// What the client gets for a block entity in a chunk packet: only what it
/// needs to draw it, never container contents.
///
/// Observed against vanilla 26.3 (`cargo xtask chunk-diff`): chests get no data
/// at all, spawners get their data without `SpawnPotentials` and `components`.
/// The other rules are assumptions until checked the same way.
fn client_data(id: &str, data: &mc_nbt::Compound) -> Option<mc_nbt::Compound> {
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

/// Spawn point from level.dat: `Data.spawn` (26.x) or `Data.SpawnX/Y/Z` (older).
fn read_spawn(path: &PathBuf) -> Result<((f64, f64, f64), f32)> {
    let raw = std::fs::read(path)?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut bytes)?;
    let (_, root, _) = mc_nbt::read_named(&bytes)?;
    let Tag::Compound(root) = root else {
        bail!("level.dat root is not a compound");
    };
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
