//! Chunk NBT (DataVersion 5023, Minecraft 26.3) → [`mc_world::Chunk`].
//!
//! Format from the public documentation ("Chunk format"), checked against
//! chunks a vanilla 26.3 server saved (`cargo xtask chunk-info`):
//! - `sections`: compounds with `Y`, `block_states {palette, data}`,
//!   `biomes {palette, data}` and optional `SkyLight` / `BlockLight` (2048 bytes);
//! - a block palette is a list of strings (default states) or, when mixed, a
//!   list of compounds: `{"": name}` for a default state, `{id, properties}`
//!   otherwise;
//! - indices are packed like the protocol, at least 4 bits for blocks, the
//!   minimum needed for biomes; a one-entry palette has no `data`.
//!
//! Encoding goes the other way on top of the chunk's stored NBT, so whatever
//! we do not model (structures, scheduled ticks, post-processing) is kept.

use std::fmt;

use mc_data::Blocks;
use mc_nbt::{Compound, Tag};
use mc_world::{
    BIOMES, BLOCKS, BitStorage, BlockEntity, Chunk, Kind, PalettedContainer, Section, SectionLight,
    bits_needed,
};

#[derive(Debug, PartialEq)]
pub struct ChunkError(pub String);

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad chunk: {}", self.0)
    }
}

impl std::error::Error for ChunkError {}

fn bad(what: impl Into<String>) -> ChunkError {
    ChunkError(what.into())
}

/// What decoding needs from the game data.
pub struct Palettes<'a> {
    pub blocks: &'a Blocks,
    /// Biome name → network id (position in our biome registry).
    pub biome: &'a dyn Fn(&str) -> Option<u32>,
    /// Used for anything that does not resolve.
    pub air: u32,
    pub fallback_biome: u32,
    /// Palette entries that did not resolve (replaced by air / fallback biome).
    pub unresolved: std::cell::Cell<u32>,
}

impl fmt::Debug for Palettes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Palettes")
            .field("air", &self.air)
            .field("fallback_biome", &self.fallback_biome)
            .field("unresolved", &self.unresolved.get())
            .finish_non_exhaustive()
    }
}

/// Result of decoding: the chunk, or why it is not usable yet.
#[derive(Debug)]
pub enum Decoded {
    Chunk(Box<Chunk>),
    /// Generation has not finished (`Status` other than `minecraft:full`).
    NotFull(String),
}

fn get<'a>(c: &'a Compound, key: &str) -> Option<&'a Tag> {
    c.get(key)
}

fn int(c: &Compound, key: &str) -> Result<i32, ChunkError> {
    match get(c, key) {
        Some(Tag::Int(v)) => Ok(*v),
        Some(Tag::Byte(v)) => Ok(i32::from(*v)),
        _ => Err(bad(format!("missing int {key}"))),
    }
}

/// `min_y` and `height` are the dimension's (e.g. -64 and 384).
pub fn decode_chunk(
    root: &Tag,
    min_y: i32,
    height: u32,
    palettes: &Palettes,
) -> Result<Decoded, ChunkError> {
    let Tag::Compound(root) = root else {
        return Err(bad("root is not a compound"));
    };
    // 26.4 renames the key to lowercase `status`; accept both.
    let status = match get(root, "Status").or_else(|| get(root, "status")) {
        Some(Tag::String(s)) => s.as_str(),
        _ => "minecraft:full",
    };
    if status != "minecraft:full" {
        return Ok(Decoded::NotFull(status.to_owned()));
    }
    decode_any(root, min_y, height, palettes).map(Decoded::Chunk)
}

/// The chunk's status (`minecraft:full` when there is none) and its blocks
/// whatever the status (a chunk still being generated has its sections too).
pub fn decode_with_status(
    root: &Tag,
    min_y: i32,
    height: u32,
    palettes: &Palettes,
) -> Result<(String, Box<Chunk>), ChunkError> {
    let Tag::Compound(root) = root else {
        return Err(bad("root is not a compound"));
    };
    let status = match get(root, "Status").or_else(|| get(root, "status")) {
        Some(Tag::String(s)) => s.clone(),
        _ => "minecraft:full".to_owned(),
    };
    Ok((status, decode_any(root, min_y, height, palettes)?))
}

