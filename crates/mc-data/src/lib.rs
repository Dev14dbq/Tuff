//! Vanilla game data, loaded from the user's own server.jar via `cargo xtask extract-data`.
//!
//! Nothing from Mojang is compiled in or committed: everything is read at runtime
//! from `cache/mcdata/<version>/` (see `xtask/src/extract_data.rs` for the layout).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

mod json_nbt;

pub use json_nbt::json_to_nbt;

/// Registries the server sends to the client in Registry Data packets.
///
/// Starts from the protocol documentation's list; the documentation lags behind
/// 26.3, so the set was completed by observing a vanilla 26.3 server on the
/// wire with `cargo xtask probe` (32 registries).
pub const SYNCHRONIZED_REGISTRIES: &[&str] = &[
    "minecraft:banner_pattern",
    "minecraft:block_transformer", // not documented, seen from vanilla
    "minecraft:cat_sound_variant",
    "minecraft:cat_variant",
    "minecraft:chat_type",
    "minecraft:chicken_sound_variant",
    "minecraft:chicken_variant",
    "minecraft:cow_sound_variant",
    "minecraft:cow_variant",
    "minecraft:damage_type",
    "minecraft:decorated_pot_pattern", // not documented, seen from vanilla
    "minecraft:dialog",
    "minecraft:dimension_type",
    "minecraft:enchantment",
    "minecraft:frog_variant",
    "minecraft:instrument",
    "minecraft:jukebox_song",
    "minecraft:painting_variant",
    "minecraft:pig_sound_variant",
    "minecraft:pig_variant",
    "minecraft:sulfur_cube_archetype",
    "minecraft:test_environment",
    "minecraft:test_instance",
    "minecraft:timeline",
    "minecraft:trim_material",
    "minecraft:trim_pattern",
    "minecraft:wolf_sound_variant",
    "minecraft:wolf_variant",
    "minecraft:world_clock",
    "minecraft:worldgen/biome",
    "minecraft:worldgen/block_state_provider", // not documented, seen from vanilla
    "minecraft:zombie_nautilus_variant",
];

/// Environment attributes a vanilla 26.3 server does not send to clients
/// (observed with `cargo xtask registry-diff`). Other `minecraft:gameplay/*`
/// attributes, like `sky_light_level` or `fast_lava`, are sent.
pub const SERVER_ONLY_ATTRIBUTES: &[&str] = &[
    "minecraft:gameplay/baby_villager_activity",
    "minecraft:gameplay/bed_rule",
    "minecraft:gameplay/bees_stay_in_hive",
    "minecraft:gameplay/can_pillager_patrol_spawn",
    "minecraft:gameplay/can_start_raid",
    "minecraft:gameplay/cat_waking_up_gift_chance",
    "minecraft:gameplay/creature_world_gen_spawn_probability",
    "minecraft:gameplay/eyeblossom_open",
    "minecraft:gameplay/increased_fire_burnout",
    "minecraft:gameplay/monsters_burn",
    "minecraft:gameplay/natural_mob_spawns",
    "minecraft:gameplay/nether_portal_spawns_piglin",
    "minecraft:gameplay/respawn_anchor_works",
    "minecraft:gameplay/snow_golem_melts",
    "minecraft:gameplay/straw_bed_rule",
    "minecraft:gameplay/surface_slime_spawn_chance",
    "minecraft:gameplay/turtle_egg_hatch_chance",
    "minecraft:gameplay/villager_activity",
];

/// Removes what a vanilla 26.3 server leaves out when it sends `registry`
/// entries to clients: spawn conditions, biome generation and server-only
/// environment attributes. With this, every registry entry we send as NBT
/// matches vanilla (`cargo xtask registry-diff`).
pub fn strip_server_only(registry: &str, entry: &mut mc_nbt::Tag) {
    let mc_nbt::Tag::Compound(root) = entry else {
        return;
    };
    if registry.ends_with("_variant") {
        root.remove("spawn_conditions");
    }
    if registry == "minecraft:worldgen/biome" {
        root.remove("carvers");
        root.remove("features");
    }
    let server_only = |key: &str, _: &mc_nbt::Tag| !SERVER_ONLY_ATTRIBUTES.contains(&key);
    if let Some(mc_nbt::Tag::Compound(attributes)) = root.get_mut("attributes") {
        attributes.retain(server_only);
    }
    // An emptied `attributes` stays (vanilla sends `{}`), an emptied `tracks` goes.
    if let Some(mc_nbt::Tag::Compound(tracks)) = root.get_mut("tracks") {
        tracks.retain(server_only);
        if tracks.is_empty() {
            root.remove("tracks");
        }
    }
}

