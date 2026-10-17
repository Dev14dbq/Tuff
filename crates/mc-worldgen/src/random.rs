//! The game's random sources (minecraft.wiki, "Random number generation"):
//! xoroshiro128++ for world generation, and java.util.Random's 48-bit LCG
//! (the "legacy" source) for older parts. Both can be forked into
//! positional factories: a source for a block position or for a name (the
//! name's MD5 for xoroshiro, its Java hash code for the legacy source).

use crate::md5::md5;

const GOLDEN_RATIO_64: u64 = 0x9E37_79B9_7F4A_7C15;
const SILVER_RATIO_64: u64 = 0x6A09_E667_F3BC_C909;

/// Stafford's 13th variant of the 64-bit mix.
pub fn mix_stafford13(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A 64-bit seed spread to 128 bits.
pub fn upgrade_seed(seed: i64) -> (u64, u64) {
    let lo = (seed as u64) ^ SILVER_RATIO_64;
    let hi = lo.wrapping_add(GOLDEN_RATIO_64);
    (mix_stafford13(lo), mix_stafford13(hi))
}

/// The seed of a block position (as the game hashes positions).
pub fn position_seed(x: i32, y: i32, z: i32) -> i64 {
    let mut l =
        (x.wrapping_mul(3_129_871) as i64) ^ (z as i64).wrapping_mul(116_129_781) ^ (y as i64);
    l = l
        .wrapping_mul(l)
        .wrapping_mul(42_317_861)
        .wrapping_add(l.wrapping_mul(11));
    l >> 16
}

/// Java's `String.hashCode`.
pub fn java_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32))
}

pub trait RandomSource {
    fn next_long(&mut self) -> i64;
    fn next_int(&mut self) -> i32;
    fn next_int_bound(&mut self, bound: i32) -> i32;
    fn next_double(&mut self) -> f64;
    fn next_float(&mut self) -> f32;
    fn next_bool(&mut self) -> bool;
    fn consume(&mut self, count: usize) {
        for _ in 0..count {
            self.next_int();
        }
    }
    /// `nextInt(min, max)` between `min` and `max`, both included.
    fn next_int_between(&mut self, min: i32, max: i32) -> i32 {
        min + self.next_int_bound(max - min + 1)
    }
    fn fork_positional(&mut self) -> Positional;
}

/// xoroshiro128++.
#[derive(Clone, Debug)]
pub struct Xoroshiro {
    lo: u64,
    hi: u64,
}

impl Xoroshiro {
    pub fn new(seed: i64) -> Self {
        let (lo, hi) = upgrade_seed(seed);
        Self::from_parts(lo, hi)
    }

    pub fn from_parts(lo: u64, hi: u64) -> Self {
        if lo == 0 && hi == 0 {
            Self {
                lo: GOLDEN_RATIO_64,
                hi: SILVER_RATIO_64,
            }
        } else {
            Self { lo, hi }
        }
    }

    fn next(&mut self) -> u64 {
        let (s0, mut s1) = (self.lo, self.hi);
        let result = s0.wrapping_add(s1).rotate_left(17).wrapping_add(s0);
        s1 ^= s0;
        self.lo = s0.rotate_left(49) ^ s1 ^ (s1 << 21);
        self.hi = s1.rotate_left(28);
        result
    }

    fn next_bits(&mut self, bits: u32) -> u64 {
        self.next() >> (64 - bits)
    }
}

impl RandomSource for Xoroshiro {
    fn next_long(&mut self) -> i64 {
        self.next() as i64
    }

    fn next_int(&mut self) -> i32 {
        self.next() as i32
    }

    fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        let bound = bound as u64;
        let mut l = u64::from(self.next_int() as u32);
        let mut m = l * bound;
        let mut low = m & 0xFFFF_FFFF;
        if low < bound {
            let threshold = (bound.wrapping_neg() & 0xFFFF_FFFF) % bound;
            while low < threshold {
                l = u64::from(self.next_int() as u32);
                m = l * bound;
                low = m & 0xFFFF_FFFF;
            }
        }
        (m >> 32) as i32
    }

    fn next_double(&mut self) -> f64 {
        self.next_bits(53) as f64 * 1.110_223_024_625_156_5E-16
    }

    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 * 5.960_464_5E-8
    }

    fn next_bool(&mut self) -> bool {
        self.next() & 1 != 0
    }

    fn fork_positional(&mut self) -> Positional {
        let lo = self.next();
        let hi = self.next();
        Positional::Xoroshiro { lo, hi }
    }
}

/// java.util.Random.
#[derive(Clone, Debug)]
pub struct Legacy {
    seed: i64,
}

const MULTIPLIER: i64 = 0x5_DEEC_E66D;
const MASK: i64 = (1 << 48) - 1;

