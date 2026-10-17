//! Chunk columns and 16³ sections, with their network encoding.
//!
//! Sections store global ids directly for now; palette-compressed in-memory
//! storage comes with world loading (ROADMAP Ф2). The network encoding already
//! uses palettes as the protocol requires.

use mc_data::Blocks;

use crate::bits::{BitStorage, bits_needed};

pub const SECTION_SIZE: usize = 16;
pub const SECTION_VOLUME: usize = 16 * 16 * 16;
/// Biomes are stored per 4×4×4 cell.
pub const BIOME_VOLUME: usize = 4 * 4 * 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    blocks: Vec<u32>,
    biomes: Vec<u32>,
}

impl Section {
    pub fn filled(block: u32, biome: u32) -> Self {
        Self {
            blocks: vec![block; SECTION_VOLUME],
            biomes: vec![biome; BIOME_VOLUME],
        }
    }

    /// From 4096 block state ids and 64 biome ids (same index order as the
    /// protocol: x fastest, then z, then y).
    pub fn from_parts(blocks: Vec<u32>, biomes: Vec<u32>) -> Self {
        assert_eq!(blocks.len(), SECTION_VOLUME, "a section has 4096 blocks");
        assert_eq!(biomes.len(), BIOME_VOLUME, "a section has 64 biome cells");
        Self { blocks, biomes }
    }

    /// Index of a block, x fastest, then z, then y.
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        (y << 8) | (z << 4) | x
    }

    pub fn block(&self, x: usize, y: usize, z: usize) -> u32 {
        self.blocks[Self::index(x, y, z)]
    }

    pub fn set_block(&mut self, x: usize, y: usize, z: usize, state: u32) {
        self.blocks[Self::index(x, y, z)] = state;
    }

    /// Network form: block count, fluid count, block states, biomes.
    pub fn write_network(&self, blocks: &Blocks, biome_count: u32, out: &mut Vec<u8>) {
        let non_air = self.blocks.iter().filter(|&&s| !blocks.is_air(s)).count();
        let fluids = self.blocks.iter().filter(|&&s| blocks.is_fluid(s)).count();
        out.extend_from_slice(&(non_air as i16).to_be_bytes());
        out.extend_from_slice(&(fluids as i16).to_be_bytes());
        let direct_blocks = bits_needed(blocks.state_count().saturating_sub(1));
        write_container(&self.blocks, 4..=8, direct_blocks, out);
        let direct_biomes = bits_needed(biome_count.saturating_sub(1));
        write_container(&self.biomes, 1..=3, direct_biomes, out);
    }
}

/// Paletted container: single value, indirect palette, or direct global ids.
/// The data array has no length prefix (since 1.21.5).
fn write_container(
    values: &[u32],
    indirect: std::ops::RangeInclusive<u32>,
    direct_bits: u32,
    out: &mut Vec<u8>,
) {
    let mut palette: Vec<u32> = Vec::new();
    for &value in values {
        if !palette.contains(&value) {
            palette.push(value);
            if palette.len() > 1 << indirect.end() {
                break; // too many for an indirect palette anyway
            }
        }
    }

    if let [single] = palette.as_slice() {
        out.push(0);
        put_varint(out, *single as i32);
        return;
    }

    let bits = bits_needed(palette.len() as u32 - 1).max(*indirect.start());
    let storage = if bits <= *indirect.end() {
        out.push(bits as u8);
        put_varint(out, palette.len() as i32);
        for &entry in &palette {
            put_varint(out, entry as i32);
        }
        let index = |v: u32| palette.iter().position(|&p| p == v).expect("in palette") as u32;
        BitStorage::from_values(bits, values.iter().map(|&v| index(v)))
    } else {
        out.push(direct_bits as u8);
        BitStorage::from_values(direct_bits, values.iter().copied())
    };
    for &word in storage.words() {
        out.extend_from_slice(&word.to_be_bytes());
    }
}

fn put_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    while v & !0x7F != 0 {
        out.push((v & 0x7F) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Stored light of one section: 2048 bytes, two 4-bit values per byte.
/// `None` means not stored (no light data for this section).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionLight {
    pub sky: Option<Vec<u8>>,
    pub block: Option<Vec<u8>>,
}

/// A block entity as stored with its chunk (world coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct BlockEntity {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub id: String,
    /// Everything else stored for it (without `id`, `x`, `y`, `z`).
    pub data: mc_nbt::Compound,
}

