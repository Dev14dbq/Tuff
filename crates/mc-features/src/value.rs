//! Numbers drawn per placement: int, float and height providers, and
//! vertical anchors (minecraft.wiki, "Configured feature" and "Placed
//! feature": `uniform`, `biased_to_bottom`, `clamped`, `clamped_normal`,
//! `weighted_list`, `trapezoid`; heights relative to the world's bottom,
//! top or sea level).

use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::rand::WorldgenRandom;

/// The world's vertical extent, for anchors.
#[derive(Clone, Copy, Debug)]
pub struct Heights {
    pub min_y: i32,
    pub height: i32,
    pub sea_level: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
    SeaLevel(i32),
}

impl Anchor {
    pub fn parse(v: &Value) -> Option<Self> {
        let get = |k: &str| v.get(k).and_then(Value::as_i64).map(|n| n as i32);
        if let Some(n) = v.as_i64() {
            return Some(Anchor::Absolute(n as i32));
        }
        get("absolute")
            .map(Anchor::Absolute)
            .or_else(|| get("above_bottom").map(Anchor::AboveBottom))
            .or_else(|| get("below_top").map(Anchor::BelowTop))
            .or_else(|| get("relative_to_sea_level").map(Anchor::SeaLevel))
    }

    pub fn resolve(self, h: Heights) -> i32 {
        match self {
            Anchor::Absolute(y) => y,
            Anchor::AboveBottom(n) => h.min_y + n,
            Anchor::BelowTop(n) => h.min_y + h.height - 1 - n,
            Anchor::SeaLevel(n) => h.sea_level + n,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum IntProvider {
    Constant(i32),
    Uniform(i32, i32),
    BiasedToBottom(i32, i32),
    Clamped(Box<IntProvider>, i32, i32),
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: i32,
        max: i32,
    },
    Weighted(Vec<(IntProvider, i32)>),
    Trapezoid {
        min: i32,
        max: i32,
        plateau: i32,
    },
}

fn int(v: &Value, k: &str) -> Option<i32> {
    v.get(k).and_then(Value::as_i64).map(|n| n as i32)
}

fn float(v: &Value, k: &str) -> Option<f32> {
    v.get(k).and_then(Value::as_f64).map(|n| n as f32)
}

impl IntProvider {
    pub fn parse(v: &Value) -> Option<Self> {
        if let Some(n) = v.as_i64() {
            return Some(IntProvider::Constant(n as i32));
        }
        let ty = v["type"].as_str()?.trim_start_matches("minecraft:");
        Some(match ty {
            "constant" => IntProvider::Constant(int(v, "value")?),
            "uniform" => IntProvider::Uniform(int(v, "min_inclusive")?, int(v, "max_inclusive")?),
            "biased_to_bottom" => {
                IntProvider::BiasedToBottom(int(v, "min_inclusive")?, int(v, "max_inclusive")?)
            }
            "clamped" => IntProvider::Clamped(
                Box::new(IntProvider::parse(&v["source"])?),
                int(v, "min_inclusive")?,
                int(v, "max_inclusive")?,
            ),
            "clamped_normal" => IntProvider::ClampedNormal {
                mean: float(v, "mean")?,
                deviation: float(v, "deviation")?,
                min: int(v, "min_inclusive")?,
                max: int(v, "max_inclusive")?,
            },
            "weighted_list" => IntProvider::Weighted(
                v["distribution"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| Some((IntProvider::parse(&e["data"])?, int(e, "weight")?)))
                    .collect(),
            ),
            "trapezoid" => IntProvider::Trapezoid {
                min: int(v, "min")?,
                max: int(v, "max")?,
                plateau: int(v, "plateau").unwrap_or(0),
            },
            _ => return None,
        })
    }

    pub fn sample(&self, r: &mut WorldgenRandom) -> i32 {
        match self {
            IntProvider::Constant(n) => *n,
            IntProvider::Uniform(min, max) => r.next_int_bound(max - min + 1) + min,
            IntProvider::BiasedToBottom(min, max) => {
                let k = r.next_int_bound(max - min + 1) + 1;
                min + r.next_int_bound(k)
            }
            IntProvider::Clamped(source, min, max) => source.sample(r).clamp(*min, *max),
            IntProvider::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => {
                let v = mean + r.next_gaussian() as f32 * deviation;
                v.clamp(*min as f32, *max as f32) as i32
            }
            IntProvider::Weighted(list) => match weighted(list, r) {
                Some(p) => p.sample(r),
                None => 0,
            },
            IntProvider::Trapezoid { min, max, plateau } => {
                let span = max - min;
                let a = (span - plateau) / 2;
                let b = span - a;
                // As `nextInt(n) - nextInt(n)` for a symmetric spread.
                let u = r.next_int_bound(b + 1);
                let v = r.next_int_bound(a + 1);
                min + a + u - v
            }
        }
    }

    pub fn max(&self) -> i32 {
        match self {
            IntProvider::Constant(n) => *n,
            IntProvider::Uniform(_, max) | IntProvider::BiasedToBottom(_, max) => *max,
            IntProvider::Clamped(_, _, max) | IntProvider::ClampedNormal { max, .. } => *max,
            IntProvider::Weighted(list) => list.iter().map(|(p, _)| p.max()).max().unwrap_or(0),
            IntProvider::Trapezoid { max, .. } => *max,
        }
    }
}

/// An entry by weight (`WeightedRandom.getRandomItem`).
pub fn weighted<'a, T>(list: &'a [(T, i32)], r: &mut WorldgenRandom) -> Option<&'a T> {
    let total: i32 = list.iter().map(|(_, w)| *w).sum();
    if total <= 0 {
        return None;
    }
    let mut pick = r.next_int_bound(total);
    for (item, w) in list {
        pick -= w;
        if pick < 0 {
            return Some(item);
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub enum FloatProvider {
    Constant(f32),
    Uniform(f32, f32),
    ClampedNormal {
        mean: f32,
        deviation: f32,
        min: f32,
        max: f32,
    },
    Trapezoid {
        min: f32,
        max: f32,
        plateau: f32,
    },
}

impl FloatProvider {
    pub fn parse(v: &Value) -> Option<Self> {
        if let Some(n) = v.as_f64() {
            return Some(FloatProvider::Constant(n as f32));
        }
        let ty = v["type"].as_str()?.trim_start_matches("minecraft:");
        Some(match ty {
            "constant" => FloatProvider::Constant(float(v, "value")?),
            "uniform" => {
                FloatProvider::Uniform(float(v, "min_inclusive")?, float(v, "max_exclusive")?)
            }
            "clamped_normal" => FloatProvider::ClampedNormal {
                mean: float(v, "mean")?,
                deviation: float(v, "deviation")?,
                min: float(v, "min")?,
                max: float(v, "max")?,
            },
            "trapezoid" => FloatProvider::Trapezoid {
                min: float(v, "min")?,
                max: float(v, "max")?,
                plateau: float(v, "plateau").unwrap_or(0.0),
            },
            _ => return None,
        })
    }

    pub fn sample(&self, r: &mut WorldgenRandom) -> f32 {
        match *self {
            FloatProvider::Constant(n) => n,
            FloatProvider::Uniform(min, max) => r.next_float() * (max - min) + min,
            FloatProvider::ClampedNormal {
                mean,
                deviation,
                min,
                max,
            } => (mean + r.next_gaussian() as f32 * deviation).clamp(min, max),
            FloatProvider::Trapezoid { min, max, plateau } => {
                let span = max - min;
                let a = (span - plateau) / 2.0;
                let b = span - a;
                min + r.next_float() * b + r.next_float() * a
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum HeightProvider {
    Constant(Anchor),
    Uniform(Anchor, Anchor),
    BiasedToBottom(Anchor, Anchor, i32),
    VeryBiasedToBottom(Anchor, Anchor, i32),
    Trapezoid(Anchor, Anchor, i32),
    Weighted(Vec<(HeightProvider, i32)>),
}

impl HeightProvider {
    pub fn parse(v: &Value) -> Option<Self> {
        if let Some(a) = Anchor::parse(v)
            && v.get("type").is_none()
        {
            return Some(HeightProvider::Constant(a));
        }
        let ty = v["type"].as_str()?.trim_start_matches("minecraft:");
        let range = || {
            Some((
                Anchor::parse(&v["min_inclusive"])?,
                Anchor::parse(&v["max_inclusive"])?,
            ))
        };
        Some(match ty {
            "constant" => HeightProvider::Constant(Anchor::parse(&v["value"])?),
            "uniform" => {
                let (a, b) = range()?;
                HeightProvider::Uniform(a, b)
            }
            "biased_to_bottom" => {
                let (a, b) = range()?;
                HeightProvider::BiasedToBottom(a, b, int(v, "inner").unwrap_or(1))
            }
            "very_biased_to_bottom" => {
                let (a, b) = range()?;
                HeightProvider::VeryBiasedToBottom(a, b, int(v, "inner").unwrap_or(1))
            }
            "trapezoid" => {
                let (a, b) = range()?;
                HeightProvider::Trapezoid(a, b, int(v, "plateau").unwrap_or(0))
            }
            "weighted_list" => HeightProvider::Weighted(
                v["distribution"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| Some((HeightProvider::parse(&e["data"])?, int(e, "weight")?)))
                    .collect(),
            ),
            _ => return None,
        })
    }

    pub fn sample(&self, r: &mut WorldgenRandom, h: Heights) -> i32 {
        match self {
            HeightProvider::Constant(a) => a.resolve(h),
            HeightProvider::Uniform(a, b) => {
                let (min, max) = (a.resolve(h), b.resolve(h));
                if min > max {
                    min
                } else {
                    r.next_int_bound(max - min + 1) + min
                }
            }
            HeightProvider::BiasedToBottom(a, b, inner) => {
                let (min, max) = (a.resolve(h), b.resolve(h));
                if max - min - inner < 0 {
                    return min;
                }
                let k = r.next_int_bound(max - min - inner + 1);
                r.next_int_bound(k + inner) + min
            }
            HeightProvider::VeryBiasedToBottom(a, b, inner) => {
                let (min, max) = (a.resolve(h), b.resolve(h));
                if max - min - inner < 0 {
                    return min;
                }
                let k = r.int_between(min + inner, max);
                let l = r.int_between(min, k - 1);
                r.int_between(min, l - 1 + inner)
            }
            HeightProvider::Trapezoid(a, b, plateau) => {
                let (min, max) = (a.resolve(h), b.resolve(h));
                if min > max {
                    return min;
                }
                let span = max - min;
                if *plateau >= span {
                    return r.int_between(min, max);
                }
                let k = (span - plateau) / 2;
                let l = span - k;
                min + r.int_between(0, l) + r.int_between(0, k)
            }
            HeightProvider::Weighted(list) => match weighted(list, r) {
                Some(p) => p.sample(r, h),
                None => h.min_y,
            },
        }
    }
}
