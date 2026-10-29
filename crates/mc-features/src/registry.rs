//! The world's features: every biome's lists merged into one order per
//! generation step, the biomes' climate (where water freezes and snow
//! lies), and decorating a chunk.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::blocks::Facts;
use crate::feature::{Ctx, Feature, Loader};
use crate::placement::{PerlinSimplex, Placed};
use crate::rand::WorldgenRandom;
use crate::value::Heights;
use crate::{P, Region};

#[derive(Debug, Default)]
struct Biome {
    /// Per step: indices into that step's order.
    steps: Vec<Vec<usize>>,
    all: HashSet<String>,
    temperature: f32,
    frozen: bool,
    precipitation: bool,
}

pub struct Features {
    pub facts: Arc<Facts>,
    pub structures: crate::structure::Structures,
    steps: Vec<Vec<Arc<Placed>>>,
    biomes: HashMap<u32, Biome>,
    pub info_noise: PerlinSimplex,
    temperature_noise: PerlinSimplex,
    frozen_noise: PerlinSimplex,
    configured: HashMap<String, Arc<Feature>>,
    pub heights: Heights,
    /// Feature types and modifiers we skip, with how often they appear.
    pub unsupported: Vec<(String, usize)>,
}

/// Configured features a grown sapling places (the `TreeGrower`s).
const SAPLING_TREES: &[&str] = &[
    "oak",
    "fancy_oak",
    "birch",
    "spruce",
    "mega_spruce",
    "mega_pine",
    "jungle_tree_no_vine",
    "mega_jungle_tree",
    "acacia",
    "dark_oak",
    "pale_oak_bonemeal",
    "cherry",
    "azalea_tree",
    "mangrove",
    "tall_mangrove",
    "oak_bees_005",
    "fancy_oak_bees_005",
    "birch_bees_005",
    "cherry_bees_005",
];

impl std::fmt::Debug for Features {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Features")
            .field("steps", &self.steps.len())
            .finish_non_exhaustive()
    }
}

