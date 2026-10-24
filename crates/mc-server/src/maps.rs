//! Filled maps as a vanilla 26.3 world stores them (`data/minecraft/maps/<id>.dat`,
//! the last id in `last_id.dat`) and what the client is sent while someone holds
//! one or hangs it in an item frame.
//!
//! Colours follow the map-item article on minecraft.wiki: a few columns a tick,
//! within 128 blocks of the holder (64 in the Nether), shaded by the column to
//! the north and by water depth. Locked maps keep the colours they had.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::{ItemStack, MapColorPatch, MapDecoration, MapItemData};

use crate::context::{Direct, Event, ServerContext};
use crate::interact::{Actor, Hands, Outcome};

const PIXELS: usize = 128 * 128;
const SPECTATOR: i32 = 3;
const CREATIVE: i32 = 1;

/// What the cartography table needs to know of a map.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapState {
    pub scale: i32,
    pub locked: bool,
}

/// Every filled map. Held on the shared server so a map made in one dimension
/// is the same map in the others, and so tests without a world folder still work.
#[derive(Default)]
pub struct Store {
    /// The next id to give out. `None` until `last_id.dat` has been read.
    next: Option<i32>,
    maps: HashMap<i32, MapFile>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("next", &self.next)
            .field("maps", &self.maps.len())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ProbeKey {
    Player(u128),
    Frame(i32),
}

#[derive(Clone, Default)]
struct Viewer {
    /// The client has been sent every pixel at least once.
    seen: bool,
    /// Inclusive pixel bounds that changed since the last packet.
    dirty: Option<(u8, u8, u8, u8)>,
    last_icons: Vec<MapDecoration>,
}

#[derive(Clone)]
struct Banner {
    color: String,
    name: Option<Tag>,
    x: i32,
    y: i32,
    z: i32,
    /// Facing byte saved for Nether banners (Overworld and End stay 0).
    direction: u8,
}

#[derive(Clone)]
struct FrameMark {
    entity_id: i32,
    /// Degrees clockwise from north: 0, 90, 180, 270.
    rotation: i32,
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Clone)]
struct MapFile {
    scale: i32,
    dimension: String,
    x_center: i32,
    z_center: i32,
    locked: bool,
    tracking: bool,
    unlimited: bool,
    colors: Vec<u8>,
    banners: Vec<Banner>,
    frames: Vec<FrameMark>,
    viewers: HashMap<u128, Viewer>,
    probes: HashMap<ProbeKey, u8>,
    dirty: bool,
}

#[derive(Clone)]
struct StackIcon {
    kind: i32,
    x: f64,
    z: f64,
    rotation: f32,
}

#[derive(Clone)]
struct HandMap {
    id: i32,
    icons: Vec<StackIcon>,
}

struct PlayerSnap {
    uuid: u128,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    game_mode: i32,
    maps: Vec<HandMap>,
}

struct FrameSnap {
    entity_id: i32,
    x: i32,
    y: i32,
    z: i32,
    facing: u8,
    pos: [f64; 3],
    map_id: i32,
    icons: Vec<StackIcon>,
}

struct Column {
    base: u8,
    height: i32,
    water: i32,
    loaded: bool,
}

struct Pixel {
    base: u8,
    height: f64,
    water: i32,
}

impl MapFile {
    fn blank(dimension: String, x: i32, z: i32) -> Self {
        Self {
            scale: 0,
            dimension,
            x_center: x,
            z_center: z,
            locked: false,
            tracking: true,
            unlimited: false,
            colors: vec![0; PIXELS],
            banners: Vec::new(),
            frames: Vec::new(),
            viewers: HashMap::new(),
            probes: HashMap::new(),
            dirty: true,
        }
    }

    fn to_data(&self) -> Compound {
        let mut data = Compound::new();
        data.insert("scale", Tag::Byte(self.scale as i8));
        data.insert("dimension", self.dimension.as_str());
        data.insert("xCenter", Tag::Int(self.x_center));
        data.insert("zCenter", Tag::Int(self.z_center));
        data.insert("locked", Tag::Byte(i8::from(self.locked)));
        data.insert("trackingPosition", Tag::Byte(i8::from(self.tracking)));
        data.insert("unlimitedTracking", Tag::Byte(i8::from(self.unlimited)));
        data.insert(
            "colors",
            Tag::ByteArray(self.colors.iter().map(|c| *c as i8).collect()),
        );
        data.insert(
            "banners",
            Tag::List(self.banners.iter().map(banner_tag).collect()),
        );
        data.insert(
            "frames",
            Tag::List(self.frames.iter().map(frame_tag).collect()),
        );
        data
    }

    fn from_data(data: &Compound) -> Self {
        let mut colors = vec![0u8; PIXELS];
        if let Some(Tag::ByteArray(raw)) = data.get("colors") {
            for (i, byte) in raw.iter().take(PIXELS).enumerate() {
                colors[i] = *byte as u8;
            }
        }
        Self {
            scale: int_at(data, "scale").clamp(0, 4),
            dimension: match data.get("dimension") {
                Some(Tag::String(s)) => s.clone(),
                _ => "minecraft:overworld".to_owned(),
            },
            x_center: int_at(data, "xCenter"),
            z_center: int_at(data, "zCenter"),
            locked: flag(data, "locked", false),
            tracking: flag(data, "trackingPosition", true),
            unlimited: flag(data, "unlimitedTracking", false),
            colors,
            banners: list_of(data, "banners", banner_of),
            frames: list_of(data, "frames", frame_of),
            viewers: HashMap::new(),
            probes: HashMap::new(),
            dirty: false,
        }
    }
}

impl Store {
    fn alloc(&mut self, ctx: &ServerContext) -> i32 {
        if self.next.is_none() {
            self.next = Some(read_last(ctx).map_or(0, |last| last + 1));
        }
        let id = self.next.unwrap_or(0);
        self.next = Some(id + 1);
        write_last(ctx, id);
        id
    }

    fn get(&mut self, ctx: &ServerContext, id: i32) -> Option<&mut MapFile> {
        if !self.maps.contains_key(&id)
            && let Some(loaded) = load_disk(ctx, id)
        {
            self.maps.insert(id, loaded);
        }
        self.maps.get_mut(&id)
    }

    fn save_dirty(&mut self, ctx: &ServerContext) {
        let ids: Vec<i32> = self
            .maps
            .iter()
            .filter(|(_, map)| map.dirty)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let Some(map) = self.maps.get(&id) else {
                continue;
            };
            if persist(ctx, id, map)
                && let Some(map) = self.maps.get_mut(&id)
            {
                map.dirty = false;
            }
        }
    }
}

fn int_tag(tag: &Tag) -> i32 {
    match tag {
        Tag::Byte(v) => i32::from(*v),
        Tag::Short(v) => i32::from(*v),
        Tag::Int(v) => *v,
        Tag::Long(v) => *v as i32,
        _ => 0,
    }
}

fn int_at(c: &Compound, key: &str) -> i32 {
    c.get(key).map(int_tag).unwrap_or(0)
}

fn flag(c: &Compound, key: &str, default: bool) -> bool {
    match c.get(key) {
        Some(tag) => int_tag(tag) != 0,
        None => default,
    }
}

