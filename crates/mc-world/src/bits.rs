//! Fixed-width values packed into 64-bit words. Entries never span two words;
//! leftover high bits of each word are padding (the format since 1.16).

/// Bits needed to store every value in `0..=max`.
pub fn bits_needed(max: u32) -> u32 {
    32 - max.leading_zeros()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitStorage {
    bits: u32,
    len: usize,
    words: Vec<i64>,
}

impl BitStorage {
    /// `bits` is 1..=32.
    pub fn new(bits: u32, len: usize) -> Self {
        assert!(
            (1..=32).contains(&bits),
            "bits per entry must be 1..=32, got {bits}"
        );
        Self {
            bits,
            len,
            words: vec![0; Self::word_count(bits, len)],
        }
    }

    pub fn word_count(bits: u32, len: usize) -> usize {
        let per_word = (64 / bits) as usize;
        len.div_ceil(per_word)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn locate(&self, index: usize) -> (usize, u32) {
        assert!(index < self.len, "index {index} out of {}", self.len);
        let per_word = (64 / self.bits) as usize;
        (index / per_word, (index % per_word) as u32 * self.bits)
    }

    fn mask(&self) -> u64 {
        (1 << self.bits) - 1
    }

    pub fn get(&self, index: usize) -> u32 {
        let (word, shift) = self.locate(index);
        ((self.words[word] as u64 >> shift) & self.mask()) as u32
    }

    pub fn set(&mut self, index: usize, value: u32) {
        let mask = self.mask();
        assert!(
            u64::from(value) <= mask,
            "{value} does not fit in {} bits",
            self.bits
        );
        let (word, shift) = self.locate(index);
        let w = self.words[word] as u64;
        self.words[word] = ((w & !(mask << shift)) | (u64::from(value) << shift)) as i64;
    }

    pub fn words(&self) -> &[i64] {
        &self.words
    }

    pub fn from_values(bits: u32, values: impl ExactSizeIterator<Item = u32>) -> Self {
        let mut storage = Self::new(bits, values.len());
        for (i, value) in values.enumerate() {
            storage.set(i, value);
        }
        storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_protocol_documentation_example() {
        // 5 bits per entry, 24 entries -> 2 longs (12 entries each, 4 padding bits).
        let values = [
            1, 2, 2, 3, 4, 4, 5, 6, 6, 4, 8, 0, 7, 4, 3, 13, 15, 16, 9, 14, 10, 12, 0, 2,
        ];
        let storage = BitStorage::from_values(5, values.into_iter());
        assert_eq!(
            storage.words(),
            &[0x0020863148418841, 0x01018A7260F68C87_u64 as i64]
        );
        assert!(values.iter().enumerate().all(|(i, &v)| storage.get(i) == v));
    }

    #[test]
    fn word_counts() {
        assert_eq!(BitStorage::word_count(4, 4096), 256);
        assert_eq!(BitStorage::word_count(15, 4096), 1024); // 4 per word
        assert_eq!(BitStorage::word_count(9, 256), 37); // heightmap: 7 per word
    }

    #[test]
    fn bits_needed_values() {
        assert_eq!(bits_needed(0), 0);
        assert_eq!(bits_needed(1), 1);
        assert_eq!(bits_needed(15), 4);
        assert_eq!(bits_needed(16), 5);
        assert_eq!(bits_needed(384), 9);
    }
}
