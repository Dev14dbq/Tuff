//! Noise (minecraft.wiki: "Noise", "Noise settings", "Density function"):
//! improved Perlin noise, octaves of it, the "normal" noise made of two
//! octave stacks, the old blended terrain noise and simplex noise.

use crate::random::{Legacy, Random, RandomSource};

/// Gradients of the 3D improved noise.
const GRADIENT: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

fn dot(g: &[f64; 3], x: f64, y: f64, z: f64) -> f64 {
    g[0] * x + g[1] * y + g[2] * z
}

fn smoothstep(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

pub fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

fn lerp2(tx: f64, ty: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    lerp(ty, lerp(tx, a, b), lerp(tx, c, d))
}

#[allow(clippy::too_many_arguments)]
fn lerp3(
    tx: f64,
    ty: f64,
    tz: f64,
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    g: f64,
    h: f64,
) -> f64 {
    lerp(tz, lerp2(tx, ty, a, b, c, d), lerp2(tx, ty, e, f, g, h))
}

/// `clampedLerp`: `lerp`, holding at the ends.
pub fn clamped_lerp(a: f64, b: f64, t: f64) -> f64 {
    if t < 0.0 {
        a
    } else if t > 1.0 {
        b
    } else {
        lerp(t, a, b)
    }
}

/// Keeps coordinates where doubles are precise enough.
fn wrap(v: f64) -> f64 {
    v - (v / 3.355_443_2E7 + 0.5).floor() * 3.355_443_2E7
}

/// One octave of improved (Perlin) noise.
#[derive(Clone, Debug)]
pub struct ImprovedNoise {
    p: [u8; 256],
    pub xo: f64,
    pub yo: f64,
    pub zo: f64,
}

impl ImprovedNoise {
    pub fn new(random: &mut dyn RandomSource) -> Self {
        let xo = random.next_double() * 256.0;
        let yo = random.next_double() * 256.0;
        let zo = random.next_double() * 256.0;
        let mut p = [0u8; 256];
        for (i, v) in p.iter_mut().enumerate() {
            *v = i as u8;
        }
        for i in 0..256 {
            let j = random.next_int_bound((256 - i) as i32) as usize;
            p.swap(i, i + j);
        }
        Self { p, xo, yo, zo }
    }

    fn hash(&self, i: i32) -> i32 {
        i32::from(self.p[(i & 255) as usize])
    }

    pub fn noise(&self, x: f64, y: f64, z: f64) -> f64 {
        self.noise_scaled(x, y, z, 0.0, 0.0)
    }

    /// With the old terrain noise's vertical smearing (`y_scale`,
    /// `y_max`).
    pub fn noise_scaled(&self, x: f64, y: f64, z: f64, y_scale: f64, y_max: f64) -> f64 {
        let (d, e, f) = (x + self.xo, y + self.yo, z + self.zo);
        let (i, j, k) = (d.floor() as i32, e.floor() as i32, f.floor() as i32);
        let (g, h, l) = (d - f64::from(i), e - f64::from(j), f - f64::from(k));
        let n = if y_scale != 0.0 {
            let m = if y_max >= 0.0 && y_max < h { y_max } else { h };
            (m / y_scale + 1.0E-7).floor() * y_scale
        } else {
            0.0
        };
        self.sample_and_lerp(i, j, k, g, h - n, l, h)
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(&self, x: i32, y: i32, z: i32, dx: f64, dy: f64, dz: f64, ty: f64) -> f64 {
        let a = self.hash(x);
        let b = self.hash(x + 1);
        let c = self.hash(a + y);
        let d = self.hash(a + y + 1);
        let e = self.hash(b + y);
        let f = self.hash(b + y + 1);
        let grad = |h: i32, x: f64, y: f64, z: f64| dot(&GRADIENT[(h & 15) as usize], x, y, z);
        let g = grad(self.hash(c + z), dx, dy, dz);
        let h = grad(self.hash(e + z), dx - 1.0, dy, dz);
        let i = grad(self.hash(d + z), dx, dy - 1.0, dz);
        let j = grad(self.hash(f + z), dx - 1.0, dy - 1.0, dz);
        let k = grad(self.hash(c + z + 1), dx, dy, dz - 1.0);
        let l = grad(self.hash(e + z + 1), dx - 1.0, dy, dz - 1.0);
        let m = grad(self.hash(d + z + 1), dx, dy - 1.0, dz - 1.0);
        let n = grad(self.hash(f + z + 1), dx - 1.0, dy - 1.0, dz - 1.0);
        lerp3(
            smoothstep(dx),
            smoothstep(ty),
            smoothstep(dz),
            g,
            h,
            i,
            j,
            k,
            l,
            m,
            n,
        )
    }
}

/// Octaves of improved noise, each twice the frequency and half the
/// amplitude of the one before.
#[derive(Clone, Debug)]
pub struct PerlinNoise {
    /// Lowest frequency first.
    levels: Vec<Option<ImprovedNoise>>,
    amplitudes: Vec<f64>,
    lowest_freq_input: f64,
    lowest_freq_value: f64,
    max_value: f64,
}

impl PerlinNoise {
    /// Octaves seeded by name (`octave_<n>`) from the source's positional
    /// factory.
    pub fn new(random: &mut dyn RandomSource, first_octave: i32, amplitudes: &[f64]) -> Self {
        let factory = random.fork_positional();
        let levels = amplitudes
            .iter()
            .enumerate()
            .map(|(i, &a)| {
                (a != 0.0).then(|| {
                    let octave = first_octave + i as i32;
                    let mut r = factory.from_hash_of(&format!("octave_{octave}"));
                    ImprovedNoise::new(&mut r)
                })
            })
            .collect();
        Self::finish(levels, first_octave, amplitudes)
    }

    /// The old way: octaves made one after another from the source, from
    /// the highest frequency down, skipping what missing octaves would use.
    pub fn legacy(random: &mut dyn RandomSource, first_octave: i32, amplitudes: &[f64]) -> Self {
        let n = amplitudes.len();
        let top = (-first_octave) as usize;
        let mut levels: Vec<Option<ImprovedNoise>> = vec![None; n];
        let noise = ImprovedNoise::new(random);
        if top < n && amplitudes[top] != 0.0 {
            levels[top] = Some(noise);
        }
        for j in (0..top).rev() {
            if j < n && amplitudes[j] != 0.0 {
                levels[j] = Some(ImprovedNoise::new(random));
            } else {
                random.consume(262);
            }
        }
        Self::finish(levels, first_octave, amplitudes)
    }

    fn finish(levels: Vec<Option<ImprovedNoise>>, first_octave: i32, amplitudes: &[f64]) -> Self {
        let n = amplitudes.len() as i32;
        let lowest_freq_input = 2f64.powi(first_octave);
        let lowest_freq_value = 2f64.powi(n - 1) / (2f64.powi(n) - 1.0);
        let mut noise = Self {
            levels,
            amplitudes: amplitudes.to_vec(),
            lowest_freq_input,
            lowest_freq_value,
            max_value: 0.0,
        };
        noise.max_value = noise.edge_value(2.0);
        noise
    }

    fn edge_value(&self, v: f64) -> f64 {
        let mut total = 0.0;
        let mut factor = self.lowest_freq_value;
        for (level, a) in self.levels.iter().zip(&self.amplitudes) {
            if level.is_some() {
                total += a * v * factor;
            }
            factor /= 2.0;
        }
        total
    }

    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let mut total = 0.0;
        let mut input = self.lowest_freq_input;
        let mut factor = self.lowest_freq_value;
        for (level, a) in self.levels.iter().zip(&self.amplitudes) {
            if let Some(noise) = level {
                let v = noise.noise(wrap(x * input), wrap(y * input), wrap(z * input));
                total += a * v * factor;
            }
            input *= 2.0;
            factor /= 2.0;
        }
        total
    }

    /// Octave `i`, counted from the highest frequency.
    pub fn octave(&self, i: usize) -> Option<&ImprovedNoise> {
        let n = self.levels.len();
        self.levels.get(n.checked_sub(1 + i)?)?.as_ref()
    }
}

/// Noise parameters from `worldgen/noise/<name>.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseParameters {
    pub first_octave: i32,
    pub amplitudes: Vec<f64>,
}

