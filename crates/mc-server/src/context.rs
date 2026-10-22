//! State shared by all connections: game data, prebuilt configuration packets,
//! the world and the list of online players.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

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
    /// The dimension they are in.
    pub dimension: String,
    /// What others see in their main hand.
    pub main_hand: Option<ItemStack>,
    /// Flying (creative players who double-jumped, and spectators).
    pub flying: bool,
    /// The address they connected from (for `/ban-ip`).
    pub address: Option<String>,
    /// Their own respawn point and tags (kept in the player file).
    pub extra: crate::players::Extra,
}

/// Packets commands send to one player.
#[derive(Clone, Debug)]
pub enum Direct {
    Title(mc_protocol::packets::play::SetTitleText),
    Subtitle(mc_protocol::packets::play::SetSubtitleText),
    ActionBar(mc_protocol::packets::play::SetActionBarText),
    Times(mc_protocol::packets::play::TitlesAnimation),
    Clear(mc_protocol::packets::play::ClearTitles),
    Sound(mc_protocol::packets::play::Sound),
    Experience(mc_protocol::packets::play::SetExperience),
    StopSound(mc_protocol::packets::play::StopSound),
    MobEffect(mc_protocol::packets::play::UpdateMobEffect),
    RemoveMobEffect(mc_protocol::packets::play::RemoveMobEffect),
    DefaultSpawn(mc_protocol::packets::play::SetDefaultSpawnPosition),
    Particles(mc_protocol::packets::play::LevelParticles),
    Camera(mc_protocol::packets::play::SetCamera),
    Boss(mc_protocol::packets::play::BossEvent),
    Transfer(mc_protocol::packets::play::Transfer),
    DisguisedChat(mc_protocol::packets::play::DisguisedChat),
    ShowDialog(mc_protocol::packets::play::ShowDialog),
    ClearDialog(mc_protocol::packets::play::ClearDialog),
    PostEffects(mc_protocol::packets::play::PostEffects),
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
    pub tnt: i32,
    pub villager: i32,
    pub wandering_trader: i32,
    pub experience_orb: i32,
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
    /// Mobs that moved or turned.
    MobsMoved(Vec<crate::mobs::MobMove>),
    /// Entity events of mobs (`EntityEvent` statuses: 2 hurt, 3 death...).
    MobsStatus(Vec<(i32, u8)>),
    /// Entities that moved or turned: where clients should put them.
    EntitySync(Vec<crate::fx::Sync>),
    /// A sound, particles or a status for clients nearby.
    Fx(crate::fx::Fx),
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
    /// A block entity's client-visible data changed (sign text).
    BlockEntity {
        position: mc_protocol::Position,
        kind: i32,
        data: mc_nbt::Compound,
    },
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
        /// The entity that did it, if not a player (primed TNT).
        direct: Option<i32>,
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
    /// Command feedback for the operators other than `from` (who ran it).
    AdminFeedback {
        from: u128,
        line: Tag,
    },
    /// A player's permission level changed: they get the commands they may
    /// use now.
    Permissions {
        uuid: u128,
    },
    /// A player's entity data changed (for them and who sees them).
    EntityData {
        uuid: u128,
        packet: mc_protocol::packets::play::SetEntityData,
    },
    /// A player's attributes changed (for them and who sees them).
    Attributes {
        uuid: u128,
        packet: mc_protocol::packets::play::UpdateAttributes,
    },
    /// `/fillbiome` changed these chunks' biomes.
    BiomesChanged {
        chunks: Vec<(i32, i32)>,
    },
    /// An event of another dimension than the sender's.
    In(usize, Box<Event>),
    /// A player left this dimension (or the game): others stop seeing them.
    PlayerGone {
        uuid: u128,
        entity_id: i32,
    },
    /// Move a player to another dimension (a portal or a command).
    ChangeDimension {
        uuid: u128,
        dimension: String,
        pose: Pose,
        /// A respawn after dying: nothing is kept.
        after_death: bool,
    },
    /// A packet for one player only.
    Direct {
        uuid: u128,
        packet: Direct,
    },
    /// Rain or thunder came or went.
    Weather(crate::weather::Changes),
    /// The world border changed.
    Border,
    /// A scoreboard or team change, for everyone.
    Scoreboard(crate::scoreboard::Packet),
    /// The difficulty changed (0–3).
    Difficulty(i32),
    /// A message for one player (`tellraw`, `msg`...).
    Message {
        uuid: u128,
        line: Tag,
        overlay: bool,
    },
    /// A player's arm swings (`times` in a row, as vanilla does on placing).
    Swing {
        uuid: u128,
        entity_id: i32,
        times: u8,
    },
    /// Metadata of non-player entities changed (a TNT fuse).
    EntitiesData(Vec<mc_protocol::packets::play::SetEntityData>),
    /// An entity event of a player (a tool breaking in their hand).
    HandBroke {
        uuid: u128,
        entity_id: i32,
        status: i8,
    },
    /// A sound at a place; players within 16 blocks of it hear it (volume
    /// 1), by the sound event's registry name.
    WorldSound {
        pos: [f64; 3],
        sound: &'static str,
        /// `SoundSource` id: 4 blocks, 7 players...
        category: i32,
        volume: f32,
        pitch: f32,
    },
    /// An explosion; players within 64 blocks see it, those it pushed with
    /// their knockback.
    Explosion {
        center: [f64; 3],
        radius: f32,
        block_count: i32,
        particle: i32,
        sound: i32,
        knockback: Vec<(u128, [f64; 3])>,
    },
    /// `/swing`: an entity swings a hand, for itself and who sees it.
    Animate {
        uuid: u128,
        entity_id: i32,
        hand: i32,
        kind: i32,
        duration: i32,
    },
    /// What a player holds in the main hand changed.
    Equipment {
        uuid: u128,
        entity_id: i32,
        main_hand: Option<ItemStack>,
    },
}

