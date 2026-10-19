//! Carvers (`worldgen/carver/<name>.json`): cave tunnels and canyons cut
//! into the terrain. Each chunk within 8 chunks may start some, by its own
//! seed; a chunk carves the parts that reach it. Since 26.3 carvers have
//! no list of blocks they replace: they cut the terrain the noise made,
//! before its materials, and what fills the hole is the aquifer's.

use serde_json::Value;

use crate::random::{Legacy, RandomSource};

/// `Mth.sin` and `Mth.cos`: a table of 65536 values.
pub struct Trig {
    table: Vec<f32>,
}

impl std::fmt::Debug for Trig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Trig").finish_non_exhaustive()
    }
}

impl Default for Trig {
    fn default() -> Self {
        Self {
            table: (0..65536)
                .map(|i| (f64::from(i) * std::f64::consts::PI * 2.0 / 65536.0).sin() as f32)
                .collect(),
        }
    }
}

impl Trig {
    pub fn sin(&self, v: f64) -> f32 {
        self.table[((v * 10430.378350470453) as i64 & 65535) as usize]
    }

    pub fn cos(&self, v: f64) -> f32 {
        self.table[((v * 10430.378350470453 + 16384.0) as i64 & 65535) as usize]
    }
}

/// A float provider.
#[derive(Clone, Copy, Debug)]
enum Float {
    Constant(f32),
    Uniform(f32, f32),
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl Float {
    fn parse(v: &Value) -> Float {
        if let Some(c) = v.as_f64() {
            return Float::Constant(c as f32);
        }
        let f = |k: &str| v[k].as_f64().unwrap_or(0.0) as f32;
        match v["type"].as_str() {
            Some("minecraft:uniform") => Float::Uniform(f("min_inclusive"), f("max_exclusive")),
            Some("minecraft:trapezoid") => Float::Trapezoid {
                min: f("min"),
                max: f("max"),
                plateau: f("plateau"),
            },
            _ => Float::Constant(f("value")),
        }
    }

    fn sample(self, r: &mut Legacy) -> f32 {
        match self {
            Float::Constant(c) => c,
            Float::Uniform(a, b) => r.next_float() * (b - a) + a,
            Float::Trapezoid { min, max, plateau } => {
                let f = max - min;
                let g = (f - plateau) / 2.0;
                let h = f - g;
                min + r.next_float() * h + r.next_float() * g
            }
        }
    }
}

/// A uniform height between two anchors.
#[derive(Clone, Copy, Debug)]
struct Height {
    min: i32,
    max: i32,
}

fn anchor(v: &Value, min_y: i32, height: i32) -> i32 {
    if let Some(a) = v["absolute"].as_i64() {
        a as i32
    } else if let Some(a) = v["above_bottom"].as_i64() {
        min_y + a as i32
    } else if let Some(a) = v["below_top"].as_i64() {
        min_y + height - 1 - a as i32
    } else {
        0
    }
}

impl Height {
    fn parse(v: &Value, min_y: i32, height: i32) -> Height {
        Height {
            min: anchor(&v["min_inclusive"], min_y, height),
            max: anchor(&v["max_inclusive"], min_y, height),
        }
    }