impl Features {
    /// `data_root` is the vanilla `data` folder. `layers` are enabled
    /// datapack `data` folders, lowest priority first: a later file replaces
    /// the same `namespace:path` under `worldgen`. `biome_order` is the
    /// dimension's biomes in the biome source's order; `biome_ids` the
    /// biome registry. Biomes that only a datapack defines are appended.
    pub fn load(
        data_root: &Path,
        layers: &[PathBuf],
        facts: Facts,
        biome_order: &[String],
        biome_ids: &HashMap<String, u32>,
        heights: Heights,
    ) -> Self {
        let facts = Arc::new(facts);
        let roots = crate::pack::roots(data_root, layers);
        let loader = Loader::new(&facts, roots.clone());
        // Each biome's lists, in the biome source's order, then datapack biomes.
        let mut lists: Vec<(String, Vec<Vec<String>>, Value)> = Vec::new();
        for name in biome_names(biome_order, layers) {
            let Some(v) = crate::pack::read_id(&roots, "biome", &name) else {
                continue;
            };
            let steps: Vec<Vec<String>> = v["features"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|step| {
                    step.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|f| f.as_str().map(crate::pack::full_id))
                        .collect()
                })
                .collect();
            lists.push((name, steps, v));
        }
        let order = sort(&lists.iter().map(|(_, s, _)| s.clone()).collect::<Vec<_>>());
        let steps: Vec<Vec<Arc<Placed>>> = order
            .iter()
            .map(|ids| {
                ids.iter()
                    .map(|id| loader.placed(&Value::String(id.clone())))
                    .collect()
            })
            .collect();
        let index: Vec<HashMap<&str, usize>> = order
            .iter()
            .map(|ids| {
                ids.iter()
                    .enumerate()
                    .map(|(i, id)| (id.as_str(), i))
                    .collect()
            })
            .collect();
        let mut biomes = HashMap::new();
        for (name, lists, v) in &lists {
            let Some(&id) = biome_ids.get(name) else {
                continue;
            };
            let mut b = Biome {
                temperature: v["temperature"].as_f64().unwrap_or(0.5) as f32,
                frozen: v["temperature_modifier"].as_str() == Some("frozen"),
                precipitation: v["has_precipitation"].as_bool().unwrap_or(true),
                ..Biome::default()
            };
            for (s, list) in lists.iter().enumerate() {
                let mut idx: Vec<usize> = list
                    .iter()
                    .filter_map(|f| index.get(s).and_then(|m| m.get(f.as_str())).copied())
                    .collect();
                idx.sort_unstable();
                b.steps.push(idx);
                b.all.extend(list.iter().cloned());
            }
            biomes.insert(id, b);
        }
        let structures =
            crate::structure::Structures::load(&roots, facts.clone(), &loader, biome_ids);
        // The trees saplings grow into, whether or not a biome plants them.
        for id in SAPLING_TREES {
            loader.configured(&Value::String((*id).to_owned()));
        }
        // Configured features a datapack adds or replaces, even when no
        // biome in this dimension lists them.
        for layer in layers {
            for id in crate::pack::read_kind(std::slice::from_ref(layer), "feature").into_keys() {
                loader.configured(&Value::String(id));
            }
        }
        let configured = loader.configured_by_id();
        let mut unsupported: Vec<(String, usize)> = loader
            .unsupported
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        unsupported.sort();
        drop(loader);
        Self {
            facts,
            structures,
            steps,
            biomes,
            info_noise: PerlinSimplex::new(2345, &[0]),
            temperature_noise: PerlinSimplex::new(1234, &[0]),
            frozen_noise: PerlinSimplex::new(3456, &[-2, -1, 0]),
            configured,
            heights,
            unsupported,
        }
    }

    pub fn configured(&self, id: &str) -> Option<&Arc<Feature>> {
        self.configured.get(id)
    }

    pub fn biome_has(&self, biome: u32, placed: &str) -> bool {
        self.biomes
            .get(&biome)
            .is_some_and(|b| b.all.contains(placed))
    }

    /// The temperature at `p` in `biome`, colder higher up.
    pub fn temperature(&self, biome: u32, p: P) -> f32 {
        let Some(b) = self.biomes.get(&biome) else {
            return 0.8;
        };
        let mut t = b.temperature;
        if b.frozen {
            let d = self
                .frozen_noise
                .value(f64::from(p.x) * 0.05, f64::from(p.z) * 0.05)
                * 7.0;
            let e = self
                .info_noise
                .value(f64::from(p.x) * 0.2, f64::from(p.z) * 0.2);
            if d + e < 0.3 {
                let g = self
                    .info_noise
                    .value(f64::from(p.x) * 0.09, f64::from(p.z) * 0.09);
                if g < 0.8 {
                    t = 0.2;
                }
            }
        }
        let snow_line = self.heights.sea_level + 17;
        if p.y > snow_line {
            let g = (self
                .temperature_noise
                .value(f64::from(p.x as f32 / 8.0), f64::from(p.z as f32 / 8.0))
                * 8.0) as f32;
            t - (g + p.y as f32 - snow_line as f32) * 0.05 / 40.0
        } else {
            t
        }
    }

    /// Whether rain or snow falls in `biome` at all.
    pub fn has_precipitation(&self, biome: u32) -> bool {
        self.biomes.get(&biome).is_none_or(|b| b.precipitation)
    }

    fn cold(&self, biome: u32, p: P) -> bool {
        self.temperature(biome, p) < 0.15
    }

    /// Whether still water at `p` turns to ice.
    pub fn should_freeze(&self, region: &dyn Region, facts: &Facts, biome: u32, p: P) -> bool {
        let h = region.heights();
        if !self.cold(biome, p) || p.y < h.min_y || p.y >= h.min_y + h.height {
            return false;
        }
        let s = region.block(p);
        facts.is(s, "minecraft:water") && facts.property(s, "level") == Some("0")
    }

    /// Whether snow settles at `p`.
    pub fn should_snow(&self, region: &dyn Region, facts: &Facts, biome: u32, p: P) -> bool {
        let h = region.heights();
        if !self.cold(biome, p) || p.y < h.min_y || p.y >= h.min_y + h.height {
            return false;
        }
        if !self.biomes.get(&biome).is_none_or(|b| b.precipitation) {
            return false;
        }
        let Some(snow) = facts.default_state("minecraft:snow") else {
            return false;
        };
        facts.is_air(region.block(p)) && facts.survives(region, p, snow)
    }

    /// Places the features of the chunk at `cx, cz` (whose biomes are
    /// `chunk_biomes`) into the region around it.
    /// `starts` are the structures whose pieces reach into the chunk.
    pub fn decorate(
        &self,
        region: &mut dyn Region,
        cx: i32,
        cz: i32,
        chunk_biomes: &BTreeSet<u32>,
        starts: &[Arc<crate::structure::Start>],
    ) {
        let ctx = Ctx {
            facts: &self.facts,
            features: self,
        };
        let (x0, z0) = (cx * 16, cz * 16);
        let origin = P::new(x0, region.heights().min_y, z0);
        let mut r = WorldgenRandom::xoroshiro(0);
        let decoration = r.set_decoration_seed(region.seed(), x0, z0);
        let h = region.heights();
        let chunk =
            crate::structure::template::BBox::chunk(cx, cz, h.min_y, h.min_y + h.height - 1);
        for (step, features) in self.steps.iter().enumerate() {
            if let Some(list) = self.structures.by_step.get(step) {
                for (index, &s) in list.iter().enumerate() {
                    r.set_feature_seed(decoration, index as i32, step as i32);
                    for start in starts.iter().filter(|st| st.structure == s) {
                        if start.bbox.intersects(&chunk) {
                            self.structures.place(self, region, &mut r, start, cx, cz);
                        }
                    }
                }
            }
            let mut wanted: BTreeSet<usize> = BTreeSet::new();
            for b in chunk_biomes {
                if let Some(list) = self.biomes.get(b).and_then(|b| b.steps.get(step)) {
                    wanted.extend(list.iter().copied());
                }
            }
            for i in wanted {
                let f = &features[i];
                r.set_feature_seed(decoration, i as i32, step as i32);
                f.place(&ctx, region, &mut r, origin, f.id.as_deref());
            }
        }
    }
}