#[allow(missing_debug_implementations)]
pub struct Shared {
    /// The dimensions that exist so far, in [`DIMENSIONS`] order (the others
    /// are made when first used).
    pub dims: [Mutex<Option<Arc<ServerContext>>>; 3],
    synced_ids: HashMap<String, HashMap<String, i32>>,
    light_tables: mc_light::Tables,
    /// Event channels of every dimension.
    pub hub: Events,
    /// Players waiting in portals (tick thread).
    pub portals: Mutex<crate::portals::Waiting>,
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
    /// How placing and breaking blocks works (until block behaviours exist).
    pub rules: BlockRules,
    /// Neighbour-dependent block shapes.
    pub shapes: mc_tick::Shapes,
    /// What players did to mobs and where mobs have spawned.
    pub mobs: crate::mobs::MobState,
    /// Inventories of the players in play.
    pub inventories: Mutex<HashMap<u128, crate::interact::Hands>>,
    /// What broken blocks drop.
    pub loot: crate::loot::BlockLoot,
    /// Crafting recipes.
    pub recipes: crate::crafting::Recipes,
    /// Enchantment definitions.
    pub enchantments: crate::enchants::Enchantments,
    /// What villagers and wandering traders sell.
    pub trades: crate::trading::Trades,
    /// Health and food of players in play.
    pub vitals: Mutex<crate::vitals::AllVitals>,
    /// Stack sizes, food and more of every item.
    pub items: crate::items::Items,
    /// Burn times of fuels.
    pub fuels: crate::furnaces::Fuels,
    /// Players in bed at night, since when.
    pub sleepers: Mutex<crate::useblocks::Sleepers>,
    /// Players eating or drinking (tick thread counts down).
    pub using: Mutex<HashMap<u128, crate::vitals::Using>>,
    /// 0 peaceful, 1 easy, 2 normal, 3 hard (level.dat).
    pub difficulty: AtomicI32,
    /// `minecraft:damage_type` entries in Registry Data order.
    damage_types: Vec<String>,
    /// Player files of the world folder (none for the flat test world).
    pub player_files: Option<crate::players::Store>,
    /// Game mode of players who never played here (level.dat `GameType`).
    pub default_game_mode: AtomicI32,
    /// `ops.json`, `whitelist.json`, bans and the user cache.
    pub lists: Mutex<crate::lists::Lists>,
    /// Whether only listed players may join (`white-list`).
    pub white_list: std::sync::atomic::AtomicBool,
    /// `minecraft:entity_type` ids of the entities we have.
    pub entity_types: EntityTypes,
    /// `minecraft:particle_type` and `minecraft:sound_event` ids of a big
    /// explosion.
    pub explosion_particle: i32,
    pub explosion_sound: i32,
    /// `minecraft:player` in `minecraft:entity_type`.
    pub player_entity_type: i32,
    /// RSA key for the online-mode key exchange (`None` in offline mode).
    pub key: Option<ServerKey>,
    /// Logged-in players by UUID.
    pub players: Mutex<HashMap<u128, OnlinePlayer>>,
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
    /// The command tree and what commands are checked against.
    pub commands: crate::commands::Commands,
    pub game_rules: Mutex<crate::rules::GameRules>,
    /// Boss bars made with `/bossbar`.
    pub bossbars: Mutex<crate::bossbars::BossBars>,
    /// Objectives, scores and teams.
    pub scoreboard: Mutex<crate::scoreboard::Scoreboard>,
    /// Command storage (`/data ... storage`).
    pub storage: Mutex<crate::storage::Storage>,
    /// `/stopwatch`: when each one started.
    /// State of the rarer commands (`/posteffect`, `/waypoint`...).
    pub rare: Mutex<crate::commands::rare::State>,
    /// Bows being drawn and bobbers cast.
    pub shooting: Mutex<crate::projectiles::State>,
    /// Leads: who holds each leashed entity.
    pub leashes: Mutex<crate::decor::leash::Leashes>,
    pub stopwatches: Mutex<std::collections::BTreeMap<String, std::time::Instant>>,
    /// Data packs, their functions and `/schedule`.
    pub packs: Mutex<crate::packs::Packs>,
    /// The world spawn (`/setworldspawn` moves it).
    pub spawn: Mutex<crate::players::Respawn>,
    next_entity_id: AtomicI32,
}