/// A full-height chunk column.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    pub min_y: i32,
    pub sections: Vec<Section>,
    /// One entry per section; empty when light was not loaded or computed.
    pub light: Vec<SectionLight>,
    /// Stored heightmaps by name (`MOTION_BLOCKING`, ...), packed like the protocol.
    pub heightmaps: Vec<(String, Vec<i64>)>,
    pub block_entities: Vec<BlockEntity>,
}

impl Chunk {
    pub fn new(x: i32, z: i32, min_y: i32, height: u32, air: u32, biome: u32) -> Self {
        assert!(
            height.is_multiple_of(16),
            "world height must be a multiple of 16"
        );
        Self {
            x,
            z,
            min_y,
            sections: vec![Section::filled(air, biome); (height / 16) as usize],
            light: Vec::new(),
            heightmaps: Vec::new(),
            block_entities: Vec::new(),
        }
    }

    pub fn height(&self) -> u32 {
        (self.sections.len() * SECTION_SIZE) as u32
    }

    /// `x` and `z` are 0..16 inside the chunk, `y` is the world coordinate.
    pub fn set_block(&mut self, x: usize, y: i32, z: usize, state: u32) {
        let local = (y - self.min_y) as usize;
        self.sections[local / 16].set_block(x, local % 16, z, state);
    }

    pub fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        let local = (y - self.min_y) as usize;
        self.sections[local / 16].block(x, local % 16, z)
    }

    /// All sections bottom to top, as the Chunk Data packet expects.
    pub fn write_sections(&self, blocks: &Blocks, biome_count: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for section in &self.sections {
            section.write_network(blocks, biome_count, &mut out);
        }
        out
    }

    /// Heightmap where a column's height is one above its highest block matching
    /// `counts`, relative to the bottom of the world (0 = empty column).
    pub fn heightmap(&self, counts: impl Fn(u32) -> bool) -> Vec<i64> {
        let height = self.height();
        let mut storage = BitStorage::new(bits_needed(height), 256);
        for z in 0..16 {
            for x in 0..16 {
                let top = (0..height as i32)
                    .rev()
                    .find(|&local| counts(self.block(x, self.min_y + local, z)))
                    .map_or(0, |local| local as u32 + 1);
                storage.set(z * 16 + x, top);
            }
        }
        storage.words().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_value_container() {
        let mut out = Vec::new();
        write_container(&[7; SECTION_VOLUME], 4..=8, 16, &mut out);
        assert_eq!(out, [0x00, 0x07]);
    }

    #[test]
    fn indirect_container_uses_at_least_minimum_bits() {
        let mut values = vec![0; SECTION_VOLUME];
        values[0] = 300;
        let mut out = Vec::new();
        write_container(&values, 4..=8, 16, &mut out);
        // bits=4, palette in first-seen order [300, 0], then 4096*4/64 = 256 longs.
        assert_eq!(&out[..5], &[4, 2, 0xac, 0x02, 0]);
        assert_eq!(out.len(), 5 + 256 * 8);
        // First long holds entries 0..16, lowest bits first: 300 -> index 0, rest -> 1.
        assert_eq!(
            &out[5..13],
            &[0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x10]
        );
    }

    #[test]
    fn direct_container_when_palette_is_too_big() {
        let values: Vec<u32> = (0..64).collect();
        let mut out = Vec::new();
        write_container(&values, 1..=3, 7, &mut out);
        assert_eq!(out[0], 7);
        assert_eq!(out.len(), 1 + BitStorage::word_count(7, 64) * 8);
    }

    #[test]
    fn heightmap_counts_from_world_bottom() {
        let mut chunk = Chunk::new(0, 0, -64, 384, 0, 0);
        chunk.set_block(0, -64, 0, 5);
        chunk.set_block(1, 10, 0, 5);
        let words = chunk.heightmap(|s| s != 0);
        // 384-high world: 9 bits per entry, 7 entries per long.
        let get = |i: usize| ((words[i / 7] as u64 >> ((i % 7) * 9)) & 0x1FF) as u32;
        assert_eq!(get(0), 1);
        assert_eq!(get(1), 75);
        assert_eq!(get(2), 0);
    }
}
