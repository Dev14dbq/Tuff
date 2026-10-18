//! Material rules (minecraft.wiki, "Surface rule"; 26.3 moved them to
//! `worldgen/material_rule` and added ore veins): what the default block
//! (stone) becomes, decided per block by a tree of conditions: bedrock at
//! the bottom, ore veins, the biome's surface (grass, sand...), deepslate.
//!
//! Each column is walked from the top down, counting how deep the current
//! layer of solid blocks goes from above ("stone depth above") and how far
//! it is to the next opening below, and where the water above starts.

use std::sync::Arc;

use serde_json::Value;

use crate::density::{Builder, Ctx, Df, Source, full_id};
use crate::noise::FbmNoise;
use crate::random::{Positional, RandomSource};

#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    Absolute(i32),
    AboveBottom(i32),
    BelowTop(i32),
}

impl Anchor {
    fn of(v: &Value) -> Anchor {
        if let Some(a) = v["absolute"].as_i64() {
            Anchor::Absolute(a as i32)
        } else if let Some(a) = v["above_bottom"].as_i64() {
            Anchor::AboveBottom(a as i32)
        } else {
            Anchor::BelowTop(v["below_top"].as_i64().unwrap_or(0) as i32)
        }
    }

    fn y(self, min_y: i32, height: i32) -> i32 {
        match self {
            Anchor::Absolute(y) => y,
            Anchor::AboveBottom(d) => min_y + d,
            Anchor::BelowTop(d) => min_y + height - 1 - d,
        }
    }
}

#[derive(Debug)]
pub enum Condition {
    Biome(Vec<u32>),
    Noise {
        noise: Arc<FbmNoise>,
        min: f64,
        max: f64,
        /// Sampled at the block's height too, not just its column.
        is_3d: bool,
    },
    Gradient {
        random: Positional,
        below: Anchor,
        above: Anchor,
    },
    YAbove {
        anchor: Anchor,
        multiplier: i32,
        add_stone_depth: bool,
    },
    Water {
        offset: i32,
        multiplier: i32,
        add_stone_depth: bool,
    },
    StoneDepth {
        offset: i32,
        add_surface_depth: bool,
        secondary: i32,
        ceiling: bool,
    },
    Steep,
    Hole,
    AbovePreliminarySurface,
    Not(Box<Condition>),
}

#[derive(Debug)]
pub enum Rule {
    Block(u32),
    Sequence(Vec<Rule>),
    Condition(Condition, Box<Rule>),
    Bands,
    OreVein {
        density: Df,
        richness: Df,
        gap: Df,
        ore: u32,
        raw: u32,
        filler: u32,
        raw_chance: f32,
    },
}

/// What rules need from the game data.
pub trait Names {
    fn state(&self, v: &Value) -> Option<u32>;
    fn biome(&self, name: &str) -> Option<u32>;
}

/// Everything about the block being decided.
#[derive(Debug)]
pub struct Column<'a> {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub min_y: i32,
    pub height: i32,
    pub stone_above: i32,
    pub stone_below: i32,
    /// First y of the water above, or `i32::MIN` when there is none.
    pub water_height: i32,
    pub surface_depth: i32,
    pub secondary: f64,
    pub min_surface: i32,
    pub biome: u32,
    /// Heights (WORLD_SURFACE_WG) of this chunk's columns, for `steep`.
    pub heights: &'a [i32; 256],
}

/// The rules with their noises and randoms.
#[derive(Debug)]
pub struct Materials {
    pub rule: Rule,
    pub surface: Arc<FbmNoise>,
    pub secondary: Arc<FbmNoise>,
    pub bands_offset: Arc<FbmNoise>,
    pub bands: Vec<u32>,
    pub base: Positional,
    pub ore: Positional,
}

impl Materials {
    pub fn load(
        builder: &mut Builder,
        source: &dyn Source,
        id: &str,
        names: &dyn Names,
    ) -> Result<Self, String> {
        let base = builder.factory();
        let rule_json = source
            .read("material_rule", &full_id(id))
            .ok_or_else(|| format!("no material rule {id}"))?;
        let rule = parse_rule(builder, source, names, &rule_json)?;
        let surface = builder
            .noise("minecraft:surface")
            .ok_or("no surface noise")?;
        let secondary = builder
            .noise("minecraft:surface_secondary")
            .ok_or("no surface_secondary noise")?;
        let bands_offset = builder
            .noise("minecraft:clay_bands_offset")
            .ok_or("no clay_bands_offset noise")?;
        let mut band_random = base.from_hash_of("minecraft:clay_bands");
        let bands = make_bands(&mut band_random, names);
        let ore = base.from_hash_of("minecraft:ore").fork_positional();
        Ok(Self {
            rule,
            surface,
            secondary,
            bands_offset,
            bands,
            base,
            ore,
        })
    }