    fn sample(self, r: &mut Legacy) -> i32 {
        r.next_int_between(self.min, self.max)
    }
}

#[derive(Clone, Copy, Debug)]
struct Cave {
    /// Tunnel systems per start chunk: `very_biased_to_bottom` up to this.
    count_max: i32,
    floor_level: Float,
    horizontal: Float,
    vertical: Float,
    room_vertical: Float,
    start_vertical: f64,
    thickness: Float,
    weird_thickness: bool,
}

#[derive(Clone, Copy, Debug)]
struct Canyon {
    vertical_rotation: Float,
    y_scale: Float,
    distance: Float,
    thickness: Float,
    horizontal: Float,
    vertical_default: f32,
    vertical_center: f32,
    width_smoothness: i32,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Cave(Cave),
    Canyon(Canyon),
}

#[derive(Clone, Copy, Debug)]
pub struct Carver {
    probability: f32,
    y: Height,
    kind: Kind,
}

impl Carver {
    pub fn parse(v: &Value, min_y: i32, height: i32) -> Option<Carver> {
        let y = Height::parse(&v["y"], min_y, height);
        let probability = v["probability"].as_f64()? as f32;
        let kind = match v["type"].as_str()? {
            "minecraft:cave" => Kind::Cave(Cave {
                count_max: v["count"]["max_inclusive"]
                    .as_i64()
                    .or_else(|| v["count"].as_i64())
                    .unwrap_or(14) as i32,
                floor_level: Float::parse(&v["floor_level"]),
                horizontal: Float::parse(&v["horizontal_radius_multiplier"]),
                vertical: Float::parse(&v["vertical_radius_multiplier"]),
                room_vertical: Float::parse(&v["room_vertical_radius_multiplier"]),
                start_vertical: v["start_vertical_radius_multiplier"]
                    .as_f64()
                    .unwrap_or(1.0),
                thickness: Float::parse(&v["thickness"]),
                weird_thickness: v["weird_thickness_bias"].as_bool().unwrap_or(false),
            }),
            "minecraft:canyon" => {
                let s = &v["shape"];
                Kind::Canyon(Canyon {
                    vertical_rotation: Float::parse(&v["vertical_rotation"]),
                    y_scale: Float::parse(&s["y_scale"]),
                    distance: Float::parse(&s["distance_factor"]),
                    thickness: Float::parse(&s["thickness"]),
                    horizontal: Float::parse(&s["horizontal_radius_factor"]),
                    vertical_default: s["vertical_radius_default_factor"].as_f64().unwrap_or(1.0)
                        as f32,
                    vertical_center: s["vertical_radius_center_factor"].as_f64().unwrap_or(0.0)
                        as f32,
                    width_smoothness: s["width_smoothness"].as_i64().unwrap_or(3).max(1) as i32,
                })
            }
            _ => return None,
        };
        Some(Carver {
            probability,
            y,
            kind,
        })
    }
}

/// How far (in blocks) a carver started in one chunk may wander.
const RANGE: i32 = (4 * 2 - 1) * 16;

/// The chunk being carved: what is carved where.
pub struct Target<'a> {
    pub cx: i32,
    pub cz: i32,
    pub min_y: i32,
    pub height: i32,
    pub trig: &'a Trig,
    /// Carves the block if it can be: whether it did.
    pub carve: &'a mut dyn FnMut(i32, i32, i32) -> bool,
    /// Blocks already carved (y, z, x within the chunk).
    pub mask: Vec<bool>,
}

impl std::fmt::Debug for Target<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("cx", &self.cx)
            .field("cz", &self.cz)
            .finish()
    }
}

impl Target<'_> {
    fn middle(&self) -> (f64, f64) {
        (f64::from(self.cx * 16 + 8), f64::from(self.cz * 16 + 8))
    }

    fn can_reach(&self, x: f64, z: f64, step: i32, max_step: i32, thickness: f32) -> bool {
        let (mx, mz) = self.middle();
        let (dx, dz) = (x - mx, z - mz);
        let remaining = f64::from(max_step - step);
        let r = f64::from(thickness + 2.0 + 16.0);
        dx * dx + dz * dz - remaining * remaining <= r * r
    }

    /// Carves an ellipsoid where `skip` allows.
    fn ellipsoid(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        h: f64,
        v: f64,
        skip: &dyn Fn(f64, f64, f64, i32) -> bool,
    ) {
        let (mx, mz) = self.middle();
        let reach = 16.0 + h * 2.0;
        if (x - mx).abs() > reach || (z - mz).abs() > reach {
            return;
        }
        let (min_x, min_z) = (self.cx * 16, self.cz * 16);
        let x0 = ((x - h).floor() as i32 - min_x - 1).max(0);
        let x1 = ((x + h).floor() as i32 - min_x).min(15);
        let y0 = ((y - v).floor() as i32 - 1).max(self.min_y + 1);
        let y1 = ((y + v).floor() as i32 + 1).min(self.min_y + self.height - 1 - 7);
        let z0 = ((z - h).floor() as i32 - min_z - 1).max(0);
        let z1 = ((z + h).floor() as i32 - min_z).min(15);
        for lx in x0..=x1 {
            let wx = min_x + lx;
            let rx = (f64::from(wx) + 0.5 - x) / h;
            for lz in z0..=z1 {
                let wz = min_z + lz;
                let rz = (f64::from(wz) + 0.5 - z) / h;
                if rx * rx + rz * rz >= 1.0 {
                    continue;
                }
                let mut wy = y1;
                while wy > y0 {
                    let ry = (f64::from(wy) - 0.5 - y) / v;
                    let i = (((wy - self.min_y) * 16 + lz) * 16 + lx) as usize;
                    if !skip(rx, ry, rz, wy) && !self.mask[i] {
                        self.mask[i] = true;
                        (self.carve)(wx, wy, wz);
                    }
                    wy -= 1;
                }
            }
        }
    }
}

