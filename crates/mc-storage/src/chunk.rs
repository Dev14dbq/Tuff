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

use std::fmt;

use mc_data::Blocks;
use mc_nbt::{Compound, Tag};
use mc_world::{
    BIOMES, BLOCKS, BlockEntity, Chunk, Kind, PalettedContainer, Section, SectionLight,
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
    Ok(Decoded::Chunk(Box::new(chunk)))
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
