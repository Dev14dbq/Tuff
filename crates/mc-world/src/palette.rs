//! Paletted containers: the in-memory form of a section's block states (4096
//! entries) and biomes (64 entries).
//!
//! A container holds one value, a local palette with packed indices, or global
//! ids when there are too many distinct values. Indices are packed exactly as
//! the protocol sends them (at least 4 bits for blocks, 1 for biomes), so a
//! chunk packet copies the words without repacking.

use crate::bits::{BitStorage, bits_needed};

/// Sizes of one kind of container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind {
    pub entries: usize,
    /// Fewest bits per index the protocol allows for a local palette.
    pub min_bits: u32,
    /// Most bits per index for a local palette; more values go global.
    pub max_bits: u32,
}

/// Block states of one section.
pub const BLOCKS: Kind = Kind {
    entries: 4096,
    min_bits: 4,
    max_bits: 8,
};

/// Biomes of one section (4×4×4 cells).
pub const BIOMES: Kind = Kind {
    entries: 64,
    min_bits: 1,
    max_bits: 3,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Storage {
    Single(u32),
    /// Palette entries may be unused after changes; they are kept, like the
    /// palette never shrinks on its own.
    Indirect {
        palette: Vec<u32>,
        indices: BitStorage,
    },
    /// Global ids; packed with the global width only when sent.
    Direct(Vec<u32>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PalettedContainer {
    kind: Kind,
    storage: Storage,
}

impl PalettedContainer {
    pub fn single(kind: Kind, value: u32) -> Self {
        Self {
            kind,
            storage: Storage::Single(value),
        }
    }

    /// From one value per entry.
    pub fn from_values(kind: Kind, values: &[u32]) -> Self {
        Self::from_values_after(kind, None, values)
    }

    /// The palette's first entry (what a fresh copy of it starts filled
    /// with).
    pub fn first(&self) -> u32 {
        match &self.storage {
            Storage::Single(v) => *v,
            Storage::Indirect { palette, .. } => palette[0],
            Storage::Direct(values) => values[0],
        }
    }

    /// [`Self::from_values`] with `first` as the palette's first entry, as
    /// a container refilled from a fresh copy of one holding `first` is.
    pub fn from_values_after(kind: Kind, first: Option<u32>, values: &[u32]) -> Self {
        assert_eq!(values.len(), kind.entries, "wrong number of entries");
        let mut palette: Vec<u32> = first.into_iter().collect();
        for &value in values {
            if !palette.contains(&value) {
                palette.push(value);
                if palette.len() > 1 << kind.max_bits {
                    return Self {
                        kind,
                        storage: Storage::Direct(values.to_vec()),
                    };
                }
            }
        }
        if let [single] = palette[..] {
            return Self::single(kind, single);
        }
        let bits = Self::index_bits(kind, palette.len());
        let index = |v: u32| palette.iter().position(|&p| p == v).expect("in palette") as u32;
        let indices = BitStorage::from_values(bits, values.iter().map(|&v| index(v)));
        Self {
            kind,
            storage: Storage::Indirect { palette, indices },
        }
    }

    /// From a palette and indices packed with `bits_needed(len - 1)` bits, raised
    /// to the kind's minimum (as stored in chunk files). `None` if the words do
    /// not fit or an index is past the palette.
    pub fn from_palette(kind: Kind, palette: Vec<u32>, words: Vec<i64>) -> Option<Self> {
        match palette.len() {
            0 => return None,
            1 => return Some(Self::single(kind, palette[0])),
            _ => {}
        }
        let bits = bits_needed(palette.len() as u32 - 1).max(kind.min_bits);
        let indices = BitStorage::from_words(bits, kind.entries, words)?;
        if (0..kind.entries).any(|i| indices.get(i) as usize >= palette.len()) {
            return None;
        }
        if bits <= kind.max_bits {
            return Some(Self {
                kind,
                storage: Storage::Indirect { palette, indices },
            });
        }
        let values: Vec<u32> = (0..kind.entries)
            .map(|i| palette[indices.get(i) as usize])
            .collect();
        Some(Self {
            kind,
            storage: Storage::Direct(values),
        })
    }

    fn index_bits(kind: Kind, palette_len: usize) -> u32 {
        bits_needed(palette_len.saturating_sub(1) as u32).max(kind.min_bits)
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn get(&self, index: usize) -> u32 {
        assert!(index < self.kind.entries, "index {index} out of range");
        match &self.storage {
            Storage::Single(value) => *value,
            Storage::Indirect { palette, indices } => palette[indices.get(index) as usize],
            Storage::Direct(values) => values[index],
        }
    }

    /// Sets one entry and returns what was there.
    pub fn set(&mut self, index: usize, value: u32) -> u32 {
        let old = self.get(index);
        if old == value {
            return old;
        }
        let kind = self.kind;
        match &mut self.storage {
            Storage::Single(current) => {
                let palette = vec![*current, value];
                let mut indices = BitStorage::new(Self::index_bits(kind, 2), kind.entries);
                indices.set(index, 1);
                self.storage = Storage::Indirect { palette, indices };
            }
            Storage::Indirect { palette, indices } => {
                let slot = match palette.iter().position(|&p| p == value) {
                    Some(slot) => slot,
                    None => {
                        palette.push(value);
                        palette.len() - 1
                    }
                };
                let bits = Self::index_bits(kind, palette.len());
                if bits > kind.max_bits {
                    let mut values: Vec<u32> = (0..kind.entries)
                        .map(|i| palette[indices.get(i) as usize])
                        .collect();
                    values[index] = value;
                    self.storage = Storage::Direct(values);
                } else {
                    if bits > indices.bits() {
                        let grown = BitStorage::from_values(
                            bits,
                            (0..kind.entries).map(|i| indices.get(i)),
                        );
                        *indices = grown;
                    }
                    indices.set(index, slot as u32);
                }
            }
            Storage::Direct(values) => values[index] = value,
        }
        old
    }

    /// Every entry in index order.
    pub fn values(&self) -> Vec<u32> {
        (0..self.kind.entries).map(|i| self.get(i)).collect()
    }

    /// Whether any entry may satisfy `test`: the palette is checked, not
    /// the entries, so a value no longer used can still answer yes.
    pub fn may_contain(&self, test: impl Fn(u32) -> bool) -> bool {
        match &self.storage {
            Storage::Single(value) => test(*value),
            Storage::Indirect { palette, .. } => palette.iter().any(|&v| test(v)),
            Storage::Direct(values) => values.iter().any(|&v| test(v)),
        }
    }

    /// How many entries satisfy `test`, testing each distinct value once.
    pub fn count(&self, test: impl Fn(u32) -> bool) -> usize {
        match &self.storage {
            Storage::Single(value) => {
                if test(*value) {
                    self.kind.entries
                } else {
                    0
                }
            }
            Storage::Indirect { palette, indices } => {
                let matching: Vec<bool> = palette.iter().map(|&v| test(v)).collect();
                (0..self.kind.entries)
                    .filter(|&i| matching[indices.get(i) as usize])
                    .count()
            }
            Storage::Direct(values) => values.iter().filter(|&&v| test(v)).count(),
        }
    }

    /// Distinct values in use, in first-seen order, and each entry's index into
    /// that list: a compact palette for saving.
    pub fn compact(&self) -> (Vec<u32>, Vec<u32>) {
        let mut palette: Vec<u32> = Vec::new();
        let indices = (0..self.kind.entries)
            .map(|i| {
                let value = self.get(i);
                match palette.iter().position(|&p| p == value) {
                    Some(slot) => slot as u32,
                    None => {
                        palette.push(value);
                        palette.len() as u32 - 1
                    }
                }
            })
            .collect();
        (palette, indices)
    }

    /// Network form: bits per entry, then the value, the palette and indices,
    /// or global ids packed with `global_bits`. The data array has no length
    /// prefix (since 1.21.5).
    pub fn write_network(&self, global_bits: u32, out: &mut Vec<u8>) {
        match &self.storage {
            Storage::Single(value) => {
                out.push(0);
                put_varint(out, *value as i32);
            }
            Storage::Indirect { palette, indices } => {
                out.push(indices.bits() as u8);
                put_varint(out, palette.len() as i32);
                for &entry in palette {
                    put_varint(out, entry as i32);
                }
                put_words(out, indices.words());
            }
            Storage::Direct(values) => {
                out.push(global_bits as u8);
                let packed = BitStorage::from_values(global_bits, values.iter().copied());
                put_words(out, packed.words());
            }
        }
    }
}

fn put_words(out: &mut Vec<u8>, words: &[i64]) {
    out.reserve(words.len() * 8);
    for word in words {
        out.extend_from_slice(&word.to_be_bytes());
    }
}

pub(crate) fn put_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    while v & !0x7F != 0 {
        out.push((v & 0x7F) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_value_container() {
        let mut out = Vec::new();
        PalettedContainer::single(BLOCKS, 7).write_network(16, &mut out);
        assert_eq!(out, [0x00, 0x07]);
    }

    #[test]
    fn indirect_container_uses_at_least_minimum_bits() {
        let mut values = vec![0; 4096];
        values[0] = 300;
        let mut out = Vec::new();
        PalettedContainer::from_values(BLOCKS, &values).write_network(16, &mut out);
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
        let container = PalettedContainer::from_values(BIOMES, &values);
        let mut out = Vec::new();
        container.write_network(7, &mut out);
        assert_eq!(out[0], 7);
        assert_eq!(out.len(), 1 + BitStorage::word_count(7, 64) * 8);
        assert_eq!(container.values(), values);
    }

    #[test]
    fn set_grows_from_single_to_indirect_to_direct() {
        let mut c = PalettedContainer::single(BLOCKS, 0);
        assert_eq!(c.set(5, 9), 0);
        assert_eq!(c.get(5), 9);
        assert_eq!(c.get(4), 0);
        // 17 distinct values need 5 bits.
        for i in 0..16 {
            c.set(100 + i, 1000 + i as u32);
        }
        let mut out = Vec::new();
        c.write_network(16, &mut out);
        assert_eq!(out[0], 5);
        // 257 distinct values go global.
        for i in 0..300 {
            c.set(1000 + i, 5000 + i as u32);
        }
        out.clear();
        c.write_network(16, &mut out);
        assert_eq!(out[0], 16);
        assert_eq!(c.get(1299), 5299);
        assert_eq!(c.get(5), 9);
        assert_eq!(c.get(4095), 0);
        assert_eq!(c.count(|v| v == 0), 4096 - 1 - 16 - 300);
    }

    #[test]
    fn stored_palette_round_trip() {
        let mut values = vec![2u32; 4096];
        values[17] = 8;
        values[4000] = 5;
        let c = PalettedContainer::from_values(BLOCKS, &values);
        let (palette, indices) = c.compact();
        assert_eq!(palette, [2, 8, 5]);
        let words = BitStorage::from_values(4, indices.into_iter())
            .words()
            .to_vec();
        let back = PalettedContainer::from_palette(BLOCKS, palette, words).unwrap();
        assert_eq!(back.values(), values);
        // An index past the palette is refused.
        let words = BitStorage::from_values(4, std::iter::repeat_n(3, 4096))
            .words()
            .to_vec();
        assert!(PalettedContainer::from_palette(BLOCKS, vec![1, 2, 3], words).is_none());
    }
}