fn list_of<T>(data: &Compound, key: &str, read: impl Fn(&Compound) -> Option<T>) -> Vec<T> {
    let Some(Tag::List(list)) = data.get(key) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|tag| match tag {
            Tag::Compound(c) => read(c),
            _ => None,
        })
        .collect()
}

fn pos_of(tag: &Tag) -> Option<(i32, i32, i32)> {
    match tag {
        Tag::Compound(c) => Some((int_at(c, "X"), int_at(c, "Y"), int_at(c, "Z"))),
        Tag::List(list) if list.len() >= 3 => {
            Some((int_tag(&list[0]), int_tag(&list[1]), int_tag(&list[2])))
        }
        Tag::IntArray(values) if values.len() >= 3 => Some((values[0], values[1], values[2])),
        _ => None,
    }
}

fn pos_tag(x: i32, y: i32, z: i32) -> Tag {
    let mut pos = Compound::new();
    pos.insert("X", Tag::Int(x));
    pos.insert("Y", Tag::Int(y));
    pos.insert("Z", Tag::Int(z));
    Tag::Compound(pos)
}

fn banner_tag(banner: &Banner) -> Tag {
    let mut c = Compound::new();
    c.insert("Color", banner.color.as_str());
    if let Some(name) = &banner.name {
        c.insert("Name", name.clone());
    }
    c.insert("Pos", pos_tag(banner.x, banner.y, banner.z));
    Tag::Compound(c)
}

fn banner_of(c: &Compound) -> Option<Banner> {
    let Tag::String(color) = c.get("Color")? else {
        return None;
    };
    let (x, y, z) = pos_of(c.get("Pos")?)?;
    Some(Banner {
        color: color.clone(),
        name: c.get("Name").cloned(),
        x,
        y,
        z,
        direction: 0,
    })
}

fn frame_tag(frame: &FrameMark) -> Tag {
    let mut c = Compound::new();
    c.insert("EntityId", Tag::Int(frame.entity_id));
    c.insert("Rotation", Tag::Int(frame.rotation));
    c.insert("Pos", pos_tag(frame.x, frame.y, frame.z));
    Tag::Compound(c)
}

fn frame_of(c: &Compound) -> Option<FrameMark> {
    let (x, y, z) = pos_of(c.get("Pos")?)?;
    Some(FrameMark {
        entity_id: int_at(c, "EntityId"),
        rotation: int_at(c, "Rotation"),
        x,
        y,
        z,
    })
}

fn map_root(map: &MapFile, version: i32) -> Compound {
    let mut root = Compound::new();
    root.insert("data", Tag::Compound(map.to_data()));
    root.insert("DataVersion", Tag::Int(version));
    root
}

fn maps_dir(ctx: &ServerContext) -> Option<std::path::PathBuf> {
    Some(ctx.world.folder()?.join("data/minecraft/maps"))
}

fn persist(ctx: &ServerContext, id: i32, map: &MapFile) -> bool {
    let Some(dir) = maps_dir(ctx) else {
        return true;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let root = map_root(map, ctx.data.version.world_version);
    crate::world::write_nbt_gz(&dir.join(format!("{id}.dat")), &root).is_ok()
}

fn load_disk(ctx: &ServerContext, id: i32) -> Option<MapFile> {
    let path = maps_dir(ctx)?.join(format!("{id}.dat"));
    let root = crate::world::read_nbt_gz(&path).ok()?;
    let Tag::Compound(data) = root.get("data")? else {
        return None;
    };
    Some(MapFile::from_data(data))
}

fn read_last(ctx: &ServerContext) -> Option<i32> {
    let path = maps_dir(ctx)?.join("last_id.dat");
    let root = crate::world::read_nbt_gz(&path).ok()?;
    match root.get("data") {
        Some(Tag::Compound(data)) => match data.get("map") {
            Some(Tag::Int(id)) => Some(*id),
            _ => None,
        },
        _ => None,
    }
}

fn write_last(ctx: &ServerContext, id: i32) {
    let Some(dir) = maps_dir(ctx) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut data = Compound::new();
    data.insert("map", Tag::Int(id));
    let mut root = Compound::new();
    root.insert("data", Tag::Compound(data));
    root.insert("DataVersion", Tag::Int(ctx.data.version.world_version));
    let _ = crate::world::write_nbt_gz(&dir.join("last_id.dat"), &root);
}

fn map_id_of(stack: &ItemStack) -> Option<i32> {
    Some(int_tag(stack.components.get("map_id")?))
}

/// The block a map of `scale` is centred on when the player stands at `block`.
fn grid_center(block: i32, scale: i32) -> i32 {
    let size = 128 * (1 << scale);
    (block + 64).div_euclid(size) * size + size / 2 - 64
}

fn pixel_origin(center: i32, pixel: i32, step: i32) -> i32 {
    (center / step + pixel - 64) * step
}

fn pixel_index(center: i32, block: i32, step: i32) -> i32 {
    block.div_euclid(step) - center.div_euclid(step) + 64
}

fn world_radius(dimension: &str) -> i32 {
    if dimension == "minecraft:the_nether" {
        64
    } else {
        128
    }
}

/// The scale and lock of the map a stack shows; `None` if it has no data.
pub fn state(ctx: &ServerContext, stack: &ItemStack) -> Option<MapState> {
    let id = map_id_of(stack)?;
    let mut store = ctx.maps.lock().expect("maps lock");
    let map = store.get(ctx, id)?;
    Some(MapState {
        scale: map.scale,
        locked: map.locked,
    })
}

/// Applies a cartography table's `map_post_processing` to a stack: a locked
/// copy, or a map one scale out (centred on the new grid, blank), each under
/// a new id. Whether the stack changed.
pub fn post_process(ctx: &ServerContext, stack: &mut ItemStack) -> bool {
    let Some(Tag::String(kind)) = stack.components.reset("map_post_processing") else {
        return false;
    };
    let Some(id) = map_id_of(stack) else {
        return true;
    };
    let mut store = ctx.maps.lock().expect("maps lock");
    let Some(mut map) = store.get(ctx, id).cloned() else {
        return true;
    };
    match kind.as_str() {
        "lock" => map.locked = true,
        "scale" => zoom_out(&mut map),
        _ => return true,
    }
    map.viewers.clear();
    map.probes.clear();
    let new_id = store.alloc(ctx);
    map.dirty = !persist(ctx, new_id, &map);
    store.maps.insert(new_id, map);
    drop(store);
    stack.components.set("map_id", Tag::Int(new_id));
    true
}

fn zoom_out(map: &mut MapFile) {
    let scale = (map.scale + 1).min(4);
    map.x_center = grid_center(map.x_center, scale);
    map.z_center = grid_center(map.z_center, scale);
    map.scale = scale;
    map.colors.fill(0);
    map.banners.clear();
    map.frames.clear();
}

/// Drops "already sent the whole map" for `uuid`, so the next view is complete.
pub fn forget(ctx: &ServerContext, uuid: u128) {
    let mut store = ctx.maps.lock().expect("maps lock");
    for map in store.maps.values_mut() {
        map.viewers.remove(&uuid);
    }
}

/// Writes maps whose colours or markers changed.
pub fn save(ctx: &ServerContext) {
    ctx.maps.lock().expect("maps lock").save_dirty(ctx);
}

/// Using an empty map fills it. Using a filled map on a banner adds that banner.
pub fn use_held(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    clicked: Option<Position>,
) -> bool {
    let Some(held) = crate::held::held(ctx, actor.uuid, hand) else {
        return false;
    };
    match held.name.as_str() {
        "minecraft:map" => fill_empty(ctx, outcome, actor, &held),
        "minecraft:filled_map" => mark_banner(ctx, outcome, actor, &held, clicked),
        _ => false,
    }
}

fn fill_empty(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    held: &crate::held::Held,
) -> bool {
    let Some(filled_id) = ctx.items.id("minecraft:filled_map") else {
        return false;
    };
    let x = grid_center(actor.pose.x.floor() as i32, 0);
    let z = grid_center(actor.pose.z.floor() as i32, 0);
    let id = {
        let mut store = ctx.maps.lock().expect("maps lock");
        let id = store.alloc(ctx);
        let map = MapFile::blank(ctx.world.dimension.clone(), x, z);
        let dirty = !persist(ctx, id, &map);
        let mut map = map;
        map.dirty = dirty;
        store.maps.insert(id, map);
        id
    };
    let mut filled = held.stack.clone();
    filled.item = filled_id;
    filled.count = 1;
    filled.components.set("map_id", Tag::Int(id));
    give_filled(ctx, outcome, actor, held, filled);
    swing(outcome, actor);
    true
}

fn give_filled(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    held: &crate::held::Held,
    filled: ItemStack,
) {
    if actor.game_mode == CREATIVE {
        drop_leftover(ctx, outcome, actor, filled);
        return;
    }
    if held.stack.count <= 1 {
        set_slot(ctx, actor.uuid, held.slot, filled);
        return;
    }
    crate::held::consume(ctx, actor.uuid, held.slot, 1);
    drop_leftover(ctx, outcome, actor, filled);
}

fn drop_leftover(ctx: &ServerContext, outcome: &mut Outcome, actor: Actor, filled: ItemStack) {
    let left = crate::held::give(ctx, actor.uuid, &filled);
    if left > 0 {
        let mut drop = filled;
        drop.count = left;
        outcome
            .drops
            .push((feet_of(&actor), crate::loot::Drop::of(&drop)));
    }
}

fn set_slot(ctx: &ServerContext, uuid: u128, slot: usize, stack: ItemStack) {
    {
        let mut inventories = ctx.inventories.lock().expect("inventories lock");
        if let Some(hands) = inventories.get_mut(&uuid) {
            hands.slots[slot] = Some(stack);
        }
    }
    crate::held::changed(ctx, uuid, &[slot]);
}

fn feet_of(actor: &Actor) -> Position {
    Position::new(
        actor.pose.x.floor() as i32,
        actor.pose.y.floor() as i32,
        actor.pose.z.floor() as i32,
    )
}

fn swing(outcome: &mut Outcome, actor: Actor) {
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
}

fn mark_banner(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    held: &crate::held::Held,
    clicked: Option<Position>,
) -> bool {
    let Some(clicked) = clicked else {
        return false;
    };
    let block = ctx
        .world
        .block(clicked, &ctx.data.blocks)
        .and_then(|state| ctx.data.blocks.block_name(state))
        .unwrap_or("");
    let Some(color) = banner_color(block) else {
        return false;
    };
    let Some(id) = map_id_of(&held.stack) else {
        return false;
    };
    let name = banner_name(ctx, clicked);
    let mut store = ctx.maps.lock().expect("maps lock");
    let Some(map) = store.get(ctx, id) else {
        return false;
    };
    // A locked map, or a map of another dimension, does not gain markers.
    if map.locked || map.dimension != ctx.world.dimension {
        drop(store);
        swing(outcome, actor);
        return true;
    }
    if map
        .banners
        .iter()
        .any(|banner| (banner.x, banner.y, banner.z) == (clicked.x, clicked.y, clicked.z))
    {
        drop(store);
        swing(outcome, actor);
        return true;
    }
    let direction = if map.dimension == "minecraft:the_nether" {
        direction_of_yaw(actor.pose.yaw)
    } else {
        0
    };
    map.banners.push(Banner {
        color,
        name,
        x: clicked.x,
        y: clicked.y,
        z: clicked.z,
        direction,
    });
    map.dirty = true;
    drop(store);
    swing(outcome, actor);
    true
}

fn banner_color(name: &str) -> Option<String> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let color = name
        .strip_suffix("_wall_banner")
        .or_else(|| name.strip_suffix("_banner"))?;
    (!color.is_empty()).then(|| color.to_owned())
}