#[derive(Debug)]
pub struct DataError {
    pub path: PathBuf,
    pub message: String,
}

impl DataError {
    fn new(path: &Path, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for DataError {}

pub type Result<T> = std::result::Result<T, DataError>;

fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).map_err(|e| DataError::new(path, e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| DataError::new(path, e.to_string()))
}

/// Everything extracted for one game version.
#[derive(Debug)]
pub struct McData {
    pub root: PathBuf,
    pub version: Version,
    pub packets: Vec<PacketEntry>,
    pub blocks: Blocks,
    pub builtin: BuiltinRegistries,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub name: String,
    pub protocol_version: i32,
    pub world_version: i32,
}

/// One row of the packet report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketEntry {
    /// `handshake`, `status`, `login`, `configuration` or `play`.
    pub phase: String,
    /// `clientbound` or `serverbound`.
    pub direction: String,
    pub name: String,
    pub id: i32,
}

impl McData {
    /// Loads `cache/mcdata/<version>/` produced by `cargo xtask extract-data`.
    pub fn load(root: &Path) -> Result<Self> {
        let version_path = root.join("version.json");
        let version = read_json(&version_path)?;
        let field = |key: &str| {
            version[key]
                .as_i64()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| DataError::new(&version_path, format!("missing {key}")))
        };
        let version = Version {
            name: version["name"].as_str().unwrap_or_default().to_owned(),
            protocol_version: field("protocol_version")?,
            world_version: field("world_version")?,
        };

        let reports = root.join("reports");
        Ok(Self {
            root: root.to_owned(),
            version,
            packets: load_packets(&reports.join("packets.json"))?,
            blocks: Blocks::load(&reports.join("blocks.json"))?,
            builtin: BuiltinRegistries::load(&reports.join("registries.json"))?,
        })
    }

    pub fn data_pack(&self) -> DataPack {
        DataPack {
            root: self.root.join("data"),
        }
    }
}

fn load_packets(path: &Path) -> Result<Vec<PacketEntry>> {
    let report = read_json(path)?;
    let phases = report
        .as_object()
        .ok_or_else(|| DataError::new(path, "not an object"))?;
    let mut packets = Vec::new();
    for (phase, directions) in phases {
        for (direction, list) in directions.as_object().into_iter().flatten() {
            for (name, packet) in list.as_object().into_iter().flatten() {
                let id = packet["protocol_id"]
                    .as_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| DataError::new(path, format!("{name} has no protocol_id")))?;
                packets.push(PacketEntry {
                    phase: phase.clone(),
                    direction: direction.clone(),
                    name: name.clone(),
                    id,
                });
            }
        }
    }
    Ok(packets)
}

// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub default_state: u32,
    pub first_state: u32,
    pub last_state: u32,
    /// Properties of the default state, sorted by name.
    pub default_properties: Vec<(String, String)>,
}

/// Block types and their global state ids, from the blocks report. Block
/// registry ids are in [`BuiltinRegistries`] (`minecraft:block`).
#[derive(Debug, Default)]
pub struct Blocks {
    by_name: HashMap<String, BlockInfo>,
    state_count: u32,
    /// Per state id: air, cave air or void air.
    air: Vec<bool>,
    /// Per state id: water, lava, or `waterlogged=true`.
    fluid: Vec<bool>,
    /// `name[k=v,...]` with properties sorted by name → state id.
    by_key: HashMap<String, u32>,
    /// State id → its `name[k=v,...]` key.
    keys: Vec<String>,
}

/// `minecraft:oak_log[axis=y]`; properties must be sorted by name.
fn state_key<'a>(name: &str, properties: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let mut key = String::from(name);
    key.push('[');
    for (i, (k, v)) in properties.enumerate() {
        if i > 0 {
            key.push(',');
        }
        key.push_str(k);
        key.push('=');
        key.push_str(v);
    }
    key.push(']');
    key
}

fn sorted_properties(properties: &Value) -> Vec<(String, String)> {
    let mut list: Vec<(String, String)> = properties
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect();
    list.sort();
    list
}