/// `setLargeFeatureSeed`.
fn large_feature_seed(r: &mut Legacy, seed: i64, cx: i32, cz: i32) {
    r.set_seed(seed);
    let a = r.next_long();
    let b = r.next_long();
    r.set_seed(i64::from(cx).wrapping_mul(a) ^ i64::from(cz).wrapping_mul(b) ^ seed);
}

/// Carves `target` with the carvers `of` each chunk within range.
pub fn carve(seed: i64, target: &mut Target, of: &dyn Fn(i32, i32) -> Vec<Carver>) {
    let mut r = Legacy::new(0);
    for dx in -8..=8 {
        for dz in -8..=8 {
            let (sx, sz) = (target.cx + dx, target.cz + dz);
            for (i, c) in of(sx, sz).iter().enumerate() {
                large_feature_seed(&mut r, seed.wrapping_add(i as i64), sx, sz);
                if r.next_float() > c.probability {
                    continue;
                }
                match c.kind {
                    Kind::Cave(cave) => carve_cave(target, &cave, c.y, &mut r, sx, sz),
                    Kind::Canyon(canyon) => carve_canyon(target, &canyon, c.y, &mut r, sx, sz),
                }
            }
        }
    }
}

fn cave_skip(floor: f64) -> impl Fn(f64, f64, f64, i32) -> bool {
    move |rx, ry, rz, _| ry <= floor || rx * rx + ry * ry + rz * rz >= 1.0
}

fn carve_cave(t: &mut Target, c: &Cave, y: Height, r: &mut Legacy, sx: i32, sz: i32) {
    let bound = c.count_max + 1;
    let a = r.next_int_bound(bound);
    let b = r.next_int_bound(a + 1);
    let n = r.next_int_bound(b + 1);
    for _ in 0..n {
        let x = f64::from(sx * 16 + r.next_int_bound(16));
        let yy = f64::from(y.sample(r));
        let z = f64::from(sz * 16 + r.next_int_bound(16));
        let hm = f64::from(c.horizontal.sample(r));
        let vm = f64::from(c.vertical.sample(r));
        let floor = f64::from(c.floor_level.sample(r));
        let skip = cave_skip(floor);
        let mut tunnels = 1;
        if r.next_int_bound(4) == 0 {
            let y_scale = f64::from(c.room_vertical.sample(r));
            let thickness = 1.0 + r.next_float() * 6.0;
            let h = 1.5 + f64::from(t.trig.sin(f64::from(std::f32::consts::FRAC_PI_2)) * thickness);
            t.ellipsoid(x + 1.0, yy, z, h, h * y_scale, &skip);
            tunnels += r.next_int_bound(4);
        }
        for _ in 0..tunnels {
            let yaw = r.next_float() * (std::f64::consts::PI * 2.0) as f32;
            let pitch = (r.next_float() - 0.5) / 4.0;
            let mut thickness = c.thickness.sample(r);
            if c.weird_thickness && r.next_int_bound(10) == 0 {
                thickness *= r.next_float() * r.next_float() * 3.0 + 1.0;
            }
            let max_step = RANGE - r.next_int_bound(RANGE / 4);
            let tunnel = Tunnel { hm, vm, floor };
            tunnel.run(
                t,
                r.next_long(),
                (x, yy, z),
                thickness,
                yaw,
                pitch,
                0,
                max_step,
                c.start_vertical,
            );
        }
    }
}

struct Tunnel {
    hm: f64,
    vm: f64,
    floor: f64,
}