fn banner_name(ctx: &ServerContext, pos: Position) -> Option<Tag> {
    let entity = ctx.world.block_entity(pos, &ctx.data.blocks)?;
    entity.data.get("CustomName").cloned()
}

fn banner_kind(color: &str) -> i32 {
    const DYES: &[&str] = &[
        "white",
        "orange",
        "magenta",
        "light_blue",
        "yellow",
        "lime",
        "pink",
        "gray",
        "light_gray",
        "cyan",
        "purple",
        "blue",
        "brown",
        "green",
        "red",
        "black",
    ];
    let index = DYES.iter().position(|dye| *dye == color).unwrap_or(0);
    10 + index as i32
}

/// One tick of every map someone here is holding or has framed.
pub fn tick(ctx: &ServerContext) {
    let hands = hands_by_player(ctx);
    let players = players_here(ctx, &hands);
    let frames = frames_here(ctx);
    let mut packets = Vec::new();
    {
        let mut store = ctx.maps.lock().expect("maps lock");
        for player in &players {
            for hand in &player.maps {
                let _ = store.get(ctx, hand.id);
            }
        }
        for frame in &frames {
            let _ = store.get(ctx, frame.map_id);
        }
        let ids: Vec<i32> = store.maps.keys().copied().collect();
        let here = ctx.world.dimension.clone();
        for id in ids {
            let Some(map) = store.maps.get_mut(&id) else {
                continue;
            };
            update_map(ctx, map, id, &players, &frames, &here, &mut packets);
        }
        store.save_dirty(ctx);
    }
    for (uuid, packet) in packets {
        let _ = ctx.events.send(Event::Direct {
            uuid,
            packet: Direct::Map(packet),
        });
    }
}

fn update_map(
    ctx: &ServerContext,
    map: &mut MapFile,
    id: i32,
    players: &[PlayerSnap],
    frames: &[FrameSnap],
    here: &str,
    packets: &mut Vec<(u128, MapItemData)>,
) {
    let same = map.dimension == here;
    if same {
        sync_frames(ctx, map, frames, id);
        for player in players.iter().filter(|player| holds(player, id)) {
            if player.game_mode == SPECTATOR {
                continue;
            }
            let at = (player.x.floor() as i32, player.z.floor() as i32);
            explore(ctx, map, at, ProbeKey::Player(player.uuid));
        }
        for frame in frames.iter().filter(|frame| frame.map_id == id) {
            explore(
                ctx,
                map,
                (frame.x, frame.z),
                ProbeKey::Frame(frame.entity_id),
            );
        }
        let spots: Vec<(i32, i32)> = players
            .iter()
            .filter(|player| player.game_mode != SPECTATOR && holds(player, id))
            .map(|player| (player.x.floor() as i32, player.z.floor() as i32))
            .collect();
        sweep_banners(ctx, map, &spots);
    }
    send_viewers(map, id, players, frames, same, packets);
}

