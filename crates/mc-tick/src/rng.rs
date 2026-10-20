//! Small fast random numbers for block logic.

use crate::dir::BlockPos;

/// splitmix64: cheap and good enough for growth and fire chances.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Seeded from where and when, so the same tick of the same block
    /// always rolls the same.
    pub fn at(p: BlockPos, time: i64) -> Self {
        let seed = (p.x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (p.y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (p.z as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
            ^ (time as u64);
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 0..n.
    pub fn below(&mut self, n: u32) -> u32 {
        ((self.next_u64() >> 32) % u64::from(n.max(1))) as u32
    }

    /// 0.0..1.0.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}