/// One dimension: its world and everything that happens in it. The rest
/// (players, rules, commands...) is shared and reached through `Deref`.
pub struct ServerContext {
    pub shared: Arc<Shared>,
    /// Index in [`DIMENSIONS`].
    pub dim: usize,
    pub world: World,
    /// Block and fluid ticks waiting to run (tick thread only).
    pub ticks: Mutex<mc_tick::ScheduledTicks>,
    /// Entities other than players (tick thread only, read by joining players).
    pub entities: Mutex<Vec<mc_entity::Entity>>,
    /// Chests and barrels someone has open.
    pub containers: Mutex<crate::containers::Containers>,
    /// Contents (`Items`) of entities that hold items (minecarts and boats
    /// with a chest, hopper minecarts), by entity id.
    pub entity_items: Mutex<HashMap<i32, mc_nbt::Compound>>,
    /// Hopper minecarts resting after taking an item: ticks left, by id.
    pub cart_rests: Mutex<HashMap<i32, i32>>,
    /// Daylight detectors in loaded chunks.
    pub daylight: Mutex<std::collections::HashSet<(i32, i32, i32)>>,
    /// Piston block events waiting for this tick (tick thread).
    pub block_events: Mutex<Vec<(mc_tick::BlockPos, u32, u8, u8)>>,
    /// Blocks on their way, moved by pistons (tick thread).
    pub moving: Mutex<Vec<(mc_tick::BlockPos, mc_tick::pistons::Moving)>>,
    /// Furnaces burning or cooking (tick thread).
    pub furnaces: Mutex<crate::furnaces::Active>,
    /// Hoppers that tick (tick thread).
    pub hoppers: Mutex<crate::hoppers::Active>,
    /// Recent redstone torch toggles by position (burnout).
    pub torch_toggles: Mutex<HashMap<(i32, i32, i32), Vec<i64>>>,
    /// Containers whose contents changed, for comparators (tick thread).
    pub container_updates: Mutex<Vec<mc_protocol::Position>>,
    pub events: Events,
    pub weather: Mutex<crate::weather::Weather>,
    pub border: Mutex<crate::border::Border>,
    /// Force-loaded chunks (`/forceload`).
    pub forced: Mutex<crate::forced::Forced>,
}