/// Biome ids to load: the dimension's order, then ids only a datapack has.
fn biome_names(biome_order: &[String], layers: &[PathBuf]) -> Vec<String> {
    let mut names = biome_order.to_vec();
    let mut seen: HashSet<String> = names.iter().cloned().collect();
    for layer in layers {
        for id in crate::pack::read_kind(std::slice::from_ref(layer), "biome").into_keys() {
            if seen.insert(id.clone()) {
                names.push(id);
            }
        }
    }
    names
}

/// One order of every feature per step that keeps each biome's order:
/// features numbered by first appearance, then a depth-first topological
/// sort, as the game's feature sorter.
fn sort(biomes: &[Vec<Vec<String>>]) -> Vec<Vec<String>> {
    let mut number: HashMap<String, usize> = HashMap::new();
    // Node: (step, number) → id.
    let mut name: BTreeMap<(usize, usize), String> = BTreeMap::new();
    let mut graph: BTreeMap<(usize, usize), BTreeSet<(usize, usize)>> = BTreeMap::new();
    let mut max_steps = 0;
    for steps in biomes {
        max_steps = max_steps.max(steps.len());
        let mut flat = Vec::new();
        for (s, list) in steps.iter().enumerate() {
            for f in list {
                let n = number.len();
                let n = *number.entry(f.clone()).or_insert(n);
                name.insert((s, n), f.clone());
                flat.push((s, n));
            }
        }
        for i in 0..flat.len() {
            let set = graph.entry(flat[i]).or_default();
            if i + 1 < flat.len() {
                set.insert(flat[i + 1]);
            }
        }
    }
    let mut visited = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut sorted = Vec::new();
    fn dfs(
        graph: &BTreeMap<(usize, usize), BTreeSet<(usize, usize)>>,
        visited: &mut BTreeSet<(usize, usize)>,
        visiting: &mut BTreeSet<(usize, usize)>,
        out: &mut Vec<(usize, usize)>,
        node: (usize, usize),
    ) -> bool {
        if visited.contains(&node) {
            return false;
        }
        if !visiting.insert(node) {
            return true;
        }
        if let Some(next) = graph.get(&node) {
            for &n in next {
                if dfs(graph, visited, visiting, out, n) {
                    return true;
                }
            }
        }
        visiting.remove(&node);
        visited.insert(node);
        out.push(node);
        false
    }
    let keys: Vec<(usize, usize)> = graph.keys().copied().collect();
    for k in keys {
        if !visited.contains(&k) && dfs(&graph, &mut visited, &mut visiting, &mut sorted, k) {
            // A cycle: the biomes disagree on an order. Keep what we have.
            visiting.clear();
        }
    }
    sorted.reverse();
    (0..max_steps)
        .map(|s| {
            sorted
                .iter()
                .filter(|(step, _)| *step == s)
                .filter_map(|k| name.get(k).cloned())
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;
    use crate::feature::Feature;

    fn facts() -> Option<crate::blocks::Facts> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        if !root.is_dir() {
            return None;
        }
        let data = mc_data::McData::load(&root).ok()?;
        let shapes = mc_tick::Shapes::new(&data, None, None);
        let rules = mc_tick::BlockRules::new(&data).ok()?;
        crate::blocks::Facts::new(&data, shapes, rules).ok()
    }

    /// A datapack layer replaces a vanilla biome's feature list and adds a
    /// configured feature, a placed feature, a biome, and a structure.
    #[test]
    fn datapack_worldgen_replaces_vanilla_files() {
        let Some(facts) = facts() else {
            eprintln!("skipped: no game data");
            return;
        };
        let data_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3/data");
        let tmp = std::env::temp_dir().join(format!("mc-worldgen-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let layer = tmp.join("data");
        std::fs::create_dir_all(layer.join("minecraft/worldgen/biome")).unwrap();
        std::fs::create_dir_all(layer.join("pack/worldgen/feature")).unwrap();
        std::fs::create_dir_all(layer.join("pack/worldgen/placed_feature")).unwrap();
        std::fs::create_dir_all(layer.join("pack/worldgen/biome")).unwrap();
        std::fs::create_dir_all(layer.join("pack/worldgen/structure")).unwrap();
        std::fs::write(
            layer.join("minecraft/worldgen/biome/plains.json"),
            r#"{"temperature":0.05,"has_precipitation":false,"features":[["pack:only_marker"]]}"#,
        )
        .unwrap();
        std::fs::write(
            layer.join("pack/worldgen/feature/marker_block.json"),
            r#"{"type":"minecraft:no_op"}"#,
        )
        .unwrap();
        std::fs::write(
            layer.join("pack/worldgen/feature/spare.json"),
            r#"{"type":"minecraft:no_op"}"#,
        )
        .unwrap();
        std::fs::write(
            layer.join("pack/worldgen/placed_feature/only_marker.json"),
            r#"{"feature":"pack:marker_block","placement":[]}"#,
        )
        .unwrap();
        std::fs::write(
            layer.join("pack/worldgen/biome/highlands.json"),
            r#"{"temperature":0.2,"has_precipitation":true,"features":[["pack:only_marker"]]}"#,
        )
        .unwrap();
        std::fs::write(
            layer.join("pack/worldgen/structure/camp.json"),
            r#"{"type":"minecraft:jigsaw","biomes":"minecraft:plains","step":"surface_structures","start_pool":"minecraft:empty","size":1,"start_height":{"absolute":0},"max_distance_from_center":80,"use_expansion_hack":false}"#,
        )
        .unwrap();

        let mut biome_ids = HashMap::new();
        biome_ids.insert("minecraft:plains".to_owned(), 0);
        biome_ids.insert("pack:highlands".to_owned(), 1);
        let heights = Heights {
            min_y: -64,
            height: 384,
            sea_level: 63,
        };
        let features = Features::load(
            &data_root,
            &[layer],
            facts,
            &["minecraft:plains".to_owned()],
            &biome_ids,
            heights,
        );

        assert!(
            features.biome_has(0, "pack:only_marker"),
            "plains should use the datapack feature list"
        );
        assert!(
            !features.biome_has(0, "minecraft:ore_dirt"),
            "the vanilla plains feature list should be replaced"
        );
        let plains = features.temperature(0, crate::P::new(0, 64, 0));
        assert!(
            (plains - 0.05).abs() < 0.001,
            "plains temperature {plains} should come from the datapack"
        );
        assert!(!features.has_precipitation(0));
        assert!(features.biome_has(1, "pack:only_marker"));
        let highlands = features.temperature(1, crate::P::new(0, 64, 0));
        assert!((highlands - 0.2).abs() < 0.001, "{highlands}");
        assert!(matches!(
            features.configured("pack:marker_block").map(|f| f.as_ref()),
            Some(Feature::NoOp)
        ));
        assert!(matches!(
            features.configured("pack:spare").map(|f| f.as_ref()),
            Some(Feature::NoOp)
        ));
        assert!(features.structures.has("pack:camp"));
        assert!(features.structures.has("minecraft:stronghold"));
        assert!(
            features
                .structures
                .has_pool("minecraft:village/plains/town_centers")
        );
        assert!(features.configured("minecraft:oak").is_some());
        assert!(
            features
                .structures
                .template("minecraft:end_city/ship")
                .is_some()
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
