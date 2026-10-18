//! Placed features: a feature with the modifiers that turn the chunk's
//! corner into the positions it is tried at (minecraft.wiki, "Placed
//! feature"). Modifiers run depth first, as the game's position streams
//! do: the feature is placed at the first position before the second one
//! is drawn.

use std::sync::Arc;

use mc_tick::Dir;
use mc_worldgen::noise::SimplexNoise;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::feature::{Ctx, Feature};
use crate::predicate::Predicate;
use crate::rand::WorldgenRandom;
use crate::value::{HeightProvider, IntProvider};
use crate::{Heightmap, P, Region};

#[derive(Debug)]
pub enum Modifier {
    Count(IntProvider),
    InSquare,
    Biome,
    Heightmap(Heightmap),
    HeightRange(HeightProvider),
    Offset(IntProvider, IntProvider, IntProvider),
    Rarity(i32),
    Chance(f32),
    Filter(Predicate),
    SurfaceWaterDepth(i32),
    SurfaceRelative {
        map: Heightmap,
        min: i32,
        max: i32,
    },
    EnvironmentScan {
        dir: Dir,
        target: Predicate,
        allowed: Predicate,
        steps: i32,
    },
    CountOnEveryLayer(IntProvider),
    NoiseThresholdCount {
        level: f64,
        below: i32,
        above: i32,
    },
    NoiseBasedCount {
        ratio: i32,
        factor: f64,
        offset: f64,
    },
    Fixed(Vec<P>),
    OneOf(Vec<Modifier>),
    /// Every position of a box from here: its faces, and its edges and
    /// inside when asked.
    Cuboid {
        xz: IntProvider,
        y: IntProvider,
        edges: bool,
        interior: bool,
    },
}

#[derive(Debug)]
pub struct Placed {
    /// Registry name, for the biome check.
    pub id: Option<String>,
    pub feature: Arc<Feature>,
    pub modifiers: Vec<Modifier>,
}

impl Modifier {
    pub fn parse(v: &Value, facts: &crate::Facts) -> Option<Self> {
        let ty = v["type"].as_str()?.trim_start_matches("minecraft:");
        let int = |k: &str| v[k].as_i64().map(|n| n as i32);
        let ip = |k: &str| IntProvider::parse(&v[k]);
        Some(match ty {
            "count" => Modifier::Count(ip("count")?),
            "in_square" => Modifier::InSquare,
            "biome" => Modifier::Biome,
            "heightmap" => Modifier::Heightmap(Heightmap::parse(v["heightmap"].as_str()?)?),
            "height_range" => Modifier::HeightRange(HeightProvider::parse(&v["height"])?),
            "offset" | "random_offset" => {
                let get = |k: &str| ip(k).unwrap_or(IntProvider::Constant(0));
                if v.get("xz_spread").is_some() {
                    let xz = get("xz_spread");
                    Modifier::Offset(xz.clone(), get("y_spread"), xz)
                } else {
                    Modifier::Offset(get("x"), get("y"), get("z"))
                }
            }
            "rarity_filter" => Modifier::Rarity(int("chance")?),
            "random_chance" => Modifier::Chance(v["chance"].as_f64()? as f32),
            "block_predicate_filter" => Modifier::Filter(Predicate::parse(&v["predicate"], facts)?),
            "surface_water_depth_filter" => Modifier::SurfaceWaterDepth(int("max_water_depth")?),
            "surface_relative_threshold_filter" => Modifier::SurfaceRelative {
                map: Heightmap::parse(v["heightmap"].as_str()?)?,
                min: int("min_inclusive").unwrap_or(i32::MIN),
                max: int("max_inclusive").unwrap_or(i32::MAX),
            },
            "environment_scan" => Modifier::EnvironmentScan {
                dir: Dir::from_name(v["direction_of_search"].as_str()?)?,
                target: Predicate::parse(&v["target_condition"], facts)?,
                allowed: match v.get("allowed_search_condition") {
                    Some(a) => Predicate::parse(a, facts)?,
                    None => Predicate::True,
                },
                steps: int("max_steps")?,
            },
            "count_on_every_layer" => Modifier::CountOnEveryLayer(ip("count")?),
            "noise_threshold_count" => Modifier::NoiseThresholdCount {
                level: v["noise_level"].as_f64()?,
                below: int("below_noise")?,
                above: int("above_noise")?,
            },
            "noise_based_count" => Modifier::NoiseBasedCount {
                ratio: int("noise_to_count_ratio")?,
                factor: v["noise_factor"].as_f64()?,
                offset: v["noise_offset"].as_f64().unwrap_or(0.0),
            },
            "fixed_placement" => Modifier::Fixed(
                v["positions"]
                    .as_array()?
                    .iter()
                    .filter_map(|p| {
                        let a = p.as_array()?;
                        Some(P::new(
                            a.first()?.as_i64()? as i32,
                            a.get(1)?.as_i64()? as i32,
                            a.get(2)?.as_i64()? as i32,
                        ))
                    })
                    .collect(),
            ),
            "cuboid" => Modifier::Cuboid {
                xz: ip("xz_size")?,
                y: ip("y_size")?,
                edges: v["include_edges"].as_bool().unwrap_or(false),
                interior: v["include_interior"].as_bool().unwrap_or(false),
            },
            "randomly_selected" => Modifier::OneOf(
                v["placements"]
                    .as_array()?
                    .iter()
                    .filter_map(|m| Modifier::parse(m, facts))
                    .collect(),
            ),
            _ => return None,
        })
    }

