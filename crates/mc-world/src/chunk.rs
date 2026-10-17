//! Chunk columns and 16³ sections, with their network encoding.
//!
//! Sections keep block states and biomes in paletted containers, packed the
//! way the protocol sends them.

use mc_data::Blocks;

use crate::bits::{BitStorage, bits_needed};
use crate::palette::{BIOMES, BLOCKS, PalettedContainer};

pub const SECTION_SIZE: usize = 16;
pub const SECTION_VOLUME: usize = 16 * 16 * 16;
/// Biomes are stored per 4×4×4 cell.
pub const BIOME_VOLUME: usize = 4 * 4 * 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub blocks: PalettedContainer,
    pub biomes: PalettedContainer,
}

impl Section {
    pub fn filled(block: u32, biome: u32) -> Self {
        Self {
            blocks: PalettedContainer::single(BLOCKS, block),
            biomes: PalettedContainer::single(BIOMES, biome),
        }
    }

    /// From 4096 block state ids and 64 biome ids (same index order as the
    /// protocol: x fastest, then z, then y).
    pub fn from_parts(blocks: Vec<u32>, biomes: Vec<u32>) -> Self {
        assert_eq!(blocks.len(), SECTION_VOLUME, "a section has 4096 blocks");
        assert_eq!(biomes.len(), BIOME_VOLUME, "a section has 64 biome cells");
        Self {
            blocks: PalettedContainer::from_values(BLOCKS, &blocks),
            biomes: PalettedContainer::from_values(BIOMES, &biomes),
        }
    }

    /// Index of a block, x fastest, then z, then y.
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        (y << 8) | (z << 4) | x
    }

    pub fn block(&self, x: usize, y: usize, z: usize) -> u32 {
        self.blocks.get(Self::index(x, y, z))
    }

    /// Returns the previous state.
    pub fn set_block(&mut self, x: usize, y: usize, z: usize, state: u32) -> u32 {
        self.blocks.set(Self::index(x, y, z), state)
    }

    /// Network form: block count, fluid count, block states, biomes.
    pub fn write_network(&self, blocks: &Blocks, biome_count: u32, out: &mut Vec<u8>) {
        let non_air = self.blocks.count(|s| !blocks.is_air(s));
        let fluids = self.blocks.count(|s| blocks.is_fluid(s));
        out.extend_from_slice(&(non_air as i16).to_be_bytes());
        out.extend_from_slice(&(fluids as i16).to_be_bytes());
        self.blocks
            .write_network(bits_needed(blocks.state_count().saturating_sub(1)), out);
        self.biomes
            .write_network(bits_needed(biome_count.saturating_sub(1)), out);
    }
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
    /// Returns the previous state. Heightmaps are not touched (see
    /// [`Chunk::update_heightmap`]).
    pub fn set_block(&mut self, x: usize, y: i32, z: usize, state: u32) -> u32 {
        let local = (y - self.min_y) as usize;
        self.sections[local / 16].set_block(x, local % 16, z, state)
    }

    /// Whether `y` (world coordinate) is inside the chunk's height.
    pub fn contains_y(&self, y: i32) -> bool {
        y >= self.min_y && y < self.min_y + self.height() as i32
    }

    /// Fixes one column of the stored heightmap `name` after the block at
    /// world height `y` changed to `state`; `counts` says which states the
    /// heightmap counts. Does nothing if the chunk has no such heightmap.
    pub fn update_heightmap(
        &mut self,
        name: &str,
        x: usize,
        y: i32,
        z: usize,
        state: u32,
        counts: impl Fn(u32) -> bool,
    ) {
        let height = self.height();
        let bits = bits_needed(height);
        let Some(index) = self.heightmaps.iter().position(|(n, _)| n == name) else {
            return;
        };
        let Some(mut storage) = BitStorage::from_words(bits, 256, self.heightmaps[index].1.clone())
        else {
            return;
        };
        let column = z * 16 + x;
        let current = storage.get(column);
        let local = (y - self.min_y) as u32;
        let top = if counts(state) {
            current.max(local + 1)
        } else if local + 1 == current {
            (0..local as i32)
                .rev()
                .find(|&l| counts(self.block(x, self.min_y + l, z)))
                .map_or(0, |l| l as u32 + 1)
        } else {
            current
        };
        if top != current {
            storage.set(column, top);
            self.heightmaps[index].1 = storage.words().to_vec();
        }
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
