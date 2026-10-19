//! Aquifers (minecraft.wiki, "Aquifer"): underground water and lava at
//! their own levels, kept apart by barriers.
//!
//! The world is cut into 16×12×16 cells (shifted 5 on x and z, 1 on y);
//! each has a center at a random spot inside (0–9, 0–8, 0–9) and a fluid
//! status: a level and a fluid. A cell high above the terrain is air, one
//! reaching above a sea-level surface is sea, the rest are flooded, partly
//! flooded or dry by noise. An empty block takes the status of its nearest
//! center; where two or three centers are about as near and their
//! statuses differ, a pressure added to the terrain density can make it
//! stone instead.

use crate::density::{Ctx, Df};
use crate::random::{Positional, RandomSource};

/// No fluid at all.
pub const NO_LEVEL: i32 = i32::MIN;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fluid {
    Air,
    Water,
    Lava,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub level: i32,
    pub fluid: Fluid,
}

impl Status {
    /// What is at `y`: the fluid below its level, air above.
    pub fn at(&self, y: i32) -> Fluid {
        if y < self.level {
            self.fluid
        } else {
            Fluid::Air
        }
    }
}

/// The aquifer functions from the noise settings.
#[derive(Debug)]
pub struct Functions {
    pub barrier: Df,
    pub floodedness: Df,
    pub spread: Df,
    pub lava: Df,
    pub exclusion: Df,
    pub surface: Df,
}

/// Where the sea and the lava sea are.
#[derive(Clone, Copy, Debug)]
pub struct Global {
    pub sea_level: i32,
    pub sea: Fluid,
    pub lava_level: i32,
}

impl Global {
    pub fn at(&self, y: i32) -> Status {
        if y < self.lava_level.min(self.sea_level) {
            Status {
                level: self.lava_level,
                fluid: Fluid::Lava,
            }
        } else {
            Status {
                level: self.sea_level,
                fluid: self.sea,
            }
        }
    }
}

const SURFACE_OFFSETS: [(i32, i32); 13] = [
    (0, 0),
    (-2, -1),
    (-1, -1),
    (0, -1),
    (1, -1),
    (-3, 0),
    (-2, 0),
    (-1, 0),
    (1, 0),
    (-2, 1),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// One chunk's aquifer cells.
pub struct Aquifer<'a> {
    f: &'a Functions,
    global: Global,
    random: Positional,
    min_x: i32,
    min_y: i32,
    min_z: i32,
    size_x: usize,
    size_z: usize,
    centers: Vec<Option<(i32, i32, i32)>>,
    statuses: Vec<Option<Status>>,
    /// Per cell, whether it and the other eleven a block near it may take
    /// its status from all have the same status (then that is the answer).
    uniform: Vec<Uniform>,
    surfaces: std::collections::HashMap<(i32, i32), i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Uniform {
    Unknown,
    No,
    Yes(Status),
}

impl std::fmt::Debug for Aquifer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aquifer")
            .field("min_x", &self.min_x)
            .finish()
    }
}

fn grid_x(x: i32) -> i32 {
    x.div_euclid(16)
}

fn grid_y(y: i32) -> i32 {
    y.div_euclid(12)
}

fn similarity(a: i64, b: i64) -> f64 {
    1.0 - (b - a).abs() as f64 / 25.0
}

impl<'a> Aquifer<'a> {
    pub fn new(
        f: &'a Functions,
        global: Global,
        random: Positional,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
    ) -> Self {
        let min_x = grid_x(chunk_x * 16) - 1;
        let max_x = grid_x(chunk_x * 16 + 15) + 1;
        let min_gy = grid_y(min_y) - 1;
        let max_gy = grid_y(min_y + height) + 1;
        let min_z = grid_x(chunk_z * 16) - 1;
        let max_z = grid_x(chunk_z * 16 + 15) + 1;
        let (size_x, size_y, size_z) = (
            (max_x - min_x + 1) as usize,
            (max_gy - min_gy + 1) as usize,
            (max_z - min_z + 1) as usize,
        );
        let n = size_x * size_y * size_z;
        Self {
            f,
            global,
            random,
            min_x,
            min_y: min_gy,
            min_z,
            size_x,
            size_z,
            centers: vec![None; n],
            statuses: vec![None; n],
            uniform: vec![Uniform::Unknown; n],
            surfaces: std::collections::HashMap::new(),
        }
    }

    fn index(&self, gx: i32, gy: i32, gz: i32) -> usize {
        let (x, y, z) = (
            (gx - self.min_x) as usize,
            (gy - self.min_y) as usize,
            (gz - self.min_z) as usize,
        );
        (y * self.size_z + z) * self.size_x + x
    }

