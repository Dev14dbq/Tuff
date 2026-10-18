//! State shared by all connections: game data, prebuilt configuration packets,
//! the world and the list of online players.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{Context as _, Result};
use mc_data::{McData, SYNCHRONIZED_REGISTRIES, resolve_tags};
use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::configuration::{RegistryData, RegistryEntry, TagRegistry, UpdateTags};
use mc_protocol::packets::play::{ItemStack, PlayerInfoEntry, SetTime, TickingState, TickingStep};
use mc_protocol::packets::{GameProfile, KnownPack};
use mc_protocol::{Direction, PacketIds, State};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::Config;
use crate::auth::ServerKey;
use crate::blocks::BlockRules;
use crate::tick::{Request, WorldTime};
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
    /// What others see in their main hand.
    pub main_hand: Option<ItemStack>,
    /// Flying (creative players who double-jumped, and spectators).
    pub flying: bool,
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

#[derive(Clone, Copy, Debug)]
pub struct EntityTypes {
    pub falling_block: i32,
    pub item: i32,
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
    /// Blocks changed; `by` is the player who did it (they already got
    /// their own updates).
    BlocksChanged {
        by: u128,
        changes: Vec<(Position, u32)>,
    },
    /// A player broke a block: others see particles and hear it.
    BlockBroken {
        by: u128,
        position: Position,
        state: u32,
    },
    /// The time every 20 ticks (world age only; the client runs the clocks),
    /// or clocks that were set or stopped.
    Time(SetTime),
    /// The tick rate changed or the game was frozen or unfrozen.
    TickingState(TickingState),
    /// A frozen game runs this many ticks.
    TickingStep(TickingStep),
    /// Move a player (a command).
    Teleport {
        uuid: u128,
        pose: Pose,
    },
    /// An entity appeared (players see it if they have its chunk).
    EntitySpawned(mc_entity::Entity),
    /// Light changed in these chunks.
    LightChanged(Vec<(i32, i32)>),
    /// A player started (`hand` 0 main, 1 off) or stopped (`None`) using an
    /// item.
    UsingItem {
        uuid: u128,
        entity_id: i32,
        hand: Option<i32>,
    },
    /// A block event clients play (a piston moving).
    BlockAction {
        position: mc_protocol::Position,
        action: u8,
        param: u8,
        /// `minecraft:block` registry id.
        block: i32,
    },
    /// A furnace's progress changed (its window's four properties).
    FurnaceProgress {
        position: mc_protocol::Position,
        values: [i16; 4],
    },
    /// A player finished eating or drinking.
    UsedItem {
        uuid: u128,
        entity_id: i32,
    },
    /// Entities moved this tick.
    EntitiesMoved(Vec<(i32, [f64; 3], bool)>),
    EntitiesRemoved(Vec<i32>),
    /// A player's game mode changed (0 survival, 1 creative, 2 adventure,
    /// 3 spectator).
    GameMode {
        uuid: u128,
        mode: i32,
    },
    /// One slot of a player's inventory window (container numbering) changed.
    InventorySlot {
        uuid: u128,
        slot: i16,
        item: Option<ItemStack>,
    },
    /// Slots of an open container changed (for its other viewers).
    ContainerSlots {
        position: Position,
        viewers: Vec<u128>,
        slots: Vec<(usize, Option<ItemStack>)>,
    },
    /// A container block is gone; windows on it close.
    ContainerGone {
        position: Position,
    },
    /// A player picked up an item entity (the pickup animation).
    ItemTaken {
        item_entity: i32,
        collector: i32,
        count: i32,
    },
    /// A player's health or food changed (for them).
    Health {
        uuid: u128,
        health: mc_protocol::packets::play::SetHealth,
    },
    /// A player was hurt (everyone who sees them).
    Hurt {
        uuid: u128,
        entity_id: i32,
        damage_type: i32,
    },
    /// A player died.
    Died {
        uuid: u128,
        entity_id: i32,
        message: Tag,
    },
    /// Disconnect a player with a reason.
    Kick {
        uuid: u128,
        reason: Tag,
    },
    /// A player's arm swings (`times` in a row, as vanilla does on placing).
    Swing {
        uuid: u128,
        entity_id: i32,
        times: u8,
    },
    /// What a player holds in the main hand changed.
    Equipment {
        uuid: u128,
        entity_id: i32,
        main_hand: Option<ItemStack>,
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
    /// How placing and breaking blocks works (until block behaviours exist).
    pub rules: BlockRules,
    /// Neighbour-dependent block shapes.
    pub shapes: mc_tick::Shapes,
    /// Block and fluid ticks waiting to run (tick thread only).
    pub ticks: Mutex<mc_tick::ScheduledTicks>,
    /// Entities other than players (tick thread only, read by joining players).
    pub entities: Mutex<Vec<mc_entity::Entity>>,
    /// Inventories of the players in play.
    pub inventories: Mutex<HashMap<u128, crate::interact::Hands>>,
    /// Chests and barrels someone has open.
    pub containers: Mutex<crate::containers::Containers>,
    /// What broken blocks drop.
    pub loot: crate::loot::BlockLoot,
    /// Crafting recipes.
    pub recipes: crate::crafting::Recipes,
    /// Health and food of players in play.
    pub vitals: Mutex<crate::vitals::AllVitals>,
    /// Stack sizes, food and more of every item.
    pub items: crate::items::Items,
    /// Piston block events waiting for this tick (tick thread).
    pub block_events: Mutex<Vec<(mc_tick::BlockPos, u32, u8, u8)>>,
    /// Blocks on their way, moved by pistons (tick thread).
    pub moving: Mutex<Vec<(mc_tick::BlockPos, mc_tick::pistons::Moving)>>,
    /// Burn times of fuels.
    pub fuels: crate::furnaces::Fuels,
    /// Furnaces burning or cooking (tick thread).
    pub furnaces: Mutex<crate::furnaces::Active>,
    /// Containers whose contents changed, for comparators (tick thread).
    pub container_updates: Mutex<Vec<mc_protocol::Position>>,
    /// Players eating or drinking (tick thread counts down).
    pub using: Mutex<HashMap<u128, crate::vitals::Using>>,
    /// 0 peaceful, 1 easy, 2 normal, 3 hard (level.dat).
    pub difficulty: i32,
    /// `minecraft:damage_type` entries in Registry Data order.
    damage_types: Vec<String>,
    /// Player files of the world folder (none for the flat test world).
    pub player_files: Option<crate::players::Store>,
    /// Game mode of players who never played here (level.dat `GameType`).
    pub default_game_mode: i32,
    /// `ops.json`, `whitelist.json`, bans and the user cache.
    pub lists: Mutex<crate::lists::Lists>,
    /// Whether only listed players may join (`white-list`).
    pub white_list: std::sync::atomic::AtomicBool,
    /// `minecraft:entity_type` ids of the entities we have.
    pub entity_types: EntityTypes,
    /// `minecraft:player` in `minecraft:entity_type`.
    pub player_entity_type: i32,
    /// RSA key for the online-mode key exchange (`None` in offline mode).
    pub key: Option<ServerKey>,
    /// Logged-in players by UUID.
    pub players: Mutex<HashMap<u128, OnlinePlayer>>,
    pub events: broadcast::Sender<Event>,
    /// The world's age and clocks, advanced by the tick thread.
    pub time: Mutex<WorldTime>,
    /// Player actions for the tick thread.
    pub actions: std::sync::mpsc::Sender<Request>,
    /// Taken by the tick thread when the server starts.
    pub(crate) action_queue: Mutex<Option<std::sync::mpsc::Receiver<Request>>>,
    /// Tick rate and whether the game is frozen, for players who join.
    pub ticking: Mutex<TickingState>,
    /// Notified by `/stop`.
    pub shutdown: tokio::sync::Notify,
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
    pub fn white_list(&self) -> bool {
        self.white_list.load(std::sync::atomic::Ordering::Relaxed)
    }

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
        let rules = BlockRules::new(&data).map_err(anyhow::Error::msg)?;
        let light = light_tables(&data, &config);
        let items = match data.builtin.get("minecraft:item") {
            Some(ids) => crate::items::Items::load(&config.data_dir, ids)?,
            None => crate::items::Items::default(),
        };
        let fuels = match data.builtin.get("minecraft:item") {
            Some(ids) => crate::furnaces::Fuels::load(&data, &items, ids),
            None => crate::furnaces::Fuels::default(),
        };
        let warned_observed = std::cell::Cell::new(false);
        let observed = |file: &str| {
            let path = config.data_dir.join("observed").join(file);
            let table = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| serde_json::from_value::<Vec<i32>>(v["states"].clone()).ok());
            if table.is_none() && !warned_observed.replace(true) {
                warn!(
                    "no {} (cargo xtask observe): block facts are guessed",
                    path.display()
                );
            }
            table
        };
        let shapes = mc_tick::Shapes::new(
            &data,
            observed("fence_attach.json"),
            observed("conductor.json"),
        )
        .with_instruments(observed("instrument.json"))
        .with_support(observed("support.json"))
        .with_push(observed("push.json"));
        let world = match &config.world {
            Some(dir) => {
                let world_shapes = mc_tick::Shapes::new(
                    &data,
                    observed("fence_attach.json"),
                    observed("conductor.json"),
                )
                .with_instruments(observed("instrument.json"))
                .with_support(observed("support.json"))
                .with_push(observed("push.json"));
                World::open(
                    dir,
                    &data,
                    &synced_ids,
                    light,
                    &rules,
                    config.seed,
                    world_shapes,
                    crate::generation::NewWorld {
                        game_mode: config.new_world_game_mode,
                        difficulty: config.new_world_difficulty,
                        hardcore: false,
                    },
                )?
            }
            None => World::flat(&data, &synced_ids, &rules, light)?,
        };
        let time = world.load_time(&synced_ids)?;
        let player_files = config.world.as_ref().map(|dir| {
            crate::players::Store::new(
                dir,
                data.builtin
                    .get("minecraft:item")
                    .unwrap_or(&HashMap::new()),
            )
        });
        let default_game_mode = world.default_game_mode();
        let difficulty = world.difficulty();
        let damage_types = registries
            .iter()
            .find(|r| r.registry == "minecraft:damage_type")
            .map(|r| r.entries.iter().map(|e| e.id.clone()).collect())
            .unwrap_or_default();
        let (actions, action_queue) = std::sync::mpsc::channel();
        let entity_type = |name: &str| {
            data.builtin
                .id("minecraft:entity_type", name)
                .with_context(|| format!("{name} missing from the registries report"))
        };
        let player_entity_type = entity_type("minecraft:player")?;
        tracing::debug!(
            "loaded {} packets, {} synchronized registries, tags for {} registries",
            ids.len(),
            registries.len(),
            tags.registries.len()
        );