impl Deref for ServerContext {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

/// The dimensions of a world, by index.
pub const DIMENSIONS: [&str; 3] = [
    "minecraft:overworld",
    "minecraft:the_nether",
    "minecraft:the_end",
];

/// Index of a dimension in [`DIMENSIONS`] (`overworld` and `minecraft:overworld`
/// both work).
pub fn dimension_index(name: &str) -> Option<usize> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    ["overworld", "the_nether", "the_end"]
        .iter()
        .position(|d| *d == name)
}

/// The channels events go through: one per dimension for what happens at a
/// place, and one for what every player gets wherever they are.
#[derive(Clone, Debug)]
pub struct Events {
    locals: Arc<Vec<broadcast::Sender<Event>>>,
    global: broadcast::Sender<Event>,
    me: usize,
}

impl Events {
    fn new() -> Self {
        Self {
            // Every movement is an event; leave room for many moving players.
            locals: Arc::new(
                (0..DIMENSIONS.len())
                    .map(|_| broadcast::channel(4096).0)
                    .collect(),
            ),
            global: broadcast::channel(4096).0,
            me: 0,
        }
    }

    fn for_dim(&self, me: usize) -> Self {
        Self { me, ..self.clone() }
    }

    /// Sends the event to who it concerns: everyone, or those in this
    /// dimension (or in the one an [`Event::In`] names).
    pub fn send(&self, event: Event) -> Result<usize, Box<broadcast::error::SendError<Event>>> {
        let (dim, event) = match event {
            Event::In(dim, inner) => (dim.min(self.locals.len() - 1), *inner),
            event => (self.me, event),
        };
        let sent = if event.is_global() {
            self.global.send(event)
        } else {
            self.locals[dim].send(event)
        };
        sent.map_err(Box::new)
    }

    /// Everything that goes to this dimension and to everyone.
    pub fn subscribe(&self) -> EventReceiver {
        EventReceiver {
            local: self.locals[self.me].subscribe(),
            global: self.global.subscribe(),
        }
    }
}

impl EventReceiver {
    /// The next event if one is waiting.
    #[cfg(test)]
    pub fn try_changed(&mut self) -> Option<Event> {
        self.local
            .try_recv()
            .ok()
            .or_else(|| self.global.try_recv().ok())
    }
}

#[derive(Debug)]
pub struct EventReceiver {
    local: broadcast::Receiver<Event>,
    global: broadcast::Receiver<Event>,
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Result<Event, broadcast::error::RecvError> {
        tokio::select! {
            event = self.local.recv() => event,
            event = self.global.recv() => event,
        }
    }
}

impl Event {
    /// Whether every player gets it, wherever they are (what is for one
    /// player, the chat, the tab list...) rather than those in the
    /// dimension it happened in.
    pub fn is_global(&self) -> bool {
        !matches!(
            self,
            Event::PlayerMoved { .. }
                | Event::PlayerGone { .. }
                | Event::BlocksChanged { .. }
                | Event::BlockBroken { .. }
                | Event::LightChanged(_)
                | Event::EntitySpawned(_)
                | Event::BlockEntity { .. }
                | Event::BlockAction { .. }
                | Event::EntitiesMoved(_)
                | Event::EntitiesRemoved(_)
                | Event::ContainerSlots { .. }
                | Event::ContainerGone { .. }
                | Event::FurnaceProgress { .. }
                | Event::ItemTaken { .. }
                | Event::BiomesChanged { .. }
                | Event::Weather(_)
                | Event::Border
                | Event::EntitiesData(_)
                | Event::Explosion { .. }
        )
    }
}