const AIR_BLOCKS: &[&str] = &["minecraft:air", "minecraft:cave_air", "minecraft:void_air"];
const FLUID_BLOCKS: &[&str] = &["minecraft:water", "minecraft:lava"];

impl Blocks {
    fn load(path: &Path) -> Result<Self> {
        let report = read_json(path)?;
        let blocks = report
            .as_object()
            .ok_or_else(|| DataError::new(path, "not an object"))?;
        let mut by_name = HashMap::with_capacity(blocks.len());
        let mut state_count = 0;
        let (mut air, mut fluid) = (Vec::new(), Vec::new());
        let mut by_key = HashMap::new();
        let mut keys = Vec::new();
        for (name, block) in blocks {
            let states = block["states"]
                .as_array()
                .ok_or_else(|| DataError::new(path, format!("{name} has no states")))?;
            let ids: Vec<u32> = states
                .iter()
                .filter_map(|s| s["id"].as_u64().and_then(|v| u32::try_from(v).ok()))
                .collect();
            let default_state = states
                .iter()
                .find(|s| s["default"].as_bool() == Some(true))
                .and_then(|s| s["id"].as_u64())
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| DataError::new(path, format!("{name} has no default state")))?;
            let (first, last) = (
                *ids.iter().min().expect("states not empty"),
                *ids.iter().max().expect("states not empty"),
            );
            state_count = state_count.max(last + 1);
            let is_air = AIR_BLOCKS.contains(&name.as_str());
            let is_fluid_block = FLUID_BLOCKS.contains(&name.as_str());
            for state in states {
                let Some(id) = state["id"].as_u64().map(|v| v as usize) else {
                    continue;
                };
                if air.len() <= id {
                    air.resize(id + 1, false);
                    fluid.resize(id + 1, false);
                    keys.resize(id + 1, String::new());
                }
                air[id] = is_air;
                fluid[id] =
                    is_fluid_block || state["properties"]["waterlogged"].as_str() == Some("true");
                let properties = sorted_properties(&state["properties"]);
                let key = state_key(
                    name,
                    properties.iter().map(|(k, v)| (k.as_str(), v.as_str())),
                );
                keys[id].clone_from(&key);
                by_key.insert(key, id as u32);
            }
            let default_properties = states
                .iter()
                .find(|s| s["default"].as_bool() == Some(true))
                .map(|s| sorted_properties(&s["properties"]))
                .unwrap_or_default();
            by_name.insert(
                name.clone(),
                BlockInfo {
                    default_state,
                    first_state: first,
                    last_state: last,
                    default_properties,
                },
            );
        }
        Ok(Self {
            by_name,
            state_count,
            air,
            fluid,
            by_key,
            keys,
        })
    }

    /// `minecraft:oak_log[axis=y]` for a state id (`minecraft:stone[]` without
    /// properties).
    pub fn describe(&self, state: u32) -> Option<&str> {
        self.keys
            .get(state as usize)
            .map(String::as_str)
            .filter(|k| !k.is_empty())
    }

    /// Block name of a state: `minecraft:oak_log`.
    pub fn block_name(&self, state: u32) -> Option<&str> {
        let key = self.describe(state)?;
        Some(&key[..key.find('[').unwrap_or(key.len())])
    }

    /// Value of one property of a state.
    pub fn property(&self, state: u32, property: &str) -> Option<&str> {
        let key = self.describe(state)?;
        let inner = key[key.find('[')? + 1..].strip_suffix(']')?;
        inner
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == property)
            .map(|(_, v)| v)
    }

    /// The same state with one property changed; `None` if the block has no
    /// such property or value.
    pub fn with_property(&self, state: u32, property: &str, value: &str) -> Option<u32> {
        self.property(state, property)?;
        let key = self.describe(state)?;
        let name = self.block_name(state)?;
        let inner = &key[name.len() + 1..key.len() - 1];
        let changed = inner
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| if k == property { (k, value) } else { (k, v) });
        self.by_key.get(&state_key(name, changed)).copied()
    }

    /// Global state id of block `name` with `properties`; properties not given
    /// take their default-state values. `None` for unknown blocks or values.
    pub fn state_id(&self, name: &str, properties: &[(&str, &str)]) -> Option<u32> {
        let info = self.by_name.get(name)?;
        if properties.is_empty() {
            return Some(info.default_state);
        }
        let mut merged: std::collections::BTreeMap<&str, &str> = info
            .default_properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        for &(k, v) in properties {
            merged.insert(k, v);
        }
        self.by_key
            .get(&state_key(name, merged.into_iter()))
            .copied()
    }

    pub fn is_air(&self, state: u32) -> bool {
        self.air.get(state as usize).copied().unwrap_or(false)
    }

    pub fn is_fluid(&self, state: u32) -> bool {
        self.fluid.get(state as usize).copied().unwrap_or(false)
    }

    pub fn get(&self, name: &str) -> Option<&BlockInfo> {
        self.by_name.get(name)
    }

    pub fn default_state(&self, name: &str) -> Option<u32> {
        self.get(name).map(|b| b.default_state)
    }

    /// Size of the global block state palette.
    pub fn state_count(&self) -> u32 {
        self.state_count
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

// ---------------------------------------------------------------------------

/// Registries defined in code (blocks, items, entity types, ...), with the
/// numeric ids the protocol uses for them.
#[derive(Debug, Default)]
pub struct BuiltinRegistries {
    registries: HashMap<String, HashMap<String, i32>>,
}

impl BuiltinRegistries {
    fn load(path: &Path) -> Result<Self> {
        let report = read_json(path)?;
        let mut registries = HashMap::new();
        for (registry, body) in report.as_object().into_iter().flatten() {
            let entries = body["entries"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(name, entry)| {
                    let id = i32::try_from(entry["protocol_id"].as_i64()?).ok()?;
                    Some((name.clone(), id))
                })
                .collect();
            registries.insert(registry.clone(), entries);
        }
        Ok(Self { registries })
    }

    pub fn get(&self, registry: &str) -> Option<&HashMap<String, i32>> {
        self.registries.get(registry)
    }

    pub fn id(&self, registry: &str, entry: &str) -> Option<i32> {
        self.get(registry)?.get(entry).copied()
    }
}

// ---------------------------------------------------------------------------

/// The vanilla data pack (`data/<namespace>/...`).
#[derive(Clone, Debug)]
pub struct DataPack {
    pub root: PathBuf,
}

/// One value of a tag file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagValue {
    Entry { id: String, required: bool },
    Tag { id: String, required: bool },
}