fn holds(player: &PlayerSnap, id: i32) -> bool {
    player.maps.iter().any(|hand| hand.id == id)
}

fn hands_by_player(ctx: &ServerContext) -> HashMap<u128, Vec<HandMap>> {
    let inventories = ctx.inventories.lock().expect("inventories lock");
    inventories
        .iter()
        .map(|(uuid, hands)| (*uuid, hand_maps(ctx, hands)))
        .collect()
}

fn hand_maps(ctx: &ServerContext, hands: &Hands) -> Vec<HandMap> {
    let mut slots = vec![hands.hand_slot(0)];
    let off = hands.hand_slot(1);
    if off != slots[0] {
        slots.push(off);
    }
    let mut out = Vec::new();
    for slot in slots {
        let Some(stack) = hands.slots.get(slot).and_then(|s| s.as_ref()) else {
            continue;
        };
        if ctx.items.name(stack.item) != Some("minecraft:filled_map") {
            continue;
        }
        let Some(id) = map_id_of(stack) else {
            continue;
        };
        out.push(HandMap {
            id,
            icons: stack_icons(stack),
        });
    }
    out
}

fn players_here(ctx: &ServerContext, hands: &HashMap<u128, Vec<HandMap>>) -> Vec<PlayerSnap> {
    let players = ctx.players.lock().expect("players lock");
    let mut out = Vec::new();
    for (uuid, player) in players.iter() {
        if player.dimension != ctx.world.dimension {
            continue;
        }
        let Some(pose) = player.pose else {
            continue;
        };
        out.push(PlayerSnap {
            uuid: *uuid,
            x: pose.x,
            y: pose.y,
            z: pose.z,
            yaw: pose.yaw,
            game_mode: player.game_mode,
            maps: hands.get(uuid).cloned().unwrap_or_default(),
        });
    }
    out
}

fn frames_here(ctx: &ServerContext) -> Vec<FrameSnap> {
    let entities = ctx.entities.lock().expect("entities lock");
    let mut out = Vec::new();
    for entity in entities.iter().filter(|entity| !entity.removed) {
        let mc_entity::Kind::Hung(hung) = &entity.kind else {
            continue;
        };
        let mc_entity::extra::Decor::Frame(frame) = &hung.decor else {
            continue;
        };
        let Some(item) = &frame.item else {
            continue;
        };
        if ctx.items.name(item.item) != Some("minecraft:filled_map") {
            continue;
        }
        let Some(map_id) = map_id_of(item) else {
            continue;
        };
        let (x, y, z) = entity.block_pos();
        out.push(FrameSnap {
            entity_id: entity.id,
            x,
            y,
            z,
            facing: hung.facing,
            pos: entity.pos,
            map_id,
            icons: stack_icons(item),
        });
    }
    out
}

fn stack_icons(stack: &ItemStack) -> Vec<StackIcon> {
    let Some(Tag::Compound(icons)) = stack.components.get("map_decorations") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, value) in icons.iter() {
        let Tag::Compound(icon) = value else {
            continue;
        };
        let Some(Tag::String(kind_name)) = icon.get("type") else {
            continue;
        };
        let Some(kind) =
            mc_protocol::components::registry_id("minecraft:map_decoration_type", kind_name)
        else {
            continue;
        };
        let Some(x) = number_of(icon.get("x")) else {
            continue;
        };
        let Some(z) = number_of(icon.get("z")) else {
            continue;
        };
        out.push(StackIcon {
            kind,
            x,
            z,
            rotation: number_of(icon.get("rotation")).unwrap_or(0.0) as f32,
        });
    }
    out
}

fn number_of(tag: Option<&Tag>) -> Option<f64> {
    match tag? {
        Tag::Double(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v)),
        Tag::Byte(v) => Some(f64::from(*v)),
        Tag::Short(v) => Some(f64::from(*v)),
        Tag::Int(v) => Some(f64::from(*v)),
        Tag::Long(v) => Some(*v as f64),
        _ => None,
    }
}

fn explore(ctx: &ServerContext, map: &mut MapFile, at: (i32, i32), who: ProbeKey) {
    if map.locked || map.dimension != ctx.world.dimension {
        return;
    }
    let phase = {
        let phase = map.probes.entry(who).or_insert(0);
        let now = *phase;
        *phase = phase.wrapping_add(1);
        now
    };
    let step = 1 << map.scale;
    let radius = world_radius(&map.dimension);
    let reach = radius >> map.scale;
    let player_px = pixel_index(map.x_center, at.0, step);
    let phase = i32::from(phase & 15);
    for px in 0..128 {
        if (px & 15) != phase || (px - player_px).abs() > reach {
            continue;
        }
        let mut column = px;
        let mut guard = 0;
        while column < 128 && guard < 128 {
            guard += 1;
            if !paint_column(ctx, map, column, at, radius) {
                break;
            }
            column += 1;
        }
    }
}

fn paint_column(
    ctx: &ServerContext,
    map: &mut MapFile,
    px: i32,
    at: (i32, i32),
    radius: i32,
) -> bool {
    let step = 1 << map.scale;
    let reach = radius / step.max(1);
    let player_pz = pixel_index(map.z_center, at.1, step);
    let z0 = (player_pz - reach).max(0);
    let z1 = (player_pz + reach).min(127);
    let nether = map.dimension == "minecraft:the_nether";
    let mut north = sample_pixel(ctx, map, px, z0 - 1, nether)
        .map(|pixel| pixel.height)
        .unwrap_or(0.0);
    let mut changed = false;
    for pz in z0..=z1 {
        let Some(pixel) = sample_pixel(ctx, map, px, pz, nether) else {
            north = 0.0;
            continue;
        };
        let ox = pixel_origin(map.x_center, px, step);
        let oz = pixel_origin(map.z_center, pz, step);
        if in_radius(ox - at.0, oz - at.1, radius, px, pz) {
            let shade = if pixel.base == 12 {
                water_shade(pixel.water, px, pz)
            } else {
                land_shade(pixel.height, north, step, px, pz)
            };
            changed |= write_px(map, px, pz, paint(pixel.base, shade));
        }
        north = pixel.height;
    }
    changed
}

fn sample_pixel(
    ctx: &ServerContext,
    map: &MapFile,
    px: i32,
    pz: i32,
    nether: bool,
) -> Option<Pixel> {
    let step = 1 << map.scale;
    let ox = pixel_origin(map.x_center, px, step);
    let oz = pixel_origin(map.z_center, pz, step);
    let mut samples = Vec::with_capacity((step * step) as usize);
    for dz in 0..step {
        for dx in 0..step {
            let column = sample_column(ctx, ox + dx, oz + dz, nether);
            if !column.loaded {
                return None;
            }
            samples.push(column);
        }
    }
    Some(majority(&samples))
}

fn sample_column(ctx: &ServerContext, x: i32, z: i32, nether: bool) -> Column {
    if !ctx.world.is_loaded(Position::new(x, 0, z)) {
        return Column {
            base: 0,
            height: 0,
            water: 0,
            loaded: false,
        };
    }
    if nether {
        return Column {
            base: nether_base(x, z),
            height: 100,
            water: 0,
            loaded: true,
        };
    }
    let min_y = ctx.world.min_y;
    let Some(top) = ctx
        .world
        .top_where(x, z, |state| !ctx.data.blocks.is_air(state))
    else {
        return Column {
            base: 0,
            height: 0,
            water: 0,
            loaded: false,
        };
    };
    if top <= min_y {
        return Column {
            base: 0,
            height: 0,
            water: 0,
            loaded: true,
        };
    }
    walk_down(ctx, x, z, top - 1, min_y)
}

