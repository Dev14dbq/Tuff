//! State shared by all connections: game data, prebuilt configuration packets,
//! the world and the list of online players.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{Context as _, Result};
use mc_data::{McData, SYNCHRONIZED_REGISTRIES, resolve_tags};
use mc_nbt::Tag;
use mc_protocol::packets::configuration::{RegistryData, RegistryEntry, TagRegistry, UpdateTags};
use mc_protocol::packets::play::PlayerInfoEntry;
use mc_protocol::packets::{GameProfile, KnownPack};
use mc_protocol::{Direction, PacketIds, State};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::Config;
use crate::auth::ServerKey;
use crate::world::World;

/// A player who passed login.
#[derive(Clone, Debug)]
pub struct OnlinePlayer {
    pub profile: GameProfile,
    pub game_mode: i32,
    pub entity_id: i32,
    /// Displayed skin parts, sent to others as entity metadata.
    pub skin_parts: u8,
    /// Where the player is; `None` until they spawn in play.
    pub pose: Option<Pose>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
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
    PlayerLeft {
        uuid: u128,
        entity_id: i32,
    },
    PlayerMoved {
        uuid: u128,
        entity_id: i32,
        pose: Pose,
    },
    SkinParts {
        uuid: u128,
        entity_id: i32,
        parts: u8,
    },
}

pub struct ServerContext {
    pub config: Config,
    pub data: McData,
    pub ids: PacketIds,
    /// The vanilla data pack as the client knows it.
    pub core_pack: KnownPack,
    /// Registry Data packets; entry order defines the network ids.
    pub registries: Vec<RegistryData>,
    /// The same registries with every entry's data as NBT, for clients that do
    /// not know the vanilla data pack.
    pub registries_with_data: Vec<RegistryData>,
    pub tags: UpdateTags,
    pub world: World,
    /// `minecraft:player` in `minecraft:entity_type`.
    pub player_entity_type: i32,
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
        let mut registries_with_data = Vec::new();
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
            let mut with_data = Vec::with_capacity(entries.len());
            for id in &entries {
                let json = pack.read_entry(registry, id)?;
                let mut data = mc_data::json_to_nbt(&json)
                    .with_context(|| format!("{registry} {id} is null"))?;
                mc_data::strip_server_only(registry, &mut data);
                with_data.push(RegistryEntry {
                    id: id.clone(),
                    data: Some(data),
                });
            }
            registries_with_data.push(RegistryData {
                registry: registry.to_owned(),
                entries: with_data,
            });
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
        let world = match &config.world {
            Some(dir) => World::open(dir, &data, &synced_ids)?,
            None => World::flat(&data, &synced_ids)?,
        };
        let player_entity_type = data
            .builtin
            .id("minecraft:entity_type", "minecraft:player")
            .context("minecraft:player missing from the registries report")?;
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
            registries_with_data,
            tags,
            world,
            player_entity_type,
            key,
            players: Mutex::new(HashMap::new()),
            // Every movement is an event; leave room for many moving players.
            events: broadcast::channel(4096).0,
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
