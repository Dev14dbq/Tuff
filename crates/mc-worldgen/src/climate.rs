//! Biomes from climate (minecraft.wiki, "Biome#Biome selection"): six
//! climate values per position (temperature, humidity, continentalness,
//! erosion, depth, weirdness), each biome given as ranges of them in the
//! biome parameter list; the biome whose ranges are nearest wins.

use serde_json::Value;

use crate::density::{Builder, Ctx, Df};

/// Climate values are compared ×10000, as whole numbers.
pub fn quantize(v: f64) -> i64 {
    ((v as f32) * 10000.0f32) as i64
}

#[derive(Clone, Debug)]
struct Point {
    ranges: [[i64; 2]; 6],
    offset: i64,
    biome: String,
}

/// The overworld's (or the nether's) biome parameter list.
#[derive(Clone, Debug, Default)]
pub struct BiomeList {
    points: Vec<Point>,
    /// A bounding-box tree over `points` so `find` need not look at all of
    /// them; node 0 is the root.
    nodes: Vec<Node>,
}

/// Leaves hold at most this many points.
const LEAF: usize = 8;

#[derive(Clone, Debug)]
struct Node {
    /// Per value, the lowest low and the highest high of the points below.
    bounds: [[i64; 2]; 6],
    min_offset: i64,
    kind: NodeKind,
}

#[derive(Clone, Debug)]
enum NodeKind {
    /// Indices into `points`.
    Leaf(Vec<u32>),
    Split(u32, u32),
}

fn gap(r: [i64; 2], t: i64) -> i64 {
    if t < r[0] {
        r[0] - t
    } else if t > r[1] {
        t - r[1]
    } else {
        0
    }
}

fn point_distance(point: &Point, target: [i64; 6]) -> i64 {
    let mut d = point.offset * point.offset;
    for (r, t) in point.ranges.iter().zip(target) {
        let e = gap(*r, t);
        d += e * e;
    }
    d
}

impl Node {
    /// No point below is nearer than this.
    fn bound(&self, target: [i64; 6]) -> i64 {
        let mut d = self.min_offset * self.min_offset;
        for (r, t) in self.bounds.iter().zip(target) {
            let e = gap(*r, t);
            d += e * e;
        }
        d
    }
}

fn build(points: &[Point], mut ids: Vec<u32>, nodes: &mut Vec<Node>) -> u32 {
    let mut bounds = [[i64::MAX, i64::MIN]; 6];
    let mut min_offset = i64::MAX;
    for &i in &ids {
        let p = &points[i as usize];
        for (b, r) in bounds.iter_mut().zip(p.ranges) {
            b[0] = b[0].min(r[0]);
            b[1] = b[1].max(r[1]);
        }
        min_offset = min_offset.min(p.offset.abs());
    }
    let at = nodes.len() as u32;
    nodes.push(Node {
        bounds,
        min_offset,
        kind: NodeKind::Leaf(Vec::new()),
    });
    if ids.len() <= LEAF {
        nodes[at as usize].kind = NodeKind::Leaf(ids);
        return at;
    }
    // Split at the median centre along the value the centres spread most.
    let centre = |i: u32, k: usize| {
        let r = points[i as usize].ranges[k];
        r[0] + r[1]
    };
    let k = (0..6)
        .max_by_key(|&k| {
            let lo = ids.iter().map(|&i| centre(i, k)).min().unwrap_or(0);
            let hi = ids.iter().map(|&i| centre(i, k)).max().unwrap_or(0);
            hi - lo
        })
        .unwrap_or(0);
    ids.sort_by_key(|&i| (centre(i, k), i));
    let right = ids.split_off(ids.len() / 2);
    let a = build(points, ids, nodes);
    let b = build(points, right, nodes);
    nodes[at as usize].kind = NodeKind::Split(a, b);
    at
}