fn walk_down(ctx: &ServerContext, x: i32, z: i32, mut y: i32, min_y: i32) -> Column {
    while y >= min_y {
        let Some(color) = color_at(ctx, x, y, z) else {
            return Column {
                base: 0,
                height: 0,
                water: 0,
                loaded: false,
            };
        };
        if color == 12 {
            return water_column(ctx, x, z, y, min_y);
        }
        if color != 0 {
            return Column {
                base: color,
                height: y,
                water: 0,
                loaded: true,
            };
        }
        y -= 1;
    }
    Column {
        base: 0,
        height: 0,
        water: 0,
        loaded: true,
    }
}

fn water_column(ctx: &ServerContext, x: i32, z: i32, mut y: i32, min_y: i32) -> Column {
    let surface = y;
    let mut water = 0;
    while y >= min_y {
        let Some(color) = color_at(ctx, x, y, z) else {
            return Column {
                base: 0,
                height: 0,
                water: 0,
                loaded: false,
            };
        };
        if color != 12 {
            break;
        }
        water += 1;
        y -= 1;
    }
    Column {
        base: 12,
        height: surface,
        water,
        loaded: true,
    }
}

fn color_at(ctx: &ServerContext, x: i32, y: i32, z: i32) -> Option<u8> {
    let state = ctx.world.block_if_loaded(Position::new(x, y, z))?;
    Some(ctx.data.blocks.map_color(state))
}

fn majority(samples: &[Column]) -> Pixel {
    let mut counts = [0u16; 62];
    let mut order = [u16::MAX; 62];
    let mut next = 0u16;
    for sample in samples {
        let index = usize::from(sample.base);
        if index == 0 || index >= counts.len() {
            continue;
        }
        if counts[index] == 0 {
            order[index] = next;
            next += 1;
        }
        counts[index] += 1;
    }
    let mut best = 0u8;
    let mut best_n = 0u16;
    let mut best_order = u16::MAX;
    for (index, count) in counts.iter().enumerate().skip(1) {
        if *count > best_n || (*count == best_n && *count > 0 && order[index] < best_order) {
            best = index as u8;
            best_n = *count;
            best_order = order[index];
        }
    }
    let mut sum = 0i64;
    let mut count = 0i32;
    let mut water = 0i32;
    for sample in samples {
        if best == 0 || sample.base == best {
            sum += i64::from(sample.height);
            count += 1;
        }
        if sample.base == 12 {
            water += sample.water;
        }
    }
    let height = if count == 0 {
        0.0
    } else {
        sum as f64 / f64::from(count)
    };
    Pixel {
        base: best,
        height,
        water,
    }
}

fn nether_base(x: i32, z: i32) -> u8 {
    let mut n = x.wrapping_add(z.wrapping_mul(231871));
    n = n
        .wrapping_mul(n)
        .wrapping_mul(31287121)
        .wrapping_add(n.wrapping_mul(11));
    if (n as u32 >> 20) & 1 == 0 { 10 } else { 11 }
}

fn land_shade(height: f64, north: f64, step: i32, px: i32, pz: i32) -> u8 {
    let dither = f64::from((px + pz) & 1) - 0.5;
    let diff = (height - north) * 4.0 / (f64::from(step) + 4.0) + dither * 0.4;
    if diff > 0.6 {
        2
    } else if diff < -0.6 {
        0
    } else {
        1
    }
}

fn water_shade(depth: i32, px: i32, pz: i32) -> u8 {
    let dither = f64::from((px + pz) & 1) * 0.2;
    let depth = f64::from(depth) * 0.1 + dither;
    if depth < 0.5 {
        2
    } else if depth > 0.9 {
        0
    } else {
        1
    }
}

fn paint(base: u8, shade: u8) -> u8 {
    if base == 0 { 0 } else { base * 4 + shade }
}

fn in_radius(dx: i32, dz: i32, radius: i32, px: i32, pz: i32) -> bool {
    let dist2 = i64::from(dx) * i64::from(dx) + i64::from(dz) * i64::from(dz);
    let radius = i64::from(radius);
    if dist2 >= radius * radius {
        return false;
    }
    let inner = (radius - 2).max(0);
    dist2 < inner * inner || (px + pz) & 1 != 0
}

fn write_px(map: &mut MapFile, px: i32, pz: i32, color: u8) -> bool {
    let Some(index) = usize::try_from(px)
        .ok()
        .zip(usize::try_from(pz).ok())
        .map(|(x, z)| x + z * 128)
        .filter(|index| *index < map.colors.len())
    else {
        return false;
    };
    if map.colors[index] == color {
        return false;
    }
    map.colors[index] = color;
    map.dirty = true;
    let (x, z) = (px as u8, pz as u8);
    for viewer in map.viewers.values_mut() {
        if !viewer.seen {
            continue;
        }
        viewer.dirty = Some(match viewer.dirty {
            None => (x, z, x, z),
            Some((x0, z0, x1, z1)) => (x0.min(x), z0.min(z), x1.max(x), z1.max(z)),
        });
    }
    true
}

fn sync_frames(ctx: &ServerContext, map: &mut MapFile, frames: &[FrameSnap], id: i32) {
    let mut changed = false;
    for frame in frames.iter().filter(|frame| frame.map_id == id) {
        let rotation = facing_degrees(frame.facing);
        if let Some(stored) = map
            .frames
            .iter_mut()
            .find(|stored| (stored.x, stored.y, stored.z) == (frame.x, frame.y, frame.z))
        {
            if stored.entity_id != frame.entity_id || stored.rotation != rotation {
                stored.entity_id = frame.entity_id;
                stored.rotation = rotation;
                changed = true;
            }
        } else {
            map.frames.push(FrameMark {
                entity_id: frame.entity_id,
                rotation,
                x: frame.x,
                y: frame.y,
                z: frame.z,
            });
            changed = true;
        }
    }
    let before = map.frames.len();
    map.frames.retain(|stored| {
        if !ctx
            .world
            .is_loaded(Position::new(stored.x, stored.y, stored.z))
        {
            return true;
        }
        frames.iter().any(|frame| {
            frame.map_id == id && (frame.x, frame.y, frame.z) == (stored.x, stored.y, stored.z)
        })
    });
    if map.frames.len() != before || changed {
        map.dirty = true;
    }
}

fn facing_degrees(facing: u8) -> i32 {
    match facing {
        3 => 180,
        4 => 270,
        5 => 90,
        _ => 0,
    }
}

fn sweep_banners(ctx: &ServerContext, map: &mut MapFile, players: &[(i32, i32)]) {
    let radius = i64::from(world_radius(&map.dimension));
    let before = map.banners.len();
    map.banners
        .retain(|banner| banner_still_there(ctx, banner, players, radius));
    if map.banners.len() != before {
        map.dirty = true;
    }
}