/// Two octave stacks summed, the second sampled at slightly scaled
/// coordinates; scaled so values mostly stay within -1..1.
#[derive(Clone, Debug)]
pub struct NormalNoise {
    first: PerlinNoise,
    second: PerlinNoise,
    value_factor: f64,
    max_value: f64,
}

const INPUT_FACTOR: f64 = 1.018_126_888_217_522_7;

impl NormalNoise {
    pub fn new(random: &mut dyn RandomSource, params: &NoiseParameters, legacy: bool) -> Self {
        let make = |r: &mut dyn RandomSource| {
            if legacy {
                PerlinNoise::legacy(r, params.first_octave, &params.amplitudes)
            } else {
                PerlinNoise::new(r, params.first_octave, &params.amplitudes)
            }
        };
        let first = make(random);
        let second = make(random);
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for (i, &a) in params.amplitudes.iter().enumerate() {
            if a != 0.0 {
                lo = lo.min(i as i32);
                hi = hi.max(i as i32);
            }
        }
        let expected_deviation = 0.1 * (1.0 + 1.0 / f64::from(hi - lo + 1));
        let value_factor = 0.166_666_666_666_666_66 / expected_deviation;
        let max_value = (first.max_value() + second.max_value()) * value_factor;
        Self {
            first,
            second,
            value_factor,
            max_value,
        }
    }

    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let (x2, y2, z2) = (x * INPUT_FACTOR, y * INPUT_FACTOR, z * INPUT_FACTOR);
        (self.first.value(x, y, z) + self.second.value(x2, y2, z2)) * self.value_factor
    }
}