    fn positions(
        &self,
        cx: &Ctx,
        region: &dyn Region,
        r: &mut WorldgenRandom,
        p: P,
        top: Option<&str>,
    ) -> Vec<P> {
        let facts = cx.facts;
        match self {
            Modifier::Count(n) => vec![p; n.sample(r).max(0) as usize],
            Modifier::InSquare => {
                let x = r.next_int_bound(16) + p.x;
                let z = r.next_int_bound(16) + p.z;
                vec![P::new(x, p.y, z)]
            }
            Modifier::Biome => match top {
                Some(id) if cx.features.biome_has(region.biome(p), id) => vec![p],
                Some(_) => Vec::new(),
                None => vec![p],
            },
            Modifier::Heightmap(map) => {
                let y = region.height(*map, p.x, p.z);
                if y > region.heights().min_y {
                    vec![p.with_y(y)]
                } else {
                    Vec::new()
                }
            }
            Modifier::HeightRange(h) => vec![p.with_y(h.sample(r, region.heights()))],
            Modifier::Offset(x, y, z) => {
                let dx = x.sample(r);
                let dy = y.sample(r);
                let dz = z.sample(r);
                vec![p.offset(dx, dy, dz)]
            }
            Modifier::Rarity(chance) => {
                if r.next_float() < 1.0 / *chance as f32 {
                    vec![p]
                } else {
                    Vec::new()
                }
            }
            Modifier::Chance(chance) => {
                if r.next_float() < *chance {
                    vec![p]
                } else {
                    Vec::new()
                }
            }
            Modifier::Filter(pred) => {
                if pred.test(region, facts, p) {
                    vec![p]
                } else {
                    Vec::new()
                }
            }
            Modifier::SurfaceWaterDepth(max) => {
                let floor = region.height(Heightmap::OceanFloor, p.x, p.z);
                let surface = region.height(Heightmap::WorldSurface, p.x, p.z);
                if surface - floor <= *max {
                    vec![p]
                } else {
                    Vec::new()
                }
            }
            Modifier::SurfaceRelative { map, min, max } => {
                let h = i64::from(region.height(*map, p.x, p.z));
                let y = i64::from(p.y);
                if h + i64::from(*min) <= y && y <= h + i64::from(*max) {
                    vec![p]
                } else {
                    Vec::new()
                }
            }
            Modifier::EnvironmentScan {
                dir,
                target,
                allowed,
                steps,
            } => {
                let h = region.heights();
                let mut q = p;
                if !allowed.test(region, facts, q) {
                    return Vec::new();
                }
                for _ in 0..*steps {
                    if target.test(region, facts, q) {
                        return vec![q];
                    }
                    q = q.rel(*dir);
                    if q.y < h.min_y || q.y >= h.min_y + h.height {
                        return Vec::new();
                    }
                    if !allowed.test(region, facts, q) {
                        break;
                    }
                }
                if target.test(region, facts, q) {
                    vec![q]
                } else {
                    Vec::new()
                }
            }
            Modifier::CountOnEveryLayer(count) => {
                let mut out = Vec::new();
                let mut layer = 0;
                loop {
                    let mut any = false;
                    let mut j = 0;
                    while j < count.sample(r) {
                        j += 1;
                        let x = r.next_int_bound(16) + p.x;
                        let z = r.next_int_bound(16) + p.z;
                        let top_y = region.height(Heightmap::MotionBlocking, x, z);
                        if let Some(y) = on_ground(region, facts, x, top_y, z, layer) {
                            out.push(P::new(x, y, z));
                            any = true;
                        }
                    }
                    layer += 1;
                    if !any {
                        break;
                    }
                }
                out
            }
            Modifier::NoiseThresholdCount {
                level,
                below,
                above,
            } => {
                let d = cx
                    .features
                    .info_noise
                    .value(f64::from(p.x) / 200.0, f64::from(p.z) / 200.0);
                let n = if d < *level { *below } else { *above };
                vec![p; n.max(0) as usize]
            }
            Modifier::NoiseBasedCount {
                ratio,
                factor,
                offset,
            } => {
                let d = cx
                    .features
                    .info_noise
                    .value(f64::from(p.x) / factor, f64::from(p.z) / factor);
                let n = ((d + offset) * f64::from(*ratio)).ceil() as i32;
                vec![p; n.max(0) as usize]
            }
            Modifier::Fixed(list) => list
                .iter()
                .copied()
                .filter(|q| q.x >> 4 == p.x >> 4 && q.z >> 4 == p.z >> 4)
                .collect(),
            Modifier::Cuboid {
                xz,
                y,
                edges,
                interior,
            } => {
                let sx = xz.sample(r);
                let sy = y.sample(r);
                let sz = xz.sample(r);
                let mut out = Vec::new();
                for m in 0..=sx {
                    for n in 0..=sy {
                        for o in 0..=sz {
                            let on = [m == 0 || m == sx, n == 0 || n == sy, o == 0 || o == sz];
                            let k = on.iter().filter(|b| **b).count();
                            let keep = match k {
                                0 => *interior,
                                1 => true,
                                _ => *edges,
                            };
                            if keep {
                                out.push(p.offset(m, n, o));
                            }
                        }
                    }
                }
                out
            }
            Modifier::OneOf(list) => {
                if list.is_empty() {
                    return vec![p];
                }
                let m = &list[r.next_int_bound(list.len() as i32) as usize];
                m.positions(cx, region, r, p, top)
            }
        }
    }
}

