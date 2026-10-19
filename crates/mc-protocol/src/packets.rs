//! Packet layouts for protocol 777 (Minecraft 26.3).
//!
//! Source: the public protocol documentation (minecraft.wiki, "Java Edition
//! protocol"), not the decompiled game. Packet ids come from [`crate::PacketIds`].
//!
//! Clientbound packets implement [`Encode`]; serverbound packets are decoded per
//! state into an enum, with `Other` for packets we do not handle yet.

use mc_nbt::Tag;

use crate::codec::{Position, Reader, Result, Write, put_nbt};
use crate::ids::State;

pub trait Packet {
    /// Resource name as in the packet report, e.g. `minecraft:login`.
    const NAME: &'static str;
    const STATE: State;
}

pub trait Encode: Packet {
    fn encode(&self, out: &mut Vec<u8>);
}

/// Player profile as sent in Login Success and Player Info Update.
#[derive(Clone, Debug, PartialEq)]
pub struct GameProfile {
    pub uuid: u128,
    pub name: String,
    pub properties: Vec<ProfileProperty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileProperty {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

impl GameProfile {
    fn encode(&self, out: &mut Vec<u8>) {
        out.put_uuid(self.uuid);
        out.put_string(&self.name);
        put_properties(out, &self.properties);
    }
}

fn put_properties(out: &mut Vec<u8>, properties: &[ProfileProperty]) {
    out.put_len(properties.len());
    for property in properties {
        out.put_string(&property.name);
        out.put_string(&property.value);
        out.put_bool(property.signature.is_some());
        if let Some(signature) = &property.signature {
            out.put_string(signature);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

fn put_known_packs(out: &mut Vec<u8>, packs: &[KnownPack]) {
    out.put_len(packs.len());
    for pack in packs {
        out.put_string(&pack.namespace);
        out.put_string(&pack.id);
        out.put_string(&pack.version);
    }
}

fn read_known_packs(r: &mut Reader) -> Result<Vec<KnownPack>> {
    let count = r.length_prefix()?;
    let mut packs = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        packs.push(KnownPack {
            namespace: r.string(32767)?,
            id: r.string(32767)?,
            version: r.string(32767)?,
        });
    }
    Ok(packs)
}

macro_rules! packet {
    ($ty:ty, $state:ident, $name:literal) => {
        impl Packet for $ty {
            const NAME: &'static str = $name;
            const STATE: State = State::$state;
        }
    };
}

// ===========================================================================
pub mod handshake {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Serverbound {
        Intention(Intention),
        Other(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct Intention {
        pub protocol_version: i32,
        pub server_address: String,
        pub server_port: u16,
        /// 1 status, 2 login, 3 transfer.
        pub intent: i32,
    }

    impl Serverbound {
        pub fn decode(name: &str, r: &mut Reader) -> Result<Self> {
            Ok(match name {
                "minecraft:intention" => Serverbound::Intention(Intention {
                    protocol_version: r.varint()?,
                    server_address: r.string(255)?,
                    server_port: r.u16()?,
                    intent: r.varint()?,
                }),
                other => Serverbound::Other(other.to_owned()),
            })
        }
    }
}

// ===========================================================================
pub mod status {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Serverbound {
        StatusRequest,
        PingRequest(i64),
        Other(String),
    }

    impl Serverbound {
        pub fn decode(name: &str, r: &mut Reader) -> Result<Self> {
            Ok(match name {
                "minecraft:status_request" => Serverbound::StatusRequest,
                "minecraft:ping_request" => Serverbound::PingRequest(r.i64()?),
                other => Serverbound::Other(other.to_owned()),
            })
        }
    }

    /// Server list entry as JSON (version, players, description, ...).
    #[derive(Clone, Debug)]
    pub struct StatusResponse {
        pub json: String,
    }
    packet!(StatusResponse, Status, "minecraft:status_response");
    impl Encode for StatusResponse {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_string(&self.json);
        }
    }

    #[derive(Clone, Debug)]
    pub struct PongResponse {
        pub timestamp: i64,
    }
    packet!(PongResponse, Status, "minecraft:pong_response");
    impl Encode for PongResponse {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i64(self.timestamp);
        }
    }
}

// ===========================================================================
pub mod login {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Serverbound {
        /// Login Start.
        Hello {
            name: String,
            uuid: u128,
        },
        /// Encryption Response: both fields are RSA-encrypted with our public key.
        Key {
            shared_secret: Vec<u8>,
            verify_token: Vec<u8>,
        },
        LoginAcknowledged,
        Other(String),
    }

    impl Serverbound {
        pub fn decode(name: &str, r: &mut Reader) -> Result<Self> {
            Ok(match name {
                "minecraft:hello" => Serverbound::Hello {
                    name: r.string(16)?,
                    uuid: r.uuid()?,
                },
                "minecraft:key" => Serverbound::Key {
                    shared_secret: r.byte_array(256)?.to_vec(),
                    verify_token: r.byte_array(256)?.to_vec(),
                },
                "minecraft:login_acknowledged" => Serverbound::LoginAcknowledged,
                other => Serverbound::Other(other.to_owned()),
            })
        }
    }

    /// Login Success.
    #[derive(Clone, Debug)]
    pub struct LoginFinished {
        pub profile: GameProfile,
        pub session_id: u128,
    }
    packet!(LoginFinished, Login, "minecraft:login_finished");
    impl Encode for LoginFinished {
        fn encode(&self, out: &mut Vec<u8>) {
            self.profile.encode(out);
            out.put_uuid(self.session_id);
        }
    }

    /// Encryption Request (named `hello` in the packet report).
    #[derive(Clone, Debug)]
    pub struct EncryptionRequest {
        /// Always empty since 1.7.
        pub server_id: String,
        /// RSA public key, DER-encoded SubjectPublicKeyInfo.
        pub public_key: Vec<u8>,
        pub verify_token: Vec<u8>,
        pub should_authenticate: bool,
    }
    packet!(EncryptionRequest, Login, "minecraft:hello");
    impl Encode for EncryptionRequest {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_string(&self.server_id);
            out.put_byte_array(&self.public_key);
            out.put_byte_array(&self.verify_token);
            out.put_bool(self.should_authenticate);
        }
    }

    /// Set Compression: packets of at least `threshold` bytes are compressed.
    #[derive(Clone, Debug)]
    pub struct LoginCompression {
        pub threshold: i32,
    }
    packet!(LoginCompression, Login, "minecraft:login_compression");
    impl Encode for LoginCompression {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.threshold);
        }
    }

    /// Disconnect during login; the reason is a JSON text component.
    #[derive(Clone, Debug)]
    pub struct LoginDisconnect {
        pub reason_json: String,
    }
    packet!(LoginDisconnect, Login, "minecraft:login_disconnect");
    impl Encode for LoginDisconnect {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_string(&self.reason_json);
        }
    }
}

// ===========================================================================
pub mod configuration {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Serverbound {
        ClientInformation(ClientInformation),
        CustomPayload {
            channel: String,
            data: Vec<u8>,
        },
        /// Acknowledge Finish Configuration.
        FinishConfiguration,
        KeepAlive(i64),
        SelectKnownPacks(Vec<KnownPack>),
        Other(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    pub struct ClientInformation {
        pub locale: String,
        pub view_distance: i8,
        pub chat_mode: i32,
        pub chat_colors: bool,
        pub skin_parts: u8,
        pub main_hand: i32,
        pub text_filtering: bool,
        pub allow_server_listings: bool,
        pub particle_status: i32,
    }

    impl ClientInformation {
        pub fn decode(r: &mut Reader) -> Result<Self> {
            Ok(Self {
                locale: r.string(16)?,
                view_distance: r.i8()?,
                chat_mode: r.varint()?,
                chat_colors: r.bool()?,
                skin_parts: r.u8()?,
                main_hand: r.varint()?,
                text_filtering: r.bool()?,
                allow_server_listings: r.bool()?,
                particle_status: r.varint()?,
            })
        }
    }

    impl Serverbound {
        pub fn decode(name: &str, r: &mut Reader) -> Result<Self> {
            Ok(match name {
                "minecraft:client_information" => {
                    Serverbound::ClientInformation(ClientInformation::decode(r)?)
                }
                "minecraft:custom_payload" => Serverbound::CustomPayload {
                    channel: r.identifier()?,
                    data: r.rest().to_vec(),
                },
                "minecraft:finish_configuration" => Serverbound::FinishConfiguration,
                "minecraft:keep_alive" => Serverbound::KeepAlive(r.i64()?),
                "minecraft:select_known_packs" => {
                    Serverbound::SelectKnownPacks(read_known_packs(r)?)
                }
                other => Serverbound::Other(other.to_owned()),
            })
        }
    }

    #[derive(Clone, Debug)]
    pub struct CustomPayload {
        pub channel: String,
        pub data: Vec<u8>,
    }
    packet!(CustomPayload, Configuration, "minecraft:custom_payload");
    impl Encode for CustomPayload {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_string(&self.channel);
            out.put_slice(&self.data);
        }
    }