        let mut lists = match &config.server_dir {
            Some(dir) => crate::lists::Lists::load(dir),
            None => crate::lists::Lists::default(),
        };
        for name in &config.ops {
            lists.add_op_name(name);
        }
        let white_list = config.white_list;
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
            rules,
            shapes,
            ticks: Mutex::new(mc_tick::ScheduledTicks::default()),
            entities: Mutex::new(Vec::new()),
            inventories: Mutex::new(HashMap::new()),
            containers: Mutex::new(HashMap::new()),
            loot: crate::loot::BlockLoot::load(&data),
            recipes: crate::crafting::Recipes::load(&data),
            vitals: Mutex::new(HashMap::new()),
            items,
            using: Mutex::new(HashMap::new()),
            container_updates: Mutex::new(Vec::new()),
            fuels,
            furnaces: Mutex::new(HashMap::new()),
            block_events: Mutex::new(Vec::new()),
            moving: Mutex::new(Vec::new()),
            difficulty,
            damage_types,
            player_files,
            default_game_mode,
            lists: Mutex::new(lists),
            white_list: std::sync::atomic::AtomicBool::new(white_list),
            entity_types: EntityTypes {
                falling_block: entity_type("minecraft:falling_block")?,
                item: entity_type("minecraft:item")?,
            },
            player_entity_type,
            key,
            players: Mutex::new(HashMap::new()),
            // Every movement is an event; leave room for many moving players.
            events: broadcast::channel(4096).0,
            time: Mutex::new(time),
            actions,
            action_queue: Mutex::new(Some(action_queue)),
            ticking: Mutex::new(TickingState {
                tick_rate: 20.0,
                frozen: false,
            }),
            shutdown: tokio::sync::Notify::new(),
            next_entity_id: AtomicI32::new(1),
            data,
        })
    }

    /// Network id of a damage type.
    pub fn damage_type(&self, name: &str) -> i32 {
        self.damage_types
            .iter()
            .position(|d| d == name)
            .unwrap_or(0) as i32
    }

    pub fn next_entity_id(&self) -> i32 {
        self.next_entity_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn online_count(&self) -> usize {
        self.players.lock().expect("players lock").len()
    }
}