fn decode_any(
    root: &Compound,
    min_y: i32,
    height: u32,
    palettes: &Palettes,
) -> Result<Box<Chunk>, ChunkError> {
    let (x, z) = (int(root, "xPos")?, int(root, "zPos")?);
    let mut chunk = Chunk::new(x, z, min_y, height, palettes.air, palettes.fallback_biome);
    let section_count = chunk.sections.len();
    chunk.light = vec![SectionLight::default(); section_count];
    let min_section = min_y.div_euclid(16);

    if let Some(Tag::List(sections)) = get(root, "sections") {
        for section in sections {
            let Tag::Compound(section) = section else {
                return Err(bad("section is not a compound"));
            };
            let y = int(section, "Y")?;
            let Some(index) = usize::try_from(y - min_section)
                .ok()
                .filter(|&i| i < section_count)
            else {
                continue; // outside the world height
            };
            let blocks = match get(section, "block_states") {
                Some(Tag::Compound(states)) => decode_blocks(states, palettes)?,
                _ => PalettedContainer::single(BLOCKS, palettes.air),
            };
            let biomes = match get(section, "biomes") {
                Some(Tag::Compound(biomes)) => decode_biomes(biomes, palettes)?,
                _ => PalettedContainer::single(BIOMES, palettes.fallback_biome),
            };
            chunk.sections[index] = Section { blocks, biomes };
            chunk.light[index] = SectionLight {
                sky: light_array(section, "SkyLight")?,
                block: light_array(section, "BlockLight")?,
            };
        }
    }

    if let Some(Tag::Compound(heightmaps)) = get(root, "Heightmaps") {
        for (name, value) in heightmaps.iter() {
            if let Tag::LongArray(words) = value {
                chunk.heightmaps.push((name.to_owned(), words.clone()));
            }
        }
    }

    if let Some(Tag::List(entities)) = get(root, "block_entities") {
        for entity in entities {
            let Tag::Compound(entity) = entity else {
                continue;
            };
            let Some(Tag::String(id)) = get(entity, "id") else {
                continue;
            };
            let mut data = entity.clone();
            for key in ["id", "x", "y", "z", "keepPacked"] {
                data.remove(key);
            }
            chunk.block_entities.push(BlockEntity {
                x: int(entity, "x")?,
                y: int(entity, "y")?,
                z: int(entity, "z")?,
                id: id.clone(),
                data,
            });
        }
    }
    Ok(Box::new(chunk))
}

fn light_array(section: &Compound, key: &str) -> Result<Option<Vec<u8>>, ChunkError> {
    match get(section, key) {
        None => Ok(None),
        Some(Tag::ByteArray(bytes)) if bytes.len() == 2048 => {
            Ok(Some(bytes.iter().map(|&b| b as u8).collect()))
        }
        Some(_) => Err(bad(format!("{key} is not 2048 bytes"))),
    }
}

/// A stored palette and its packed indices → container. Indices use
/// `bits_needed(len - 1)` bits, at least 4 for blocks (the protocol's packing).
fn container(
    kind: Kind,
    ids: Vec<u32>,
    data: Option<&Tag>,
    what: &str,
) -> Result<PalettedContainer, ChunkError> {
    if ids.is_empty() {
        return Err(bad(format!("empty {what} palette")));
    }
    if ids.len() == 1 {
        return Ok(PalettedContainer::single(kind, ids[0]));
    }
    let Some(Tag::LongArray(words)) = data else {
        return Err(bad(format!(
            "{what} palette with several entries but no data"
        )));
    };
    PalettedContainer::from_palette(kind, ids, words.clone())
        .ok_or_else(|| bad(format!("{what} data does not match its palette")))
}

fn decode_blocks(states: &Compound, palettes: &Palettes) -> Result<PalettedContainer, ChunkError> {
    let Some(Tag::List(palette)) = get(states, "palette") else {
        return Err(bad("block_states without palette"));
    };
    let ids: Vec<u32> = palette
        .iter()
        .map(|entry| block_entry(entry, palettes))
        .collect();
    container(BLOCKS, ids, get(states, "data"), "block")
}