impl Legacy {
    pub fn new(seed: i64) -> Self {
        Self {
            seed: (seed ^ MULTIPLIER) & MASK,
        }
    }

    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed ^ MULTIPLIER) & MASK;
    }

    pub fn next_bits(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(MULTIPLIER).wrapping_add(0xB) & MASK;
        (self.seed >> (48 - bits)) as i32
    }

    /// `nextGaussian` (polar method, as java.util.Random).
    pub fn next_gaussian(&mut self) -> f64 {
        loop {
            let v1 = 2.0 * self.next_double() - 1.0;
            let v2 = 2.0 * self.next_double() - 1.0;
            let s = v1 * v1 + v2 * v2;
            if s < 1.0 && s != 0.0 {
                return v1 * (-2.0 * s.ln() / s).sqrt();
            }
        }
    }
}

impl RandomSource for Legacy {
    fn next_long(&mut self) -> i64 {
        let hi = i64::from(self.next_bits(32));
        let lo = i64::from(self.next_bits(32));
        (hi << 32).wrapping_add(lo)
    }

    fn next_int(&mut self) -> i32 {
        self.next_bits(32)
    }

    fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        if bound & (bound - 1) == 0 {
            return ((i64::from(bound) * i64::from(self.next_bits(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next_bits(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }

    fn next_double(&mut self) -> f64 {
        let hi = i64::from(self.next_bits(26));
        let lo = i64::from(self.next_bits(27));
        ((hi << 27) + lo) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    fn next_float(&mut self) -> f32 {
        self.next_bits(24) as f32 / (1 << 24) as f32
    }

    fn next_bool(&mut self) -> bool {
        self.next_bits(1) != 0
    }

    fn fork_positional(&mut self) -> Positional {
        Positional::Legacy {
            seed: self.next_long(),
        }
    }
}

/// Makes random sources for positions and names.
#[derive(Clone, Copy, Debug)]
pub enum Positional {
    Xoroshiro { lo: u64, hi: u64 },
    Legacy { seed: i64 },
}

/// Either kind of source.
#[derive(Clone, Debug)]
pub enum Random {
    Xoroshiro(Xoroshiro),
    Legacy(Legacy),
}

impl Positional {
    pub fn at(&self, x: i32, y: i32, z: i32) -> Random {
        let seed = position_seed(x, y, z);
        match *self {
            Positional::Xoroshiro { lo, hi } => {
                Random::Xoroshiro(Xoroshiro::from_parts((seed as u64) ^ lo, hi))
            }
            Positional::Legacy { seed: s } => Random::Legacy(Legacy::new(seed ^ s)),
        }
    }

    pub fn from_hash_of(&self, name: &str) -> Random {
        match *self {
            Positional::Xoroshiro { lo, hi } => {
                let d = md5(name.as_bytes());
                let a = u64::from_be_bytes(d[0..8].try_into().expect("8 bytes"));
                let b = u64::from_be_bytes(d[8..16].try_into().expect("8 bytes"));
                Random::Xoroshiro(Xoroshiro::from_parts(a ^ lo, b ^ hi))
            }
            Positional::Legacy { seed } => {
                Random::Legacy(Legacy::new(i64::from(java_hash(name)) ^ seed))
            }
        }
    }

    pub fn from_seed(&self, seed: i64) -> Random {
        match *self {
            Positional::Xoroshiro { lo, hi } => Random::Xoroshiro(Xoroshiro::from_parts(
                (seed as u64) ^ lo,
                (seed as u64) ^ hi,
            )),
            Positional::Legacy { .. } => Random::Legacy(Legacy::new(seed)),
        }
    }
}

macro_rules! both {
    ($self:ident, $r:ident => $e:expr) => {
        match $self {
            Random::Xoroshiro($r) => $e,
            Random::Legacy($r) => $e,
        }
    };
}

impl RandomSource for Random {
    fn next_long(&mut self) -> i64 {
        both!(self, r => r.next_long())
    }
    fn next_int(&mut self) -> i32 {
        both!(self, r => r.next_int())
    }
    fn next_int_bound(&mut self, bound: i32) -> i32 {
        both!(self, r => r.next_int_bound(bound))
    }
    fn next_double(&mut self) -> f64 {
        both!(self, r => r.next_double())
    }
    fn next_float(&mut self) -> f32 {
        both!(self, r => r.next_float())
    }
    fn next_bool(&mut self) -> bool {
        both!(self, r => r.next_bool())
    }
    fn consume(&mut self, count: usize) {
        both!(self, r => r.consume(count))
    }
    fn fork_positional(&mut self) -> Positional {
        both!(self, r => r.fork_positional())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_random_matches_known_values() {
        // java.util.Random(42): nextInt() = -1170105035, nextInt(10) = 0
        // (then 3), nextLong... the classic first values.
        let mut r = Legacy::new(42);
        assert_eq!(r.next_int(), -1_170_105_035);
        let mut r = Legacy::new(42);
        assert_eq!(r.next_int_bound(10), 0);
        assert_eq!(r.next_int_bound(10), 3);
        let mut r = Legacy::new(0);
        assert_eq!(r.next_long(), -4_962_768_465_676_381_896);
    }

    #[test]
    fn java_string_hash() {
        assert_eq!(java_hash("hello"), 99_162_322);
        assert_eq!(java_hash(""), 0);
    }
}
