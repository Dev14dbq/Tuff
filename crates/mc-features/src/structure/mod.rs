//! Structures (minecraft.wiki, "Structure", "Structure set"): where they
//! start (a spread grid per structure set, seeded from the world seed and
//! the set's salt), what they are made of (jigsaw pieces, or a type's own
//! pieces), placing the part of each piece that lies in a chunk during
//! decoration, and the terrain shaped around them (the beardifier).

pub mod built;
pub mod end_city;
pub mod fortress;
pub mod jigsaw;
pub mod legacy;
pub mod mineshaft;
pub mod observed;
pub mod stronghold;
pub mod template;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::Facts;
use crate::feature::Loader;
use crate::rand::WorldgenRandom;
use crate::registry::Features;
use crate::value::{HeightProvider, Heights};
use crate::{P, Region};
use jigsaw::Pool;
use template::{BBox, Template, Templates};

/// How terrain is shaped around a structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapt {
    None,
    Bury,
    BeardThin,
    BeardBox,
    Encapsulate,
}

#[derive(Debug)]
pub enum Kind {
    Jigsaw {
        start_pool: String,
        /// The jigsaw of the start piece that sits at the start position.
        start_jigsaw: Option<String>,
        size: i32,
        start_height: HeightProvider,
        project: Option<bool>,
        max_distance: i32,
        expansion_hack: bool,
    },
    /// A structure type with its own pieces (temples, mineshafts...).
    Legacy(String, Value),
}

#[derive(Debug)]
pub struct Structure {
    pub id: String,
    pub kind: Kind,
    pub biomes: HashSet<u32>,
    pub step: usize,
    pub adapt: Adapt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reduction {
    Default,
    Legacy1,
    Legacy2,
    Legacy3,
}

#[derive(Debug)]
enum Placement {
    Spread {
        spacing: i32,
        separation: i32,
        salt: i32,
        triangular: bool,
        frequency: f32,
        reduction: Reduction,
        exclusion: Option<(String, i32)>,
    },
    /// Strongholds: rings around the world's centre.
    Rings {
        distance: i32,
        spread: i32,
        count: i32,
        preferred: HashSet<u32>,
    },
}

#[derive(Debug)]
struct Set {
    id: String,
    structures: Vec<(usize, i32)>,
    placement: Placement,
    /// Added to a start chunk's corner where `/locate` points.
    locate_offset: (i32, i32, i32),
}

/// A structure that starts in a chunk, with its pieces.
#[derive(Debug)]
pub struct Start {
    pub structure: usize,
    pub chunk: (i32, i32),
    pub pieces: Vec<Piece>,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub enum Piece {
    Jigsaw(jigsaw::Piece),
    Legacy(legacy::Piece),
}

impl Piece {
    pub fn bbox(&self) -> BBox {
        match self {
            Piece::Jigsaw(p) => p.bbox,
            Piece::Legacy(p) => p.bbox,
        }
    }
}

/// What starting a structure needs from the generator.
pub trait Ground {
    /// First free y above the column: the sea counts unless `ocean_floor`.
    fn height(&mut self, x: i32, z: i32, ocean_floor: bool) -> i32;
    /// The biome of a 4×4×4 cell.
    fn biome(&mut self, qx: i32, qy: i32, qz: i32) -> u32;
    /// Whether the bare terrain (stone, sea or air from the noise) has
    /// something at `y` that the heightmap counts: anything but air, or
    /// only what blocks motion for `ocean_floor`.
    fn occupied(&mut self, x: i32, y: i32, z: i32, ocean_floor: bool) -> bool {
        y < self.height(x, z, ocean_floor)
    }
}

/// What placing structure pieces needs.
#[derive(Clone, Copy, Debug)]
pub struct Ctx2<'a> {
    pub structures: &'a Structures,
    pub features: &'a Features,
}

impl Ctx2<'_> {
    pub fn features_ctx(&self) -> crate::feature::Ctx<'_> {
        crate::feature::Ctx {
            facts: &self.features.facts,
            features: self.features,
        }
    }
}

pub struct Structures {
    facts: Arc<Facts>,
    template_root: PathBuf,
    templates: Templates,
    pools: HashMap<String, Arc<Pool>>,
    pub structures: Vec<Structure>,
    sets: Vec<Set>,
    /// Per step: the structures in it, in registry order.
    pub by_step: Vec<Vec<usize>>,
    /// Biomes cold enough for snowy ruined portals.
    pub cold_biomes: HashSet<u32>,
    /// Stronghold positions per set, worked out once.
    rings: Mutex<HashMap<String, Vec<(i32, i32)>>>,
    /// The biomes around a monument (ocean or river); the middle is checked
    /// as for any structure.
    pub monument_biomes: HashSet<u32>,
    observed_dir: PathBuf,
    observed: std::sync::OnceLock<observed::Observed>,
}

