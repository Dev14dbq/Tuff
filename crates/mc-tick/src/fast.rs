//! A quick hash for block positions (nothing an outsider chooses), for the
//! sets and maps block logic touches every tick.

use std::hash::{BuildHasher, Hasher};

#[derive(Clone, Copy, Debug, Default)]
pub struct Fast;

#[derive(Debug, Default)]
pub struct FastHasher(u64);

impl Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(u64::from(b));
        }
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn write_usize(&mut self, v: usize) {
        self.write_u64(v as u64);
    }
    fn write_i32(&mut self, v: i32) {
        self.write_u64(u64::from(v as u32));
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(u64::from(v));
    }
}

impl BuildHasher for Fast {
    type Hasher = FastHasher;
    fn build_hasher(&self) -> FastHasher {
        FastHasher(0)
    }
}

pub type FastSet<T> = std::collections::HashSet<T, Fast>;
pub type FastMap<K, V> = std::collections::HashMap<K, V, Fast>;