fn banner_still_there(
    ctx: &ServerContext,
    banner: &Banner,
    players: &[(i32, i32)],
    radius: i64,
) -> bool {
    let near = players.iter().any(|(x, z)| {
        let dx = i64::from(banner.x - x);
        let dz = i64::from(banner.z - z);
        dx * dx + dz * dz < radius * radius
    });
    if !near {
        return true;
    }
    let pos = Position::new(banner.x, banner.y, banner.z);
    if !ctx.world.is_loaded(pos) {
        return true;
    }
    ctx.world
        .block_if_loaded(pos)
        .and_then(|state| ctx.data.blocks.block_name(state))
        .unwrap_or("")
        .ends_with("_banner")
}

fn send_viewers(
    map: &mut MapFile,
    id: i32,
    players: &[PlayerSnap],
    frames: &[FrameSnap],
    here: bool,
    packets: &mut Vec<(u128, MapItemData)>,
) {
    let mut watching = Vec::new();
    for player in players {
        let near = here
            && frames
                .iter()
                .any(|frame| frame.map_id == id && near_frame(player, frame));
        if holds(player, id) || near {
            watching.push(player.uuid);
        }
    }
    map.viewers.retain(|uuid, _| watching.contains(uuid));
    for uuid in watching {
        let extra = extra_icons(uuid, id, players, frames);
        let icons = decorations(map, players, &extra, here);
        let Some(packet) = take_packet(map, id, uuid, icons) else {
            continue;
        };
        packets.push((uuid, packet));
    }
}

fn take_packet(
    map: &mut MapFile,
    id: i32,
    uuid: u128,
    icons: Vec<MapDecoration>,
) -> Option<MapItemData> {
    let send = match map.viewers.get(&uuid) {
        None => true,
        Some(viewer) => !viewer.seen || viewer.dirty.is_some() || viewer.last_icons != icons,
    };
    if !send {
        return None;
    }
    let patch = match map.viewers.get(&uuid) {
        Some(viewer) if viewer.seen => viewer.dirty.map(|bounds| rect_patch(&map.colors, bounds)),
        _ => Some(MapColorPatch {
            columns: 128,
            rows: 128,
            x: 0,
            z: 0,
            data: map.colors.clone(),
        }),
    };
    let (scale, locked) = (map.scale as u8, map.locked);
    let viewer = map.viewers.entry(uuid).or_default();
    viewer.seen = true;
    viewer.dirty = None;
    viewer.last_icons = icons.clone();
    Some(MapItemData {
        map_id: id,
        scale,
        locked,
        icons: Some(icons),
        patch,
    })
}

fn near_frame(player: &PlayerSnap, frame: &FrameSnap) -> bool {
    let dx = player.x - frame.pos[0];
    let dy = player.y - frame.pos[1];
    let dz = player.z - frame.pos[2];
    dx * dx + dy * dy + dz * dz <= 128.0 * 128.0
}

fn extra_icons(
    uuid: u128,
    id: i32,
    players: &[PlayerSnap],
    frames: &[FrameSnap],
) -> Vec<StackIcon> {
    let Some(player) = players.iter().find(|player| player.uuid == uuid) else {
        return Vec::new();
    };
    let mut icons: Vec<StackIcon> = player
        .maps
        .iter()
        .filter(|hand| hand.id == id)
        .flat_map(|hand| hand.icons.iter().cloned())
        .collect();
    if icons.is_empty()
        && let Some(frame) = frames
            .iter()
            .find(|frame| frame.map_id == id && near_frame(player, frame))
    {
        icons.extend(frame.icons.iter().cloned());
    }
    icons
}

fn decorations(
    map: &MapFile,
    players: &[PlayerSnap],
    extra: &[StackIcon],
    here: bool,
) -> Vec<MapDecoration> {
    let mut out = Vec::new();
    if map.tracking && here {
        for player in players
            .iter()
            .filter(|player| player.game_mode != SPECTATOR)
        {
            if let Some(icon) = player_icon(map, player) {
                push_icon(&mut out, icon);
            }
        }
    }
    for frame in &map.frames {
        push_icon(
            &mut out,
            placed(
                map,
                1,
                f64::from(frame.x) + 0.5,
                f64::from(frame.z) + 0.5,
                direction_of_degrees(frame.rotation as f32),
                None,
            ),
        );
    }
    for banner in &map.banners {
        push_icon(
            &mut out,
            placed(
                map,
                banner_kind(&banner.color),
                f64::from(banner.x),
                f64::from(banner.z),
                banner.direction,
                banner.name.as_ref().map(text_component),
            ),
        );
    }
    for icon in extra {
        push_icon(
            &mut out,
            placed(
                map,
                icon.kind,
                icon.x,
                icon.z,
                direction_of_degrees(icon.rotation),
                None,
            ),
        );
    }
    out
}

fn push_icon(out: &mut Vec<MapDecoration>, icon: MapDecoration) {
    if out.len() < 256 {
        out.push(icon);
    }
}

fn player_icon(map: &MapFile, player: &PlayerSnap) -> Option<MapDecoration> {
    let dx = player.x - f64::from(map.x_center);
    let dz = player.z - f64::from(map.z_center);
    let step = f64::from(1 << map.scale);
    let (f, g) = (dx / step, dz / step);
    let on_map = f.abs() <= 63.0 && g.abs() <= 63.0;
    if on_map {
        return Some(MapDecoration {
            kind: 0,
            x: axis_byte(f),
            z: axis_byte(g),
            direction: direction_of_yaw(player.yaw),
            name: None,
        });
    }
    let far = 320.0 * (f64::from(map.scale) + 1.0);
    let away = dx.abs().max(dz.abs()) > far;
    if away && !map.unlimited {
        return None;
    }
    Some(MapDecoration {
        kind: if away { 7 } else { 6 },
        x: axis_byte(f),
        z: axis_byte(g),
        direction: direction_of_offset(dx, dz),
        name: None,
    })
}

fn placed(
    map: &MapFile,
    kind: i32,
    x: f64,
    z: f64,
    direction: u8,
    name: Option<Tag>,
) -> MapDecoration {
    let step = f64::from(1 << map.scale);
    MapDecoration {
        kind,
        x: axis_byte((x - f64::from(map.x_center)) / step),
        z: axis_byte((z - f64::from(map.z_center)) / step),
        direction,
        name,
    }
}

fn axis_byte(offset: f64) -> i8 {
    if offset <= -63.0 {
        -128
    } else if offset >= 63.0 {
        127
    } else {
        (offset * 2.0 + 0.5) as i32 as i8
    }
}

fn direction_of_yaw(yaw: f32) -> u8 {
    ((yaw + 180.0) * 16.0 / 360.0).floor().rem_euclid(16.0) as u8
}

fn direction_of_degrees(degrees: f32) -> u8 {
    (degrees.rem_euclid(360.0) / 22.5).round().rem_euclid(16.0) as u8
}

fn direction_of_offset(dx: f64, dz: f64) -> u8 {
    let degrees = dz.atan2(dx).to_degrees();
    ((degrees + 90.0) / 22.5).round().rem_euclid(16.0) as u8
}

fn text_component(tag: &Tag) -> Tag {
    match tag {
        Tag::String(text) => {
            let mut component = Compound::new();
            component.insert("text", text.as_str());
            Tag::Compound(component)
        }
        other => other.clone(),
    }
}

