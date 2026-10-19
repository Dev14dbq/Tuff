//! Density functions (minecraft.wiki, "Density function"): the expression
//! trees in `worldgen/density_function` and the noise settings, evaluated at
//! block positions.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::noise::{BlendedNoise, FbmNoise, FbmParameters, SimplexNoise, clamped_lerp};
use crate::random::{Positional, RandomSource};

/// Reads data pack files (`worldgen/<kind>/<path>.json`).
pub trait Source {
    fn read(&self, kind: &str, id: &str) -> Option<Value>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    fn of(v: &Value) -> Axis {
        match v.as_str() {
            Some("x") => Axis::X,
            Some("z") => Axis::Z,
            _ => Axis::Y,
        }
    }

    fn pick(self, x: i32, y: i32, z: i32) -> i32 {
        match self {
            Axis::X => x,
            Axis::Y => y,
            Axis::Z => z,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiling {
    Clamp,
    Repeat,
    Mirrored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Euclidean,
    EuclideanSquared,
    Manhattan,
    Chebyshev,
}

fn unary(op: Unary, v: f64) -> f64 {
    match op {
        Unary::Abs => v.abs(),
        Unary::Square => v * v,
        Unary::Cube => v * v * v,
        Unary::HalfNegative => {
            if v > 0.0 {
                v
            } else {
                v * 0.5
            }
        }
        Unary::QuarterNegative => {
            if v > 0.0 {
                v
            } else {
                v * 0.25
            }
        }
        Unary::Squeeze => {
            let c = v.clamp(-1.0, 1.0);
            c / 2.0 - c * c * c / 24.0
        }
        Unary::Negate => -v,
        Unary::Sqrt => v.sqrt(),
        Unary::Log => v.ln(),
        Unary::Sign => {
            if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        Unary::Reciprocal => 1.0 / v,
    }
}

/// `b` is only asked for when it can matter.
fn binary(op: Binary, a: f64, b: impl FnOnce() -> f64) -> f64 {
    match op {
        // The game skips the second side when it cannot matter;
        // the result is the same.
        Binary::Mul if a == 0.0 => 0.0,
        _ => {
            let b = b();
            match op {
                Binary::Add => a + b,
                Binary::Sub => a - b,
                Binary::Mul => a * b,
                Binary::Div => a / b,
                Binary::Min => a.min(b),
                Binary::Max => a.max(b),
                Binary::Pow => a.powf(b),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Abs,
    Square,
    Cube,
    HalfNegative,
    QuarterNegative,
    Squeeze,
    Negate,
    Sqrt,
    Log,
    Sign,
    Reciprocal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Pow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Ceil,
    Floor,
    Round,
    Truncate,
}

/// A cubic spline over a coordinate (float precision, as the game).
#[derive(Debug)]
pub struct Spline {
    coordinate: Df,
    points: Vec<(f32, SplineValue, f32)>,
}

#[derive(Debug)]
pub enum SplineValue {
    Const(f32),
    Spline(Box<Spline>),
}

impl Spline {
    fn eval(&self, x: i32, y: i32, z: i32) -> f32 {
        let c = self.coordinate.eval(x, y, z) as f32;
        let value = |v: &SplineValue| match v {
            SplineValue::Const(f) => *f,
            SplineValue::Spline(s) => s.eval(x, y, z),
        };
        let n = self.points.len();
        // Last point at or before c.
        let i = self.points.iter().rposition(|p| p.0 <= c);
        match i {
            None => {
                let (loc, v, d) = &self.points[0];
                value(v) + d * (c - loc)
            }
            Some(i) if i == n - 1 => {
                let (loc, v, d) = &self.points[n - 1];
                value(v) + d * (c - loc)
            }
            Some(i) => {
                let (l0, v0, d0) = &self.points[i];
                let (l1, v1, d1) = &self.points[i + 1];
                let (a, b) = (value(v0), value(v1));
                let w = l1 - l0;
                let t = (c - l0) / w;
                let p = d0 * w - (b - a);
                let q = -d1 * w + (b - a);
                (a + t * (b - a)) + t * (1.0 - t) * (p + t * (q - p))
            }
        }
    }
}

/// One density function.
#[derive(Debug)]
pub enum Df {
    Constant(f64),
    /// A shared function; with a cache slot when it does not depend on y
    /// (cached per column while generating a chunk).
    Ref(Arc<Df>, Option<u32>),
    Unary(Unary, Box<Df>),
    Binary(Binary, Box<Df>, Box<Df>),
    Round(Rounding, Box<Df>, Box<Df>),
    Clamp(Box<Df>, f64, f64),
    Gradient {
        axis: Axis,
        tiling: Tiling,
        from: i32,
        to: i32,
        from_value: f64,
        to_value: f64,
    },
    Lerp(Box<Df>, Box<Df>, Box<Df>),
    Noise {
        noise: Arc<FbmNoise>,
        xz_scale: f64,
        y_scale: f64,
        shift: [Option<Box<Df>>; 3],
    },
    Shift {
        noise: Arc<FbmNoise>,
        kind: u8,
    },
    RangeChoice {
        input: Box<Df>,
        min: f64,
        max: f64,
        inside: Box<Df>,
        outside: Box<Df>,
    },
    IntervalSelect {
        input: Box<Df>,
        thresholds: Vec<f64>,
        functions: Vec<Df>,
    },
    Spline(Box<Spline>),
    /// Sampled at cell corners and interpolated in between (directly when
    /// evaluated outside a chunk).
    Interpolated {
        input: Box<Df>,
        xz: i32,
        y: i32,
        /// Its grid in [`Ctx`].
        slot: u32,
    },
    OldBlended(Arc<BlendedNoise>),
    FindTopSurface {
        density: Box<Df>,
        upper: Box<Df>,
        lower: i32,
        cell: i32,
    },
    Slice {
        axis: Axis,
        coordinate: i32,
        input: Box<Df>,
    },
    Distance {
        point: [i32; 3],
        metric: Metric,
    },
    EndIslands(Arc<SimplexNoise>),
}

impl Df {
    pub fn eval(&self, x: i32, y: i32, z: i32) -> f64 {
        match self {
            Df::Constant(c) => *c,
            Df::Ref(f, _) => f.eval(x, y, z),
            Df::Unary(op, input) => unary(*op, input.eval(x, y, z)),
            Df::Binary(op, a, b) => binary(*op, a.eval(x, y, z), || b.eval(x, y, z)),
            Df::Round(mode, input, multiple) => {
                let (v, m) = (input.eval(x, y, z), multiple.eval(x, y, z));
                let q = v / m;
                let r = match mode {
                    Rounding::Ceil => q.ceil(),
                    Rounding::Floor => q.floor(),
                    Rounding::Round => (q + 0.5).floor(),
                    Rounding::Truncate => q.trunc(),
                };
                r * m
            }
            Df::Clamp(input, lo, hi) => input.eval(x, y, z).clamp(*lo, *hi),
            Df::Gradient {
                axis,
                tiling,
                from,
                to,
                from_value,
                to_value,
            } => {
                let c = f64::from(axis.pick(x, y, z));
                let mut t = (c - f64::from(*from)) / f64::from(to - from);
                match tiling {
                    Tiling::Clamp => {}
                    Tiling::Repeat => t -= t.floor(),
                    Tiling::Mirrored => {
                        let k = t.floor();
                        t -= k;
                        if (k as i64).rem_euclid(2) == 1 {
                            t = 1.0 - t;
                        }
                    }
                }
                clamped_lerp(*from_value, *to_value, t)
            }
            Df::Lerp(alpha, first, second) => {
                let a = alpha.eval(x, y, z);
                if a == 0.0 {
                    return first.eval(x, y, z);
                }
                if a == 1.0 {
                    return second.eval(x, y, z);
                }
                let (f, s) = (first.eval(x, y, z), second.eval(x, y, z));
                f + a * (s - f)
            }
            Df::Noise {
                noise,
                xz_scale,
                y_scale,
                shift,
            } => {
                let sh = |i: usize| shift[i].as_ref().map_or(0.0, |d| d.eval(x, y, z));
                noise.value(
                    f64::from(x) * xz_scale + sh(0),
                    f64::from(y) * y_scale + sh(1),
                    f64::from(z) * xz_scale + sh(2),
                )
            }
            Df::Shift { noise, kind } => {
                let (fx, fy, fz) = (
                    f64::from(x) * 0.25,
                    f64::from(y) * 0.25,
                    f64::from(z) * 0.25,
                );
                4.0 * match kind {
                    0 => noise.value(fx, 0.0, fz),
                    1 => noise.value(fz, fx, 0.0),
                    _ => noise.value(fx, fy, fz),
                }
            }
            Df::RangeChoice {
                input,
                min,
                max,
                inside,
                outside,
            } => {
                let v = input.eval(x, y, z);
                if v >= *min && v < *max {
                    inside.eval(x, y, z)
                } else {
                    outside.eval(x, y, z)
                }
            }
            Df::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let v = input.eval(x, y, z);
                let i = thresholds
                    .iter()
                    .position(|&t| v < t)
                    .unwrap_or(thresholds.len());
                functions[i].eval(x, y, z)
            }
            Df::Spline(s) => f64::from(s.eval(x, y, z)),
            Df::Interpolated { input, .. } => input.eval(x, y, z),
            Df::OldBlended(noise) => noise.value(f64::from(x), f64::from(y), f64::from(z)),
            Df::FindTopSurface {
                density,
                upper,
                lower,
                cell,
            } => {
                let top = upper.eval(x, y, z).floor() as i32;
                let mut at = top.div_euclid(*cell) * cell;
                while at >= *lower {
                    if density.eval(x, at, z) > 0.0 {
                        return f64::from(at);
                    }
                    at -= cell;
                }
                f64::from(*lower)
            }
            Df::Slice {
                axis,
                coordinate,
                input,
            } => match axis {
                Axis::X => input.eval(*coordinate, y, z),
                Axis::Y => input.eval(x, *coordinate, z),
                Axis::Z => input.eval(x, y, *coordinate),
            },
            Df::Distance { point, metric } => {
                let d = [
                    f64::from(x - point[0]),
                    f64::from(y - point[1]),
                    f64::from(z - point[2]),
                ];
                match metric {
                    Metric::Euclidean => (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt(),
                    Metric::EuclideanSquared => d[0] * d[0] + d[1] * d[1] + d[2] * d[2],
                    Metric::Manhattan => d[0].abs() + d[1].abs() + d[2].abs(),
                    Metric::Chebyshev => d[0].abs().max(d[1].abs()).max(d[2].abs()),
                }
            }
            Df::EndIslands(noise) => end_islands(noise, x / 8, z / 8),
        }
    }
}

/// A quick hash for the per-chunk caches (keys are addresses and
/// coordinates, nothing an outsider chooses).
#[derive(Clone, Copy, Debug, Default)]
pub struct Fast;

#[derive(Debug, Default)]
pub struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
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
        self.write_u64(v as u32 as u64);
    }
    fn write_u32(&mut self, v: u32) {
        self.write_u64(u64::from(v));
    }
}

impl std::hash::BuildHasher for Fast {
    type Hasher = FastHasher;
    fn build_hasher(&self) -> FastHasher {
        FastHasher(0)
    }
}

/// Per-chunk evaluation state: interpolation grids and column caches,
/// found by the slot their node got when it was built.
#[derive(Debug, Default)]
pub struct Ctx {
    /// Lowest block of the chunk (x, z) and the world's height range.
    pub x0: i32,
    pub z0: i32,
    pub min_y: i32,
    pub height: i32,
    grids: Vec<Option<Grid>>,
    /// 256 columns of the chunk per slot; NaN where not worked out yet.
    columns: Vec<f64>,
    /// Columns outside the chunk.
    far: HashMap<(u32, i32, i32), f64, Fast>,
}

#[derive(Debug)]
struct Grid {
    flat: bool,
    xz: i32,
    y: i32,
    nx: usize,
    ny: usize,
    values: Vec<f64>,
    /// Which corners are worked out, and which cells have all theirs.
    known: Vec<bool>,
    cells: Vec<bool>,
}

impl Ctx {
    pub fn new(x0: i32, z0: i32, min_y: i32, height: i32) -> Self {
        Self {
            x0,
            z0,
            min_y,
            height,
            ..Self::default()
        }
    }
}

impl Df {
    /// Whether the value does not depend on y.
    pub fn is_2d(&self) -> bool {
        match self {
            Df::Constant(_) | Df::FindTopSurface { .. } | Df::EndIslands(_) => true,
            Df::Ref(_, slot) => slot.is_some(),
            Df::Unary(_, a) | Df::Clamp(a, ..) => a.is_2d(),
            Df::Binary(_, a, b) | Df::Round(_, a, b) => a.is_2d() && b.is_2d(),
            Df::Gradient { axis, .. } => *axis != Axis::Y,
            Df::Lerp(a, b, c) => a.is_2d() && b.is_2d() && c.is_2d(),
            Df::Noise { y_scale, shift, .. } => {
                *y_scale == 0.0 && shift.iter().flatten().all(|s| s.is_2d())
            }
            Df::Shift { kind, .. } => *kind != 2,
            Df::RangeChoice {
                input,
                inside,
                outside,
                ..
            } => input.is_2d() && inside.is_2d() && outside.is_2d(),
            Df::IntervalSelect {
                input, functions, ..
            } => input.is_2d() && functions.iter().all(Df::is_2d),
            Df::Spline(s) => s.is_2d(),
            Df::Interpolated { input, .. } => input.is_2d(),
            Df::OldBlended(_) | Df::Distance { .. } => false,
            Df::Slice { axis, input, .. } => *axis == Axis::Y || input.is_2d(),
        }
    }

    /// Whether an interpolated node is somewhere inside, so that
    /// [`Df::eval_in`] may give other values than [`Df::eval`].
    pub fn interpolates(&self) -> bool {
        match self {
            Df::Interpolated { .. } => true,
            Df::Ref(f, _) => f.interpolates(),
            Df::Unary(_, a) | Df::Clamp(a, ..) => a.interpolates(),
            Df::Binary(_, a, b) | Df::Round(_, a, b) => a.interpolates() || b.interpolates(),
            Df::Lerp(a, b, c) => a.interpolates() || b.interpolates() || c.interpolates(),
            Df::Noise { shift, .. } => shift.iter().flatten().any(|s| s.interpolates()),
            Df::RangeChoice {
                input,
                inside,
                outside,
                ..
            } => input.interpolates() || inside.interpolates() || outside.interpolates(),
            Df::IntervalSelect {
                input, functions, ..
            } => input.interpolates() || functions.iter().any(Df::interpolates),
            Df::Spline(s) => s.interpolates(),
            Df::FindTopSurface { density, upper, .. } => {
                density.interpolates() || upper.interpolates()
            }
            Df::Slice { input, .. } => input.interpolates(),
            Df::Constant(_)
            | Df::Gradient { .. }
            | Df::Shift { .. }
            | Df::OldBlended(_)
            | Df::Distance { .. }
            | Df::EndIslands(_) => false,
        }
    }

    /// The value inside a chunk: interpolated nodes from their cell
    /// corners, column-independent of y ones once per column.
    pub fn eval_in(&self, x: i32, y: i32, z: i32, ctx: &mut Ctx) -> f64 {
        match self {
            Df::Ref(f, None) => f.eval_in(x, y, z, ctx),
            Df::Ref(f, Some(slot)) => {
                let (lx, lz) = (x.wrapping_sub(ctx.x0) as u32, z.wrapping_sub(ctx.z0) as u32);
                if lx >= 16 || lz >= 16 {
                    let key = (*slot, x, z);
                    if let Some(&v) = ctx.far.get(&key) {
                        return v;
                    }
                    let v = f.eval_in(x, y, z, ctx);
                    ctx.far.insert(key, v);
                    return v;
                }
                let i = *slot as usize * 256 + (lz * 16 + lx) as usize;
                if let Some(&v) = ctx.columns.get(i)
                    && !v.is_nan()
                {
                    return v;
                }
                let v = f.eval_in(x, y, z, ctx);
                if ctx.columns.len() <= i {
                    ctx.columns.resize((*slot as usize + 1) * 256, f64::NAN);
                }
                ctx.columns[i] = v;
                v
            }
            Df::Interpolated {
                input,
                xz,
                y: cy,
                slot,
            } => {
                let key = *slot as usize;
                if ctx.grids.get(key).is_none_or(Option::is_none) {
                    // Something flat needs one layer only.
                    let flat = input.is_2d();
                    let nx = (16 / xz) as usize + 1;
                    let ny = if flat {
                        1
                    } else {
                        (ctx.height / cy) as usize + 1
                    };
                    if ctx.grids.len() <= key {
                        ctx.grids.resize_with(key + 1, || None);
                    }
                    ctx.grids[key] = Some(Grid {
                        xz: *xz,
                        y: *cy,
                        nx,
                        ny,
                        flat,
                        values: vec![0.0; nx * ny * nx],
                        known: vec![false; nx * ny * nx],
                        cells: vec![false; nx * ny.max(2) * nx],
                    });
                }
                let g = ctx.grids[key].as_ref().expect("grid made above");
                let flat = g.flat;
                let (lx, ly, lz) = (x - ctx.x0, if flat { 0 } else { y - ctx.min_y }, z - ctx.z0);
                let (i, j, k) = (
                    (lx.div_euclid(g.xz) as usize).min(g.nx - 2),
                    (ly.div_euclid(g.y) as usize).min(g.ny.saturating_sub(2)),
                    (lz.div_euclid(g.xz) as usize).min(g.nx - 2),
                );
                // Corners are worked out when a cell first needs them (a
                // single column needs few), each once.
                let cell = (k * g.ny.max(2) + j) * g.nx + i;
                if !g.cells[cell] {
                    let (nx, ny, gxz, gy) = (g.nx, g.ny, g.xz, g.y);
                    let top = if flat { 0 } else { 1 };
                    for (di, dj, dk) in (0..=1).flat_map(|dk| {
                        (0..=top).flat_map(move |dj| (0..=1).map(move |di| (di, dj, dk)))
                    }) {
                        let (ci, cj, ck) = (i + di, j + dj, k + dk);
                        let c = (ck * ny + cj) * nx + ci;
                        if ctx.grids[key].as_ref().expect("grid").known[c] {
                            continue;
                        }
                        let v = input.eval_in(
                            ctx.x0 + ci as i32 * gxz,
                            ctx.min_y + cj as i32 * gy,
                            ctx.z0 + ck as i32 * gxz,
                            ctx,
                        );
                        let g = ctx.grids[key].as_mut().expect("grid");
                        g.values[c] = v;
                        g.known[c] = true;
                    }
                    ctx.grids[key].as_mut().expect("grid").cells[cell] = true;
                }
                let g = ctx.grids[key].as_ref().expect("grid made above");
                let at = |i: usize, j: usize, k: usize| g.values[(k * g.ny + j) * g.nx + i];
                let tx = f64::from(lx - i as i32 * g.xz) / f64::from(g.xz);
                let ty = f64::from(ly - j as i32 * g.y) / f64::from(g.y);
                let tz = f64::from(lz - k as i32 * g.xz) / f64::from(g.xz);
                let l = crate::noise::lerp;
                let y0 = l(
                    tz,
                    l(tx, at(i, j, k), at(i + 1, j, k)),
                    l(tx, at(i, j, k + 1), at(i + 1, j, k + 1)),
                );
                if flat {
                    return y0;
                }
                let y1 = l(
                    tz,
                    l(tx, at(i, j + 1, k), at(i + 1, j + 1, k)),
                    l(tx, at(i, j + 1, k + 1), at(i + 1, j + 1, k + 1)),
                );
                l(ty, y0, y1)
            }
            Df::Unary(op, input) => unary(*op, input.eval_in(x, y, z, ctx)),
            Df::Binary(op, a, b) => {
                let av = a.eval_in(x, y, z, ctx);
                binary(*op, av, || b.eval_in(x, y, z, ctx))
            }
            Df::Clamp(input, lo, hi) => input.eval_in(x, y, z, ctx).clamp(*lo, *hi),
            Df::Lerp(alpha, first, second) => {
                let a = alpha.eval_in(x, y, z, ctx);
                if a == 0.0 {
                    return first.eval_in(x, y, z, ctx);
                }
                if a == 1.0 {
                    return second.eval_in(x, y, z, ctx);
                }
                let (f, s) = (first.eval_in(x, y, z, ctx), second.eval_in(x, y, z, ctx));
                f + a * (s - f)
            }
            Df::RangeChoice {
                input,
                min,
                max,
                inside,
                outside,
            } => {
                let v = input.eval_in(x, y, z, ctx);
                if v >= *min && v < *max {
                    inside.eval_in(x, y, z, ctx)
                } else {
                    outside.eval_in(x, y, z, ctx)
                }
            }
            Df::IntervalSelect {
                input,
                thresholds,
                functions,
            } => {
                let v = input.eval_in(x, y, z, ctx);
                let i = thresholds
                    .iter()
                    .position(|&t| v < t)
                    .unwrap_or(thresholds.len());
                functions[i].eval_in(x, y, z, ctx)
            }
            Df::Noise {
                noise,
                xz_scale,
                y_scale,
                shift,
            } => {
                let mut sh = [0.0; 3];
                for (i, s) in shift.iter().enumerate() {
                    if let Some(d) = s {
                        sh[i] = d.eval_in(x, y, z, ctx);
                    }
                }
                noise.value(
                    f64::from(x) * xz_scale + sh[0],
                    f64::from(y) * y_scale + sh[1],
                    f64::from(z) * xz_scale + sh[2],
                )
            }
            // The rest has no chunk-level state.
            other => other.eval(x, y, z),
        }
    }
}

impl Spline {
    fn interpolates(&self) -> bool {
        self.coordinate.interpolates()
            || self.points.iter().any(|(_, v, _)| match v {
                SplineValue::Const(_) => false,
                SplineValue::Spline(s) => s.interpolates(),
            })
    }

    fn is_2d(&self) -> bool {
        self.coordinate.is_2d()
            && self.points.iter().all(|(_, v, _)| match v {
                SplineValue::Const(_) => true,
                SplineValue::Spline(s) => s.is_2d(),
            })
    }
}

/// The End's outer islands (minecraft.wiki, "The End#Generation").
fn end_islands(noise: &SimplexNoise, x: i32, z: i32) -> f64 {
    let (cx, cz) = (x / 2, z / 2);
    let (dx, dz) = (x % 2, z % 2);
    let mut f = 100.0 - ((x * x + z * z) as f32).sqrt() * 8.0;
    f = f.clamp(-100.0, 80.0);
    for i in -12..=12 {
        for j in -12..=12 {
            let (px, pz) = (i64::from(cx + i), i64::from(cz + j));
            if px * px + pz * pz > 4096 && noise.value_2d(px as f64, pz as f64) < -0.9 {
                let size = ((px.abs() as f32) * 3439.0 + (pz.abs() as f32) * 147.0) % 13.0 + 9.0;
                let (ex, ez) = ((dx - i * 2) as f32, (dz - j * 2) as f32);
                let g = 100.0 - (ex * ex + ez * ez).sqrt() * size;
                f = f.max(g.clamp(-100.0, 80.0));
            }
        }
    }
    (f64::from(f) - 8.0) / 128.0
}

/// Builds density functions from data, seeding noises as the generator
/// does.
pub struct Builder<'a> {
    source: &'a dyn Source,
    factory: Positional,
    legacy: bool,
    named: HashMap<String, Arc<Df>>,
    noises: HashMap<String, Arc<FbmNoise>>,
    blended: Option<Arc<BlendedNoise>>,
    seed: i64,
    /// Cache slots handed out so far, and those of shared functions.
    slots: u32,
    ref_slots: HashMap<usize, u32>,
}

impl std::fmt::Debug for Builder<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("legacy", &self.legacy)
            .finish()
    }
}

impl<'a> Builder<'a> {
    pub fn new(source: &'a dyn Source, seed: i64, legacy: bool) -> Self {
        let factory = if legacy {
            crate::random::Legacy::new(seed).fork_positional()
        } else {
            crate::random::Xoroshiro::new(seed).fork_positional()
        };
        Self {
            source,
            factory,
            legacy,
            named: HashMap::new(),
            noises: HashMap::new(),
            blended: None,
            seed,
            slots: 0,
            ref_slots: HashMap::new(),
        }
    }

    pub fn factory(&self) -> Positional {
        self.factory
    }

    /// A named noise, seeded from its name.
    pub fn noise(&mut self, id: &str) -> Option<Arc<FbmNoise>> {
        let id = full_id(id);
        if let Some(n) = self.noises.get(&id) {
            return Some(n.clone());
        }
        let params = FbmParameters::from_json(&self.source.read("noise", &id)?)?;
        let mut random = self.factory.from_hash_of(&id);
        let noise = Arc::new(FbmNoise::new(&mut random, &params, self.legacy));
        self.noises.insert(id, noise.clone());
        Some(noise)
    }

    /// A density function: a number, a reference, or an inline object.
    pub fn build(&mut self, v: &Value) -> Result<Df, String> {
        match v {
            Value::Number(n) => Ok(Df::Constant(n.as_f64().unwrap_or(0.0))),
            Value::String(id) => {
                let f = self.named(id)?;
                Ok(self.reference(f))
            }
            Value::Object(_) => self.object(v),
            other => Err(format!("not a density function: {other}")),
        }
    }

    /// A reference to a shared function, with a column cache slot (one per
    /// function) when it does not depend on y.
    fn reference(&mut self, f: Arc<Df>) -> Df {
        if !f.is_2d() {
            return Df::Ref(f, None);
        }
        let next = &mut self.slots;
        let slot = *self
            .ref_slots
            .entry(Arc::as_ptr(&f) as usize)
            .or_insert_with(|| {
                *next += 1;
                *next - 1
            });
        Df::Ref(f, Some(slot))
    }

    fn slot(&mut self) -> u32 {
        self.slots += 1;
        self.slots - 1
    }

    pub fn named(&mut self, id: &str) -> Result<Arc<Df>, String> {
        let id = full_id(id);
        if let Some(f) = self.named.get(&id) {
            return Ok(f.clone());
        }
        let json = self
            .source
            .read("density_function", &id)
            .ok_or_else(|| format!("no density function {id}"))?;
        let f = Arc::new(self.build(&json)?);
        self.named.insert(id, f.clone());
        Ok(f)
    }

    fn boxed(&mut self, v: &Value) -> Result<Box<Df>, String> {
        self.build(v).map(Box::new)
    }

    fn noise_field(&mut self, v: &Value) -> Result<Arc<FbmNoise>, String> {
        match v {
            Value::String(id) => self.noise(id).ok_or_else(|| format!("no noise {id}")),
            Value::Object(_) => {
                let params = FbmParameters::from_json(v).ok_or("bad inline noise")?;
                let mut random = self.factory.from_hash_of("minecraft:inline");
                Ok(Arc::new(FbmNoise::new(&mut random, &params, self.legacy)))
            }
            _ => Err("bad noise".into()),
        }
    }

    fn spline(&mut self, v: &Value) -> Result<Spline, String> {
        let coordinate = self.build(&v["coordinate"])?;
        let mut points = Vec::new();
        for p in v["points"].as_array().ok_or("spline needs points")? {
            let value = match &p["value"] {
                Value::Number(n) => SplineValue::Const(n.as_f64().unwrap_or(0.0) as f32),
                other => SplineValue::Spline(Box::new(self.spline(other)?)),
            };
            points.push((
                p["location"].as_f64().unwrap_or(0.0) as f32,
                value,
                p["derivative"].as_f64().unwrap_or(0.0) as f32,
            ));
        }
        Ok(Spline { coordinate, points })
    }

    fn object(&mut self, v: &Value) -> Result<Df, String> {
        let kind = v["type"]
            .as_str()
            .ok_or("density function without type")?
            .trim_start_matches("minecraft:");
        let unary =
            |b: &mut Self, op| -> Result<Df, String> { Ok(Df::Unary(op, b.boxed(&v["input"])?)) };
        let binary = |b: &mut Self, op| -> Result<Df, String> {
            Ok(Df::Binary(op, b.boxed(&v["left"])?, b.boxed(&v["right"])?))
        };
        let f = |k: &str| v[k].as_f64().unwrap_or(0.0);
        Ok(match kind {
            "constant" => Df::Constant(f("argument")),
            "abs" => unary(self, Unary::Abs)?,
            "square" => unary(self, Unary::Square)?,
            "cube" => unary(self, Unary::Cube)?,
            "half_negative" => unary(self, Unary::HalfNegative)?,
            "quarter_negative" => unary(self, Unary::QuarterNegative)?,
            "squeeze" => unary(self, Unary::Squeeze)?,
            "negate" => unary(self, Unary::Negate)?,
            "sqrt" => unary(self, Unary::Sqrt)?,
            "log" => unary(self, Unary::Log)?,
            "sign" => unary(self, Unary::Sign)?,
            "reciprocal" => unary(self, Unary::Reciprocal)?,
            "add" => binary(self, Binary::Add)?,
            "sub" => binary(self, Binary::Sub)?,
            "mul" => binary(self, Binary::Mul)?,
            "div" => binary(self, Binary::Div)?,
            "min" => binary(self, Binary::Min)?,
            "max" => binary(self, Binary::Max)?,
            "pow" => Df::Binary(
                Binary::Pow,
                self.boxed(&v["base"])?,
                self.boxed(&v["exponent"])?,
            ),
            "ceil" | "floor" | "round" | "truncate" => {
                let mode = match kind {
                    "ceil" => Rounding::Ceil,
                    "floor" => Rounding::Floor,
                    "round" => Rounding::Round,
                    _ => Rounding::Truncate,
                };
                let multiple = if v.get("multiple").is_some() {
                    self.boxed(&v["multiple"])?
                } else {
                    Box::new(Df::Constant(1.0))
                };
                Df::Round(mode, self.boxed(&v["input"])?, multiple)
            }
            "clamp" => Df::Clamp(self.boxed(&v["input"])?, f("min"), f("max")),
            "gradient" | "y_clamped_gradient" => Df::Gradient {
                axis: Axis::of(&v["axis"]),
                tiling: match v["tiling"].as_str() {
                    Some("repeat") => Tiling::Repeat,
                    Some("mirrored_repeat") => Tiling::Mirrored,
                    _ => Tiling::Clamp,
                },
                from: v["from_coordinate"]
                    .as_i64()
                    .or(v["from_y"].as_i64())
                    .unwrap_or(0) as i32,
                to: v["to_coordinate"]
                    .as_i64()
                    .or(v["to_y"].as_i64())
                    .unwrap_or(1) as i32,
                from_value: f("from_value"),
                to_value: f("to_value"),
            },
            "lerp" => Df::Lerp(
                self.boxed(&v["alpha"])?,
                self.boxed(&v["first"])?,
                self.boxed(&v["second"])?,
            ),
            "noise" | "shifted_noise" => {
                let mut shift: [Option<Box<Df>>; 3] = [None, None, None];
                for (i, k) in ["shift_x", "shift_y", "shift_z"].into_iter().enumerate() {
                    if v.get(k).is_some() {
                        shift[i] = Some(self.boxed(&v[k])?);
                    }
                }
                Df::Noise {
                    noise: self.noise_field(&v["noise"])?,
                    xz_scale: f("xz_scale"),
                    y_scale: f("y_scale"),
                    shift,
                }
            }
            "shift_a" | "shift_b" | "shift" => Df::Shift {
                noise: self.noise_field(&v["argument"].clone().max_or(&v["noise"]))?,
                kind: match kind {
                    "shift_a" => 0,
                    "shift_b" => 1,
                    _ => 2,
                },
            },
            "range_choice" => Df::RangeChoice {
                input: self.boxed(&v["input"])?,
                min: f("min_inclusive"),
                max: f("max_exclusive"),
                inside: self.boxed(&v["when_in_range"])?,
                outside: self.boxed(&v["when_out_of_range"])?,
            },
            "interval_select" => Df::IntervalSelect {
                input: self.boxed(&v["input"])?,
                thresholds: v["thresholds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_f64)
                    .collect(),
                functions: v["functions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|x| self.build(x))
                    .collect::<Result<_, _>>()?,
            },
            "spline" => Df::Spline(Box::new(self.spline(&v["spline"])?)),
            "interpolated" => Df::Interpolated {
                input: self.boxed(&v["input"])?,
                xz: v["cell_size_xz"].as_i64().unwrap_or(4).max(1) as i32,
                y: v["cell_size_y"].as_i64().unwrap_or(8).max(1) as i32,
                slot: self.slot(),
            },
            "cache" | "cache_once" | "cache_2d" | "flat_cache" | "cache_all_in_cell"
            | "blend_density" => {
                let f = Arc::new(self.build(&v["input"])?);
                self.reference(f)
            }
            "blend_alpha" => Df::Constant(1.0),
            "blend_offset" | "beardifier" => Df::Constant(0.0),
            "old_blended_noise" => {
                let noise = match &self.blended {
                    Some(n) => n.clone(),
                    None => {
                        let mut random = self.factory.from_hash_of("minecraft:terrain");
                        let n = Arc::new(BlendedNoise::new(
                            &mut random,
                            f("xz_scale"),
                            f("y_scale"),
                            f("xz_factor"),
                            f("y_factor"),
                            f("smear_scale_multiplier"),
                        ));
                        self.blended = Some(n.clone());
                        n
                    }
                };
                Df::OldBlended(noise)
            }
            "find_top_surface" => Df::FindTopSurface {
                density: self.boxed(&v["density"])?,
                upper: self.boxed(&v["upper_bound"])?,
                lower: v["lower_bound"].as_i64().unwrap_or(-64) as i32,
                cell: v["cell_height"].as_i64().unwrap_or(1).max(1) as i32,
            },
            "slice" => Df::Slice {
                axis: Axis::of(&v["axis"]),
                coordinate: v["coordinate"].as_i64().unwrap_or(0) as i32,
                input: self.boxed(&v["input"])?,
            },
            "distance_to_point" => {
                let p: Vec<i32> = v["point"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                    .map(|x| x as i32)
                    .collect();
                Df::Distance {
                    point: [
                        p.first().copied().unwrap_or(0),
                        p.get(1).copied().unwrap_or(0),
                        p.get(2).copied().unwrap_or(0),
                    ],
                    metric: match v["metric"].as_str() {
                        Some("euclidean_squared") => Metric::EuclideanSquared,
                        Some("manhattan") => Metric::Manhattan,
                        Some("chebyshev") => Metric::Chebyshev,
                        _ => Metric::Euclidean,
                    },
                }
            }
            "end_outer_islands" | "end_islands" => {
                Df::EndIslands(Arc::new(crate::noise::end_islands_noise(self.seed)))
            }
            other => return Err(format!("unknown density function type {other}")),
        })
    }
}

/// `temperature` → `minecraft:temperature`.
pub fn full_id(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

trait OrElse {
    fn max_or(self, other: &Value) -> Value;
}

impl OrElse for Value {
    fn max_or(self, other: &Value) -> Value {
        if self.is_null() { other.clone() } else { self }
    }
}