/// The old 3D terrain noise ("old_blended_noise").
#[derive(Clone, Debug)]
pub struct BlendedNoise {
    min_limit: PerlinNoise,
    max_limit: PerlinNoise,
    main: PerlinNoise,
    xz_multiplier: f64,
    y_multiplier: f64,
    xz_factor: f64,
    y_factor: f64,
    smear: f64,
}

impl BlendedNoise {
    pub fn new(
        random: &mut dyn RandomSource,
        xz_scale: f64,
        y_scale: f64,
        xz_factor: f64,
        y_factor: f64,
        smear_scale_multiplier: f64,
    ) -> Self {
        let ones16 = [1.0; 16];
        let ones8 = [1.0; 8];
        let min_limit = PerlinNoise::legacy(random, -15, &ones16);
        let max_limit = PerlinNoise::legacy(random, -15, &ones16);
        let main = PerlinNoise::legacy(random, -7, &ones8);
        Self {
            min_limit,
            max_limit,
            main,
            xz_multiplier: 684.412 * xz_scale,
            y_multiplier: 684.412 * y_scale,
            xz_factor,
            y_factor,
            smear: smear_scale_multiplier,
        }
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let (d, e, f) = (
            x * self.xz_multiplier,
            y * self.y_multiplier,
            z * self.xz_multiplier,
        );
        let (g, h, i) = (d / self.xz_factor, e / self.y_factor, f / self.xz_factor);
        let j = self.y_multiplier * self.smear;
        let k = j / self.y_factor;
        let (mut lo, mut hi, mut main) = (0.0, 0.0, 0.0);
        let mut o = 1.0;
        for p in 0..8 {
            if let Some(noise) = self.main.octave(p) {
                main += noise.noise_scaled(wrap(g * o), wrap(h * o), wrap(i * o), k * o, h * o) / o;
            }
            o /= 2.0;
        }
        let q = (main / 10.0 + 1.0) / 2.0;
        let (only_max, only_min) = (q >= 1.0, q <= 0.0);
        let mut o = 1.0;
        for r in 0..16 {
            let (s, t, u) = (wrap(d * o), wrap(e * o), wrap(f * o));
            let v = j * o;
            if !only_max && let Some(noise) = self.min_limit.octave(r) {
                lo += noise.noise_scaled(s, t, u, v, e * o) / o;
            }
            if !only_min && let Some(noise) = self.max_limit.octave(r) {
                hi += noise.noise_scaled(s, t, u, v, e * o) / o;
            }
            o /= 2.0;
        }
        clamped_lerp(lo / 512.0, hi / 512.0, q) / 128.0
    }
}