impl DataPack {
    fn namespaces(&self) -> Result<Vec<String>> {
        let mut namespaces = Vec::new();
        let entries =
            fs::read_dir(&self.root).map_err(|e| DataError::new(&self.root, e.to_string()))?;
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                namespaces.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        namespaces.sort();
        Ok(namespaces)
    }

    /// Entries of a data-driven registry: one per JSON file under
    /// `data/<namespace>/<registry path>/`, sorted by id.
    pub fn registry_entries(&self, registry: &str) -> Result<Vec<String>> {
        let path = registry_path(registry);
        let mut entries = Vec::new();
        for namespace in self.namespaces()? {
            let dir = self.root.join(&namespace).join(path);
            for file in json_files(&dir)? {
                entries.push(format!("{namespace}:{file}"));
            }
        }
        entries.sort();
        Ok(entries)
    }

    /// Path of one registry entry's JSON file.
    pub fn entry_path(&self, registry: &str, entry: &str) -> PathBuf {
        let (namespace, path) = entry.split_once(':').unwrap_or(("minecraft", entry));
        self.root
            .join(namespace)
            .join(registry_path(registry))
            .join(format!("{path}.json"))
    }

    pub fn read_entry(&self, registry: &str, entry: &str) -> Result<Value> {
        read_json(&self.entry_path(registry, entry))
    }

    /// Registry paths that have a `tags/` directory, e.g. `block`, `worldgen/biome`.
    pub fn tag_registries(&self) -> Result<Vec<String>> {
        let mut registries = HashSet::new();
        for namespace in self.namespaces()? {
            let tags = self.root.join(namespace).join("tags");
            for file in json_files(&tags)? {
                // file = "<registry path>/<tag path>"; registry paths have at most
                // one level of nesting ("worldgen/biome").
                let mut parts = file.splitn(3, '/');
                let first = parts.next().unwrap_or_default();
                let candidate = match (parts.next(), first) {
                    (Some(second), "worldgen") => format!("worldgen/{second}"),
                    _ => first.to_owned(),
                };
                registries.insert(candidate);
            }
        }
        let mut registries: Vec<String> = registries.into_iter().collect();
        registries.sort();
        Ok(registries)
    }