    #[derive(Clone, Debug)]
    pub struct Disconnect {
        pub reason: Tag,
    }
    packet!(Disconnect, Configuration, "minecraft:disconnect");
    impl Encode for Disconnect {
        fn encode(&self, out: &mut Vec<u8>) {
            put_nbt(out, &self.reason);
        }
    }

    #[derive(Clone, Debug)]
    pub struct KeepAlive {
        pub id: i64,
    }
    packet!(KeepAlive, Configuration, "minecraft:keep_alive");
    impl Encode for KeepAlive {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i64(self.id);
        }
    }

    /// Feature Flags.
    #[derive(Clone, Debug)]
    pub struct UpdateEnabledFeatures {
        pub flags: Vec<String>,
    }
    packet!(
        UpdateEnabledFeatures,
        Configuration,
        "minecraft:update_enabled_features"
    );
    impl Encode for UpdateEnabledFeatures {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_len(self.flags.len());
            for flag in &self.flags {
                out.put_string(flag);
            }
        }
    }

    /// Known Packs (clientbound).
    #[derive(Clone, Debug)]
    pub struct SelectKnownPacks {
        pub packs: Vec<KnownPack>,
    }
    packet!(
        SelectKnownPacks,
        Configuration,
        "minecraft:select_known_packs"
    );
    impl Encode for SelectKnownPacks {
        fn encode(&self, out: &mut Vec<u8>) {
            put_known_packs(out, &self.packs);
        }
    }

    #[derive(Clone, Debug)]
    pub struct RegistryEntry {
        pub id: String,
        /// `None` when the client takes the data from a known pack.
        pub data: Option<Tag>,
    }

    /// One synchronized registry. Entry order defines the network ids.
    #[derive(Clone, Debug)]
    pub struct RegistryData {
        pub registry: String,
        pub entries: Vec<RegistryEntry>,
    }
    packet!(RegistryData, Configuration, "minecraft:registry_data");
    impl Encode for RegistryData {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_string(&self.registry);
            out.put_len(self.entries.len());
            for entry in &self.entries {
                out.put_string(&entry.id);
                out.put_bool(entry.data.is_some());
                if let Some(data) = &entry.data {
                    put_nbt(out, data);
                }
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct TagRegistry {
        pub registry: String,
        /// Tag name (without `#`) and the numeric ids of its members.
        pub tags: Vec<(String, Vec<i32>)>,
    }

    #[derive(Clone, Debug)]
    pub struct UpdateTags {
        pub registries: Vec<TagRegistry>,
    }
    packet!(UpdateTags, Configuration, "minecraft:update_tags");
    impl Encode for UpdateTags {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_len(self.registries.len());
            for registry in &self.registries {
                out.put_string(&registry.registry);
                out.put_len(registry.tags.len());
                for (name, ids) in &registry.tags {
                    out.put_string(name);
                    out.put_len(ids.len());
                    for &id in ids {
                        out.put_varint(id);
                    }
                }
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct FinishConfiguration;
    packet!(
        FinishConfiguration,
        Configuration,
        "minecraft:finish_configuration"
    );
    impl Encode for FinishConfiguration {
        fn encode(&self, _out: &mut Vec<u8>) {}
    }
}

// ===========================================================================
pub mod play {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub enum Serverbound {
        /// Confirm Teleportation; since 26.3 it also reports where the client ended up.
        AcceptTeleportation {
            teleport_id: i32,
            x: f64,
            y: f64,
            z: f64,
            yaw: f32,
            pitch: f32,
        },
        KeepAlive(i64),
        MovePlayerPos {
            x: f64,
            y: f64,
            z: f64,
            flags: u8,
        },
        MovePlayerPosRot {
            x: f64,
            y: f64,
            z: f64,
            yaw: f32,
            pitch: f32,
            flags: u8,
        },
        MovePlayerRot {
            yaw: f32,
            pitch: f32,
            flags: u8,
        },
        MovePlayerStatusOnly {
            flags: u8,
        },
        /// Settings changed in game (same layout as in configuration).
        ClientInformation(configuration::ClientInformation),
        PlayerLoaded,
        ClientTickEnd,
        ChunkBatchReceived {
            chunks_per_tick: f32,
        },
        /// Only the message text; signing fields are ignored for now.
        Chat {
            message: String,
        },
        /// A command; also the signed kind, its signatures ignored.
        ChatCommand {
            command: String,
        },
        /// The client asks the server to complete `text` (`ask_server`
        /// nodes).
        CommandSuggestion {
            id: i32,
            text: String,
        },
        /// Digging, dropping items, swapping hands (`status`, see [`dig`]).
        PlayerAction {
            status: i32,
            position: Position,
            /// 0 −Y, 1 +Y, 2 −Z, 3 +Z, 4 −X, 5 +X.
            face: i8,
            sequence: i32,
        },
        /// Right click on a block.
        UseItemOn {
            /// 0 main hand, 1 off hand.
            hand: i32,
            position: Position,
            face: i32,
            /// Where on the face the crosshair is, 0..1 per axis.
            cursor: [f32; 3],
            inside_block: bool,
            world_border_hit: bool,
            sequence: i32,
        },
        /// Right click in the air.
        UseItem {
            hand: i32,
            sequence: i32,
            yaw: f32,
            pitch: f32,
        },
        /// Selected hotbar slot, 0..9.
        SetCarriedItem {
            slot: i16,
        },
        /// A creative-mode client sets an inventory slot (or −1 to drop it).
        SetCreativeModeSlot {
            slot: i16,
            item: Option<ItemStack>,
        },
        /// Left click that hits nothing (the hand swings).
        Punch,
        /// A click in a window; `changed` is the client's view of the slots
        /// it changed and `carried` of the cursor after the click.
        ContainerClick {
            window: i32,
            state_id: i32,
            slot: i16,
            button: i8,
            mode: i32,
            changed: Vec<(i16, Option<HashedStack>)>,
            carried: Option<HashedStack>,
        },
        ContainerClose {
            window: i32,
        },
        /// Left click on an entity.
        Attack {
            entity_id: i32,
        },
        /// 0 respawn, 1 statistics, 2 game rules.
        ClientCommand {
            action: i32,
        },
        /// Keys held: 0x10 jump, 0x20 sneak, 0x40 sprint.
        PlayerInput {
            flags: u8,
        },
        /// The player started (0x02) or stopped flying.
        PlayerAbilities {
            flags: u8,
        },
        Other(String),
    }

    /// Player Action statuses.
    pub mod dig {
        pub const START: i32 = 0;
        pub const CANCEL: i32 = 1;
        pub const FINISH: i32 = 2;
    }

    /// An item stack in a window click: components only as hashes.
    #[derive(Clone, Debug, PartialEq)]
    pub struct HashedStack {
        pub item: i32,
        pub count: i32,
        pub has_component_changes: bool,
    }

    impl HashedStack {
        pub fn decode(r: &mut Reader) -> Result<Option<Self>> {
            if !r.bool()? {
                return Ok(None);
            }
            let (item, count) = (r.varint()?, r.varint()?);
            let added = r.length_prefix()?;
            for _ in 0..added {
                r.varint()?;
                r.i32()?;
            }
            let removed = r.length_prefix()?;
            for _ in 0..removed {
                r.varint()?;
            }
            Ok(Some(Self {
                item,
                count,
                has_component_changes: added + removed > 0,
            }))
        }
    }

    /// An item stack as the client sends it. Component changes are not
    /// decoded yet; `has_component_changes` says whether there were any.
    #[derive(Clone, Debug, PartialEq)]
    pub struct ItemStack {
        pub count: i32,
        /// Id in the `minecraft:item` registry.
        pub item: i32,
        pub has_component_changes: bool,
    }

    impl ItemStack {
        /// Slot data; `None` for an empty slot. Reading stops after the
        /// component counts when there are components.
        pub fn decode(r: &mut Reader) -> Result<Option<Self>> {
            let count = r.varint()?;
            if count <= 0 {
                return Ok(None);
            }
            let item = r.varint()?;
            let (added, removed) = (r.varint()?, r.varint()?);
            if added != 0 || removed != 0 {
                r.rest();
            }
            Ok(Some(Self {
                count,
                item,
                has_component_changes: added != 0 || removed != 0,
            }))
        }

        /// Slot data without component changes.
        pub fn encode(stack: Option<&Self>, out: &mut Vec<u8>) {
            match stack {
                None => out.put_varint(0),
                Some(stack) => {
                    out.put_varint(stack.count);
                    out.put_varint(stack.item);
                    out.put_varint(0);
                    out.put_varint(0);
                }
            }
        }
    }

    impl Serverbound {
        pub fn decode(name: &str, r: &mut Reader) -> Result<Self> {
            Ok(match name {
                "minecraft:accept_teleportation" => Serverbound::AcceptTeleportation {
                    teleport_id: r.varint()?,
                    x: r.f64()?,
                    y: r.f64()?,
                    z: r.f64()?,
                    yaw: r.f32()?,
                    pitch: r.f32()?,
                },
                "minecraft:keep_alive" => Serverbound::KeepAlive(r.i64()?),
                "minecraft:move_player_pos" => Serverbound::MovePlayerPos {
                    x: r.f64()?,
                    y: r.f64()?,
                    z: r.f64()?,
                    flags: r.u8()?,
                },
                "minecraft:move_player_pos_rot" => Serverbound::MovePlayerPosRot {
                    x: r.f64()?,
                    y: r.f64()?,
                    z: r.f64()?,
                    yaw: r.f32()?,
                    pitch: r.f32()?,
                    flags: r.u8()?,
                },
                "minecraft:move_player_rot" => Serverbound::MovePlayerRot {
                    yaw: r.f32()?,
                    pitch: r.f32()?,
                    flags: r.u8()?,
                },
                "minecraft:move_player_status_only" => {
                    Serverbound::MovePlayerStatusOnly { flags: r.u8()? }
                }
                "minecraft:client_information" => {
                    Serverbound::ClientInformation(configuration::ClientInformation::decode(r)?)
                }
                "minecraft:player_loaded" => Serverbound::PlayerLoaded,
                "minecraft:client_tick_end" => Serverbound::ClientTickEnd,
                "minecraft:chunk_batch_received" => Serverbound::ChunkBatchReceived {
                    chunks_per_tick: r.f32()?,
                },
                "minecraft:chat" => Serverbound::Chat {
                    message: r.string(256)?,
                },
                "minecraft:chat_command" | "minecraft:chat_command_signed" => {
                    let command = r.string(32767)?;
                    // Timestamp, salt, argument signatures, acknowledged
                    // messages: not checked.
                    let _ = r.bytes(r.remaining());
                    Serverbound::ChatCommand { command }
                }
                "minecraft:command_suggestion" => Serverbound::CommandSuggestion {
                    id: r.varint()?,
                    text: r.string(32500)?,
                },
                "minecraft:player_action" => Serverbound::PlayerAction {
                    status: r.varint()?,
                    position: r.position()?,
                    face: r.i8()?,
                    sequence: r.varint()?,
                },
                "minecraft:use_item_on" => Serverbound::UseItemOn {
                    hand: r.varint()?,
                    position: r.position()?,
                    face: r.varint()?,
                    cursor: [r.f32()?, r.f32()?, r.f32()?],
                    inside_block: r.bool()?,
                    world_border_hit: r.bool()?,
                    sequence: r.varint()?,
                },
                "minecraft:use_item" => Serverbound::UseItem {
                    hand: r.varint()?,
                    sequence: r.varint()?,
                    yaw: r.f32()?,
                    pitch: r.f32()?,
                },
                "minecraft:set_carried_item" => Serverbound::SetCarriedItem { slot: r.i16()? },
                "minecraft:set_creative_mode_slot" => Serverbound::SetCreativeModeSlot {
                    slot: r.i16()?,
                    item: ItemStack::decode(r)?,
                },
                "minecraft:punch" => Serverbound::Punch,
                "minecraft:container_click" => {
                    let (window, state_id) = (r.varint()?, r.varint()?);
                    let (slot, button, mode) = (r.i16()?, r.i8()?, r.varint()?);
                    let count = r.length_prefix()?;
                    if count > 128 {
                        return Err(crate::codec::DecodeError::Invalid(
                            "number of changed slots",
                        ));
                    }
                    let mut changed = Vec::with_capacity(count);
                    for _ in 0..count {
                        changed.push((r.i16()?, HashedStack::decode(r)?));
                    }
                    Serverbound::ContainerClick {
                        window,
                        state_id,
                        slot,
                        button,
                        mode,
                        changed,
                        carried: HashedStack::decode(r)?,
                    }
                }
                "minecraft:container_close" => Serverbound::ContainerClose {
                    window: r.varint()?,
                },
                "minecraft:attack" => Serverbound::Attack {
                    entity_id: r.varint()?,
                },
                "minecraft:client_command" => Serverbound::ClientCommand {
                    action: r.varint()?,
                },
                "minecraft:player_input" => Serverbound::PlayerInput { flags: r.u8()? },
                "minecraft:player_abilities" => Serverbound::PlayerAbilities { flags: r.u8()? },
                other => Serverbound::Other(other.to_owned()),
            })
        }
    }

    /// Login (play).
    #[derive(Clone, Debug)]
    pub struct Login {
        pub entity_id: i32,
        pub hardcore: bool,
        pub dimensions: Vec<String>,
        pub max_players: i32,
        pub view_distance: i32,
        pub simulation_distance: i32,
        pub reduced_debug_info: bool,
        pub enable_respawn_screen: bool,
        pub do_limited_crafting: bool,
        /// Id in the `minecraft:dimension_type` registry as we sent it.
        pub dimension_type: i32,
        pub dimension: String,
        pub hashed_seed: i64,
        /// 0 survival, 1 creative, 2 adventure, 3 spectator.
        pub game_mode: i32,
        pub previous_game_mode: Option<i32>,
        pub is_debug: bool,
        pub is_flat: bool,
        pub death_location: Option<(String, Position)>,
        pub portal_cooldown: i32,
        pub sea_level: i32,
        pub online_mode: bool,
        pub enforces_secure_chat: bool,
    }
    packet!(Login, Play, "minecraft:login");
    impl Encode for Login {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i32(self.entity_id);
            out.put_bool(self.hardcore);
            out.put_len(self.dimensions.len());
            for dimension in &self.dimensions {
                out.put_string(dimension);
            }
            out.put_varint(self.max_players);
            out.put_varint(self.view_distance);
            out.put_varint(self.simulation_distance);
            out.put_bool(self.reduced_debug_info);
            out.put_bool(self.enable_respawn_screen);
            out.put_bool(self.do_limited_crafting);
            out.put_varint(self.dimension_type);
            out.put_string(&self.dimension);
            out.put_i64(self.hashed_seed);
            out.put_varint(self.game_mode);
            // 0 = undefined, otherwise game mode + 1.
            out.put_varint(self.previous_game_mode.map_or(0, |mode| mode + 1));
            out.put_bool(self.is_debug);
            out.put_bool(self.is_flat);
            out.put_bool(self.death_location.is_some());
            if let Some((dimension, position)) = &self.death_location {
                out.put_string(dimension);
                out.put_position(*position);
            }
            out.put_varint(self.portal_cooldown);
            out.put_varint(self.sea_level);
            out.put_bool(self.online_mode);
            out.put_bool(self.enforces_secure_chat);
        }
    }

    /// Respawn: back into a world (after death, or another dimension).
    #[derive(Clone, Debug)]
    pub struct Respawn {
        pub dimension_type: i32,
        pub dimension: String,
        pub hashed_seed: i64,
        pub game_mode: i32,
        pub previous_game_mode: Option<i32>,
        pub is_debug: bool,
        pub is_flat: bool,
        pub death_location: Option<(String, Position)>,
        pub portal_cooldown: i32,
        pub sea_level: i32,
        /// 0x01 keep attributes, 0x02 keep metadata (0 after death).
        pub data_kept: u8,
    }
    packet!(Respawn, Play, "minecraft:respawn");
    impl Encode for Respawn {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.dimension_type);
            out.put_string(&self.dimension);
            out.put_i64(self.hashed_seed);
            out.put_varint(self.game_mode);
            out.put_varint(self.previous_game_mode.map_or(0, |mode| mode + 1));
            out.put_bool(self.is_debug);
            out.put_bool(self.is_flat);
            out.put_bool(self.death_location.is_some());
            if let Some((dimension, position)) = &self.death_location {
                out.put_string(dimension);
                out.put_position(*position);
            }
            out.put_varint(self.portal_cooldown);
            out.put_varint(self.sea_level);
            out.put_u8(self.data_kept);
        }
    }

    /// Set Health: the player's health and food.
    #[derive(Clone, Debug)]
    pub struct SetHealth {
        pub health: f32,
        pub food: i32,
        pub saturation: f32,
    }
    packet!(SetHealth, Play, "minecraft:set_health");
    impl Encode for SetHealth {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_f32(self.health);
            out.put_varint(self.food);
            out.put_f32(self.saturation);
        }
    }

    /// Combat Death: the death screen.
    #[derive(Clone, Debug)]
    pub struct PlayerCombatKill {
        pub player: i32,
        pub message: Tag,
    }
    packet!(PlayerCombatKill, Play, "minecraft:player_combat_kill");
    impl Encode for PlayerCombatKill {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.player);
            crate::codec::put_nbt(out, &self.message);
        }
    }

    /// Block Action: a block event clients play themselves (pistons, note
    /// blocks, chest lids).
    #[derive(Clone, Debug)]
    pub struct BlockEvent {
        pub position: Position,
        pub action: u8,
        pub param: u8,
        /// `minecraft:block` registry id.
        pub block: i32,
    }
    packet!(BlockEvent, Play, "minecraft:block_event");
    impl Encode for BlockEvent {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_position(self.position);
            out.put_u8(self.action);
            out.put_u8(self.param);
            out.put_varint(self.block);
        }
    }

    /// Set Container Property: a window's number (furnace progress...).
    #[derive(Clone, Debug)]
    pub struct ContainerSetData {
        pub window: i32,
        pub property: i16,
        pub value: i16,
    }
    packet!(ContainerSetData, Play, "minecraft:container_set_data");
    impl Encode for ContainerSetData {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.window);
            out.put_i16(self.property);
            out.put_i16(self.value);
        }
    }

    /// Commands: the command tree the client completes and checks against
    /// (the body is built by `mc_command`).
    #[derive(Clone, Debug)]
    pub struct Commands {
        pub body: Vec<u8>,
    }
    packet!(Commands, Play, "minecraft:commands");
    impl Encode for Commands {
        fn encode(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.body);
        }
    }

    /// Command Suggestions Response: completions for the text from `start`
    /// (`length` characters), each with an optional tooltip.
    #[derive(Clone, Debug)]
    pub struct CommandSuggestions {
        pub id: i32,
        pub start: i32,
        pub length: i32,
        pub matches: Vec<(String, Option<Tag>)>,
    }
    packet!(CommandSuggestions, Play, "minecraft:command_suggestions");
    impl Encode for CommandSuggestions {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.id);
            out.put_varint(self.start);
            out.put_varint(self.length);
            out.put_varint(self.matches.len() as i32);
            for (text, tooltip) in &self.matches {
                out.put_string(text);
                out.put_bool(tooltip.is_some());
                if let Some(t) = tooltip {
                    crate::codec::put_nbt(out, t);
                }
            }
        }
    }

    /// Set Title Text, Set Subtitle Text, Set Action Bar Text.
    macro_rules! text_packet {
        ($ty:ident, $name:literal) => {
            #[derive(Clone, Debug)]
            pub struct $ty {
                pub text: Tag,
            }
            packet!($ty, Play, $name);
            impl Encode for $ty {
                fn encode(&self, out: &mut Vec<u8>) {
                    crate::codec::put_nbt(out, &self.text);
                }
            }
        };
    }
    text_packet!(SetTitleText, "minecraft:set_title_text");
    text_packet!(SetSubtitleText, "minecraft:set_subtitle_text");
    text_packet!(SetActionBarText, "minecraft:set_action_bar_text");

    /// Set Titles Animation (ticks).
    #[derive(Clone, Debug)]
    pub struct TitlesAnimation {
        pub fade_in: i32,
        pub stay: i32,
        pub fade_out: i32,
    }
    packet!(TitlesAnimation, Play, "minecraft:set_titles_animation");
    impl Encode for TitlesAnimation {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i32(self.fade_in);
            out.put_i32(self.stay);
            out.put_i32(self.fade_out);
        }
    }

    /// Clear Titles (`reset` also resets the times).
    #[derive(Clone, Debug)]
    pub struct ClearTitles {
        pub reset: bool,
    }
    packet!(ClearTitles, Play, "minecraft:clear_titles");
    impl Encode for ClearTitles {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_bool(self.reset);
        }
    }

    /// Sound Effect at a position: a sound event by registry id, or by
    /// name when it is not in the registry.
    #[derive(Clone, Debug)]
    pub struct Sound {
        /// `minecraft:sound_event` id, or `None` with `name`.
        pub id: Option<i32>,
        pub name: String,
        pub category: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        pub volume: f32,
        pub pitch: f32,
        pub seed: i64,
    }
    packet!(Sound, Play, "minecraft:sound");
    impl Encode for Sound {
        fn encode(&self, out: &mut Vec<u8>) {
            match self.id {
                Some(id) => out.put_varint(id + 1),
                None => {
                    out.put_varint(0);
                    out.put_string(&self.name);
                    out.put_bool(false);
                }
            }
            out.put_varint(self.category);
            out.put_i32((self.x * 8.0) as i32);
            out.put_i32((self.y * 8.0) as i32);
            out.put_i32((self.z * 8.0) as i32);
            out.put_f32(self.volume);
            out.put_f32(self.pitch);
            out.put_i64(self.seed);
        }
    }

    /// Stop Sound: by category and/or name, or every sound.
    #[derive(Clone, Debug)]
    pub struct StopSound {
        pub category: Option<i32>,
        pub name: Option<String>,
    }
    packet!(StopSound, Play, "minecraft:stop_sound");
    impl Encode for StopSound {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_u8(u8::from(self.category.is_some()) | (u8::from(self.name.is_some()) << 1));
            if let Some(c) = self.category {
                out.put_varint(c);
            }
            if let Some(n) = &self.name {
                out.put_string(n);
            }
        }
    }

    /// Initialize World Border.
    #[derive(Clone, Debug)]
    pub struct InitializeBorder {
        pub x: f64,
        pub z: f64,
        pub old_size: f64,
        pub new_size: f64,
        /// Milliseconds the change takes.
        pub speed: i64,
        pub portal_boundary: i32,
        pub warning_blocks: i32,
        pub warning_time: i32,
    }
    packet!(InitializeBorder, Play, "minecraft:initialize_border");
    impl Encode for InitializeBorder {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_f64(self.x);
            out.put_f64(self.z);
            out.put_f64(self.old_size);
            out.put_f64(self.new_size);
            out.put_varlong(self.speed);
            out.put_varint(self.portal_boundary);
            out.put_varint(self.warning_blocks);
            out.put_varint(self.warning_time);
        }
    }

    /// Change Difficulty.
    #[derive(Clone, Debug)]
    pub struct ChangeDifficulty {
        pub difficulty: u8,
        pub locked: bool,
    }
    packet!(ChangeDifficulty, Play, "minecraft:change_difficulty");
    impl Encode for ChangeDifficulty {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_u8(self.difficulty);
            out.put_bool(self.locked);
        }
    }

    /// Entity Event: a status for an entity's client-side logic (9: done
    /// using an item).
    #[derive(Clone, Debug)]
    pub struct EntityEvent {
        pub entity_id: i32,
        pub status: i8,
    }
    packet!(EntityEvent, Play, "minecraft:entity_event");
    impl Encode for EntityEvent {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i32(self.entity_id);
            out.put_i8(self.status);
        }
    }

    /// Damage Event: an entity was hurt (red flash, sound).
    #[derive(Clone, Debug)]
    pub struct DamageEvent {
        pub entity_id: i32,
        /// Id in the `minecraft:damage_type` registry.
        pub damage_type: i32,
    }
    packet!(DamageEvent, Play, "minecraft:damage_event");
    impl Encode for DamageEvent {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            out.put_varint(self.damage_type);
            out.put_varint(0); // no causing entity
            out.put_varint(0); // no direct entity
            out.put_bool(false); // no source position
        }
    }

    pub mod game_event {
        pub const BEGIN_RAINING: u8 = 1;
        pub const END_RAINING: u8 = 2;
        /// Value: 0 survival, 1 creative, 2 adventure, 3 spectator.
        pub const CHANGE_GAME_MODE: u8 = 3;
        /// Value: rain level 0–1.
        pub const RAIN_LEVEL: u8 = 7;
        /// Value: thunder level 0–1.
        pub const THUNDER_LEVEL: u8 = 8;
        pub const START_WAITING_FOR_CHUNKS: u8 = 13;
    }

    #[derive(Clone, Debug)]
    pub struct GameEvent {
        pub event: u8,
        pub value: f32,
    }
    packet!(GameEvent, Play, "minecraft:game_event");
    impl Encode for GameEvent {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_u8(self.event);
            out.put_f32(self.value);
        }
    }

    /// Update Time (26.x): world age and the world clocks. The client advances
    /// each clock by its rate on its own; vanilla sends every clock on join and
    /// when one is set or stopped, and only the age every second otherwise.
    #[derive(Clone, Debug)]
    pub struct SetTime {
        pub world_age: i64,
        pub clocks: Vec<ClockState>,
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct ClockState {
        /// Id in the `minecraft:world_clock` registry.
        pub clock: i32,
        pub ticks: i64,
        pub fraction: f32,
        /// Clock ticks per game tick (1 normally, 0 when time is stopped).
        pub rate: f32,
    }
    packet!(SetTime, Play, "minecraft:set_time");
    impl Encode for SetTime {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i64(self.world_age);
            out.put_len(self.clocks.len());
            for clock in &self.clocks {
                out.put_varint(clock.clock);
                out.put_varlong(clock.ticks);
                out.put_f32(clock.fraction);
                out.put_f32(clock.rate);
            }
        }
    }

    pub mod heightmap {
        pub const WORLD_SURFACE: i32 = 1;
        pub const MOTION_BLOCKING: i32 = 4;
        pub const MOTION_BLOCKING_NO_LEAVES: i32 = 5;
    }

    /// Light for the sections of a chunk plus one below and one above the world.
    /// Masks hold one bit per section (bit 0 = below the world); arrays are 2048
    /// bytes (two values per byte), one per set bit of the matching mask.
    #[derive(Clone, Debug, Default)]
    pub struct LightData {
        pub sky_mask: Vec<u64>,
        pub block_mask: Vec<u64>,
        pub empty_sky_mask: Vec<u64>,
        pub empty_block_mask: Vec<u64>,
        pub sky: Vec<Vec<u8>>,
        pub block: Vec<Vec<u8>>,
    }

    impl LightData {
        fn encode(&self, out: &mut Vec<u8>) {
            // Observed from vanilla 26.3: masks are byte bit sets, not long arrays.
            out.put_byte_bitset(&self.sky_mask);
            out.put_byte_bitset(&self.block_mask);
            out.put_byte_bitset(&self.empty_sky_mask);
            out.put_byte_bitset(&self.empty_block_mask);
            for arrays in [&self.sky, &self.block] {
                out.put_len(arrays.len());
                for array in arrays {
                    out.put_byte_array(array);
                }
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct ChunkBlockEntity {
        /// `(x & 15) << 4 | (z & 15)`.
        pub packed_xz: u8,
        pub y: i16,
        pub kind: i32,
        /// What the client needs to render it; `None` is sent as TAG_End.
        pub data: Option<Tag>,
    }

    /// Chunk Data and Update Light.
    #[derive(Clone, Debug)]
    pub struct LevelChunkWithLight {
        pub x: i32,
        pub z: i32,
        /// Heightmap type and its packed long array.
        pub heightmaps: Vec<(i32, Vec<i64>)>,
        /// Encoded chunk sections, bottom to top.
        pub data: Vec<u8>,
        pub block_entities: Vec<ChunkBlockEntity>,
        pub light: LightData,
    }
    packet!(
        LevelChunkWithLight,
        Play,
        "minecraft:level_chunk_with_light"
    );
    impl Encode for LevelChunkWithLight {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i32(self.x);
            out.put_i32(self.z);
            out.put_len(self.heightmaps.len());
            for (kind, data) in &self.heightmaps {
                out.put_varint(*kind);
                out.put_len(data.len());
                for &word in data {
                    out.put_i64(word);
                }
            }
            out.put_byte_array(&self.data);
            out.put_len(self.block_entities.len());
            for entity in &self.block_entities {
                out.put_u8(entity.packed_xz);
                out.put_i16(entity.y);
                out.put_varint(entity.kind);
                match &entity.data {
                    Some(data) => put_nbt(out, data),
                    None => out.put_u8(mc_nbt::tag_type::END),
                }
            }
            self.light.encode(out);
        }
    }

    /// Set Center Chunk.
    #[derive(Clone, Debug)]
    pub struct SetChunkCacheCenter {
        pub x: i32,
        pub z: i32,
    }
    packet!(
        SetChunkCacheCenter,
        Play,
        "minecraft:set_chunk_cache_center"
    );
    impl Encode for SetChunkCacheCenter {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.x);
            out.put_varint(self.z);
        }
    }

    /// Unload Chunk. Z comes first on the wire (read as one long by the client).
    #[derive(Clone, Debug)]
    pub struct ForgetLevelChunk {
        pub x: i32,
        pub z: i32,
    }
    packet!(ForgetLevelChunk, Play, "minecraft:forget_level_chunk");
    impl Encode for ForgetLevelChunk {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i32(self.z);
            out.put_i32(self.x);
        }
    }

    #[derive(Clone, Debug)]
    pub struct ChunkBatchStart;
    packet!(ChunkBatchStart, Play, "minecraft:chunk_batch_start");
    impl Encode for ChunkBatchStart {
        fn encode(&self, _out: &mut Vec<u8>) {}
    }

    #[derive(Clone, Debug)]
    pub struct ChunkBatchFinished {
        pub batch_size: i32,
    }
    packet!(ChunkBatchFinished, Play, "minecraft:chunk_batch_finished");
    impl Encode for ChunkBatchFinished {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.batch_size);
        }
    }

    /// Synchronize Player Position. `flags` are Teleport Flags (relative bits).
    #[derive(Clone, Debug)]
    pub struct PlayerPosition {
        pub teleport_id: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        pub velocity: [f64; 3],
        pub yaw: f32,
        pub pitch: f32,
        pub flags: i32,
    }
    packet!(PlayerPosition, Play, "minecraft:player_position");
    impl Encode for PlayerPosition {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.teleport_id);
            out.put_f64(self.x);
            out.put_f64(self.y);
            out.put_f64(self.z);
            for v in self.velocity {
                out.put_f64(v);
            }
            out.put_f32(self.yaw);
            out.put_f32(self.pitch);
            out.put_i32(self.flags);
        }
    }

    #[derive(Clone, Debug)]
    pub struct SetDefaultSpawnPosition {
        pub dimension: String,
        pub position: Position,
        pub yaw: f32,
        pub pitch: f32,
    }
    packet!(
        SetDefaultSpawnPosition,
        Play,
        "minecraft:set_default_spawn_position"
    );
    impl Encode for SetDefaultSpawnPosition {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_string(&self.dimension);
            out.put_position(self.position);
            out.put_f32(self.yaw);
            out.put_f32(self.pitch);
        }
    }

    pub mod abilities {
        pub const INVULNERABLE: u8 = 0x01;
        pub const FLYING: u8 = 0x02;
        pub const ALLOW_FLYING: u8 = 0x04;
        pub const INSTANT_BREAK: u8 = 0x08;
    }

    #[derive(Clone, Debug)]
    pub struct PlayerAbilities {
        pub flags: u8,
        pub flying_speed: f32,
        pub fov_modifier: f32,
    }
    packet!(PlayerAbilities, Play, "minecraft:player_abilities");
    impl Encode for PlayerAbilities {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_u8(self.flags);
            out.put_f32(self.flying_speed);
            out.put_f32(self.fov_modifier);
        }
    }

    /// Entity metadata field indices, observed from a vanilla 26.3 server.
    pub mod metadata {
        /// Dropped item entity: the stack (Slot).
        pub const ITEM: u8 = 8;
        /// Living entity: hand states (Byte): 1 using an item, 2 the off hand.
        pub const HAND_STATES: u8 = 8;
        /// Living entity health (Float).
        pub const HEALTH: u8 = 9;
        /// Player: displayed skin parts (Byte), the bit mask from Client Information.
        pub const PLAYER_SKIN_PARTS: u8 = 16;
    }

    /// One entity metadata value; the variant decides the serializer type id.
    #[derive(Clone, Debug, PartialEq)]
    pub enum MetadataValue {
        Byte(i8),
        VarInt(i32),
        Float(f32),
        Item(Option<ItemStack>),
        Boolean(bool),
    }

    impl MetadataValue {
        fn encode(&self, out: &mut Vec<u8>) {
            match self {
                MetadataValue::Byte(v) => {
                    out.put_varint(0);
                    out.put_i8(*v);
                }
                MetadataValue::VarInt(v) => {
                    out.put_varint(1);
                    out.put_varint(*v);
                }
                MetadataValue::Float(v) => {
                    out.put_varint(3);
                    out.put_f32(*v);
                }
                MetadataValue::Item(stack) => {
                    out.put_varint(7);
                    ItemStack::encode(stack.as_ref(), out);
                }
                MetadataValue::Boolean(v) => {
                    out.put_varint(8);
                    out.put_bool(*v);
                }
            }
        }
    }

    /// Set Entity Metadata: changed fields of one entity.
    #[derive(Clone, Debug)]
    pub struct SetEntityData {
        pub entity_id: i32,
        pub entries: Vec<(u8, MetadataValue)>,
    }
    packet!(SetEntityData, Play, "minecraft:set_entity_data");
    impl Encode for SetEntityData {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            for (index, value) in &self.entries {
                out.put_u8(*index);
                value.encode(out);
            }
            out.put_u8(0xff);
        }
    }

    /// Spawn Entity. Velocity is sent as LpVec3; only zero velocity (a single
    /// 0x00 byte) is supported for now.
    #[derive(Clone, Debug)]
    pub struct AddEntity {
        pub entity_id: i32,
        pub uuid: u128,
        /// Id in `minecraft:entity_type`.
        pub kind: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        pub pitch: f32,
        pub yaw: f32,
        pub head_yaw: f32,
        pub data: i32,
    }
    packet!(AddEntity, Play, "minecraft:add_entity");
    impl Encode for AddEntity {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            out.put_uuid(self.uuid);
            out.put_varint(self.kind);
            out.put_f64(self.x);
            out.put_f64(self.y);
            out.put_f64(self.z);
            out.put_u8(0); // LpVec3 zero velocity
            out.put_angle(self.pitch);
            out.put_angle(self.yaw);
            out.put_angle(self.head_yaw);
            out.put_varint(self.data);
        }
    }

    #[derive(Clone, Debug)]
    pub struct RemoveEntities {
        pub entity_ids: Vec<i32>,
    }
    packet!(RemoveEntities, Play, "minecraft:remove_entities");
    impl Encode for RemoveEntities {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_len(self.entity_ids.len());
            for &id in &self.entity_ids {
                out.put_varint(id);
            }
        }
    }

    /// Teleport Entity in the documentation (`entity_position_sync` in the
    /// packet report): absolute position as a linear path, plus rotation.
    #[derive(Clone, Debug)]
    pub struct EntityPositionSync {
        pub entity_id: i32,
        pub x: f64,
        pub y: f64,
        pub z: f64,
        pub yaw: f32,
        pub pitch: f32,
        pub on_ground: bool,
    }
    packet!(EntityPositionSync, Play, "minecraft:entity_position_sync");
    impl Encode for EntityPositionSync {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            out.put_varint(0); // position path type: linear
            out.put_f64(self.x);
            out.put_f64(self.y);
            out.put_f64(self.z);
            out.put_f32(self.yaw);
            out.put_f32(self.pitch);
            out.put_bool(self.on_ground);
        }
    }

    /// Set Head Rotation.
    #[derive(Clone, Debug)]
    pub struct RotateHead {
        pub entity_id: i32,
        pub head_yaw: f32,
    }
    packet!(RotateHead, Play, "minecraft:rotate_head");
    impl Encode for RotateHead {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            out.put_angle(self.head_yaw);
        }
    }

    /// Block Update: one block changed.
    #[derive(Clone, Debug)]
    pub struct BlockUpdate {
        pub position: Position,
        pub state: u32,
    }
    packet!(BlockUpdate, Play, "minecraft:block_update");
    impl Encode for BlockUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_position(self.position);
            out.put_varint(self.state as i32);
        }
    }

    /// Update Light: new light for some sections of a chunk.
    #[derive(Clone, Debug)]
    pub struct LightUpdate {
        pub x: i32,
        pub z: i32,
        pub light: LightData,
    }
    packet!(LightUpdate, Play, "minecraft:light_update");
    impl Encode for LightUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.x);
            out.put_varint(self.z);
            self.light.encode(out);
        }
    }

    /// Update Section Blocks: several blocks of one 16³ section changed.
    #[derive(Clone, Debug)]
    pub struct SectionBlocksUpdate {
        /// Section coordinates (block coordinates >> 4).
        pub section: (i32, i32, i32),
        /// Position inside the section (x, y, z each 0..16) and new state.
        pub blocks: Vec<((u8, u8, u8), u32)>,
    }
    packet!(SectionBlocksUpdate, Play, "minecraft:section_blocks_update");
    impl Encode for SectionBlocksUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            let (x, y, z) = self.section;
            let packed = ((i64::from(x) & 0x3F_FFFF) << 42)
                | (i64::from(y) & 0xF_FFFF)
                | ((i64::from(z) & 0x3F_FFFF) << 20);
            out.put_i64(packed);
            out.put_len(self.blocks.len());
            for &((bx, by, bz), state) in &self.blocks {
                let local = (i64::from(bx) << 8) | (i64::from(bz) << 4) | i64::from(by);
                out.put_varlong((i64::from(state) << 12) | local);
            }
        }
    }

    /// Set Ticking State: the client's tick rate and whether it is frozen.
    #[derive(Clone, Debug)]
    pub struct TickingState {
        pub tick_rate: f32,
        pub frozen: bool,
    }
    packet!(TickingState, Play, "minecraft:ticking_state");
    impl Encode for TickingState {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_f32(self.tick_rate);
            out.put_bool(self.frozen);
        }
    }

    /// Step Tick: a frozen client runs this many ticks.
    #[derive(Clone, Debug)]
    pub struct TickingStep {
        pub steps: i32,
    }
    packet!(TickingStep, Play, "minecraft:ticking_step");
    impl Encode for TickingStep {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.steps);
        }
    }

    /// Acknowledge Block Change: the client may stop predicting changes up to
    /// this sequence and show what the server sent.
    #[derive(Clone, Debug)]
    pub struct BlockChangedAck {
        pub sequence: i32,
    }
    packet!(BlockChangedAck, Play, "minecraft:block_changed_ack");
    impl Encode for BlockChangedAck {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.sequence);
        }
    }

    /// World Event: a sound or particle effect at a block.
    #[derive(Clone, Debug)]
    pub struct LevelEvent {
        pub event: i32,
        pub position: Position,
        pub data: i32,
        /// Heard everywhere, not only nearby.
        pub global: bool,
    }
    packet!(LevelEvent, Play, "minecraft:level_event");
    impl Encode for LevelEvent {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i32(self.event);
            out.put_position(self.position);
            out.put_i32(self.data);
            out.put_bool(self.global);
        }
    }

    pub mod level_event {
        /// Block broken: particles and sound; data is the block state id.
        pub const BLOCK_BROKEN: i32 = 2001;
    }

    /// Swing Animation (26.x): an entity swings a hand.
    #[derive(Clone, Debug)]
    pub struct SwingAnimation {
        pub entity_id: i32,
        /// 0 main hand, 1 off hand.
        pub hand: i32,
        /// 0 none, 1 whack, 2 stab.
        pub kind: i32,
        /// In ticks.
        pub duration: i32,
    }
    packet!(SwingAnimation, Play, "minecraft:swing_animation");
    impl Encode for SwingAnimation {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            out.put_varint(self.hand);
            out.put_varint(self.kind);
            out.put_varint(self.duration);
        }
    }

    /// Pickup Item: the item flies to whoever picked it up (the entity is
    /// removed separately).
    #[derive(Clone, Debug)]
    pub struct TakeItemEntity {
        pub collected: i32,
        pub collector: i32,
        pub count: i32,
    }
    packet!(TakeItemEntity, Play, "minecraft:take_item_entity");
    impl Encode for TakeItemEntity {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.collected);
            out.put_varint(self.collector);
            out.put_varint(self.count);
        }
    }

    /// Set Player Inventory Slot: one slot by player inventory index (0–8
    /// hotbar, 9–35 main, 36–39 armour, 40 off hand).
    #[derive(Clone, Debug)]
    pub struct SetPlayerInventory {
        pub slot: i32,
        pub item: Option<ItemStack>,
    }
    packet!(SetPlayerInventory, Play, "minecraft:set_player_inventory");
    impl Encode for SetPlayerInventory {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.slot);
            ItemStack::encode(self.item.as_ref(), out);
        }
    }

    /// Set Container Slot: one slot of a window (0 = the inventory, numbered
    /// as the window: 36–44 hotbar, 9–35 main, 5–8 armour, 45 off hand).
    #[derive(Clone, Debug)]
    pub struct ContainerSetSlot {
        pub window: i32,
        pub state_id: i32,
        pub slot: i16,
        pub item: Option<ItemStack>,
    }
    packet!(ContainerSetSlot, Play, "minecraft:container_set_slot");
    impl Encode for ContainerSetSlot {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.window);
            out.put_varint(self.state_id);
            out.put_i16(self.slot);
            ItemStack::encode(self.item.as_ref(), out);
        }
    }

    /// Open Screen: a container window (chest, furnace...).
    #[derive(Clone, Debug)]
    pub struct OpenScreen {
        pub window: i32,
        /// Id in the `minecraft:menu` registry.
        pub menu: i32,
        pub title: Tag,
    }
    packet!(OpenScreen, Play, "minecraft:open_screen");
    impl Encode for OpenScreen {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.window);
            out.put_varint(self.menu);
            crate::codec::put_nbt(out, &self.title);
        }
    }

    /// Close Container: the server closes a window.
    #[derive(Clone, Debug)]
    pub struct ContainerCloseClientbound {
        pub window: i32,
    }
    packet!(ContainerCloseClientbound, Play, "minecraft:container_close");
    impl Encode for ContainerCloseClientbound {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.window);
        }
    }

    /// Set Held Item: which hotbar slot is selected.
    #[derive(Clone, Debug)]
    pub struct SetHeldSlot {
        pub slot: i32,
    }
    packet!(SetHeldSlot, Play, "minecraft:set_held_slot");
    impl Encode for SetHeldSlot {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.slot);
        }
    }

    /// Set Container Content: every slot of a window (0 = the inventory).
    #[derive(Clone, Debug)]
    pub struct ContainerSetContent {
        pub window: i32,
        pub state_id: i32,
        pub slots: Vec<Option<ItemStack>>,
        pub carried: Option<ItemStack>,
    }
    packet!(ContainerSetContent, Play, "minecraft:container_set_content");
    impl Encode for ContainerSetContent {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.window);
            out.put_varint(self.state_id);
            out.put_len(self.slots.len());
            for slot in &self.slots {
                ItemStack::encode(slot.as_ref(), out);
            }
            ItemStack::encode(self.carried.as_ref(), out);
        }
    }

    /// Set Equipment: what an entity holds and wears.
    #[derive(Clone, Debug)]
    pub struct SetEquipment {
        pub entity_id: i32,
        /// Slot (0 main hand, 1 off hand, 2–5 armour, ...) and item.
        pub slots: Vec<(u8, Option<ItemStack>)>,
    }
    packet!(SetEquipment, Play, "minecraft:set_equipment");
    impl Encode for SetEquipment {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_varint(self.entity_id);
            for (i, (slot, item)) in self.slots.iter().enumerate() {
                // The top bit says another entry follows.
                let more = if i + 1 < self.slots.len() { 0x80 } else { 0 };
                out.put_u8(slot | more);
                ItemStack::encode(item.as_ref(), out);
            }
        }
    }

    pub mod player_info {
        pub const ADD_PLAYER: u8 = 0x01;
        pub const INITIALIZE_CHAT: u8 = 0x02;
        pub const UPDATE_GAME_MODE: u8 = 0x04;
        pub const UPDATE_LISTED: u8 = 0x08;
        pub const UPDATE_LATENCY: u8 = 0x10;
        pub const UPDATE_DISPLAY_NAME: u8 = 0x20;
        pub const UPDATE_LIST_PRIORITY: u8 = 0x40;
        pub const UPDATE_HAT: u8 = 0x80;
        pub const ALL: u8 = 0xFF;
    }

    /// One player in Player Info Update. Only the fields selected by the
    /// packet's `actions` are written.
    #[derive(Clone, Debug)]
    pub struct PlayerInfoEntry {
        pub profile: GameProfile,
        pub game_mode: i32,
        pub listed: bool,
        pub latency_ms: i32,
        pub display_name: Option<Tag>,
        pub list_priority: i32,
        pub show_hat: bool,
    }

    /// Adds or updates entries of the player list (tab list, skins).
    #[derive(Clone, Debug)]
    pub struct PlayerInfoUpdate {
        pub actions: u8,
        pub players: Vec<PlayerInfoEntry>,
    }
    packet!(PlayerInfoUpdate, Play, "minecraft:player_info_update");
    impl Encode for PlayerInfoUpdate {
        fn encode(&self, out: &mut Vec<u8>) {
            use player_info::*;
            let has = |action: u8| self.actions & action != 0;
            out.put_u8(self.actions);
            out.put_len(self.players.len());
            for player in &self.players {
                out.put_uuid(player.profile.uuid);
                if has(ADD_PLAYER) {
                    out.put_string(&player.profile.name);
                    put_properties(out, &player.profile.properties);
                }
                if has(INITIALIZE_CHAT) {
                    out.put_bool(false); // no signed chat session
                }
                if has(UPDATE_GAME_MODE) {
                    out.put_varint(player.game_mode);
                }
                if has(UPDATE_LISTED) {
                    out.put_bool(player.listed);
                }
                if has(UPDATE_LATENCY) {
                    out.put_varint(player.latency_ms);
                }
                if has(UPDATE_DISPLAY_NAME) {
                    out.put_bool(player.display_name.is_some());
                    if let Some(name) = &player.display_name {
                        put_nbt(out, name);
                    }
                }
                if has(UPDATE_LIST_PRIORITY) {
                    out.put_varint(player.list_priority);
                }
                if has(UPDATE_HAT) {
                    out.put_bool(player.show_hat);
                }
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct PlayerInfoRemove {
        pub uuids: Vec<u128>,
    }
    packet!(PlayerInfoRemove, Play, "minecraft:player_info_remove");
    impl Encode for PlayerInfoRemove {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_len(self.uuids.len());
            for &uuid in &self.uuids {
                out.put_uuid(uuid);
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct KeepAlive {
        pub id: i64,
    }
    packet!(KeepAlive, Play, "minecraft:keep_alive");
    impl Encode for KeepAlive {
        fn encode(&self, out: &mut Vec<u8>) {
            out.put_i64(self.id);
        }
    }

    #[derive(Clone, Debug)]
    pub struct SystemChat {
        pub content: Tag,
        /// Action bar instead of chat.
        pub overlay: bool,
    }
    packet!(SystemChat, Play, "minecraft:system_chat");
    impl Encode for SystemChat {
        fn encode(&self, out: &mut Vec<u8>) {
            put_nbt(out, &self.content);
            out.put_bool(self.overlay);
        }
    }

    #[derive(Clone, Debug)]
    pub struct Disconnect {
        pub reason: Tag,
    }
    packet!(Disconnect, Play, "minecraft:disconnect");
    impl Encode for Disconnect {
        fn encode(&self, out: &mut Vec<u8>) {
            put_nbt(out, &self.reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intention_decodes() {
        // protocol 777, "localhost", 25565, login
        let mut body = Vec::new();
        body.put_varint(777);
        body.put_string("localhost");
        body.put_u16(25565);
        body.put_varint(2);
        let packet =
            handshake::Serverbound::decode("minecraft:intention", &mut Reader::new(&body)).unwrap();
        assert_eq!(
            packet,
            handshake::Serverbound::Intention(handshake::Intention {
                protocol_version: 777,
                server_address: "localhost".into(),
                server_port: 25565,
                intent: 2,
            })
        );
    }

    #[test]
    fn registry_data_layout() {
        let packet = configuration::RegistryData {
            registry: "minecraft:dimension_type".into(),
            entries: vec![configuration::RegistryEntry {
                id: "minecraft:overworld".into(),
                data: None,
            }],
        };
        let mut out = Vec::new();
        packet.encode(&mut out);
        let mut expected = Vec::new();
        expected.put_string("minecraft:dimension_type");
        expected.put_varint(1);
        expected.put_string("minecraft:overworld");
        expected.push(0);
        assert_eq!(out, expected);
    }

    #[test]
    fn entity_data_matches_vanilla_bytes() {
        // Captured from a vanilla 26.3 server: health 20, all skin parts shown.
        let packet = play::SetEntityData {
            entity_id: 3,
            entries: vec![
                (play::metadata::HEALTH, play::MetadataValue::Float(20.0)),
                (
                    play::metadata::PLAYER_SKIN_PARTS,
                    play::MetadataValue::Byte(0x7f),
                ),
            ],
        };
        let mut out = Vec::new();
        packet.encode(&mut out);
        assert_eq!(
            out,
            [
                0x03, 0x09, 0x03, 0x41, 0xa0, 0x00, 0x00, 0x10, 0x00, 0x7f, 0xff
            ]
        );
    }

    #[test]
    fn player_info_matches_vanilla_bytes() {
        // Captured from a vanilla 26.3 server (player "Probe2", offline mode).
        let uuid = 0x435b21f3_1630_3420_8717_2c6f1c759dd3;
        let packet = play::PlayerInfoUpdate {
            actions: play::player_info::ALL,
            players: vec![play::PlayerInfoEntry {
                profile: GameProfile {
                    uuid,
                    name: "Probe2".into(),
                    properties: vec![],
                },
                game_mode: 0,
                listed: true,
                latency_ms: 0,
                display_name: None,
                list_priority: 0,
                show_hat: true,
            }],
        };
        let mut out = Vec::new();
        packet.encode(&mut out);
        let mut expected = vec![0xff, 0x01];
        expected.extend_from_slice(&uuid.to_be_bytes());
        expected.extend_from_slice(b"\x06Probe2\x00\x00\x00\x01\x00\x00\x00\x01");
        assert_eq!(out, expected);
    }

    #[test]
    fn unknown_packets_are_kept_by_name() {
        let packet =
            play::Serverbound::decode("minecraft:swing", &mut Reader::new(&[0x00])).unwrap();
        assert_eq!(packet, play::Serverbound::Other("minecraft:swing".into()));
    }

    #[test]
    fn block_packets_match_vanilla_bytes() {
        // Seen from vanilla 26.3 (`cargo xtask blocks`): the watcher gets the
        // builder's (entity 495) main hand set to item 163, then a swing.
        let mut out = Vec::new();
        play::SetEquipment {
            entity_id: 495,
            slots: vec![(
                0,
                Some(play::ItemStack {
                    count: 1,
                    item: 163,
                    has_component_changes: false,
                }),
            )],
        }
        .encode(&mut out);
        assert_eq!(out, [0xef, 0x03, 0x00, 0x01, 0xa3, 0x01, 0x00, 0x00]);
        out.clear();
        play::SwingAnimation {
            entity_id: 495,
            hand: 0,
            kind: 1,
            duration: 6,
        }
        .encode(&mut out);
        assert_eq!(out, [0xef, 0x03, 0x00, 0x01, 0x06]);
    }

    #[test]
    fn creative_slot_and_use_item_on_decode() {
        let mut body = Vec::new();
        body.put_i16(36);
        body.put_varint(1);
        body.put_varint(1); // stone
        body.put_varint(0);
        body.put_varint(0);
        let packet =
            play::Serverbound::decode("minecraft:set_creative_mode_slot", &mut Reader::new(&body))
                .unwrap();
        assert_eq!(
            packet,
            play::Serverbound::SetCreativeModeSlot {
                slot: 36,
                item: Some(play::ItemStack {
                    count: 1,
                    item: 1,
                    has_component_changes: false
                }),
            }
        );

        let mut body = Vec::new();
        body.put_varint(0);
        body.put_position(Position::new(-8, -61, 10));
        body.put_varint(1);
        for c in [0.5f32, 1.0, 0.5] {
            body.put_f32(c);
        }
        body.put_bool(false);
        body.put_bool(false);
        body.put_varint(3);
        let mut r = Reader::new(&body);
        let packet = play::Serverbound::decode("minecraft:use_item_on", &mut r).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(
            packet,
            play::Serverbound::UseItemOn {
                hand: 0,
                position: Position::new(-8, -61, 10),
                face: 1,
                cursor: [0.5, 1.0, 0.5],
                inside_block: false,
                world_border_hit: false,
                sequence: 3,
            }
        );
    }

    #[test]
    fn set_time_matches_vanilla_bytes() {
        // Vanilla 26.3 on join (`cargo xtask watch --packets set_time`): age
        // 5462, overworld clock 1014 and end clock 4992, both running.
        let mut out = Vec::new();
        play::SetTime {
            world_age: 0x1556,
            clocks: vec![
                play::ClockState {
                    clock: 0,
                    ticks: 1014,
                    fraction: 0.0,
                    rate: 1.0,
                },
                play::ClockState {
                    clock: 1,
                    ticks: 4992,
                    fraction: 0.0,
                    rate: 1.0,
                },
            ],
        }
        .encode(&mut out);
        let expected = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x56, 0x02, 0x00, 0xf6, 0x07, 0x00, 0x00,
            0x00, 0x00, 0x3f, 0x80, 0x00, 0x00, 0x01, 0x80, 0x27, 0x00, 0x00, 0x00, 0x00, 0x3f,
            0x80, 0x00, 0x00,
        ];
        assert_eq!(out, expected);
    }
}