/// One block palette entry → global state id (air if unknown).
fn block_entry(entry: &Tag, palettes: &Palettes) -> u32 {
    let id = match entry {
        Tag::String(name) => palettes.blocks.state_id(name, &[]),
        Tag::Compound(c) => match (get(c, ""), get(c, "id")) {
            (Some(Tag::String(name)), _) => palettes.blocks.state_id(name, &[]),
            (_, Some(Tag::String(name))) => {
                let properties: Vec<(&str, &str)> = match get(c, "properties") {
                    Some(Tag::Compound(props)) => props
                        .iter()
                        .filter_map(|(k, v)| match v {
                            Tag::String(v) => Some((k, v.as_str())),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                palettes.blocks.state_id(name, &properties)
            }
            _ => None,
        },
        _ => None,
    };
    id.unwrap_or_else(|| {
        palettes.unresolved.set(palettes.unresolved.get() + 1);
        palettes.air
    })
}

fn decode_biomes(biomes: &Compound, palettes: &Palettes) -> Result<PalettedContainer, ChunkError> {
    let Some(Tag::List(palette)) = get(biomes, "palette") else {
        return Err(bad("biomes without palette"));
    };
    let ids: Vec<u32> = palette
        .iter()
        .map(|entry| match entry {
            Tag::String(name) => (palettes.biome)(name),
            _ => None,
        })
        .map(|id| {
            id.unwrap_or_else(|| {
                palettes.unresolved.set(palettes.unresolved.get() + 1);
                palettes.fallback_biome
            })
        })
        .collect();
    container(BIOMES, ids, get(biomes, "data"), "biome")
}

/// What encoding needs from the game data.
#[derive(Debug)]
pub struct Names<'a> {
    pub blocks: &'a Blocks,
    /// Biome network id → name.
    pub biomes: &'a [String],
    /// Written when there is no stored chunk to start from.
    pub data_version: i32,
}

/// One block palette entry: the name for a default state, `{id, properties}`
/// otherwise (vanilla writes the same; a mixed list wraps the strings).
fn block_palette_entry(state: u32, names: &Names) -> Result<Tag, ChunkError> {
    let blocks = names.blocks;
    let name = blocks
        .block_name(state)
        .ok_or_else(|| bad(format!("unknown block state {state}")))?;
    if blocks.default_state(name) == Some(state) {
        return Ok(Tag::String(name.to_owned()));
    }
    let key = blocks.describe(state).unwrap_or_default();
    let inner = key[name.len()..]
        .trim_start_matches('[')
        .trim_end_matches(']');
    let mut properties = Compound::new();
    for (k, v) in inner.split(',').filter_map(|kv| kv.split_once('=')) {
        properties.insert(k, Tag::String(v.to_owned()));
    }
    let mut entry = Compound::new();
    entry.insert("id", Tag::String(name.to_owned()));
    entry.insert("properties", Tag::Compound(properties));
    Ok(Tag::Compound(entry))
}

/// A list whose elements may differ in type: if they do, every element that is
/// not a compound is wrapped as `{"": value}`.
fn mixed_list(items: Vec<Tag>) -> Tag {
    let first = items.first().map(Tag::type_id);
    if items.iter().all(|t| Some(t.type_id()) == first) {
        return Tag::List(items);
    }
    Tag::List(
        items
            .into_iter()
            .map(|t| match t {
                Tag::Compound(c) => Tag::Compound(c),
                other => {
                    let mut wrapper = Compound::new();
                    wrapper.insert("", other);
                    Tag::Compound(wrapper)
                }
            })
            .collect(),
    )
}

fn encode_container(
    container: &PalettedContainer,
    entry: impl Fn(u32) -> Result<Tag, ChunkError>,
) -> Result<Compound, ChunkError> {
    let (palette, indices) = container.compact();
    let mut out = Compound::new();
    let entries = palette
        .iter()
        .map(|&v| entry(v))
        .collect::<Result<Vec<_>, _>>()?;
    out.insert("palette", mixed_list(entries));
    if palette.len() > 1 {
        let bits = bits_needed(palette.len() as u32 - 1).max(container.kind().min_bits);
        let words = BitStorage::from_values(bits, indices.into_iter());
        out.insert("data", Tag::LongArray(words.words().to_vec()));
    }
    Ok(out)
}

fn light_tag(bytes: &[u8]) -> Tag {
    Tag::ByteArray(bytes.iter().map(|&b| b as i8).collect())
}

/// Chunk → NBT, starting from `stored` (the chunk's NBT as it was read, if
/// any). Sections, heightmaps and block entities are replaced. With
/// `relight`, `isLightOn` is cleared so vanilla recomputes light on load.
pub fn encode_chunk(
    chunk: &Chunk,
    stored: Option<&Tag>,
    names: &Names,
    relight: bool,
) -> Result<Tag, ChunkError> {
    let mut root = match stored {
        Some(Tag::Compound(root)) => root.clone(),
        Some(_) => return Err(bad("stored root is not a compound")),
        None => {
            let mut root = Compound::new();
            root.insert("DataVersion", Tag::Int(names.data_version));
            root.insert("Status", Tag::String("minecraft:full".into()));
            root
        }
    };
    // A finished chunk (one stored while still generating is finished now).
    root.insert("Status", Tag::String("minecraft:full".into()));
    let min_section = chunk.min_y.div_euclid(16);
    root.insert("xPos", Tag::Int(chunk.x));
    root.insert("zPos", Tag::Int(chunk.z));
    root.insert("yPos", Tag::Int(min_section));

    // Sections by Y: stored ones outside the world height (light only) stay.
    let mut by_y: Vec<(i32, Compound)> = match root.get("sections") {
        Some(Tag::List(list)) => list
            .iter()
            .filter_map(|t| match t {
                Tag::Compound(c) => Some((int(c, "Y").ok()?, c.clone())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    for (i, section) in chunk.sections.iter().enumerate() {
        let y = min_section + i as i32;
        let index = match by_y.iter().position(|(sy, _)| *sy == y) {
            Some(index) => index,
            None => {
                let mut fresh = Compound::new();
                fresh.insert("Y", Tag::Byte(y as i8));
                by_y.push((y, fresh));
                by_y.len() - 1
            }
        };
        let compound = &mut by_y[index].1;
        compound.insert(
            "block_states",
            Tag::Compound(encode_container(&section.blocks, |s| {
                block_palette_entry(s, names)
            })?),
        );
        compound.insert(
            "biomes",
            Tag::Compound(encode_container(&section.biomes, |b| {
                names
                    .biomes
                    .get(b as usize)
                    .map(|n| Tag::String(n.clone()))
                    .ok_or_else(|| bad(format!("unknown biome {b}")))
            })?),
        );
        compound.remove("SkyLight");
        compound.remove("BlockLight");
        if let Some(light) = chunk.light.get(i) {
            if let Some(sky) = &light.sky {
                compound.insert("SkyLight", light_tag(sky));
            }
            if let Some(block) = &light.block {
                compound.insert("BlockLight", light_tag(block));
            }
        }
    }
    by_y.sort_by_key(|(y, _)| *y);
    root.insert(
        "sections",
        Tag::List(by_y.into_iter().map(|(_, c)| Tag::Compound(c)).collect()),
    );

    let mut heightmaps = Compound::new();
    for (name, words) in &chunk.heightmaps {
        heightmaps.insert(name.as_str(), Tag::LongArray(words.clone()));
    }
    root.insert("Heightmaps", Tag::Compound(heightmaps));

    let entities = chunk
        .block_entities
        .iter()
        .map(|be| {
            let mut c = Compound::new();
            c.insert("id", Tag::String(be.id.clone()));
            c.insert("x", Tag::Int(be.x));
            c.insert("y", Tag::Int(be.y));
            c.insert("z", Tag::Int(be.z));
            for (k, v) in be.data.iter() {
                c.insert(k, v.clone());
            }
            Tag::Compound(c)
        })
        .collect();
    root.insert("block_entities", Tag::List(entities));
    if relight {
        root.insert("isLightOn", Tag::Byte(0));
    }
    Ok(Tag::Compound(root))
}