fn rect_patch(colors: &[u8], bounds: (u8, u8, u8, u8)) -> MapColorPatch {
    let (x0, z0, x1, z1) = bounds;
    let columns = x1 - x0 + 1;
    let rows = z1 - z0 + 1;
    let mut data = Vec::with_capacity(usize::from(columns) * usize::from(rows));
    for z in z0..=z1 {
        for x in x0..=x1 {
            data.push(colors[usize::from(x) + usize::from(z) * 128]);
        }
    }
    MapColorPatch {
        columns,
        rows,
        x: x0,
        z: z0,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Pose;

    #[test]
    fn centres_follow_the_scale_grid() {
        assert_eq!(grid_center(0, 0), 0);
        assert_eq!(grid_center(0, 3), 448);
        assert_eq!(pixel_origin(448, 0, 8), -64);
        assert_eq!(pixel_origin(448, 127, 8), 952);
        assert_eq!(pixel_index(0, 0, 1), 64);
    }

    #[test]
    fn shades_match_the_wiki_bands() {
        assert_eq!(land_shade(0.0, 0.0, 1, 0, 0), 1);
        assert_eq!(land_shade(10.0, 0.0, 1, 0, 0), 2);
        assert_eq!(land_shade(0.0, 10.0, 1, 0, 0), 0);
        assert_eq!(water_shade(0, 0, 0), 2);
        assert_eq!(water_shade(6, 0, 0), 1);
        assert_eq!(water_shade(12, 0, 0), 0);
        assert_eq!(paint(1, 1), 5);
        assert_eq!(paint(0, 2), 0);
        let once = nether_base(3, 9);
        assert!(once == 10 || once == 11);
        assert_eq!(nether_base(3, 9), once);
    }

    #[test]
    fn facing_bytes() {
        assert_eq!(direction_of_yaw(180.0), 0);
        assert_eq!(direction_of_yaw(0.0), 8);
        assert_eq!(direction_of_yaw(90.0), 12);
        assert_eq!(direction_of_yaw(-90.0), 4);
        assert_eq!(direction_of_offset(0.0, -1.0), 0);
        assert_eq!(direction_of_offset(1.0, 0.0), 4);
        assert_eq!(direction_of_offset(0.0, 1.0), 8);
        assert_eq!(direction_of_offset(-1.0, 0.0), 12);
        assert_eq!(axis_byte(0.0), 0);
        assert_eq!(axis_byte(200.0), 127);
        assert_eq!(axis_byte(-200.0), -128);
    }

    #[test]
    fn map_file_roundtrip() {
        let mut map = MapFile::blank("minecraft:overworld".into(), 0, 0);
        map.colors[10] = 5;
        map.locked = true;
        map.unlimited = true;
        map.banners.push(Banner {
            color: "white".into(),
            name: Some(Tag::String("Home".into())),
            x: 1,
            y: 2,
            z: 3,
            direction: 0,
        });
        map.frames.push(FrameMark {
            entity_id: 7,
            rotation: 90,
            x: 4,
            y: 5,
            z: 6,
        });
        let back = MapFile::from_data(&map.to_data());
        assert_eq!(back.colors[10], 5);
        assert!(back.colors.iter().filter(|c| **c == 0).count() >= PIXELS - 1);
        assert!(back.locked && back.unlimited && back.tracking);
        assert_eq!(back.banners[0].color, "white");
        assert_eq!(back.banners[0].name, Some(Tag::String("Home".into())));
        assert_eq!(back.frames[0].entity_id, 7);
        assert_eq!(back.frames[0].rotation, 90);
        let dir = std::env::temp_dir().join(format!("mc-maps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.dat");
        crate::world::write_nbt_gz(&path, &map_root(&map, 1)).unwrap();
        let read = crate::world::read_nbt_gz(&path).unwrap();
        let Tag::Compound(data) = read.get("data").unwrap() else {
            panic!("data");
        };
        assert_eq!(MapFile::from_data(data).colors[10], 5);
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    fn server() -> Option<std::sync::Arc<ServerContext>> {
        crate::testkit::flat()
    }

    fn actor_at(uuid: u128, entity: i32, pos: [f64; 3], yaw: f32, mode: i32) -> Actor {
        Actor {
            uuid,
            entity_id: entity,
            game_mode: mode,
            pose: Pose {
                x: pos[0],
                y: pos[1],
                z: pos[2],
                yaw,
                pitch: 0.0,
                on_ground: true,
            },
            sneaking: false,
        }
    }

    fn fill_at(ctx: &ServerContext, pos: [f64; 3]) -> i32 {
        crate::testkit::join(ctx, 1, 10, pos, 0);
        let empty = ctx.items.id("minecraft:map").expect("map");
        crate::testkit::wear(ctx, 1, 36, ItemStack::new(empty, 1));
        let actor = actor_at(1, 10, pos, 180.0, 0);
        let outcome = crate::interact::use_item(ctx, actor, 0, 180.0, 0.0);
        assert!(
            outcome
                .events
                .iter()
                .any(|event| matches!(event, Event::Swing { .. }))
        );
        let stack = ctx
            .inventories
            .lock()
            .expect("inventories lock")
            .get(&1)
            .unwrap()
            .slots[36]
            .clone()
            .unwrap();
        assert_eq!(ctx.items.name(stack.item), Some("minecraft:filled_map"));
        let id = map_id_of(&stack).unwrap();
        let state = state(ctx, &stack).unwrap();
        assert_eq!(state.scale, 0);
        assert!(!state.locked);
        id
    }

    #[test]
    fn filling_colours_grass_and_sends_the_map() {
        let Some(ctx) = server() else { return };
        let blocks = &ctx.data.blocks;
        assert_eq!(
            blocks.map_color(blocks.default_state("minecraft:grass_block").unwrap()),
            1
        );
        assert_eq!(
            blocks.map_color(blocks.default_state("minecraft:water").unwrap()),
            12
        );
        assert_eq!(
            blocks.map_color(blocks.default_state("minecraft:glass").unwrap()),
            0
        );
        assert_eq!(
            blocks.map_color(blocks.default_state("minecraft:stone").unwrap()),
            11
        );
        let wet = blocks
            .state_id("minecraft:oak_leaves", &[("waterlogged", "true")])
            .unwrap();
        assert_eq!(blocks.map_color(wet), 12);
        assert_eq!(
            blocks.map_color(
                blocks
                    .state_id("minecraft:oak_log", &[("axis", "y")])
                    .unwrap()
            ),
            13
        );
        assert_eq!(
            blocks.map_color(
                blocks
                    .state_id("minecraft:oak_log", &[("axis", "x")])
                    .unwrap()
            ),
            34
        );
        let id = fill_at(&ctx, [0.0, -59.0, 0.0]);
        {
            let map = &ctx.maps.lock().expect("maps lock").maps[&id];
            assert_eq!(map.dimension, "minecraft:overworld");
            assert!(map.tracking);
            assert_eq!((map.x_center, map.z_center), (0, 0));
        }
        if let Some(player) = ctx.players.lock().expect("players lock").get_mut(&1)
            && let Some(pose) = &mut player.pose
        {
            pose.yaw = 180.0;
        }
        let mut seen = ctx.events.subscribe();
        for _ in 0..16 {
            tick(&ctx);
        }
        assert_eq!(
            ctx.maps.lock().expect("maps lock").maps[&id].colors[64 + 64 * 128],
            5
        );
        let mut full = false;
        let mut marker = false;
        while let Some(event) = seen.try_changed() {
            let Event::Direct {
                packet: Direct::Map(packet),
                ..
            } = event
            else {
                continue;
            };
            assert_eq!(packet.map_id, id);
            if packet
                .patch
                .as_ref()
                .is_some_and(|patch| patch.columns == 128)
            {
                full = true;
                assert_eq!(packet.patch.unwrap().data[64 + 64 * 128], 5);
            }
            marker |=
                packet.icons.unwrap_or_default().iter().any(|icon| {
                    icon.kind == 0 && icon.direction == 0 && icon.x == 0 && icon.z == 0
                });
        }
        assert!(full && marker);
    }

    #[test]
    fn a_locked_map_does_not_resample() {
        let Some(ctx) = server() else { return };
        let id = fill_at(&ctx, [0.0, -59.0, 0.0]);
        for _ in 0..16 {
            tick(&ctx);
        }
        let before = ctx.maps.lock().expect("maps lock").maps[&id].colors[64 + 64 * 128];
        assert_eq!(before, 5);
        ctx.maps
            .lock()
            .expect("maps lock")
            .maps
            .get_mut(&id)
            .unwrap()
            .locked = true;
        crate::testkit::set(&ctx, [0, -61, 0], "minecraft:stone");
        tick(&ctx);
        assert_eq!(
            ctx.maps.lock().expect("maps lock").maps[&id].colors[64 + 64 * 128],
            before
        );
    }

    #[test]
    fn markers_cover_players_frames_banners_and_explorer_targets() {
        let Some(ctx) = server() else { return };
        let id = fill_at(&ctx, [0.0, -59.0, 0.0]);
        crate::testkit::join(&ctx, 2, 11, [200.0, -59.0, 0.0], 0);
        crate::testkit::join(&ctx, 3, 12, [400.0, -59.0, 0.0], 0);
        let mut seen = ctx.events.subscribe();
        tick(&ctx);
        let icons = take_icons(&mut seen);
        assert!(icons.iter().any(|icon| icon.kind == 6 && icon.x == 127));
        assert!(!icons.iter().any(|icon| icon.kind == 7));
        ctx.maps
            .lock()
            .expect("maps lock")
            .maps
            .get_mut(&id)
            .unwrap()
            .unlimited = true;
        tick(&ctx);
        let icons = take_icons(&mut seen);
        assert!(icons.iter().any(|icon| icon.kind == 7));

        crate::testkit::set(&ctx, [2, -60, 2], "minecraft:white_banner");
        let mut name = Compound::new();
        name.insert("CustomName", Tag::String("Home".into()));
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: 2,
                y: -60,
                z: 2,
                id: "minecraft:banner".into(),
                data: name,
            },
            &ctx.data.blocks,
        );
        let actor = actor_at(1, 10, [2.0, -59.0, 1.0], 180.0, 0);
        assert!(
            crate::interact::use_item_on(&ctx, actor, 0, Position::new(2, -60, 2), 1, [0.5; 3], 1)
                .events
                .iter()
                .any(|e| matches!(e, Event::Swing { .. }))
        );
        assert_eq!(
            ctx.maps.lock().expect("maps lock").maps[&id].banners.len(),
            1
        );
        tick(&ctx);
        let icons = take_icons(&mut seen);
        assert!(
            icons
                .iter()
                .any(|icon| icon.kind == 10 && icon.name.is_some())
        );

        let stack = ctx
            .inventories
            .lock()
            .expect("inventories lock")
            .get(&1)
            .unwrap()
            .slots[36]
            .clone()
            .unwrap();
        ctx.entities
            .lock()
            .expect("entities lock")
            .push(mc_entity::Entity {
                id: 50,
                uuid: 99,
                kind: mc_entity::Kind::Hung(mc_entity::extra::Hung {
                    decor: mc_entity::extra::Decor::Frame(mc_entity::extra::Frame {
                        glow: false,
                        item: Some(stack),
                        rotation: 0,
                        invisible: false,
                        fixed: false,
                    }),
                    facing: 2,
                    yaw: 0.0,
                    anchor: [0, -60, 0],
                }),
                pos: [0.5, -60.0, 0.5],
                vel: [0.0; 3],
                on_ground: false,
                age: 0,
                removed: false,
                tags: Vec::new(),
            });
        tick(&ctx);
        assert_eq!(
            ctx.maps.lock().expect("maps lock").maps[&id].frames.len(),
            1
        );
        assert!(take_icons(&mut seen).iter().any(|icon| icon.kind == 1));

        {
            let mut inventories = ctx.inventories.lock().expect("inventories lock");
            let stack = inventories.get_mut(&1).unwrap().slots[36].as_mut().unwrap();
            let mut icon = Compound::new();
            icon.insert("type", "minecraft:mansion");
            icon.insert("x", Tag::Double(0.0));
            icon.insert("z", Tag::Double(0.0));
            icon.insert("rotation", Tag::Float(0.0));
            let mut icons = Compound::new();
            icons.insert("woodland", Tag::Compound(icon));
            stack
                .components
                .set("map_decorations", Tag::Compound(icons));
        }
        tick(&ctx);
        assert!(take_icons(&mut seen).iter().any(|icon| icon.kind == 8));
    }

    fn take_icons(seen: &mut crate::context::EventReceiver) -> Vec<MapDecoration> {
        let mut icons = Vec::new();
        while let Some(event) = seen.try_changed() {
            let Event::Direct {
                packet: Direct::Map(packet),
                ..
            } = event
            else {
                continue;
            };
            icons.extend(packet.icons.unwrap_or_default());
        }
        icons
    }

    #[test]
    fn cartography_scale_copies_onto_a_blank_grid() {
        let Some(ctx) = server() else { return };
        let id = fill_at(&ctx, [0.0, -59.0, 0.0]);
        for _ in 0..4 {
            tick(&ctx);
        }
        let mut stack = ctx
            .inventories
            .lock()
            .expect("inventories lock")
            .get(&1)
            .unwrap()
            .slots[36]
            .clone()
            .unwrap();
        stack
            .components
            .set("map_post_processing", Tag::from("scale"));
        assert!(post_process(&ctx, &mut stack));
        let new_id = map_id_of(&stack).unwrap();
        assert_ne!(new_id, id);
        let scaled = state(&ctx, &stack).unwrap();
        assert_eq!(scaled.scale, 1);
        assert!(!scaled.locked);
        let store = ctx.maps.lock().expect("maps lock");
        assert!(store.maps[&new_id].colors.iter().all(|c| *c == 0));
        assert_eq!(store.maps[&new_id].x_center, 64);
        assert_eq!(store.maps[&id].scale, 0);
        drop(store);

        let mut missing = ItemStack::new(ctx.items.id("minecraft:filled_map").unwrap(), 1);
        missing.components.set("map_id", Tag::Int(99));
        missing
            .components
            .set("map_post_processing", Tag::from("lock"));
        assert!(post_process(&ctx, &mut missing));
        assert_eq!(map_id_of(&missing), Some(99));
        assert!(missing.components.get("map_post_processing").is_none());
    }
}