    fn center(&mut self, gx: i32, gy: i32, gz: i32) -> (i32, i32, i32) {
        let i = self.index(gx, gy, gz);
        if let Some(c) = self.centers[i] {
            return c;
        }
        let mut r = self.random.at(gx, gy, gz);
        let c = (
            gx * 16 + r.next_int_bound(10),
            gy * 12 + r.next_int_bound(9),
            gz * 16 + r.next_int_bound(10),
        );
        self.centers[i] = Some(c);
        c
    }

    fn surface(&mut self, x: i32, z: i32, ctx: &mut Ctx) -> i32 {
        if let Some(&s) = self.surfaces.get(&(x, z)) {
            return s;
        }
        let s = self.f.surface.eval_in(x, 0, z, ctx).floor() as i32;
        self.surfaces.insert((x, z), s);
        s
    }

    fn status(&mut self, i: usize, (x, y, z): (i32, i32, i32), ctx: &mut Ctx) -> Status {
        if let Some(s) = self.statuses[i] {
            return s;
        }
        let s = self.compute_status(x, y, z, ctx);
        self.statuses[i] = Some(s);
        s
    }

    fn compute_status(&mut self, x: i32, y: i32, z: i32, ctx: &mut Ctx) -> Status {
        let global = self.global.at(y);
        let mut lowest = i32::MAX;
        let (top, bottom) = (y + 12, y - 12);
        let mut sea_here = false;
        for (dx, dz) in SURFACE_OFFSETS {
            let (sx, sz) = (x + dx * 16, z + dz * 16);
            let surface = self.surface(sx, sz, ctx);
            let raised = surface + 8;
            let own = dx == 0 && dz == 0;
            if own && bottom > raised {
                return global;
            }
            let pokes = top > raised;
            if pokes || own {
                let fs = self.global.at(raised);
                if fs.at(raised) != Fluid::Air {
                    if own {
                        sea_here = true;
                    }
                    if pokes {
                        return fs;
                    }
                }
            }
            lowest = lowest.min(surface);
        }
        let level = self.surface_level(x, y, z, global, lowest, sea_here, ctx);
        Status {
            level,
            fluid: self.fluid_type(x, y, z, global, level, ctx),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn surface_level(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        global: Status,
        lowest: i32,
        sea_here: bool,
        ctx: &mut Ctx,
    ) -> i32 {
        let (full, partial) = if self.f.exclusion.eval(x, y, z) > 0.0 {
            (-1.0, -1.0)
        } else {
            let below = lowest + 8 - y;
            let t = if sea_here {
                crate::noise::clamped_lerp(1.0, 0.0, f64::from(below) / 64.0)
            } else {
                0.0
            };
            let g = self.f.floodedness.eval(x, y, z).clamp(-1.0, 1.0);
            let h = map(t, 1.0, 0.0, -0.3, 0.8);
            let k = map(t, 1.0, 0.0, -0.8, 0.4);
            (g - h, g - k)
        };
        let _ = ctx;
        if full > 0.0 {
            global.level
        } else if partial > 0.0 {
            let (kx, ky, kz) = (x.div_euclid(16), y.div_euclid(40), z.div_euclid(16));
            let base = ky * 40 + 20;
            let d = self.f.spread.eval(kx, ky, kz) * 10.0;
            let step = (d / 3.0).floor() as i32 * 3;
            lowest.min(base + step)
        } else {
            NO_LEVEL
        }
    }

    fn fluid_type(
        &self,
        x: i32,
        y: i32,
        z: i32,
        global: Status,
        level: i32,
        _ctx: &mut Ctx,
    ) -> Fluid {
        if level <= -10 && level != NO_LEVEL && global.fluid != Fluid::Lava {
            let (kx, ky, kz) = (x.div_euclid(64), y.div_euclid(40), z.div_euclid(64));
            if self.f.lava.eval(kx, ky, kz).abs() > 0.3 {
                return Fluid::Lava;
            }
        }
        global.fluid
    }

    fn pressure(
        &self,
        x: i32,
        y: i32,
        z: i32,
        barrier: &mut Option<f64>,
        a: Status,
        b: Status,
    ) -> f64 {
        let (fa, fb) = (a.at(y), b.at(y));
        if (fa == Fluid::Lava && fb == Fluid::Water) || (fa == Fluid::Water && fb == Fluid::Lava) {
            return 2.0;
        }
        let diff = (a.level as i64 - b.level as i64).abs();
        if diff == 0 {
            return 0.0;
        }
        let mid = 0.5 * (a.level as f64 + b.level as f64);
        let e = f64::from(y) + 0.5 - mid;
        let half = diff as f64 / 2.0;
        let o = half - e.abs();
        let q = if e > 0.0 {
            if o > 0.0 { o / 1.5 } else { o / 2.5 }
        } else {
            let p = 3.0 + o;
            if p > 0.0 { p / 3.0 } else { p / 10.0 }
        };
        let s = if (-2.0..=2.0).contains(&q) {
            *barrier.get_or_insert_with(|| self.f.barrier.eval(x, y, z))
        } else {
            0.0
        };
        2.0 * (s + q)
    }

    fn uniform_at(&mut self, l: i32, m: i32, n: i32) -> Uniform {
        let at = self.index(l, m, n);
        if self.uniform[at] != Uniform::Unknown {
            return self.uniform[at];
        }
        let mut first = None;
        let mut u = Uniform::No;
        'cells: {
            for dx in 0..=1 {
                for dy in -1..=1 {
                    for dz in 0..=1 {
                        let i = self.index(l + dx, m + dy, n + dz);
                        // Only statuses worked out already: the long way
                        // works out just the ones it needs.
                        let Some(s) = self.statuses[i] else {
                            return Uniform::Unknown;
                        };
                        if *first.get_or_insert(s) != s {
                            break 'cells;
                        }
                    }
                }
            }
            u = Uniform::Yes(first.expect("twelve cells"));
        }
        self.uniform[at] = u;
        u
    }

    /// What fills an empty block (`density` ≤ 0) at a position: `None` for
    /// stone (a barrier), else air, water or lava.
    pub fn substance(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        density: f64,
        ctx: &mut Ctx,
    ) -> Option<Fluid> {
        if density > 0.0 {
            return None;
        }
        let global = self.global.at(y);
        if global.at(y) == Fluid::Lava {
            return Some(Fluid::Lava);
        }
        let (l, m, n) = (grid_x(x - 5), grid_y(y + 1), grid_x(z - 5));
        // Where all twelve cells agree, pressures are all 0 and the nearest
        // one's status is the answer, as it is for the long way below.
        if let Uniform::Yes(s) = self.uniform_at(l, m, n) {
            return Some(s.at(y));
        }
        let mut best = [(i64::MAX, 0usize, (0, 0, 0)); 4];
        for dx in 0..=1 {
            for dy in -1..=1 {
                for dz in 0..=1 {
                    let (gx, gy, gz) = (l + dx, m + dy, n + dz);
                    let i = self.index(gx, gy, gz);
                    let c = self.center(gx, gy, gz);
                    let d = (c.0 - x) as i64 * (c.0 - x) as i64
                        + (c.1 - y) as i64 * (c.1 - y) as i64
                        + (c.2 - z) as i64 * (c.2 - z) as i64;
                    // Keep the four nearest, nearest first.
                    let mut entry = (d, i, c);
                    // An equally near later cell goes first, as the game's.
                    for slot in best.iter_mut() {
                        if entry.0 <= slot.0 {
                            std::mem::swap(slot, &mut entry);
                        }
                    }
                }
            }
        }
        let s1 = self.status(best[0].1, best[0].2, ctx);
        let d12 = similarity(best[0].0, best[1].0);
        let fluid = s1.at(y);
        if d12 <= 0.0 {
            return Some(fluid);
        }
        if fluid == Fluid::Water && self.global.at(y - 1).at(y - 1) == Fluid::Lava {
            return Some(fluid);
        }
        let mut barrier = None;
        let s2 = self.status(best[1].1, best[1].2, ctx);
        if density + d12 * self.pressure(x, y, z, &mut barrier, s1, s2) > 0.0 {
            return None;
        }
        let s3 = self.status(best[2].1, best[2].2, ctx);
        let d13 = similarity(best[0].0, best[2].0);
        if d13 > 0.0 && density + d12 * d13 * self.pressure(x, y, z, &mut barrier, s1, s3) > 0.0 {
            return None;
        }
        let d23 = similarity(best[1].0, best[2].0);
        if d23 > 0.0 && density + d12 * d23 * self.pressure(x, y, z, &mut barrier, s2, s3) > 0.0 {
            return None;
        }
        Some(fluid)
    }
}

/// `map`: `v` from `a..b` to `c..d`, not held at the ends.
fn map(v: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    c + (v - a) / (b - a) * (d - c)
}
