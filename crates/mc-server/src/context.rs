//! State shared by all connections: game data, prebuilt configuration packets,
//! the world and the list of online players.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{Context as _, Result};
use mc_data::{McData, SYNCHRONIZED_REGISTRIES, resolve_tags};
use mc_nbt::Tag;
use mc_protocol::packets::configuration::{RegistryData, RegistryEntry, TagRegistry, UpdateTags};
use mc_protocol::packets::play::{LevelChunkWithLight, LightData, PlayerInfoEntry, heightmap};
use mc_protocol::packets::{GameProfile, KnownPack};
use mc_protocol::{Direction, PacketIds, State};
use mc_worldgen::{FlatGenerator, FlatLayer};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::Config;
use crate::auth::ServerKey;

/// A player who passed login.
#[derive(Clone, Debug)]
pub struct OnlinePlayer {
    pub profile: GameProfile,
    pub game_mode: i32,
}

impl OnlinePlayer {
    pub fn info_entry(&self) -> PlayerInfoEntry {
        PlayerInfoEntry {
            profile: self.profile.clone(),
            game_mode: self.game_mode,
            listed: true,
            latency_ms: 0,
            display_name: None,
            list_priority: 0,
            show_hat: true,
        }
    }
}

/// Things every connection in play needs to hear about.
#[derive(Clone, Debug)]
pub enum Event {
    Chat(Tag),
    PlayerJoined(OnlinePlayer),
    PlayerLeft(u128),
}

pub struct ServerContext {
    pub config: Config,
    pub data: McData,
    pub ids: PacketIds,
    /// The vanilla data pack as the client knows it.
    pub core_pack: KnownPack,
    /// Registry Data packets; entry order defines the network ids.
    pub registries: Vec<RegistryData>,
    pub tags: UpdateTags,
    pub world: FlatWorld,
    /// RSA key for the online-mode key exchange (`None` in offline mode).
    pub key: Option<ServerKey>,
    /// Logged-in players by UUID.
    pub players: Mutex<HashMap<u128, OnlinePlayer>>,
    pub events: broadcast::Sender<Event>,
    next_entity_id: AtomicI32,
}

impl std::fmt::Debug for ServerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerContext")
            .field("version", &self.data.version)
            .finish_non_exhaustive()
    }
}

impl ServerContext {
    pub fn new(config: Config, data: McData) -> Result<Self> {
        let mut ids = PacketIds::new();
        for packet in &data.packets {
            if let (Some(state), Some(direction)) = (
                State::from_report_name(&packet.phase),
                Direction::from_report_name(&packet.direction),
            ) {
                ids.insert(state, direction, &packet.name, packet.id);
            }
        }

        let pack = data.data_pack();
        // Network ids of data-driven registry entries are their position in our list.
        let mut synced_ids: HashMap<String, HashMap<String, i32>> = HashMap::new();
        let mut registries = Vec::new();
        for &registry in SYNCHRONIZED_REGISTRIES {
            let entries = pack.registry_entries(registry)?;
            synced_ids.insert(
                registry.to_owned(),
                entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (e.clone(), i as i32))
                    .collect(),
            );
            registries.push(RegistryData {
                registry: registry.to_owned(),
                entries: entries
                    .into_iter()
                    .map(|id| RegistryEntry { id, data: None })
                    .collect(),
            });
        }

        let key = if config.online_mode {
            Some(ServerKey::generate()?)
        } else {
            None
        };
        let tags = build_tags(&data, &synced_ids)?;
        let world = FlatWorld::new(&data, &synced_ids)?;
        info!(
            "loaded {} packets, {} synchronized registries, tags for {} registries",
            ids.len(),
            registries.len(),
            tags.registries.len()
        );

        Ok(Self {
            core_pack: KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: data.version.name.clone(),
            },
            config,
            ids,
            registries,
            tags,
            world,
            key,
            players: Mutex::new(HashMap::new()),
            events: broadcast::channel(256).0,
            next_entity_id: AtomicI32::new(1),
            data,
        })
    }

    pub fn next_entity_id(&self) -> i32 {
        self.next_entity_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn online_count(&self) -> usize {
        self.players.lock().expect("players lock").len()
    }
}