    /// Surface depth of a column: 3 or so, more where the surface noise is
    /// high.
    pub fn surface_depth(&self, x: i32, z: i32) -> i32 {
        let d = self.surface.value(f64::from(x), 0.0, f64::from(z));
        let mut r = self.base.at(x, 0, z);
        (d * 2.75 + 3.0 + r.next_double() * 0.25) as i32
    }

    pub fn secondary(&self, x: i32, z: i32) -> f64 {
        self.secondary.value(f64::from(x), 0.0, f64::from(z))
    }

    /// The block the default block at this place becomes, if any rule says.
    pub fn apply(&self, c: &Column, ctx: &mut Ctx) -> Option<u32> {
        self.run(&self.rule, c, ctx)
    }

    fn run(&self, rule: &Rule, c: &Column, ctx: &mut Ctx) -> Option<u32> {
        match rule {
            Rule::Block(s) => Some(*s),
            Rule::Sequence(list) => list.iter().find_map(|r| self.run(r, c, ctx)),
            Rule::Condition(cond, then) => {
                if self.test(cond, c) {
                    self.run(then, c, ctx)
                } else {
                    None
                }
            }
            Rule::Bands => {
                let off = (self.bands_offset.value(f64::from(c.x), 0.0, f64::from(c.z)) * 4.0)
                    .round() as i32;
                let n = self.bands.len() as i32;
                Some(self.bands[(c.y + off + n).rem_euclid(n) as usize])
            }
            Rule::OreVein {
                density,
                richness,
                gap,
                ore,
                raw,
                filler,
                raw_chance,
            } => {
                let solid = density.eval_in(c.x, c.y, c.z, ctx);
                if solid < 0.0 {
                    return None;
                }
                let mut r = self.ore.at(c.x, c.y, c.z);
                if f64::from(r.next_float()) > solid {
                    return None;
                }
                let rich = richness.eval_in(c.x, c.y, c.z, ctx);
                if f64::from(r.next_float()) < rich && gap.eval_in(c.x, c.y, c.z, ctx) < 0.0 {
                    Some(if r.next_float() < *raw_chance {
                        *raw
                    } else {
                        *ore
                    })
                } else {
                    Some(*filler)
                }
            }
        }
    }

    fn test(&self, cond: &Condition, c: &Column) -> bool {
        match cond {
            Condition::Biome(list) => list.contains(&c.biome),
            Condition::Noise {
                noise,
                min,
                max,
                is_3d,
            } => {
                let y = if *is_3d { f64::from(c.y) } else { 0.0 };
                let v = noise.value(f64::from(c.x), y, f64::from(c.z));
                v >= *min && v <= *max
            }
            Condition::Gradient {
                random,
                below,
                above,
            } => {
                let (lo, hi) = (below.y(c.min_y, c.height), above.y(c.min_y, c.height));
                if c.y <= lo {
                    return true;
                }
                if c.y >= hi {
                    return false;
                }
                let d = 1.0 + (f64::from(c.y) - f64::from(lo)) / f64::from(hi - lo) * (0.0 - 1.0);
                let mut r = random.at(c.x, c.y, c.z);
                f64::from(r.next_float()) < d
            }
            Condition::YAbove {
                anchor,
                multiplier,
                add_stone_depth,
            } => {
                let extra = if *add_stone_depth { c.stone_above } else { 0 };
                c.y + extra >= anchor.y(c.min_y, c.height) + c.surface_depth * multiplier
            }
            Condition::Water {
                offset,
                multiplier,
                add_stone_depth,
            } => {
                let extra = if *add_stone_depth { c.stone_above } else { 0 };
                c.water_height == i32::MIN
                    || c.y + extra >= c.water_height + offset + c.surface_depth * multiplier
            }
            Condition::StoneDepth {
                offset,
                add_surface_depth,
                secondary,
                ceiling,
            } => {
                let depth = if *ceiling {
                    c.stone_below
                } else {
                    c.stone_above
                };
                let surface = if *add_surface_depth {
                    c.surface_depth
                } else {
                    0
                };
                let extra = if *secondary == 0 {
                    0
                } else {
                    ((c.secondary + 1.0) / 2.0 * f64::from(*secondary)) as i32
                };
                depth <= 1 + offset + surface + extra
            }
            Condition::Steep => {
                let (i, j) = ((c.x & 15) as usize, (c.z & 15) as usize);
                let h = |x: usize, z: usize| c.heights[z * 16 + x];
                let (k, l) = (j.saturating_sub(1), (j + 1).min(15));
                if h(i, l) >= h(i, k) + 4 {
                    return true;
                }
                let (o, p) = (i.saturating_sub(1), (i + 1).min(15));
                h(o, j) >= h(p, j) + 4
            }
            Condition::Hole => c.surface_depth <= 0,
            Condition::AbovePreliminarySurface => c.y >= c.min_surface,
            Condition::Not(inner) => !self.test(inner, c),
        }
    }
}

