//! Biomes from climate (minecraft.wiki, "Biome#Biome selection"): six
//! climate values per position (temperature, humidity, continentalness,
//! erosion, depth, weirdness), each biome given as ranges of them in the
//! biome parameter list; the biome whose ranges are nearest wins.

use serde_json::Value;

use crate::density::{Builder, Df};

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
            .collect();
        Self { points }
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

    /// The nearest biome to a climate (quantized values).
    pub fn find(&self, target: [i64; 6]) -> &str {
        let mut best = (i64::MAX, "minecraft:plains");
        for point in &self.points {
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
}

impl Climate {
    pub fn from_router(builder: &mut Builder, router: &Value) -> Result<Self, String> {
        Ok(Self {
            temperature: builder.build(&router["temperature"])?,
            vegetation: builder.build(&router["vegetation"])?,
            continents: builder.build(&router["continents"])?,
            erosion: builder.build(&router["erosion"])?,
            depth: builder.build(&router["depth"])?,
            ridges: builder.build(&router["ridges"])?,
        })
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