impl Tunnel {
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        t: &mut Target,
        seed: i64,
        (mut x, mut y, mut z): (f64, f64, f64),
        thickness: f32,
        mut yaw: f32,
        mut pitch: f32,
        step: i32,
        max_step: i32,
        y_scale: f64,
    ) {
        let mut r = Legacy::new(seed);
        let branch_at = r.next_int_bound(max_step / 2) + max_step / 4;
        let steep = r.next_int_bound(6) == 0;
        let (mut yaw_v, mut pitch_v) = (0.0f32, 0.0f32);
        let skip = cave_skip(self.floor);
        for i in step..max_step {
            let a = std::f32::consts::PI * i as f32 / max_step as f32;
            let h = 1.5 + f64::from(t.trig.sin(f64::from(a)) * thickness);
            let v = h * y_scale;
            let cp = t.trig.cos(f64::from(pitch));
            x += f64::from(t.trig.cos(f64::from(yaw)) * cp);
            y += f64::from(t.trig.sin(f64::from(pitch)));
            z += f64::from(t.trig.sin(f64::from(yaw)) * cp);
            pitch *= if steep { 0.92 } else { 0.7 };
            pitch += pitch_v * 0.1;
            yaw += yaw_v * 0.1;
            pitch_v *= 0.9;
            yaw_v *= 0.75;
            pitch_v += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
            yaw_v += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
            if i == branch_at && thickness > 1.0 {
                for side in [-1.0f32, 1.0] {
                    let seed = r.next_long();
                    let th = r.next_float() * 0.5 + 0.5;
                    let turn = std::f32::consts::FRAC_PI_2;
                    let yaw2 = if side < 0.0 { yaw - turn } else { yaw + turn };
                    self.run(t, seed, (x, y, z), th, yaw2, pitch / 3.0, i, max_step, 1.0);
                }
                return;
            }
            if r.next_int_bound(4) != 0 {
                if !t.can_reach(x, z, i, max_step, thickness) {
                    return;
                }
                t.ellipsoid(x, y, z, h * self.hm, v * self.vm, &skip);
            }
        }
    }
}

fn carve_canyon(t: &mut Target, c: &Canyon, y: Height, r: &mut Legacy, sx: i32, sz: i32) {
    let x = f64::from(sx * 16 + r.next_int_bound(16));
    let yy = f64::from(y.sample(r));
    let z = f64::from(sz * 16 + r.next_int_bound(16));
    let yaw = r.next_float() * (std::f64::consts::PI * 2.0) as f32;
    let pitch = c.vertical_rotation.sample(r);
    let y_scale = f64::from(c.y_scale.sample(r));
    let thickness = c.thickness.sample(r);
    let max_step = (RANGE as f32 * c.distance.sample(r)) as i32;
    let seed = r.next_long();
    canyon_run(
        t,
        c,
        seed,
        (x, yy, z),
        thickness,
        yaw,
        pitch,
        max_step,
        y_scale,
    );
}

#[allow(clippy::too_many_arguments)]
fn canyon_run(
    t: &mut Target,
    c: &Canyon,
    seed: i64,
    (mut x, mut y, mut z): (f64, f64, f64),
    thickness: f32,
    mut yaw: f32,
    mut pitch: f32,
    max_step: i32,
    y_scale: f64,
) {
    let mut r = Legacy::new(seed);
    let depth = t.height as usize;
    let mut widths = vec![0.0f32; depth];
    let mut w = 1.0f32;
    for (i, slot) in widths.iter_mut().enumerate() {
        if i == 0 || r.next_int_bound(c.width_smoothness) == 0 {
            w = 1.0 + r.next_float() * r.next_float();
        }
        *slot = w * w;
    }
    let min_y = t.min_y;
    let skip = move |rx: f64, ry: f64, rz: f64, wy: i32| {
        let i = (wy - min_y) as usize;
        (rx * rx + rz * rz) * f64::from(widths[i - 1]) + ry * ry / 6.0 >= 1.0
    };
    let (mut yaw_v, mut pitch_v) = (0.0f32, 0.0f32);
    for i in 0..max_step {
        let a = i as f32 * std::f32::consts::PI / max_step as f32;
        let mut h = 1.5 + f64::from(t.trig.sin(f64::from(a)) * thickness);
        let v = h * y_scale;
        h *= f64::from(c.horizontal.sample(&mut r));
        let f = 1.0 - (0.5 - i as f32 / max_step as f32).abs() * 2.0;
        let g = c.vertical_default + c.vertical_center * f;
        let v = f64::from(g) * v * f64::from(r.next_float() * (1.0 - 0.75) + 0.75);
        let cp = t.trig.cos(f64::from(pitch));
        let sp = t.trig.sin(f64::from(pitch));
        x += f64::from(t.trig.cos(f64::from(yaw)) * cp);
        y += f64::from(sp);
        z += f64::from(t.trig.sin(f64::from(yaw)) * cp);
        pitch *= 0.7;
        pitch += pitch_v * 0.05;
        yaw += yaw_v * 0.05;
        pitch_v *= 0.8;
        yaw_v *= 0.5;
        pitch_v += (r.next_float() - r.next_float()) * r.next_float() * 2.0;
        yaw_v += (r.next_float() - r.next_float()) * r.next_float() * 4.0;
        if r.next_int_bound(4) != 0 {
            if !t.can_reach(x, z, i, max_step, thickness) {
                return;
            }
            t.ellipsoid(x, y, z, h, v, &skip);
        }
    }
}