/// Opacity and emission per state. Opacity comes from
/// `observed/opacity.json`: how much sky light a block takes away going down
/// into a closed cell under it. Light at 15 passes a clear block unchanged,
/// any other block takes `max(1, opacity)` and the cell one more, so a value
/// `m` means opacity `m - 1` (0 stays 0, 15 stays 15). A state that did not
/// stay as placed (sand falls, water flows) takes the value most other states
/// of its block show; without the file, blocks a fence attaches to all round
/// are opaque.
fn light_tables(data: &McData, config: &Config) -> mc_light::Tables {
    let read = |file: &str| -> Option<Vec<i32>> {
        let text = std::fs::read_to_string(config.data_dir.join("observed").join(file)).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        serde_json::from_value(v["states"].clone()).ok()
    };
    let (observed, attach) = (read("opacity.json"), read("fence_attach.json"));
    let blocks = &data.blocks;
    let states = blocks.state_count() as usize;
    let opacity_of = |m: i32| match m {
        0 => 0u8,
        1..=14 => (m - 1) as u8,
        _ => 15,
    };
    // The usual value of each block's states that stayed as placed (apart
    // for waterlogged ones, which dim light).
    let wet = |state: u32| blocks.property(state, "waterlogged") == Some("true");
    let mut usual: HashMap<(&str, bool), HashMap<i32, usize>> = HashMap::new();
    if let Some(observed) = &observed {
        for (state, &m) in observed.iter().enumerate() {
            if (0..16).contains(&m)
                && let Some(name) = blocks.block_name(state as u32)
            {
                *usual
                    .entry((name, wet(state as u32)))
                    .or_default()
                    .entry(m)
                    .or_default() += 1;
            }
        }
    }
    let mut opacity = Vec::with_capacity(states);
    let mut emission = Vec::with_capacity(states);
    let mut faces = Vec::with_capacity(states);
    for state in 0..states as u32 {
        let name = blocks.block_name(state).unwrap_or("");
        let m = observed
            .as_ref()
            .and_then(|t| t.get(state as usize).copied());
        let solid = || {
            attach
                .as_ref()
                .and_then(|t| t.get(state as usize))
                .is_some_and(|&a| a >= 0 && a & 15 == 15)
        };
        let shaped = light_faces(name, &|p| blocks.property(state, p));
        faces.push(shaped.unwrap_or(0));
        if shaped.is_some() {
            // Observed: these pass light like air, waterlogged ones take 2.
            opacity.push(if wet(state) { 2 } else { 0 });
        } else {
            opacity.push(match m {
                Some(m) if (0..16).contains(&m) => opacity_of(m),
                Some(m) if m >= 16 => {
                    let key = (name, wet(state));
                    match usual.get(&key).and_then(|c| c.iter().max_by_key(|e| e.1)) {
                        Some((&m, _)) => opacity_of(m),
                        None => opacity_of(m - 16),
                    }
                }
                _ if blocks.is_air(state) => 0,
                _ => {
                    if solid() {
                        15
                    } else {
                        0
                    }
                }
            });
        }
        emission.push(mc_light::emission(name, &|p| {
            blocks.property(state, p).map(str::to_owned)
        }));
    }
    mc_light::Tables::new(opacity, emission).with_faces(faces)
}