/// The y above the `layer`th ground (from the top) of a column, for
/// `count_on_every_layer`.
fn on_ground(
    region: &dyn Region,
    facts: &crate::Facts,
    x: i32,
    top: i32,
    z: i32,
    layer: i32,
) -> Option<i32> {
    let empty =
        |s: u32| facts.is_air(s) || facts.is(s, "minecraft:water") || facts.is(s, "minecraft:lava");
    let mut seen = 0;
    let mut above = region.block(P::new(x, top, z));
    let min = region.heights().min_y;
    let mut y = top;
    while y > min {
        let below = region.block(P::new(x, y - 1, z));
        if !empty(below) && empty(above) && facts.block(below) != facts.bedrock_block {
            if seen == layer {
                return Some(y);
            }
            seen += 1;
        }
        above = below;
        y -= 1;
    }
    None
}

impl Placed {
    /// Places the feature at every position the modifiers give; true if it
    /// went in at least once. `top` names the biome-listed feature for the
    /// biome check.
    pub fn place(
        &self,
        cx: &Ctx,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        origin: P,
        top: Option<&str>,
    ) -> bool {
        let mut placed = false;
        self.walk(0, cx, region, r, origin, top, &mut placed);
        placed
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        i: usize,
        cx: &Ctx,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        p: P,
        top: Option<&str>,
        placed: &mut bool,
    ) {
        let Some(m) = self.modifiers.get(i) else {
            if self.feature.place(cx, region, r, p) {
                *placed = true;
            }
            return;
        };
        for q in m.positions(cx, region, r, p, top) {
            self.walk(i + 1, cx, region, r, q, top, placed);
        }
    }
}

/// Simplex octaves summed, as the game's biome noises (`BIOME_INFO_NOISE`,
/// temperature noise).
#[derive(Clone, Debug)]
pub struct PerlinSimplex {
    levels: Vec<Option<SimplexNoise>>,
    input: f64,
    value: f64,
}

impl PerlinSimplex {
    pub fn new(seed: i64, octaves: &[i32]) -> Self {
        let mut r = WorldgenRandom::legacy(seed);
        let first = -octaves.iter().copied().min().unwrap_or(0);
        let last = octaves.iter().copied().max().unwrap_or(0);
        let k = (first + last + 1) as usize;
        let base = SimplexNoise::new(&mut r);
        let l = last;
        let mut levels: Vec<Option<SimplexNoise>> = vec![None; k];
        if l >= 0 && (l as usize) < k && octaves.contains(&0) {
            levels[l as usize] = Some(base.clone());
        }
        for (m, level) in levels.iter_mut().enumerate().skip((l + 1) as usize) {
            if octaves.contains(&(l - m as i32)) {
                *level = Some(SimplexNoise::new(&mut r));
            } else {
                r.consume(262);
            }
        }
        // Octaves above 0 (seeded from the first noise's value) are not
        // used by any noise features read.
        Self {
            levels,
            input: 2f64.powi(last),
            value: 1.0 / (2f64.powi(k as i32) - 1.0),
        }
    }

    pub fn value(&self, x: f64, z: f64) -> f64 {
        let (mut d, mut e, mut f) = (0.0, self.input, self.value);
        for n in self.levels.iter() {
            if let Some(n) = n {
                d += n.value_2d(x * e, z * e) * f;
            }
            e /= 2.0;
            f *= 2.0;
        }
        d
    }
}