impl std::fmt::Debug for ServerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerContext")
            .field("version", &self.data.version)
            .finish_non_exhaustive()
    }
}

impl ServerContext {
    /// The dimension at `index` in [`DIMENSIONS`], made when first needed.
    /// `None` if it cannot be made.
    pub fn dimension(&self, index: usize) -> Option<Arc<ServerContext>> {
        let cell = self.shared.dims.get(index)?;
        let mut cell = cell.lock().expect("dimensions lock");
        if cell.is_none() {
            match self.build_dimension(index) {
                Ok(ctx) => *cell = Some(ctx),
                Err(err) => {
                    warn!("cannot open {}: {err:#}", DIMENSIONS[index]);
                    return None;
                }
            }
        }
        cell.clone()
    }

    /// The dimension at `index`, or this one if it cannot be had.
    pub fn at(&self, index: usize) -> Arc<ServerContext> {
        self.dimension(index)
            .or_else(|| self.dimension(self.dim))
            .expect("this dimension exists")
    }

    /// A dimension by name (`minecraft:the_nether`).
    pub fn in_dimension(&self, name: &str) -> Option<Arc<ServerContext>> {
        self.dimension(dimension_index(name)?)
    }

    /// The dimensions made so far.
    pub fn open_dimensions(&self) -> Vec<Arc<ServerContext>> {
        self.shared
            .dims
            .iter()
            .filter_map(|d| d.lock().expect("dimensions lock").clone())
            .collect()
    }

    /// The dimension a player is in (this one if they are nowhere).
    pub fn of_player(&self, uuid: u128) -> Arc<ServerContext> {
        let name = self
            .players
            .lock()
            .expect("players lock")
            .get(&uuid)
            .map(|p| p.dimension.clone());
        name.and_then(|n| self.in_dimension(&n))
            .or_else(|| self.dimension(self.dim))
            .expect("this dimension exists")
    }