    /// Raw tag definitions of one registry, keyed by tag id.
    pub fn tags(&self, registry: &str) -> Result<BTreeMap<String, Vec<TagValue>>> {
        let path = registry_path(registry);
        let mut tags = BTreeMap::new();
        for namespace in self.namespaces()? {
            let dir = self.root.join(&namespace).join("tags").join(path);
            for file in json_files(&dir)? {
                let full = dir.join(format!("{file}.json"));
                let json = read_json(&full)?;
                let values = json["values"]
                    .as_array()
                    .ok_or_else(|| DataError::new(&full, "tag without values"))?
                    .iter()
                    .map(|value| {
                        parse_tag_value(value).ok_or_else(|| DataError::new(&full, "bad tag value"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                tags.insert(format!("{namespace}:{file}"), values);
            }
        }
        Ok(tags)
    }
}

/// `minecraft:worldgen/biome` -> `worldgen/biome`.
fn registry_path(registry: &str) -> &str {
    registry.strip_prefix("minecraft:").unwrap_or(registry)
}

fn parse_tag_value(value: &Value) -> Option<TagValue> {
    let (id, required) = match value {
        Value::String(s) => (s.as_str(), true),
        Value::Object(o) => (
            o.get("id")?.as_str()?,
            o.get("required").and_then(Value::as_bool).unwrap_or(true),
        ),
        _ => return None,
    };
    Some(match id.strip_prefix('#') {
        Some(tag) => TagValue::Tag {
            id: tag.to_owned(),
            required,
        },
        None => TagValue::Entry {
            id: id.to_owned(),
            required,
        },
    })
}

/// Relative paths (without `.json`, `/`-separated) of all JSON files under `dir`.
/// A missing directory is empty.
fn json_files(dir: &Path) -> Result<Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(DataError::new(dir, err.to_string())),
        };
        for entry in entries {
            let path = entry
                .map_err(|e| DataError::new(dir, e.to_string()))?
                .path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let relative = path.strip_prefix(base).expect("walked under base");
                let relative = relative.with_extension("");
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect();
                out.push(parts.join("/"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// Resolves tag definitions into member ids. `id_of` maps an entry name to its
/// numeric id. Optional values that do not resolve are skipped; required ones
/// are reported in the returned list of problems instead of failing the whole set.
pub fn resolve_tags(
    tags: &BTreeMap<String, Vec<TagValue>>,
    id_of: impl Fn(&str) -> Option<i32>,
) -> (BTreeMap<String, Vec<i32>>, Vec<String>) {
    fn resolve(
        name: &str,
        tags: &BTreeMap<String, Vec<TagValue>>,
        id_of: &dyn Fn(&str) -> Option<i32>,
        done: &mut BTreeMap<String, Vec<i32>>,
        visiting: &mut HashSet<String>,
        problems: &mut Vec<String>,
    ) -> Option<Vec<i32>> {
        if let Some(ids) = done.get(name) {
            return Some(ids.clone());
        }
        let values = tags.get(name)?;
        if !visiting.insert(name.to_owned()) {
            problems.push(format!("tag cycle through #{name}"));
            return None;
        }
        let mut ids = Vec::new();
        for value in values {
            match value {
                TagValue::Entry { id, required } => match id_of(id) {
                    Some(numeric) => ids.push(numeric),
                    None if *required => problems.push(format!("#{name}: unknown entry {id}")),
                    None => {}
                },
                TagValue::Tag { id, required } => {
                    match resolve(id, tags, id_of, done, visiting, problems) {
                        Some(members) => ids.extend(members),
                        None if *required => problems.push(format!("#{name}: unknown tag #{id}")),
                        None => {}
                    }
                }
            }
        }
        visiting.remove(name);
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(*id));
        done.insert(name.to_owned(), ids.clone());
        Some(ids)
    }

    let mut done = BTreeMap::new();
    let mut problems = Vec::new();
    for name in tags.keys() {
        resolve(
            name,
            tags,
            &id_of,
            &mut done,
            &mut HashSet::new(),
            &mut problems,
        );
    }
    (done, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> TagValue {
        TagValue::Entry {
            id: id.into(),
            required: true,
        }
    }

    fn tag(id: &str) -> TagValue {
        TagValue::Tag {
            id: id.into(),
            required: true,
        }
    }

    #[test]
    fn tag_values_parse() {
        let json: Value = serde_json::json!([
            "minecraft:stone",
            "#minecraft:logs",
            {"id": "mod:thing", "required": false}
        ]);
        let values: Vec<_> = json
            .as_array()
            .unwrap()
            .iter()
            .map(|v| parse_tag_value(v).unwrap())
            .collect();
        assert_eq!(
            values,
            vec![
                entry("minecraft:stone"),
                tag("minecraft:logs"),
                TagValue::Entry {
                    id: "mod:thing".into(),
                    required: false
                },
            ]
        );
    }

    #[test]
    fn nested_tags_resolve_without_duplicates() {
        let mut tags = BTreeMap::new();
        tags.insert(
            "minecraft:a".to_owned(),
            vec![entry("minecraft:x"), tag("minecraft:b")],
        );
        tags.insert(
            "minecraft:b".to_owned(),
            vec![entry("minecraft:y"), entry("minecraft:x")],
        );
        tags.insert(
            "minecraft:c".to_owned(),
            vec![TagValue::Entry {
                id: "minecraft:missing".into(),
                required: false,
            }],
        );
        let ids = HashMap::from([("minecraft:x", 1), ("minecraft:y", 2)]);
        let (resolved, problems) = resolve_tags(&tags, |name| ids.get(name).copied());
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(resolved["minecraft:a"], vec![1, 2]);
        assert_eq!(resolved["minecraft:b"], vec![2, 1]);
        assert_eq!(resolved["minecraft:c"], Vec::<i32>::new());
    }

    #[test]
    fn problems_are_reported() {
        let mut tags = BTreeMap::new();
        tags.insert("minecraft:a".to_owned(), vec![tag("minecraft:a")]);
        tags.insert("minecraft:b".to_owned(), vec![entry("minecraft:nope")]);
        let (_, problems) = resolve_tags(&tags, |_| None);
        assert_eq!(problems.len(), 3, "{problems:?}"); // cycle, unknown tag via cycle, unknown entry
    }

    /// Runs against real extracted data when it exists (`cargo xtask extract-data`).
    #[test]
    fn loads_extracted_data_if_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        if !root.join("version.json").is_file() {
            eprintln!("skipped: no extracted data at {}", root.display());
            return;
        }
        let data = McData::load(&root).unwrap();
        assert_eq!(data.version.protocol_version, 777);
        assert_eq!(data.blocks.default_state("minecraft:air"), Some(0));
        assert!(data.blocks.is_air(0));
        let stone = data.blocks.default_state("minecraft:stone").unwrap();
        assert!(!data.blocks.is_air(stone) && !data.blocks.is_fluid(stone));
        let water = data.blocks.default_state("minecraft:water").unwrap();
        assert!(data.blocks.is_fluid(water));
        // Properties: missing ones default, unknown values fail.
        let blocks = &data.blocks;
        assert_eq!(
            blocks.state_id("minecraft:grass_block", &[]),
            blocks.default_state("minecraft:grass_block")
        );
        let snowy = blocks
            .state_id("minecraft:grass_block", &[("snowy", "true")])
            .unwrap();
        assert_ne!(Some(snowy), blocks.default_state("minecraft:grass_block"));
        let lichen = blocks
            .state_id("minecraft:glow_lichen", &[("north", "true")])
            .unwrap();
        assert_eq!(
            blocks.state_id(
                "minecraft:glow_lichen",
                &[("north", "true"), ("waterlogged", "false")]
            ),
            Some(lichen)
        );
        assert_eq!(
            blocks.state_id("minecraft:grass_block", &[("snowy", "maybe")]),
            None
        );
        assert_eq!(blocks.state_id("minecraft:no_such_block", &[]), None);
        assert!(data.blocks.state_count() > 20_000);
        assert!(
            data.builtin
                .id("minecraft:item", "minecraft:stone")
                .is_some()
        );

        let pack = data.data_pack();
        for registry in SYNCHRONIZED_REGISTRIES {
            assert!(
                !pack.registry_entries(registry).unwrap().is_empty(),
                "{registry} is empty"
            );
        }
        assert!(
            pack.tag_registries()
                .unwrap()
                .contains(&"worldgen/biome".to_owned())
        );
        let block_tags = pack.tags("minecraft:block").unwrap();
        let blocks = data.builtin.get("minecraft:block").unwrap();
        let (resolved, problems) = resolve_tags(&block_tags, |name| blocks.get(name).copied());
        assert!(problems.is_empty(), "{problems:?}");
        assert!(resolved["minecraft:climbable"].contains(&blocks["minecraft:ladder"]));
    }
}