impl std::fmt::Debug for Structures {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Structures")
            .field("structures", &self.structures.len())
            .finish_non_exhaustive()
    }
}

/// A JSON value as NBT (for `append_static` and the like).
pub fn json_to_nbt(v: &Value) -> mc_nbt::Tag {
    use mc_nbt::Tag;
    match v {
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64() {
            Some(i) if i32::try_from(i).is_ok() => Tag::Int(i as i32),
            Some(i) => Tag::Long(i),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(a) => Tag::List(a.iter().map(json_to_nbt).collect()),
        Value::Object(o) => {
            let mut c = mc_nbt::Compound::new();
            for (k, v) in o {
                c.insert(k.as_str(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
        Value::Null => Tag::Byte(0),
    }
}

const STEPS: [&str; 11] = [
    "raw_generation",
    "lakes",
    "local_modifications",
    "underground_structures",
    "surface_structures",
    "strongholds",
    "underground_ores",
    "underground_decoration",
    "fluid_springs",
    "vegetal_decoration",
    "top_layer_modification",
];

fn read_dir_json(dir: &Path) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Value)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if p.extension().is_some_and(|x| x == "json")
                && let Ok(text) = std::fs::read_to_string(&p)
                && let Ok(v) = serde_json::from_str(&text)
            {
                let rel = p.strip_prefix(base).unwrap_or(&p).with_extension("");
                let id = rel.to_string_lossy().replace('\\', "/");
                out.push((format!("minecraft:{id}"), v));
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Biome ids of a holder set: an id, a list, or a tag (`#...`).
fn biome_set(v: &Value, tags: &Path, ids: &HashMap<String, u32>, depth: usize) -> HashSet<u32> {
    let mut out = HashSet::new();
    let add = |name: &str, out: &mut HashSet<u32>| {
        if let Some(tag) = name.strip_prefix('#') {
            if depth > 8 {
                return;
            }
            let path = tags.join(format!("{}.json", tag.trim_start_matches("minecraft:")));
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(t) = serde_json::from_str::<Value>(&text)
            {
                for e in t["values"].as_array().into_iter().flatten() {
                    let n = e.as_str().or_else(|| e["id"].as_str()).unwrap_or("");
                    out.extend(biome_set(
                        &Value::String(n.to_owned()),
                        tags,
                        ids,
                        depth + 1,
                    ));
                }
            }
        } else if let Some(&id) = ids.get(name) {
            out.insert(id);
        }
    };
    match v {
        Value::String(s) => add(s, &mut out),
        Value::Array(a) => {
            for e in a {
                if let Some(s) = e.as_str() {
                    add(s, &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

impl Structures {
    pub fn load(
        worldgen: &Path,
        facts: Arc<Facts>,
        loader: &Loader,
        biome_ids: &HashMap<String, u32>,
    ) -> Self {
        let data = worldgen.parent().unwrap_or(worldgen).to_owned();
        let tags = data.join("tags/worldgen/biome");
        let lists: HashMap<String, Value> = read_dir_json(&worldgen.join("processor_list"))
            .into_iter()
            .map(|(k, v)| (k.trim_start_matches("minecraft:").to_owned(), v))
            .collect();
        let pools: HashMap<String, Arc<Pool>> = read_dir_json(&worldgen.join("template_pool"))
            .into_iter()
            .map(|(k, v)| (k, Arc::new(Pool::parse(&v, loader, &lists))))
            .collect();
        let mut structures = Vec::new();
        let mut index = HashMap::new();
        for (id, v) in read_dir_json(&worldgen.join("structure")) {
            let ty = v["type"]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("minecraft:")
                .to_owned();
            let kind = if ty == "jigsaw" {
                Kind::Jigsaw {
                    start_pool: v["start_pool"].as_str().unwrap_or("").to_owned(),
                    start_jigsaw: v["start_jigsaw_name"].as_str().map(str::to_owned),
                    size: v["size"].as_i64().unwrap_or(1) as i32,
                    start_height: HeightProvider::parse(&v["start_height"])
                        .unwrap_or(HeightProvider::Constant(crate::value::Anchor::Absolute(0))),
                    project: v["project_start_to_heightmap"]
                        .as_str()
                        .map(|h| h == "OCEAN_FLOOR_WG"),
                    max_distance: v["max_distance_from_center"].as_i64().unwrap_or(80) as i32,
                    expansion_hack: v["use_expansion_hack"].as_bool().unwrap_or(false),
                }
            } else {
                Kind::Legacy(ty, v.clone())
            };
            let step = STEPS
                .iter()
                .position(|s| Some(*s) == v["step"].as_str())
                .unwrap_or(4);
            let adapt = match v["terrain_adaptation"].as_str() {
                Some("bury") => Adapt::Bury,
                Some("beard_thin") => Adapt::BeardThin,
                Some("beard_box") => Adapt::BeardBox,
                Some("encapsulate") => Adapt::Encapsulate,
                _ => Adapt::None,
            };
            index.insert(id.clone(), structures.len());
            structures.push(Structure {
                id,
                kind,
                biomes: biome_set(&v["biomes"], &tags, biome_ids, 0),
                step,
                adapt,
            });
        }
        let mut by_step = vec![Vec::new(); STEPS.len()];
        for (i, s) in structures.iter().enumerate() {
            by_step[s.step].push(i);
        }
        let mut sets = Vec::new();
        for (id, v) in read_dir_json(&worldgen.join("structure_set")) {
            let list: Vec<(usize, i32)> = v["structures"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    let s = e["structure"].as_str()?;
                    Some((*index.get(s)?, e["weight"].as_i64().unwrap_or(1) as i32))
                })
                .collect();
            let p = &v["placement"];
            let salt = p["salt"].as_i64().unwrap_or(0) as i32;
            let placement = if p["type"].as_str() == Some("minecraft:concentric_rings") {
                Placement::Rings {
                    distance: p["distance"].as_i64().unwrap_or(32) as i32,
                    spread: p["spread"].as_i64().unwrap_or(3) as i32,
                    count: p["count"].as_i64().unwrap_or(128) as i32,
                    preferred: biome_set(&p["preferred_biomes"], &tags, biome_ids, 0),
                }
            } else {
                Placement::Spread {
                    spacing: p["spacing"].as_i64().unwrap_or(32) as i32,
                    separation: p["separation"].as_i64().unwrap_or(8) as i32,
                    salt,
                    triangular: p["spread_type"].as_str() == Some("triangular"),
                    frequency: p["frequency"].as_f64().unwrap_or(1.0) as f32,
                    reduction: match p["frequency_reduction_method"].as_str() {
                        Some("legacy_type_1") => Reduction::Legacy1,
                        Some("legacy_type_2") => Reduction::Legacy2,
                        Some("legacy_type_3") => Reduction::Legacy3,
                        _ => Reduction::Default,
                    },
                    exclusion: p.get("exclusion_zone").map(|e| {
                        (
                            e["other_set"].as_str().unwrap_or("").to_owned(),
                            e["chunk_count"].as_i64().unwrap_or(0) as i32,
                        )
                    }),
                }
            };
            let off = |i: usize| p["locate_offset"][i].as_i64().unwrap_or(0) as i32;
            sets.push(Set {
                id,
                structures: list,
                placement,
                locate_offset: (off(0), off(1), off(2)),
            });
        }
        let monument_biomes = biome_set(
            &Value::String("#minecraft:required_ocean_monument_surrounding".into()),
            &tags,
            biome_ids,
            0,
        );
        let cold_biomes = read_dir_json(&worldgen.join("biome"))
            .into_iter()
            .filter(|(_, v)| v["temperature"].as_f64().unwrap_or(0.5) < 0.15)
            .filter_map(|(id, _)| biome_ids.get(&id).copied())
            .collect();
        Self {
            facts,
            cold_biomes,
            template_root: data.join("structure"),
            templates: Templates::default(),
            pools,
            structures,
            sets,
            by_step,
            rings: Mutex::new(HashMap::new()),
            monument_biomes,
            observed_dir: data
                .parent()
                .and_then(Path::parent)
                .map(|d| d.join("observed"))
                .unwrap_or_default(),
            observed: std::sync::OnceLock::new(),
        }
    }

    /// The monuments and mansions a vanilla server was seen building,
    /// read the first time one is needed.
    pub fn observed(&self) -> &observed::Observed {
        self.observed.get_or_init(|| {
            let o = observed::Observed::load(&self.observed_dir, &self.facts);
            if o.monuments.is_empty() || o.mansions.is_empty() {
                eprintln!(
                    "no recorded monuments or mansions in {} (cargo xtask observe-structures); \
                     they will be missing",
                    self.observed_dir.display()
                );
            }
            o
        })
    }

    pub fn facts(&self) -> &Facts {
        &self.facts
    }

    pub fn template(&self, id: &str) -> Option<Arc<Template>> {
        self.templates.get(&self.template_root, &self.facts, id)
    }

    pub fn pool(&self, id: &str) -> Option<Arc<Pool>> {
        let key = if id.contains(':') {
            id.to_owned()
        } else {
            format!("minecraft:{id}")
        };
        if key == "minecraft:empty" {
            return Some(Arc::new(Pool {
                templates: Vec::new(),
                fallback: "minecraft:empty".into(),
            }));
        }
        self.pools.get(&key).cloned()
    }

    fn spread_chunk(
        seed: i64,
        x: i32,
        z: i32,
        spacing: i32,
        separation: i32,
        salt: i32,
        triangular: bool,
    ) -> (i32, i32) {
        let i = x.div_euclid(spacing);
        let j = z.div_euclid(spacing);
        let mut r = WorldgenRandom::legacy(0);
        r.set_large_feature_with_salt(seed, i, j, salt);
        let k = spacing - separation;
        let ev = |r: &mut WorldgenRandom| {
            if triangular {
                (r.next_int_bound(k) + r.next_int_bound(k)) / 2
            } else {
                r.next_int_bound(k)
            }
        };
        let l = ev(&mut r);
        let m = ev(&mut r);
        (i * spacing + l, j * spacing + m)
    }

    fn is_placement(&self, set: &Set, seed: i64, x: i32, z: i32) -> bool {
        match &set.placement {
            Placement::Spread {
                spacing,
                separation,
                salt,
                triangular,
                ..
            } => {
                Self::spread_chunk(seed, x, z, *spacing, *separation, *salt, *triangular) == (x, z)
            }
            Placement::Rings { .. } => self
                .rings
                .lock()
                .expect("rings lock")
                .get(&set.id)
                .is_some_and(|list| list.contains(&(x, z))),
        }
    }

    fn is_structure_chunk(&self, set: &Set, seed: i64, x: i32, z: i32) -> bool {
        if !self.is_placement(set, seed, x, z) {
            return false;
        }
        if let Placement::Spread {
            salt,
            frequency,
            reduction,
            exclusion,
            ..
        } = &set.placement
        {
            if *frequency < 1.0 && !reduce(*reduction, seed, *salt, x, z, *frequency) {
                return false;
            }
            if let Some((other, count)) = exclusion
                && let Some(o) = self.sets.iter().find(|s| &s.id == other)
            {
                for dx in -count..=*count {
                    for dz in -count..=*count {
                        if self.is_placement(o, seed, x + dx, z + dz) {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// The stronghold ring positions (chunk coordinates), from the world
    /// seed; each moved to a nearby preferred biome when there is one.
    /// Places the ring structures (strongholds). `biome` is the noise
    /// biome of a 4×4×4 cell; the searches run on several threads.
    pub fn prepare_rings(&self, seed: i64, biome: &(dyn Fn(i32, i32, i32) -> u32 + Sync)) {
        for set in &self.sets {
            let Placement::Rings {
                distance,
                spread,
                count,
                ref preferred,
            } = set.placement
            else {
                continue;
            };
            if self.rings.lock().expect("rings lock").contains_key(&set.id) {
                continue;
            }
            // Where each one would go, and the seed of its search: the
            // searches draw from their own forks, so they can run apart.
            let mut r = WorldgenRandom::legacy(seed);
            let mut angle = r.next_double() * std::f64::consts::PI * 2.0;
            let mut spots = Vec::new();
            let (mut in_ring, mut ring, mut per_ring) = (0, 0, spread);
            for n in 0..count {
                let dist = f64::from(4 * distance + distance * ring * 6)
                    + (r.next_double() - 0.5) * (f64::from(distance) * 2.5);
                let cx = (angle.cos() * dist).round() as i32;
                let cz = (angle.sin() * dist).round() as i32;
                spots.push((cx, cz, r.next_long()));
                angle += std::f64::consts::PI * 2.0 / f64::from(per_ring);
                in_ring += 1;
                if in_ring == per_ring {
                    in_ring = 0;
                    ring += 1;
                    per_ring += 2 * per_ring / (ring + 1);
                    per_ring = per_ring.min(count - n);
                    angle += r.next_double() * std::f64::consts::PI * 2.0;
                }
            }
            // Moved to a preferred biome within 112 blocks, picked at random
            // among those there.
            let search = |&(cx, cz, fork_seed): &(i32, i32, i64)| {
                let mut fork = WorldgenRandom::legacy(fork_seed);
                let (qx, qz) = ((cx * 16 + 8) >> 2, (cz * 16 + 8) >> 2);
                let k = 112 >> 2;
                let mut found: Option<(i32, i32)> = None;
                let mut m = 0;
                for p in -k..=k {
                    for q in -k..=k {
                        let (bx, bz) = (qx + q, qz + p);
                        if !preferred.contains(&biome(bx, 0, bz)) {
                            continue;
                        }
                        if found.is_none() || fork.next_int_bound(m + 1) == 0 {
                            found = Some(((bx << 2) >> 4, (bz << 2) >> 4));
                        }
                        m += 1;
                    }
                }
                found.unwrap_or((cx, cz))
            };
            let threads = crate::worker_threads().min(spots.len().max(1));
            let per = spots.len().div_ceil(threads).max(1);
            let list: Vec<(i32, i32)> = std::thread::scope(|s| {
                let parts: Vec<_> = spots
                    .chunks(per)
                    .map(|part| s.spawn(move || part.iter().map(search).collect::<Vec<_>>()))
                    .collect();
                parts
                    .into_iter()
                    .flat_map(|h| h.join().expect("ring search"))
                    .collect()
            });
            self.rings
                .lock()
                .expect("rings lock")
                .insert(set.id.clone(), list);
        }
    }

    /// The nearest of the `wanted` structures (indices) from block `pos`,
    /// searched as vanilla's `/locate` does: the ring placements by the
    /// distance of their chunk middles, then the spread ones ring by ring of
    /// placement cells out to `radius`, stopping at the first ring with a
    /// find. `starts` gives the structures starting in a chunk. Returns the
    /// structure and the block shown (the chunk corner plus the set's
    /// locate offset, at y 0).
    pub fn locate(
        &self,
        seed: i64,
        wanted: &HashSet<usize>,
        pos: (i32, i32, i32),
        radius: i32,
        starts: &mut dyn FnMut(i32, i32) -> Vec<Arc<Start>>,
    ) -> Option<(usize, (i32, i32, i32))> {
        let dist2 = |a: (i32, i32, i32)| {
            let d = |u: i32, v: i32| f64::from(u - v).powi(2);
            d(a.0, pos.0) + d(a.1, pos.1) + d(a.2, pos.2)
        };
        let found_at =
            |set: &Set, x: i32, z: i32, starts: &mut dyn FnMut(i32, i32) -> Vec<Arc<Start>>| {
                starts(x, z)
                    .iter()
                    .find(|s| {
                        wanted.contains(&s.structure)
                            && set.structures.iter().any(|e| e.0 == s.structure)
                    })
                    .map(|s| {
                        let o = set.locate_offset;
                        (s.structure, (x * 16 + o.0, o.1, z * 16 + o.2))
                    })
            };
        let sets: Vec<&Set> = self
            .sets
            .iter()
            .filter(|s| s.structures.iter().any(|e| wanted.contains(&e.0)))
            .collect();
        let mut best: Option<(usize, (i32, i32, i32))> = None;
        let mut best_d = f64::MAX;
        for set in &sets {
            if !matches!(set.placement, Placement::Rings { .. }) {
                continue;
            }
            let ring = self
                .rings
                .lock()
                .expect("rings lock")
                .get(&set.id)
                .cloned()
                .unwrap_or_default();
            for (x, z) in ring {
                let d = dist2((x * 16 + 8, 32, z * 16 + 8));
                if (best.is_none() || d < best_d)
                    && let Some(f) = found_at(set, x, z, starts)
                {
                    best = Some(f);
                    best_d = d;
                }
            }
        }
        let (cx, cz) = (pos.0 >> 4, pos.2 >> 4);
        for r in 0..=radius {
            let mut any = false;
            for set in &sets {
                let Placement::Spread {
                    spacing,
                    separation,
                    salt,
                    triangular,
                    ..
                } = set.placement
                else {
                    continue;
                };
                let mut hit = None;
                'ring: for dx in -r..=r {
                    for dz in -r..=r {
                        if dx.abs() != r && dz.abs() != r {
                            continue;
                        }
                        let (x, z) = Self::spread_chunk(
                            seed,
                            cx + spacing * dx,
                            cz + spacing * dz,
                            spacing,
                            separation,
                            salt,
                            triangular,
                        );
                        if let Some(f) = found_at(set, x, z, starts) {
                            hit = Some(f);
                            break 'ring;
                        }
                    }
                }
                if let Some(f) = hit {
                    any = true;
                    let d = dist2(f.1);
                    if d < best_d {
                        best_d = d;
                        best = Some(f);
                    }
                }
            }
            if any {
                break;
            }
        }
        best
    }

    /// Structures starting in chunk `x, z`.
    pub fn starts(
        &self,
        seed: i64,
        x: i32,
        z: i32,
        heights: Heights,
        ground: &mut dyn Ground,
    ) -> Vec<Arc<Start>> {
        self.starts_as(seed, x, z, heights, ground, None)
    }

    /// [`Self::starts`] with only the start piece of jigsaw structures, and
    /// only sets that can place one of `wanted`, as vanilla checks where
    /// one could start (`/locate`): much quicker. A treasure search must
    /// not assemble the mineshafts and fossils in every chunk it looks at.
    pub fn start_pieces(
        &self,
        seed: i64,
        x: i32,
        z: i32,
        heights: Heights,
        ground: &mut dyn Ground,
        wanted: &HashSet<usize>,
    ) -> Vec<Arc<Start>> {
        self.starts_as(seed, x, z, heights, ground, Some(wanted))
    }

    fn starts_as(
        &self,
        seed: i64,
        x: i32,
        z: i32,
        heights: Heights,
        ground: &mut dyn Ground,
        // Start pieces only, of the sets that can place one of these.
        only: Option<&HashSet<usize>>,
    ) -> Vec<Arc<Start>> {
        let start_only = only.is_some();
        let mut out = Vec::new();
        for set in &self.sets {
            if only.is_some_and(|want| !set.structures.iter().any(|e| want.contains(&e.0))) {
                continue;
            }
            if set.structures.is_empty() || !self.is_structure_chunk(set, seed, x, z) {
                continue;
            }
            if set.structures.len() == 1 {
                if let Some(s) =
                    self.generate(set.structures[0].0, seed, x, z, heights, ground, start_only)
                {
                    out.push(Arc::new(s));
                }
                continue;
            }
            let mut list = set.structures.clone();
            let mut r = WorldgenRandom::legacy(0);
            r.set_large_feature_seed(seed, x, z);
            let mut total: i32 = list.iter().map(|e| e.1).sum();
            while !list.is_empty() && total > 0 {
                let mut j = r.next_int_bound(total);
                let mut k = 0;
                for e in &list {
                    j -= e.1;
                    if j < 0 {
                        break;
                    }
                    k += 1;
                }
                let k = k.min(list.len() - 1);
                let e = list[k];
                if let Some(s) = self.generate(e.0, seed, x, z, heights, ground, start_only) {
                    out.push(Arc::new(s));
                    break;
                }
                list.remove(k);
                total -= e.1;
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn generate(
        &self,
        index: usize,
        seed: i64,
        x: i32,
        z: i32,
        heights: Heights,
        ground: &mut dyn Ground,
        start_only: bool,
    ) -> Option<Start> {
        self.generate_in(index, seed, x, z, heights, ground, start_only, false)
    }

    /// `/place structure`: structure `id` started in chunk `x, z`,
    /// whatever the biome there.
    pub fn generate_anywhere(
        &self,
        id: &str,
        seed: i64,
        x: i32,
        z: i32,
        heights: Heights,
        ground: &mut dyn Ground,
    ) -> Option<Start> {
        let index = self.structures.iter().position(|s| s.id == id)?;
        self.generate_in(index, seed, x, z, heights, ground, false, true)
    }

    /// `/place jigsaw`: pieces grown from a random piece of `pool` whose
    /// jigsaw named `target` sits at `anchor`. `Err(true)` for a pool that
    /// does not exist, `Err(false)` when nothing could be placed.
    #[allow(clippy::too_many_arguments)]
    pub fn generate_jigsaw(
        &self,
        pool: &str,
        target: &str,
        max_depth: i32,
        anchor: P,
        seed: i64,
        heights: Heights,
        ground: &mut dyn Ground,
    ) -> Result<Start, bool> {
        struct G<'g>(&'g mut dyn Ground);
        impl jigsaw::Ground for G<'_> {
            fn height(&mut self, x: i32, z: i32) -> i32 {
                self.0.height(x, z, false)
            }
        }
        let mut r = WorldgenRandom::legacy(seed);
        let pieces = jigsaw::assemble_at(
            self,
            &mut r,
            &mut G(ground),
            pool,
            target,
            max_depth,
            anchor,
            heights.min_y,
            heights.min_y + heights.height - 1,
        )?;
        let bbox = pieces
            .iter()
            .map(|p| p.bbox)
            .reduce(BBox::union)
            .ok_or(false)?;
        Ok(Start {
            structure: 0,
            chunk: (anchor.x >> 4, anchor.z >> 4),
            pieces: pieces.into_iter().map(Piece::Jigsaw).collect(),
            bbox,
        })
    }

    /// Whether there is a template pool `id`.
    pub fn has_pool(&self, id: &str) -> bool {
        self.pool(id).is_some()
    }

    /// Whether there is a structure `id`.
    pub fn has(&self, id: &str) -> bool {
        self.structures.iter().any(|s| s.id == id)
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_in(
        &self,
        index: usize,
        seed: i64,
        x: i32,
        z: i32,
        heights: Heights,
        ground: &mut dyn Ground,
        start_only: bool,
        any_biome: bool,
    ) -> Option<Start> {
        let s = &self.structures[index];
        let mut r = WorldgenRandom::legacy(0);
        r.set_large_feature_seed(seed, x, z);
        let (pieces, at) = match &s.kind {
            Kind::Jigsaw {
                start_pool,
                start_jigsaw,
                size,
                start_height,
                project,
                max_distance,
                expansion_hack,
            } => {
                let y = start_height.sample(&mut r, heights);
                struct G<'g> {
                    g: &'g mut dyn Ground,
                    ocean: bool,
                }
                impl jigsaw::Ground for G<'_> {
                    fn height(&mut self, x: i32, z: i32) -> i32 {
                        self.g.height(x, z, self.ocean)
                    }
                }
                let mut g = G {
                    g: ground,
                    ocean: project.unwrap_or(false),
                };
                let (pieces, at) = jigsaw::assemble(
                    self,
                    &mut r,
                    &mut g,
                    start_pool,
                    start_jigsaw.as_deref(),
                    if start_only { 0 } else { *size },
                    P::new(x * 16, y, z * 16),
                    *expansion_hack,
                    project.is_some(),
                    *max_distance,
                    heights.min_y,
                    heights.min_y + heights.height - 1,
                )?;
                (
                    pieces.into_iter().map(Piece::Jigsaw).collect::<Vec<_>>(),
                    at,
                )
            }
            Kind::Legacy(ty, v) => {
                let (pieces, at) =
                    legacy::generate(self, ty, v, &mut r, seed, x, z, heights, ground)?;
                (pieces.into_iter().map(Piece::Legacy).collect(), at)
            }
        };
        if pieces.is_empty() {
            return None;
        }
        let biome = ground.biome(at.x >> 2, at.y >> 2, at.z >> 2);
        if std::env::var_os("DEBUG_STARTS").is_some() {
            eprintln!(
                "{} at chunk {x},{z}: centre {at:?}, biome {biome}, ok {}, {} pieces",
                s.id,
                s.biomes.contains(&biome),
                pieces.len()
            );
        }
        if !any_biome && !s.biomes.contains(&biome) {
            return None;
        }
        let bbox = pieces
            .iter()
            .map(Piece::bbox)
            .reduce(BBox::union)
            .expect("pieces not empty");
        Some(Start {
            structure: index,
            chunk: (x, z),
            pieces,
            bbox,
        })
    }

    /// Places the parts of `start` in chunk `cx, cz`.
    pub fn place(
        &self,
        features: &Features,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        start: &Start,
        cx: i32,
        cz: i32,
    ) {
        let h = region.heights();
        let chunk = BBox::chunk(cx, cz, h.min_y, h.min_y + h.height - 1);
        let first = start.pieces[0].bbox();
        let c = first.center();
        let pivot = P::new(c.x, first.min.y, c.z);
        let cx2 = Ctx2 {
            structures: self,
            features,
        };
        for piece in &start.pieces {
            if !piece.bbox().intersects(&chunk) {
                continue;
            }
            match piece {
                Piece::Jigsaw(p) => {
                    let ok = p
                        .element
                        .place(&cx2, region, r, p.pos, pivot, p.rotation, chunk);
                    if std::env::var_os("DEBUG_STARTS").is_some() {
                        eprintln!(
                            "  piece at {:?} in chunk {cx},{cz}: {ok} ({:?})",
                            p.pos,
                            match &*p.element {
                                jigsaw::Element::Single { template, .. } => template.as_str(),
                                _ => "-",
                            }
                        );
                    }
                }
                Piece::Legacy(p) => legacy::place(&cx2, region, r, p, chunk, start),
            }
        }
        if matches!(&self.structures[start.structure].kind, Kind::Legacy(ty, _) if ty == "woodland_mansion")
        {
            let mut floor: Option<BBox> = None;
            for piece in &start.pieces {
                if let Piece::Legacy(p) = piece
                    && let legacy::Kind::Template(t) = &*p.kind
                {
                    let b = p.bbox;
                    let here = BBox::from_corners(
                        P::new(b.min.x, t.pos.y, b.min.z),
                        P::new(b.max.x, t.pos.y, b.max.z),
                    );
                    floor = Some(floor.map_or(here, |f| f.union(here)));
                }
            }
            if let Some(floor) = floor {
                observed::mansion_foundation(self.facts(), region, floor, chunk);
            }
        }
    }
}

fn reduce(method: Reduction, seed: i64, salt: i32, x: i32, z: i32, frequency: f32) -> bool {
    let mut r = WorldgenRandom::legacy(0);
    match method {
        Reduction::Default => {
            r.set_large_feature_with_salt(seed, salt, x, z);
            r.next_float() < frequency
        }
        Reduction::Legacy1 => {
            let (i, j) = (x >> 4, z >> 4);
            let mut r = WorldgenRandom::legacy(i64::from(i ^ (j << 4)) ^ seed);
            r.next_int();
            r.next_int_bound((1.0 / frequency) as i32) == 0
        }
        Reduction::Legacy2 => {
            r.set_large_feature_with_salt(seed, x, z, 10_387_320);
            r.next_float() < frequency
        }
        Reduction::Legacy3 => {
            r.set_large_feature_seed(seed, x, z);
            r.next_double() < f64::from(frequency)
        }
    }
}

/// Terrain shaped to fit the structures near a chunk.
#[derive(Debug, Default)]
pub struct Beard {
    rigids: Vec<(BBox, Adapt, i32)>,
    junctions: Vec<jigsaw::Junction>,
}

fn kernel() -> &'static [f32] {
    static K: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    K.get_or_init(|| {
        let mut k = vec![0f32; 24 * 24 * 24];
        for z in 0..24 {
            for x in 0..24 {
                for y in 0..24 {
                    k[z * 576 + x * 24 + y] =
                        compute_beard(x as i32 - 12, y as i32 - 12, z as i32 - 12) as f32;
                }
            }
        }
        k
    })
}

fn fast_inv_sqrt(x: f64) -> f64 {
    let half = 0.5 * x;
    let i = 0x5FE6_EB50_C7B5_37A9_i64 - (x.to_bits() as i64 >> 1);
    let y = f64::from_bits(i as u64);
    y * (1.5 - half * y * y)
}

/// The kernel: a Gaussian bump; the sign comes from the height above the
/// piece's ground.
fn compute_beard(x: i32, y: i32, z: i32) -> f64 {
    let d = f64::from(x * x + y * y + z * z);
    (-d / 16.0).exp()
}

fn beard_contribution(x: i32, y: i32, z: i32, to_ground: i32) -> f64 {
    let (i, j, k) = (x + 12, y + 12, z + 12);
    if !(0..24).contains(&i) || !(0..24).contains(&j) || !(0..24).contains(&k) {
        return 0.0;
    }
    let d = f64::from(to_ground) + 0.5;
    let e = f64::from(x * x) + d * d + f64::from(z * z);
    let f = -d * fast_inv_sqrt(e / 2.0) / 2.0;
    f * f64::from(kernel()[(k * 576 + i * 24 + j) as usize])
}

fn bury_contribution(x: f64, y: f64, z: f64) -> f64 {
    let d = (x * x + y * y + z * z).sqrt();
    (1.0 - d / 6.0).clamp(0.0, 1.0)
}

impl Beard {
    /// From the structures whose pieces come within 12 blocks of chunk
    /// `cx, cz`.
    pub fn new(structures: &Structures, starts: &[Arc<Start>], cx: i32, cz: i32) -> Self {
        let mut b = Beard::default();
        let (x0, z0) = (cx * 16, cz * 16);
        let close = |bb: &BBox| {
            bb.max.x >= x0 - 12
                && bb.min.x <= x0 + 15 + 12
                && bb.max.z >= z0 - 12
                && bb.min.z <= z0 + 15 + 12
        };
        for s in starts {
            let adapt = structures.structures[s.structure].adapt;
            if adapt == Adapt::None {
                continue;
            }
            for piece in &s.pieces {
                if !close(&piece.bbox()) {
                    continue;
                }
                match piece {
                    Piece::Jigsaw(p) => {
                        if p.element.projection() == jigsaw::Projection::Rigid {
                            b.rigids.push((p.bbox, adapt, p.ground_delta));
                        }
                        for j in &p.junctions {
                            if j.x > x0 - 12
                                && j.z > z0 - 12
                                && j.x < x0 + 15 + 12
                                && j.z < z0 + 15 + 12
                            {
                                b.junctions.push(*j);
                            }
                        }
                    }
                    Piece::Legacy(p) => b.rigids.push((p.bbox, adapt, 0)),
                }
            }
        }
        b
    }

    pub fn is_empty(&self) -> bool {
        self.rigids.is_empty() && self.junctions.is_empty()
    }

    pub fn compute(&self, i: i32, j: i32, k: i32) -> f64 {
        let mut d = 0.0;
        for (bb, adapt, l) in &self.rigids {
            let m = 0.max((bb.min.x - i).max(i - bb.max.x));
            let n = 0.max((bb.min.z - k).max(k - bb.max.z));
            let o = bb.min.y + l;
            let p = j - o;
            let q = match adapt {
                Adapt::None => 0,
                Adapt::Bury | Adapt::BeardThin => p,
                Adapt::BeardBox => 0.max((o - j).max(j - bb.max.y)),
                Adapt::Encapsulate => 0.max((bb.min.y - j).max(j - bb.max.y)),
            };
            d += match adapt {
                Adapt::None => 0.0,
                Adapt::Bury => bury_contribution(f64::from(m), f64::from(q) / 2.0, f64::from(n)),
                Adapt::BeardThin | Adapt::BeardBox => beard_contribution(m, q, n, p) * 0.8,
                Adapt::Encapsulate => {
                    bury_contribution(f64::from(m) / 2.0, f64::from(q) / 2.0, f64::from(n) / 2.0)
                        * 0.8
                }
            };
        }
        for jn in &self.junctions {
            let (r, l, m) = (i - jn.x, j - jn.ground_y, k - jn.z);
            d += beard_contribution(r, l, m, l) * 0.4;
        }
        d
    }
}