/// 2D simplex noise (the End's islands).
#[derive(Clone, Debug)]
pub struct SimplexNoise {
    p: [i32; 512],
    pub xo: f64,
    pub yo: f64,
    pub zo: f64,
}

const F2: f64 = 0.366_025_403_784_438_6; // (sqrt(3) - 1) / 2
const G2: f64 = 0.211_324_865_405_187_13; // (3 - sqrt(3)) / 6

impl SimplexNoise {
    pub fn new(random: &mut dyn RandomSource) -> Self {
        let xo = random.next_double() * 256.0;
        let yo = random.next_double() * 256.0;
        let zo = random.next_double() * 256.0;
        let mut p = [0i32; 512];
        for (i, v) in p.iter_mut().take(256).enumerate() {
            *v = i as i32;
        }
        for i in 0..256 {
            let j = random.next_int_bound((256 - i) as i32) as usize;
            p.swap(i, i + j);
        }
        // p[256..] stays 0, as the game's table.
        Self { p, xo, yo, zo }
    }

    fn hash(&self, i: i32) -> i32 {
        self.p[(i & 255) as usize]
    }

    fn corner(&self, index: i32, x: f64, y: f64, z: f64, offset: f64) -> f64 {
        let t = offset - x * x - y * y - z * z;
        if t < 0.0 {
            0.0
        } else {
            let t = t * t;
            t * t * dot(&GRADIENT[index as usize], x, y, z)
        }
    }

    pub fn value_2d(&self, x: f64, y: f64) -> f64 {
        let s = (x + y) * F2;
        let i = (x + s).floor() as i32;
        let j = (y + s).floor() as i32;
        let t = f64::from(i + j) * G2;
        let (x0, y0) = (x - (f64::from(i) - t), y - (f64::from(j) - t));
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let (x1, y1) = (x0 - f64::from(i1) + G2, y0 - f64::from(j1) + G2);
        let (x2, y2) = (x0 - 1.0 + 2.0 * G2, y0 - 1.0 + 2.0 * G2);
        let (ii, jj) = (i & 255, j & 255);
        let gi0 = self.hash(ii + self.hash(jj)) % 12;
        let gi1 = self.hash(ii + i1 + self.hash(jj + j1)) % 12;
        let gi2 = self.hash(ii + 1 + self.hash(jj + 1)) % 12;
        let n0 = self.corner(gi0, x0, y0, 0.0, 0.5);
        let n1 = self.corner(gi1, x1, y1, 0.0, 0.5);
        let n2 = self.corner(gi2, x2, y2, 0.0, 0.5);
        70.0 * (n0 + n1 + n2)
    }
}

/// The noise behind the End's islands, seeded as the game does.
pub fn end_islands_noise(seed: i64) -> SimplexNoise {
    let mut random = Legacy::new(seed);
    random.consume(17292);
    SimplexNoise::new(&mut random)
}

/// A named noise's source from the generator's positional factory.
pub fn instantiate(
    factory: &crate::random::Positional,
    name: &str,
    params: &NoiseParameters,
    legacy: bool,
) -> NormalNoise {
    let mut random: Random = factory.from_hash_of(name);
    NormalNoise::new(&mut random, params, legacy)
}