fn parse_rule(
    builder: &mut Builder,
    source: &dyn Source,
    names: &dyn Names,
    v: &Value,
) -> Result<Rule, String> {
    if let Value::String(id) = v {
        let json = source
            .read("material_rule", &full_id(id))
            .ok_or_else(|| format!("no material rule {id}"))?;
        return parse_rule(builder, source, names, &json);
    }
    let kind = v["type"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches("minecraft:");
    Ok(match kind {
        "block" => Rule::Block(
            names
                .state(&v["result_state"])
                .ok_or("unknown block in rule")?,
        ),
        "sequence" => Rule::Sequence(
            v["sequence"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| parse_rule(builder, source, names, r))
                .collect::<Result<_, _>>()?,
        ),
        "condition" => Rule::Condition(
            parse_condition(builder, source, names, &v["if_true"])?,
            Box::new(parse_rule(builder, source, names, &v["then_run"])?),
        ),
        "bandlands" => Rule::Bands,
        "ore_vein" => {
            let block = |k: &str| names.state(&v[k]).ok_or(format!("unknown {k}"));
            Rule::OreVein {
                density: builder.build(&v["density"])?,
                richness: builder.build(&v["richness"])?,
                gap: builder.build(&v["filler_gap"])?,
                ore: block("ore_block")?,
                raw: block("raw_ore_block")?,
                filler: block("filler_block")?,
                raw_chance: v["raw_ore_chance"].as_f64().unwrap_or(0.02) as f32,
            }
        }
        other => return Err(format!("unknown material rule {other}")),
    })
}

fn parse_condition(
    builder: &mut Builder,
    source: &dyn Source,
    names: &dyn Names,
    v: &Value,
) -> Result<Condition, String> {
    if let Value::String(id) = v {
        let json = source
            .read("material_condition", &full_id(id))
            .ok_or_else(|| format!("no material condition {id}"))?;
        return parse_condition(builder, source, names, &json);
    }
    let kind = v["type"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches("minecraft:");
    let int = |k: &str| v[k].as_i64().unwrap_or(0) as i32;
    let flag = |k: &str| v[k].as_bool().unwrap_or(false);
    Ok(match kind {
        "biome" => Condition::Biome(match &v["biome_is"] {
            Value::String(s) => names.biome(s).into_iter().collect(),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .filter_map(|s| names.biome(s))
                .collect(),
            _ => Vec::new(),
        }),
        "noise_threshold" => Condition::Noise {
            noise: builder
                .noise(v["noise"].as_str().unwrap_or(""))
                .ok_or("unknown noise in rule")?,
            min: v["min_threshold"].as_f64().unwrap_or(f64::MIN),
            max: v["max_threshold"].as_f64().unwrap_or(f64::MAX),
            is_3d: flag("is_3d"),
        },
        "vertical_gradient" => Condition::Gradient {
            random: builder
                .factory()
                .from_hash_of(&full_id(v["random_name"].as_str().unwrap_or("")))
                .fork_positional(),
            below: Anchor::of(&v["true_at_and_below"]),
            above: Anchor::of(&v["false_at_and_above"]),
        },
        "y_above" => Condition::YAbove {
            anchor: Anchor::of(&v["anchor"]),
            multiplier: int("surface_depth_multiplier"),
            add_stone_depth: flag("add_stone_depth"),
        },
        "water" => Condition::Water {
            offset: int("offset"),
            multiplier: int("surface_depth_multiplier"),
            add_stone_depth: flag("add_stone_depth"),
        },
        "stone_depth" => Condition::StoneDepth {
            offset: int("offset"),
            add_surface_depth: flag("add_surface_depth"),
            secondary: int("secondary_depth_range"),
            ceiling: v["surface_type"].as_str() == Some("ceiling"),
        },
        "steep" => Condition::Steep,
        "hole" => Condition::Hole,
        "above_preliminary_surface" => Condition::AbovePreliminarySurface,
        "not" => Condition::Not(Box::new(parse_condition(
            builder,
            source,
            names,
            &v["invert"],
        )?)),
        other => return Err(format!("unknown material condition {other}")),
    })
}

/// Badlands terracotta bands (192 of them, from the `clay_bands` random).
fn make_bands(random: &mut dyn RandomSource, names: &dyn Names) -> Vec<u32> {
    let st = |n: &str| {
        names
            .state(&Value::String(format!("minecraft:{n}")))
            .unwrap_or(0)
    };
    let (terracotta, orange, yellow, brown, red, white, light_gray) = (
        st("terracotta"),
        st("orange_terracotta"),
        st("yellow_terracotta"),
        st("brown_terracotta"),
        st("red_terracotta"),
        st("white_terracotta"),
        st("light_gray_terracotta"),
    );
    let mut bands = vec![terracotta; 192];
    let mut i = 0usize;
    while i < bands.len() {
        i += random.next_int_bound(5) as usize + 1;
        if i < bands.len() {
            bands[i] = orange;
        }
        i += 1;
    }
    let make = |random: &mut dyn RandomSource, bands: &mut Vec<u32>, min_width: i32, state: u32| {
        let count = random.next_int_between(6, 15);
        for _ in 0..count {
            let width = min_width + random.next_int_bound(3);
            let start = random.next_int_bound(bands.len() as i32) as usize;
            let mut m = 0;
            while start + m < bands.len() && (m as i32) < width {
                bands[start + m] = state;
                m += 1;
            }
        }
    };
    make(random, &mut bands, 1, yellow);
    make(random, &mut bands, 2, brown);
    make(random, &mut bands, 1, red);
    let whites = random.next_int_between(9, 15);
    let mut placed = 0;
    let mut l = 0usize;
    while placed < whites && l < bands.len() {
        bands[l] = white;
        if l > 1 && random.next_bool() {
            bands[l - 1] = light_gray;
        }
        if l + 1 < bands.len() && random.next_bool() {
            bands[l + 1] = light_gray;
        }
        placed += 1;
        l += random.next_int_bound(16) as usize + 4;
    }
    bands
}

/// The biome zoom (minecraft.wiki, "Biome#Biome zoom"): which of the eight
/// nearby 4×4×4 cells a block takes its biome from, chosen by a seeded
/// jitter of the cell corners.
pub fn zoom_seed(seed: i64) -> i64 {
    let d = crate::sha256::sha256(&seed.to_le_bytes());
    i64::from_le_bytes(d[0..8].try_into().expect("8 bytes"))
}

fn lcg(seed: i64, add: i64) -> i64 {
    let s = seed.wrapping_mul(
        seed.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407),
    );
    s.wrapping_add(add)
}

fn fiddle(l: i64) -> f64 {
    let d = ((l >> 24).rem_euclid(1024)) as f64 / 1024.0;
    (d - 0.5) * 0.9
}

/// The cell (quart coordinates) whose biome the block at `x, y, z` shows.
pub fn zoomed_cell(seed: i64, x: i32, y: i32, z: i32) -> (i32, i32, i32) {
    let (i, j, k) = (x - 2, y - 2, z - 2);
    let (l, m, n) = (i >> 2, j >> 2, k >> 2);
    let (d, e, f) = (
        f64::from(i & 3) / 4.0,
        f64::from(j & 3) / 4.0,
        f64::from(k & 3) / 4.0,
    );
    let mut best = (0, f64::INFINITY);
    for p in 0..8 {
        let (bx, by, bz) = (p & 4 == 0, p & 2 == 0, p & 1 == 0);
        let q = if bx { l } else { l + 1 };
        let r = if by { m } else { m + 1 };
        let s = if bz { n } else { n + 1 };
        let h = if bx { d } else { d - 1.0 };
        let t = if by { e } else { e - 1.0 };
        let u = if bz { f } else { f - 1.0 };
        let mut a = lcg(seed, i64::from(q));
        a = lcg(a, i64::from(r));
        a = lcg(a, i64::from(s));
        a = lcg(a, i64::from(q));
        a = lcg(a, i64::from(r));
        a = lcg(a, i64::from(s));
        let fx = fiddle(a);
        a = lcg(a, seed);
        let fy = fiddle(a);
        a = lcg(a, seed);
        let fz = fiddle(a);
        let v = (u + fz) * (u + fz) + (t + fy) * (t + fy) + (h + fx) * (h + fx);
        if best.1 > v {
            best = (p, v);
        }
    }
    let p = best.0;
    (
        if p & 4 == 0 { l } else { l + 1 },
        if p & 2 == 0 { m } else { m + 1 },
        if p & 1 == 0 { n } else { n + 1 },
    )
}