/// Full faces of the blocks that stop light by their shape (minecraft.wiki,
/// "Opacity"), as [`mc_light::FACES`] bits; `None` for every other block.
fn light_faces<'a>(name: &str, property: &dyn Fn(&str) -> Option<&'a str>) -> Option<u8> {
    let face = |f: &str| {
        mc_light::FACES
            .iter()
            .position(|n| *n == f)
            .map_or(0, |i| 1u8 << i)
    };
    let half = |p: Option<&str>| {
        if p == Some("top") {
            face("up")
        } else {
            face("down")
        }
    };
    let name = name.trim_start_matches("minecraft:");
    if name.ends_with("_slab") {
        return Some(match property("type") {
            Some("double") => 63,
            p => half(p),
        });
    }
    if name.ends_with("_stairs") {
        let facing = property("facing").unwrap_or("north");
        let turn = |clockwise: bool| {
            let order = ["north", "east", "south", "west"];
            let i = order.iter().position(|d| *d == facing).unwrap_or(0);
            order[if clockwise { (i + 1) % 4 } else { (i + 3) % 4 }]
        };
        let sides = match property("shape") {
            Some("straight") => face(facing),
            Some("inner_left") => face(facing) | face(turn(false)),
            Some("inner_right") => face(facing) | face(turn(true)),
            _ => 0,
        };
        return Some(half(property("half")) | sides);
    }
    match name {
        "snow" if property("layers") == Some("8") => Some(63),
        "snow"
        | "farmland"
        | "dirt_path"
        | "daylight_detector"
        | "enchanting_table"
        | "end_portal_frame"
        | "lectern"
        | "stonecutter"
        | "sculk_sensor"
        | "calibrated_sculk_sensor"
        | "sculk_shrieker" => Some(face("down")),
        "piston_head" => Some(face(property("facing").unwrap_or("up"))),
        _ => None,
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