impl BiomeList {
    /// From `reports/biome_parameters/minecraft/<dimension>.json`.
    pub fn from_json(v: &Value) -> Self {
        let range = |p: &Value| -> [i64; 2] {
            match p {
                Value::Array(a) => [
                    quantize(a.first().and_then(Value::as_f64).unwrap_or(0.0)),
                    quantize(a.get(1).and_then(Value::as_f64).unwrap_or(0.0)),
                ],
                other => {
                    let q = quantize(other.as_f64().unwrap_or(0.0));
                    [q, q]
                }
            }
        };
        let points = v["biomes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| {
                let p = &b["parameters"];
                Point {
                    ranges: [
                        range(&p["temperature"]),
                        range(&p["humidity"]),
                        range(&p["continentalness"]),
                        range(&p["erosion"]),
                        range(&p["depth"]),
                        range(&p["weirdness"]),
                    ],
                    offset: quantize(p["offset"].as_f64().unwrap_or(0.0)),
                    biome: b["biome"].as_str().unwrap_or("minecraft:plains").to_owned(),
                }
            })
            .collect::<Vec<Point>>();
        let mut nodes = Vec::new();
        if !points.is_empty() {
            build(&points, (0..points.len() as u32).collect(), &mut nodes);
        }
        Self { points, nodes }
    }

    /// Distance from a climate to the nearest point of `biome`.
    pub fn distance_to(&self, target: [i64; 6], biome: &str) -> i64 {
        self.points
            .iter()
            .filter(|p| p.biome == biome)
            .map(|point| {
                let mut d = point.offset * point.offset;
                for (r, t) in point.ranges.iter().zip(target) {
                    let e = if t < r[0] {
                        r[0] - t
                    } else if t > r[1] {
                        t - r[1]
                    } else {
                        0
                    };
                    d += e * e;
                }
                d
            })
            .min()
            .unwrap_or(i64::MAX)
    }

    /// The nearest biome to a climate (quantized values); of points equally
    /// near, the first listed.
    pub fn find(&self, target: [i64; 6]) -> &str {
        if self.nodes.is_empty() {
            return "minecraft:plains";
        }
        // Neighbouring cells mostly share a biome: starting from the last
        // answer (on this thread) prunes nearly everything at once.
        thread_local! {
            static LAST: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        let last = LAST
            .with(std::cell::Cell::get)
            .min(self.points.len() as u32 - 1);
        let mut best = (point_distance(&self.points[last as usize], target), last);
        self.search(0, target, &mut best);
        LAST.with(|l| l.set(best.1));
        &self.points[best.1 as usize].biome
    }

    fn search(&self, node: u32, target: [i64; 6], best: &mut (i64, u32)) {
        match &self.nodes[node as usize].kind {
            NodeKind::Leaf(ids) => {
                for &i in ids {
                    let d = point_distance(&self.points[i as usize], target);
                    if (d, i) < *best {
                        *best = (d, i);
                    }
                }
            }
            &NodeKind::Split(a, b) => {
                let (da, db) = (
                    self.nodes[a as usize].bound(target),
                    self.nodes[b as usize].bound(target),
                );
                let order = if db < da {
                    [(b, db), (a, da)]
                } else {
                    [(a, da), (b, db)]
                };
                for (n, bound) in order {
                    if bound <= best.0 {
                        self.search(n, target, best);
                    }
                }
            }
        }
    }

    /// `find` the slow way, for checking it.
    #[cfg(test)]
    fn find_linear(&self, target: [i64; 6]) -> &str {
        let mut best = (i64::MAX, "minecraft:plains");
        for point in &self.points {
            let d = point_distance(point, target);
            if d < best.0 {
                best = (d, &point.biome);
            }
        }
        best.1
    }
}

/// The router functions climate reads.
#[derive(Debug)]
pub struct Climate {
    pub temperature: Df,
    pub vegetation: Df,
    pub continents: Df,
    pub erosion: Df,
    pub depth: Df,
    pub ridges: Df,
    /// None of them interpolates: inside a chunk they may use its column
    /// caches and give the same values.
    cacheable: bool,
}

impl Climate {
    pub fn from_router(builder: &mut Builder, router: &Value) -> Result<Self, String> {
        let mut c = Self {
            temperature: builder.build(&router["temperature"])?,
            vegetation: builder.build(&router["vegetation"])?,
            continents: builder.build(&router["continents"])?,
            erosion: builder.build(&router["erosion"])?,
            depth: builder.build(&router["depth"])?,
            ridges: builder.build(&router["ridges"])?,
            cacheable: false,
        };
        c.cacheable = !c.all().iter().any(|f| f.interpolates());
        Ok(c)
    }

    fn all(&self) -> [&Df; 6] {
        [
            &self.temperature,
            &self.vegetation,
            &self.continents,
            &self.erosion,
            &self.depth,
            &self.ridges,
        ]
    }

    /// [`Climate::sample`] inside a chunk: what does not depend on y is
    /// worked out once per column.
    pub fn sample_in(&self, qx: i32, qy: i32, qz: i32, ctx: &mut Ctx) -> [i64; 6] {
        if !self.cacheable {
            return self.sample(qx, qy, qz);
        }
        let (x, y, z) = (qx * 4, qy * 4, qz * 4);
        self.all().map(|f| quantize(f.eval_in(x, y, z, ctx)))
    }

    /// Climate at a quart position (4×4×4 biome cell), sampled at its
    /// lowest corner.
    pub fn sample(&self, qx: i32, qy: i32, qz: i32) -> [i64; 6] {
        let (x, y, z) = (qx * 4, qy * 4, qz * 4);
        [
            quantize(self.temperature.eval(x, y, z)),
            quantize(self.vegetation.eval(x, y, z)),
            quantize(self.continents.eval(x, y, z)),
            quantize(self.erosion.eval(x, y, z)),
            quantize(self.depth.eval(x, y, z)),
            quantize(self.ridges.eval(x, y, z)),
        ]
    }
}

/// Data pack files from an extracted data directory.
#[derive(Debug)]
pub struct DataDir(pub std::path::PathBuf);

impl crate::density::Source for DataDir {
    fn read(&self, kind: &str, id: &str) -> Option<Value> {
        let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
        let file = self
            .0
            .join("data")
            .join(ns)
            .join("worldgen")
            .join(kind)
            .join(format!("{path}.json"));
        let text = std::fs::read_to_string(file).ok()?;
        serde_json::from_str(&text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_finds_what_the_list_finds() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../cache/mcdata/26.3/reports/biome_parameters/minecraft/overworld.json"
        );
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let list = BiomeList::from_json(&serde_json::from_str(&text).unwrap());
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s % 30001) as i64 - 15000
        };
        for _ in 0..200_000 {
            let t = [next(), next(), next(), next(), next() / 3, next()];
            assert_eq!(list.find(t), list.find_linear(t), "{t:?}");
        }
    }
}