/// How a noise's output is scaled (26.3 `normalize`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalize {
    /// `base_amplitude` is the expected amplitude: 99.7% of samples fall
    /// within it.
    Expected,
    /// `base_amplitude` is the first octave's amplitude.
    Off,
    /// The old format's scaling.
    Legacy,
}

/// A noise from `worldgen/noise/<name>.json` (26.3 format): octaves of
/// improved noise from `base_octave` on, halving in amplitude and doubling
/// in frequency, each with its `amplitude_modifiers` factor; two stacks
/// summed (the second at slightly scaled coordinates), scaled by
/// `base_amplitude`.
#[derive(Clone, Debug, PartialEq)]
pub struct FbmParameters {
    pub base_octave: i32,
    pub octave_count: usize,
    pub modifiers: Vec<f64>,
    pub base_amplitude: f64,
    pub normalize: Normalize,
}

impl FbmParameters {
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let octave_count = v["octave_count"].as_u64().unwrap_or(1) as usize;
        let modifiers = match v["amplitude_modifiers"].as_array() {
            Some(list) => list.iter().filter_map(serde_json::Value::as_f64).collect(),
            None => vec![1.0; octave_count],
        };
        let normalize = match &v["normalize"] {
            serde_json::Value::Bool(false) => Normalize::Off,
            serde_json::Value::String(s) if s == "legacy" => Normalize::Legacy,
            _ => Normalize::Expected,
        };
        Some(Self {
            base_octave: v["base_octave"].as_i64()? as i32,
            octave_count,
            modifiers,
            base_amplitude: v["base_amplitude"].as_f64().unwrap_or(1.0),
            normalize,
        })
    }
}

/// The 26.3 noise.
#[derive(Clone, Debug)]
pub struct FbmNoise {
    first: PerlinNoise,
    second: PerlinNoise,
    factor: f64,
    max_value: f64,
}

impl FbmNoise {
    /// `old_init`: octaves made one after another from `random` (the
    /// Nether's legacy biome noises) rather than seeded by name.
    pub fn new(random: &mut dyn RandomSource, params: &FbmParameters, old_init: bool) -> Self {
        let n = params.octave_count;
        let make = |r: &mut dyn RandomSource| {
            if old_init {
                PerlinNoise::legacy(r, params.base_octave, &params.modifiers)
            } else {
                PerlinNoise::new(r, params.base_octave, &params.modifiers)
            }
        };
        let first = make(random);
        let second = make(random);
        // Observed: vanilla's 26.3 noise data, converted from the old
        // format, gives exactly the old outputs (biomes match the reference
        // world cell for cell with it, not with base_amplitude taken as the
        // expected amplitude): the old scale from the span of octaves in
        // use. `normalize: false` takes base_amplitude as the first
        // octave's amplitude.
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for (i, &a) in params.modifiers.iter().enumerate() {
            if a != 0.0 {
                lo = lo.min(i as i32);
                hi = hi.max(i as i32);
            }
        }
        let old_scale = if lo <= hi {
            0.166_666_666_666_666_66 / (0.1 * (1.0 + 1.0 / f64::from(hi - lo + 1)))
        } else {
            0.0
        };
        // PerlinNoise weighs octave i by lfv / 2^i.
        let lfv = 2f64.powi(n as i32 - 1) / (2f64.powi(n as i32) - 1.0);
        let factor = match params.normalize {
            Normalize::Expected | Normalize::Legacy => old_scale,
            Normalize::Off => params.base_amplitude / lfv,
        };
        let max_value = (first.max_value() + second.max_value()) * factor;
        Self {
            first,
            second,
            factor,
            max_value,
        }
    }

    pub fn max_value(&self) -> f64 {
        self.max_value
    }

    pub fn value(&self, x: f64, y: f64, z: f64) -> f64 {
        let (x2, y2, z2) = (x * INPUT_FACTOR, y * INPUT_FACTOR, z * INPUT_FACTOR);
        (self.first.value(x, y, z) + self.second.value(x2, y2, z2)) * self.factor
    }
}
