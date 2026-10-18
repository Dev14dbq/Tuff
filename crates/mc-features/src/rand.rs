//! The random source features use: java.util.Random's methods on top of a
//! bit source, which is xoroshiro128++ (its top bits) or the legacy LCG.
//! Each chunk's decoration seeds it from the world seed and the chunk's
//! corner, then each feature from that and its place in the step's order
//! (minecraft.wiki, "Seed (level generation)"; checked against vanilla's
//! ores on the reference world).

use mc_worldgen::random::{Legacy, Positional, RandomSource, Xoroshiro};

#[derive(Clone, Debug)]
enum Bits {
    Xoroshiro(Xoroshiro),
    Legacy(Legacy),
}

#[derive(Clone, Debug)]
pub struct WorldgenRandom {
    bits: Bits,
}

impl WorldgenRandom {
    pub fn xoroshiro(seed: i64) -> Self {
        Self {
            bits: Bits::Xoroshiro(Xoroshiro::new(seed)),
        }
    }

    pub fn legacy(seed: i64) -> Self {
        Self {
            bits: Bits::Legacy(Legacy::new(seed)),
        }
    }

    pub fn set_seed(&mut self, seed: i64) {
        match &mut self.bits {
            Bits::Xoroshiro(x) => *x = Xoroshiro::new(seed),
            Bits::Legacy(l) => l.set_seed(seed),
        }
    }

    fn next(&mut self, bits: u32) -> i32 {
        match &mut self.bits {
            Bits::Xoroshiro(x) => ((x.next_long() as u64) >> (64 - bits)) as i32,
            Bits::Legacy(l) => l.next_bits(bits),
        }
    }

    /// Seeds the decoration of the chunk whose corner is `x, z`; returns
    /// the decoration seed.
    pub fn set_decoration_seed(&mut self, seed: i64, x: i32, z: i32) -> i64 {
        self.set_seed(seed);
        let a = self.next_long() | 1;
        let b = self.next_long() | 1;
        let s = (i64::from(x).wrapping_mul(a)).wrapping_add(i64::from(z).wrapping_mul(b)) ^ seed;
        self.set_seed(s);
        s
    }

    pub fn set_feature_seed(&mut self, decoration: i64, index: i32, step: i32) {
        self.set_seed(
            decoration
                .wrapping_add(i64::from(index))
                .wrapping_add(10000 * i64::from(step)),
        );
    }

    /// Seeds per chunk for carvers and structure starts.
    pub fn set_large_feature_seed(&mut self, seed: i64, cx: i32, cz: i32) {
        self.set_seed(seed);
        let a = self.next_long();
        let b = self.next_long();
        self.set_seed(i64::from(cx).wrapping_mul(a) ^ i64::from(cz).wrapping_mul(b) ^ seed);
    }

    /// Seeds structure placement on a spread grid.
    pub fn set_large_feature_with_salt(&mut self, seed: i64, x: i32, z: i32, salt: i32) {
        self.set_seed(
            i64::from(x)
                .wrapping_mul(341_873_128_712)
                .wrapping_add(i64::from(z).wrapping_mul(132_897_987_541))
                .wrapping_add(seed)
                .wrapping_add(i64::from(salt)),
        );
    }

    /// The polar method over `next_double`, as java.util.Random.
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

    /// `Mth.nextInt`: `min` when `min >= max`, else between both included.
    pub fn int_between(&mut self, min: i32, max: i32) -> i32 {
        if min >= max {
            min
        } else {
            self.next_int_bound(max - min + 1) + min
        }
    }

    /// `Mth.nextFloat`: `min` when `min >= max`.
    pub fn float_between(&mut self, min: f32, max: f32) -> f32 {
        if min >= max {
            min
        } else {
            self.next_float() * (max - min) + min
        }
    }

    pub fn pick<'a, T>(&mut self, list: &'a [T]) -> &'a T {
        &list[self.next_int_bound(list.len() as i32) as usize]
    }

    /// Shuffles as `Util.shuffle` (Fisher–Yates from the end).
    pub fn shuffle<T>(&mut self, list: &mut [T]) {
        for i in (1..list.len()).rev() {
            let j = self.next_int_bound(i as i32 + 1) as usize;
            list.swap(i, j);
        }
    }
}

impl RandomSource for WorldgenRandom {
    fn next_long(&mut self) -> i64 {
        let hi = i64::from(self.next(32));
        let lo = i64::from(self.next(32));
        (hi << 32).wrapping_add(lo)
    }

    fn next_int(&mut self) -> i32 {
        self.next(32)
    }

    fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");
        if bound & (bound - 1) == 0 {
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }

    fn next_double(&mut self) -> f64 {
        let hi = i64::from(self.next(26));
        let lo = i64::from(self.next(27));
        ((hi << 27) + lo) as f64 * 1.110_223_024_625_156_5E-16
    }

    fn next_float(&mut self) -> f32 {
        self.next(24) as f32 * 5.960_464_5E-8
    }

    fn next_bool(&mut self) -> bool {
        self.next(1) != 0
    }

    fn fork_positional(&mut self) -> Positional {
        match &mut self.bits {
            Bits::Xoroshiro(x) => x.fork_positional(),
            Bits::Legacy(_) => Positional::Legacy {
                seed: self.next_long(),
            },
        }
    }
}