    /// `minecraft:overworld`, `minecraft:the_nether` or `minecraft:the_end`.
    pub fn dimension_name(&self) -> &'static str {
        DIMENSIONS[self.dim]
    }

    fn build_dimension(&self, index: usize) -> Result<Arc<ServerContext>> {
        use mc_worldgen::generator::Dimension as Kind;
        let kind = [Kind::Overworld, Kind::Nether, Kind::End][index];
        let synced = &self.shared.synced_ids;
        let rules = &self.rules;
        let light = self.shared.light_tables.clone();
        let name = DIMENSIONS[index].trim_start_matches("minecraft:");
        let world = match &self.config.world {
            Some(dir) => {
                let biome_ids: HashMap<String, u32> = synced["minecraft:worldgen/biome"]
                    .iter()
                    .map(|(k, &v)| (k.clone(), v as u32))
                    .collect();
                let generator = match crate::generation::seed_of(dir) {
                    Some(seed) => Some((
                        crate::generation::dimension(&self.data, seed, &biome_ids, kind)?,
                        seed,
                    )),
                    None => None,
                };
                World::open_dimension(
                    dir,
                    kind,
                    generator,
                    &self.data,
                    synced,
                    light,
                    rules,
                    make_shapes(&self.data, &self.config),
                )?
            }
            None => World::flat_in(&self.data, synced, rules, light, DIMENSIONS[index])?,
        };
        let folder = world.folder().map(Path::to_owned);
        let version = self.data.version.world_version;
        Ok(Arc::new(ServerContext {
            shared: Arc::clone(&self.shared),
            dim: index,
            world,
            ticks: Mutex::new(mc_tick::ScheduledTicks::default()),
            entities: Mutex::new(Vec::new()),
            containers: Mutex::new(HashMap::new()),
            entity_items: Mutex::new(HashMap::new()),
            cart_rests: Mutex::new(HashMap::new()),
            daylight: Mutex::new(Default::default()),
            block_events: Mutex::new(Vec::new()),
            moving: Mutex::new(Vec::new()),
            furnaces: Mutex::new(HashMap::new()),
            hoppers: Mutex::new(Default::default()),
            torch_toggles: Mutex::new(HashMap::new()),
            container_updates: Mutex::new(Vec::new()),
            weather: Mutex::new(Default::default()),
            border: Mutex::new(crate::border::Border::load_in(
                folder.as_deref(),
                name,
                version,
            )),
            forced: Mutex::new(crate::forced::Forced::load_in(
                folder.as_deref(),
                name,
                version,
            )),
            events: self.shared.hub.for_dim(index),
        }))
    }

    pub fn white_list(&self) -> bool {
        self.white_list.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn new(config: Config, data: McData) -> Result<Arc<Self>> {
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

        mc_protocol::components::install_registries(
            data.builtin
                .iter()
                .chain(synced_ids.iter().map(|(k, v)| (k.as_str(), v))),
        );

        let key = if config.online_mode {
            Some(ServerKey::generate()?)
        } else {
            None
        };
        let tags = build_tags(&data, &synced_ids)?;
        let rules = BlockRules::new(&data).map_err(anyhow::Error::msg)?;
        let light = light_tables(&data, &config);
        let light_tables = light.clone();
        let hub = Events::new();
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
        let config_data_dir = config.data_dir.clone();
        let game_rules = crate::rules::GameRules::load(
            &config.data_dir,
            world.folder(),
            data.version.world_version,
        );
        let weather = crate::weather::Weather::load(world.folder(), data.version.world_version);
        let border = crate::border::Border::load(world.folder(), data.version.world_version);
        let forced = crate::forced::Forced::load(world.folder(), data.version.world_version);
        let commands = crate::commands::Commands::load(&config_data_dir);
        let packs = crate::packs::Packs::load(config.level_dir.as_deref(), &commands);
        let storage = crate::storage::Storage::load(world.folder(), data.version.world_version);
        let scoreboard =
            crate::scoreboard::Scoreboard::load(world.folder(), data.version.world_version);
        let bossbars = crate::bossbars::BossBars::load(world.folder(), data.version.world_version);
        let spawn = crate::players::Respawn {
            dimension: world.dimension.clone(),
            pos: (
                world.spawn.0.floor() as i32,
                world.spawn.1.floor() as i32,
                world.spawn.2.floor() as i32,
            ),
            yaw: world.spawn_yaw,
            pitch: 0.0,
            forced: false,
        };
        let recipes = crate::crafting::Recipes::load(&data);
        let shared = Arc::new(Shared {
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
            rules,
            shapes,
            mobs: Default::default(),
            inventories: Mutex::new(HashMap::new()),
            loot: crate::loot::BlockLoot::load(&data),
            enchantments: crate::enchants::Enchantments::load(
                &data,
                synced_ids
                    .get("minecraft:enchantment")
                    .unwrap_or(&HashMap::new()),
                &recipes.item_tags,
            ),
            recipes,
            trades: crate::trading::Trades::load(&data, &items),
            vitals: Mutex::new(HashMap::new()),
            items,
            using: Mutex::new(HashMap::new()),
            fuels,
            sleepers: Mutex::new(HashMap::new()),
            difficulty: AtomicI32::new(difficulty),
            damage_types,
            player_files,
            default_game_mode: AtomicI32::new(default_game_mode),
            lists: Mutex::new(lists),
            white_list: std::sync::atomic::AtomicBool::new(white_list),
            explosion_particle: data
                .builtin
                .id("minecraft:particle_type", "minecraft:explosion_emitter")
                .context("explosion_emitter missing from the registries report")?,
            explosion_sound: data
                .builtin
                .id("minecraft:sound_event", "minecraft:entity.generic.explode")
                .context("entity.generic.explode missing from the registries report")?,
            entity_types: EntityTypes {
                falling_block: entity_type("minecraft:falling_block")?,
                item: entity_type("minecraft:item")?,
                tnt: entity_type("minecraft:tnt")?,
                villager: entity_type("minecraft:villager")?,
                wandering_trader: entity_type("minecraft:wandering_trader")?,
                experience_orb: entity_type("minecraft:experience_orb")?,
            },
            player_entity_type,
            key,
            players: Mutex::new(HashMap::new()),
            time: Mutex::new(time),
            actions,
            action_queue: Mutex::new(Some(action_queue)),
            ticking: Mutex::new(TickingState {
                tick_rate: 20.0,
                frozen: false,
            }),
            shutdown: tokio::sync::Notify::new(),
            next_entity_id: AtomicI32::new(1),
            packs: Mutex::new(packs),
            commands,
            game_rules: Mutex::new(game_rules),
            spawn: Mutex::new(spawn),
            bossbars: Mutex::new(bossbars),
            scoreboard: Mutex::new(scoreboard),
            storage: Mutex::new(storage),
            rare: Mutex::default(),
            shooting: Mutex::default(),
            leashes: Mutex::default(),
            stopwatches: Mutex::default(),
            data,
            dims: Default::default(),
            synced_ids,
            light_tables,
            hub: hub.clone(),
            portals: Mutex::default(),
        });
        let overworld = Arc::new(ServerContext {
            shared: Arc::clone(&shared),
            dim: 0,

            world,
            ticks: Mutex::new(mc_tick::ScheduledTicks::default()),
            entities: Mutex::new(Vec::new()),
            containers: Mutex::new(HashMap::new()),
            entity_items: Mutex::new(HashMap::new()),
            cart_rests: Mutex::new(HashMap::new()),
            daylight: Mutex::new(Default::default()),
            container_updates: Mutex::new(Vec::new()),
            hoppers: Mutex::new(Default::default()),
            torch_toggles: Mutex::new(HashMap::new()),
            furnaces: Mutex::new(HashMap::new()),
            block_events: Mutex::new(Vec::new()),
            moving: Mutex::new(Vec::new()),
            // Every movement is an event; leave room for many moving players.
            events: hub.for_dim(0),
            weather: Mutex::new(weather),
            border: Mutex::new(border),
            forced: Mutex::new(forced),
        });
        *shared.dims[0].lock().expect("dimensions lock") = Some(Arc::clone(&overworld));
        Ok(overworld)
    }

    pub fn difficulty(&self) -> i32 {
        self.difficulty.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn default_game_mode(&self) -> i32 {
        self.default_game_mode
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn game_rule_bool(&self, name: &str) -> bool {
        self.game_rules.lock().expect("game rules lock").bool(name)
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

fn make_shapes(data: &McData, config: &Config) -> mc_tick::Shapes {
    let observed = |file: &str| {
        let path = config.data_dir.join("observed").join(file);
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| serde_json::from_value::<Vec<i32>>(v["states"].clone()).ok())
    };
    mc_tick::Shapes::new(
        data,
        observed("fence_attach.json"),
        observed("conductor.json"),
    )
    .with_instruments(observed("instrument.json"))
    .with_support(observed("support.json"))
    .with_push(observed("push.json"))
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

#[cfg(test)]
mod hub_tests {
    use super::*;

    #[tokio::test]
    async fn events_reach_the_right_receivers() {
        let hub = Events::new();
        let (a, b) = (hub.for_dim(0), hub.for_dim(1));
        let mut ra = a.subscribe();
        let mut rb = b.subscribe();
        a.send(Event::Chat(Tag::String("hi".into()))).unwrap();
        a.send(Event::Border).unwrap();
        // Local and global events may arrive in either order.
        let mut got: Vec<_> = vec![ra.recv().await.unwrap(), ra.recv().await.unwrap()];
        got.sort_by_key(|e| e.is_global());
        assert!(matches!(got[0], Event::Border) && matches!(got[1], Event::Chat(_)));
        assert!(matches!(rb.recv().await, Ok(Event::Chat(_))));
        assert!(rb.try_changed().is_none());
        // Aimed at another dimension.
        a.send(Event::In(1, Box::new(Event::Border))).unwrap();
        assert!(matches!(rb.recv().await, Ok(Event::Border)));
        assert!(ra.try_changed().is_none());
    }
}