/// Tags for every registry the client knows: synchronized ones (ids from our
/// Registry Data order) and built-in ones (ids from the registries report).
fn build_tags(data: &McData, synced: &HashMap<String, HashMap<String, i32>>) -> Result<UpdateTags> {
    let pack = data.data_pack();
    let mut registries = Vec::new();
    for path in pack.tag_registries()? {
        let registry = format!("minecraft:{path}");
        let ids = match synced
            .get(&registry)
            .or_else(|| data.builtin.get(&registry))
        {
            Some(ids) => ids,
            None => {
                debug!("not sending tags of server-only registry {registry}");
                continue;
            }
        };
        let (resolved, problems) =
            resolve_tags(&pack.tags(&registry)?, |name| ids.get(name).copied());
        for problem in &problems {
            warn!("{registry}: {problem}");
        }
        registries.push(TagRegistry {
            registry,
            tags: resolved.into_iter().collect(),
        });
    }
    Ok(UpdateTags { registries })
}

/// The superflat overworld everyone plays in for now.
#[derive(Debug)]
pub struct FlatWorld {
    pub dimension: String,
    pub dimension_type_id: i32,
    pub min_y: i32,
    pub height: u32,
    pub sea_level: i32,
    pub spawn: (f64, f64, f64),
    /// Every chunk of a flat world is identical except for its coordinates, so
    /// the expensive part is encoded once.
    template: ChunkTemplate,
}

#[derive(Debug)]
struct ChunkTemplate {
    sections: Vec<u8>,
    heightmaps: Vec<(i32, Vec<i64>)>,
    light: LightData,
}

impl FlatWorld {
    fn new(data: &McData, synced: &HashMap<String, HashMap<String, i32>>) -> Result<Self> {
        let dimension = "minecraft:overworld".to_owned();
        let dimension_type = data
            .data_pack()
            .read_entry("minecraft:dimension_type", &dimension)?;
        let min_y = dimension_type["min_y"]
            .as_i64()
            .context("dimension type min_y")? as i32;
        let height = dimension_type["height"]
            .as_u64()
            .context("dimension type height")? as u32;
        let dimension_type_id = synced["minecraft:dimension_type"][&dimension];
        let biomes = &synced["minecraft:worldgen/biome"];
        let plains = biomes["minecraft:plains"] as u32;

        let state = |name: &str| {
            data.blocks
                .default_state(name)
                .with_context(|| format!("block {name} not in the blocks report"))
        };
        let generator = FlatGenerator {
            layers: vec![
                FlatLayer {
                    state: state("minecraft:bedrock")?,
                    thickness: 1,
                },
                FlatLayer {
                    state: state("minecraft:dirt")?,
                    thickness: 2,
                },
                FlatLayer {
                    state: state("minecraft:grass_block")?,
                    thickness: 1,
                },
            ],
            biome: plains,
            air: state("minecraft:air")?,
        };
        let chunk = generator.generate(0, 0, min_y, height);
        let blocks = &data.blocks;
        let solid = |s: u32| !blocks.is_air(s);
        let surface = chunk.heightmap(solid);
        let template = ChunkTemplate {
            sections: chunk.write_sections(blocks, biomes.len() as u32),
            heightmaps: vec![
                (heightmap::WORLD_SURFACE, surface.clone()),
                (heightmap::MOTION_BLOCKING, surface.clone()),
                (heightmap::MOTION_BLOCKING_NO_LEAVES, surface),
            ],
            light: full_sky_light(height),
        };

        let surface_y = generator.surface_y(min_y);
        Ok(Self {
            dimension,
            dimension_type_id,
            min_y,
            height,
            sea_level: surface_y,
            spawn: (0.5, f64::from(surface_y), 0.5),
            template,
        })
    }

    pub fn chunk_packet(&self, x: i32, z: i32) -> LevelChunkWithLight {
        LevelChunkWithLight {
            x,
            z,
            heightmaps: self.template.heightmaps.clone(),
            data: self.template.sections.clone(),
            block_entities: Vec::new(),
            light: self.template.light.clone(),
        }
    }
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
