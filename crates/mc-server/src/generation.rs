//! New chunks for world folders: the noise generator (`mc_worldgen`) set up
//! from the world's seed and the data pack, and new worlds created on disk
//! the way vanilla 26.3 lays them out (`level.dat`,
//! `data/minecraft/world_gen_settings.dat`,
//! `dimensions/minecraft/overworld/region`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context as _, Result};
use mc_data::{Blocks, McData};
use mc_nbt::{Compound, Tag};
use mc_worldgen::generator::{BiomeSource, Dimension, NoiseGenerator};
use serde_json::Value;

/// The data pack as the generator reads it.
struct Pack<'a>(&'a McData);

impl mc_worldgen::density::Source for Pack<'_> {
    fn read(&self, kind: &str, id: &str) -> Option<Value> {
        self.0
            .data_pack()
            .read_entry(&format!("minecraft:worldgen/{kind}"), id)
            .ok()
    }
}

struct GameNames<'a> {
    blocks: &'a Blocks,
    biomes: &'a HashMap<String, u32>,
}

impl mc_worldgen::materials::Names for GameNames<'_> {
    fn state(&self, v: &Value) -> Option<u32> {
        match v {
            Value::String(n) => self.blocks.default_state(&mc_worldgen::density::full_id(n)),
            Value::Object(o) => {
                let name = mc_worldgen::density::full_id(o.get("Name")?.as_str()?);
                let mut state = self.blocks.default_state(&name)?;
                if let Some(Value::Object(props)) = o.get("Properties") {
                    for (k, val) in props {
                        state = self.blocks.with_property(state, k, val.as_str()?)?;
                    }
                }
                Some(state)
            }
            _ => None,
        }
    }

    fn biome(&self, name: &str) -> Option<u32> {
        self.biomes
            .get(&mc_worldgen::density::full_id(name))
            .copied()
    }
}

/// The overworld noise generator for `seed`; `biomes` is the biome
/// registry (name → network id).
pub fn overworld(
    data: &McData,
    seed: i64,
    biomes: &HashMap<String, u32>,
) -> Result<NoiseGenerator> {
    dimension(data, seed, biomes, Dimension::Overworld)
}

/// The biome source of a dimension of a normal world.
fn biome_source(data: &McData, dimension: Dimension) -> Result<BiomeSource> {
    let Some(preset) = dimension.preset() else {
        return Ok(BiomeSource::TheEnd);
    };
    let parameters: Value = serde_json::from_str(
        &std::fs::read_to_string(
            data.root
                .join(format!("reports/biome_parameters/minecraft/{preset}.json")),
        )
        .context("biome parameter report (cargo xtask extract-data)")?,
    )?;
    Ok(BiomeSource::MultiNoise(parameters))
}

/// The biomes a dimension's source can give, in its order (features are
/// ordered by it).
fn biome_order(data: &McData, dimension: Dimension) -> Result<Vec<String>> {
    let mut order: Vec<String> = Vec::new();
    match biome_source(data, dimension)? {
        BiomeSource::MultiNoise(parameters) => {
            for entry in parameters["biomes"].as_array().into_iter().flatten() {
                if let Some(name) = entry["biome"].as_str()
                    && !order.iter().any(|n| n == name)
                {
                    order.push(name.to_owned());
                }
            }
        }
        BiomeSource::TheEnd => {
            for name in [
                "the_end",
                "end_highlands",
                "end_midlands",
                "small_end_islands",
                "end_barrens",
            ] {
                order.push(format!("minecraft:{name}"));
            }
        }
    }
    Ok(order)
}

/// Whether lava is fast in a dimension (its type's `gameplay/fast_lava`).
pub fn fast_lava(data: &McData, dimension: Dimension) -> bool {
    data.data_pack()
        .read_entry("minecraft:dimension_type", dimension.id())
        .ok()
        .and_then(|t| t["attributes"]["minecraft:gameplay/fast_lava"].as_bool())
        .unwrap_or(false)
}

/// The noise generator of a dimension of a normal world (vanilla's
/// `world_preset/normal`) for `seed`; `biomes` is the biome registry.
pub fn dimension(
    data: &McData,
    seed: i64,
    biomes: &HashMap<String, u32>,
    dimension: Dimension,
) -> Result<NoiseGenerator> {
    let pack = Pack(data);
    let settings = data
        .data_pack()
        .read_entry("minecraft:worldgen/noise_settings", dimension.settings())
        .with_context(|| format!("{} noise settings", dimension.settings()))?;
    let source = biome_source(data, dimension)?;
    let names = GameNames {
        blocks: &data.blocks,
        biomes,
    };
    let state = |n: &str| data.blocks.default_state(n).unwrap_or(0);
    NoiseGenerator::new(
        &pack,
        seed,
        &settings,
        &source,
        &names,
        biomes.clone(),
        mc_worldgen::generator::Blocks {
            air: state("minecraft:air"),
            stone: state("minecraft:stone"),
            water: state("minecraft:water"),
            lava: state("minecraft:lava"),
        },
    )
    .map_err(anyhow::Error::msg)
}

/// The seed of a world folder, if its overworld uses the noise generator.
pub fn seed_of(dir: &Path) -> Option<i64> {
    let settings =
        super::world::read_nbt_gz(&dir.join("data/minecraft/world_gen_settings.dat")).ok();
    let from_settings = settings.as_ref().and_then(|root| {
        let Some(Tag::Compound(data)) = root.get("data") else {
            return None;
        };
        noise_overworld(data).then_some(())?;
        match data.get("seed") {
            Some(Tag::Long(s)) => Some(*s),
            _ => None,
        }
    });
    if from_settings.is_some() {
        return from_settings;
    }
    // Older layout: WorldGenSettings inside level.dat.
    let level = super::world::read_nbt_gz(&dir.join("level.dat")).ok()?;
    let Some(Tag::Compound(data)) = level.get("Data") else {
        return None;
    };
    let Some(Tag::Compound(wgs)) = data.get("WorldGenSettings") else {
        return None;
    };
    noise_overworld(wgs).then_some(())?;
    match wgs.get("seed") {
        Some(Tag::Long(s)) => Some(*s),
        _ => None,
    }
}

fn noise_overworld(settings: &Compound) -> bool {
    let Some(Tag::Compound(dims)) = settings.get("dimensions") else {
        return false;
    };
    let Some(Tag::Compound(ow)) = dims.get("minecraft:overworld") else {
        return false;
    };
    let Some(Tag::Compound(generator)) = ow.get("generator") else {
        return false;
    };
    matches!(generator.get("type"), Some(Tag::String(t)) if t == "minecraft:noise")
        && matches!(generator.get("settings"), Some(Tag::String(s)) if s == "minecraft:overworld")
}

/// A spawn on land near 0, 0: the first chunk (in widening rings) whose
/// middle column ends in a solid block at or above sea level.
pub fn find_spawn(generator: &NoiseGenerator, blocks: &Blocks) -> (i32, i32, i32) {
    let fluid = |s: u32| blocks.is_fluid(s) || blocks.is_air(s);
    for r in 0..24i32 {
        for cx in -r..=r {
            for cz in -r..=r {
                if cx.abs() != r && cz.abs() != r {
                    continue;
                }
                let g = generator.generate(cx, cz);
                let (min_y, height) = (generator.min_y, generator.height);
                let at = |y: i32| g.blocks[(((y - min_y) * 16 + 8) * 16 + 8) as usize];
                let top = (min_y..min_y + height)
                    .rev()
                    .find(|&y| !blocks.is_air(at(y)));
                if let Some(top) = top
                    && !fluid(at(top))
                    && top >= generator.sea_level
                {
                    return (cx * 16 + 8, top + 1, cz * 16 + 8);
                }
            }
        }
    }
    (0, 100, 0)
}

/// Writes a new world folder for `seed` with its spawn.
/// How a new world starts (from `server.properties`).
#[derive(Clone, Copy, Debug)]
pub struct NewWorld {
    pub game_mode: i32,
    pub difficulty: i32,
    pub hardcore: bool,
}

pub fn create(
    dir: &Path,
    data: &McData,
    seed: i64,
    spawn: (i32, i32, i32),
    new: NewWorld,
) -> Result<()> {
    let difficulty_name =
        ["peaceful", "easy", "normal", "hard"][new.difficulty.clamp(0, 3) as usize];
    std::fs::create_dir_all(dir.join("dimensions/minecraft/overworld/region"))?;
    std::fs::create_dir_all(dir.join("data/minecraft"))?;
    let version = data.version.world_version;
    let s = |v: &str| Tag::String(v.to_owned());
    let mut spawn_tag = Compound::new();
    spawn_tag.insert("pos", Tag::IntArray(vec![spawn.0, spawn.1, spawn.2]));
    spawn_tag.insert("yaw", Tag::Float(0.0));
    spawn_tag.insert("pitch", Tag::Float(0.0));
    spawn_tag.insert("dimension", s("minecraft:overworld"));
    let mut difficulty = Compound::new();
    difficulty.insert("difficulty", s(difficulty_name));
    difficulty.insert("hardcore", Tag::Byte(i8::from(new.hardcore)));
    difficulty.insert("locked", Tag::Byte(0));
    let mut version_tag = Compound::new();
    version_tag.insert("Id", Tag::Int(version));
    version_tag.insert("Name", s(&data.version.name));
    version_tag.insert("Series", s("main"));
    version_tag.insert("Snapshot", Tag::Byte(0));
    let mut packs = Compound::new();
    packs.insert("Enabled", Tag::List(vec![s("vanilla")]));
    packs.insert("Disabled", Tag::List(Vec::new()));
    let mut level = Compound::new();
    level.insert("DataVersion", Tag::Int(version));
    level.insert(
        "LevelName",
        s(&dir
            .file_name()
            .map_or_else(|| "world".into(), |n| n.to_string_lossy().into_owned())),
    );
    level.insert("GameType", Tag::Int(new.game_mode));
    level.insert("Time", Tag::Long(0));
    level.insert("spawn", Tag::Compound(spawn_tag));
    level.insert("difficulty_settings", Tag::Compound(difficulty));
    level.insert("initialized", Tag::Byte(1));
    level.insert("allowCommands", Tag::Byte(1));
    level.insert("version", Tag::Int(19133));
    level.insert("Version", Tag::Compound(version_tag));
    level.insert("version_history", Tag::List(vec![Tag::Int(version)]));
    level.insert("DataPacks", Tag::Compound(packs));
    level.insert("ServerBrands", Tag::List(vec![s("minecraft-rewrite")]));
    level.insert("WasModded", Tag::Byte(1));
    level.insert("LastPlayed", Tag::Long(0));
    let mut root = Compound::new();
    root.insert("Data", Tag::Compound(level));
    super::world::write_nbt_gz(&dir.join("level.dat"), &root)?;

    let dimension = |settings: &str, preset: Option<&str>, kind: &str| {
        let mut source = Compound::new();
        match preset {
            Some(p) => {
                source.insert("type", s("minecraft:multi_noise"));
                source.insert("preset", s(p));
            }
            None => {
                source.insert("type", s("minecraft:the_end"));
            }
        }
        let mut generator = Compound::new();
        generator.insert("type", s("minecraft:noise"));
        generator.insert("settings", s(settings));
        generator.insert("biome_source", Tag::Compound(source));
        let mut d = Compound::new();
        d.insert("type", s(kind));
        d.insert("generator", Tag::Compound(generator));
        Tag::Compound(d)
    };
    let mut dims = Compound::new();
    dims.insert(
        "minecraft:overworld",
        dimension(
            "minecraft:overworld",
            Some("minecraft:overworld"),
            "minecraft:overworld",
        ),
    );
    dims.insert(
        "minecraft:the_nether",
        dimension(
            "minecraft:nether",
            Some("minecraft:nether"),
            "minecraft:the_nether",
        ),
    );
    dims.insert(
        "minecraft:the_end",
        dimension("minecraft:end", None, "minecraft:the_end"),
    );
    let mut settings = Compound::new();
    settings.insert("seed", Tag::Long(seed));
    settings.insert("generate_structures", Tag::Byte(1));
    settings.insert("bonus_chest", Tag::Byte(0));
    settings.insert("dimensions", Tag::Compound(dims));
    let mut root = Compound::new();
    root.insert("data", Tag::Compound(settings));
    root.insert("DataVersion", Tag::Int(version));
    super::world::write_nbt_gz(&dir.join("data/minecraft/world_gen_settings.dat"), &root)?;
    Ok(())
}

/// The overworld's features, in the biome source's order of biomes.
pub fn features(
    data: &McData,
    shapes: mc_tick::Shapes,
    biomes: &HashMap<String, u32>,
    generator: &NoiseGenerator,
) -> Result<mc_features::Features> {
    dimension_features(data, shapes, biomes, generator, Dimension::Overworld)
}

/// A dimension's features, in its biome source's order of biomes.
pub fn dimension_features(
    data: &McData,
    shapes: mc_tick::Shapes,
    biomes: &HashMap<String, u32>,
    generator: &NoiseGenerator,
    dimension: Dimension,
) -> Result<mc_features::Features> {
    let rules = mc_tick::BlockRules::new(data).map_err(anyhow::Error::msg)?;
    let facts = mc_features::Facts::new(data, shapes, rules).map_err(anyhow::Error::msg)?;
    let order = biome_order(data, dimension)?;
    let heights = mc_features::Heights {
        min_y: generator.min_y,
        height: generator.height,
        sea_level: generator.sea_level,
    };
    let started = std::time::Instant::now();
    let features = mc_features::Features::load(
        &data.data_pack().root.join("minecraft/worldgen"),
        facts,
        &order,
        biomes,
        heights,
    );
    tracing::debug!(
        "features loaded in {} ms; not generated yet: {}",
        started.elapsed().as_millis(),
        features
            .unsupported
            .iter()
            .map(|(k, n)| format!("{k} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(features)
}

#[cfg(test)]
mod speed {
    /// `cargo test --release -p mc-server terrain_speed -- --ignored --nocapture`
    #[test]
    #[ignore = "a timing, not a check"]
    fn terrain_speed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&root).unwrap();
        let report: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                root.join("reports/biome_parameters/minecraft/overworld.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut biomes = std::collections::HashMap::new();
        for b in report["biomes"].as_array().unwrap() {
            let n = biomes.len() as u32;
            biomes
                .entry(b["biome"].as_str().unwrap().to_owned())
                .or_insert(n);
        }
        let g = super::overworld(&data, -1294266004, &biomes).unwrap();
        let t = std::time::Instant::now();
        let n: i32 = std::env::var("TERRAIN_N").map_or(16, |v| v.parse().unwrap());
        // A checksum of what was made: optimisations must not change it.
        let mut sum = 0u64;
        for i in 0..n {
            let made = g.generate(i % 16 - 8, i / 16 - 8);
            for v in made.blocks.iter().chain(&made.biomes) {
                sum = (sum.rotate_left(5) ^ u64::from(*v)).wrapping_mul(0x100_0000_01b3);
            }
        }
        // The same on every core at once: what the server gets.
        let threads: i32 = std::env::var("TERRAIN_THREADS").map_or(0, |v| v.parse().unwrap());
        if threads > 0 {
            let t = std::time::Instant::now();
            let next = std::sync::atomic::AtomicI32::new(0);
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= n * 4 {
                                break;
                            }
                            std::hint::black_box(g.generate(i % 32 + 40, i / 32 + 40));
                        }
                    });
                }
            });
            println!(
                "{threads} threads: {:.0} chunks/s",
                f64::from(n * 4) / t.elapsed().as_secs_f64()
            );
        }
        println!(
            "terrain: {:.1} ms a chunk, checksum {sum:016x}",
            t.elapsed().as_secs_f64() * 1000.0 / f64::from(n)
        );
    }
}

#[cfg(test)]
mod dimension_world {
    use std::collections::HashMap;

    use mc_worldgen::generator::Dimension;

    struct Nothing;

    impl crate::pipeline::Finished for Nothing {
        fn is_finished(&self, _: i32, _: i32) -> bool {
            false
        }

        fn block(&self, _: mc_tick::BlockPos) -> Option<u32> {
            None
        }

        fn stored_proto(&self, _: i32, _: i32) -> Option<mc_world::Chunk> {
            None
        }
    }

    /// Generates a dimension around 0, 0 with everything (terrain, carvers,
    /// structures, features) into a world folder, to compare with a vanilla
    /// one (`cargo xtask worldgen-compare <vanilla> <ours> --dimension ..`):
    /// `DIM=nether SEED=12345 RADIUS=7 CENTER="0 0" OUT=<dir> cargo test --release -p
    /// mc-server generate_dimension -- --ignored --nocapture`.
    #[test]
    #[ignore = "writes a world for comparing by hand"]
    fn generate_dimension() {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        let dimension = Dimension::parse(&env("DIM", "nether")).expect("DIM");
        let seed: i64 = env("SEED", "12345").parse().unwrap();
        let radius: i32 = env("RADIUS", "7").parse().unwrap();
        let center: Vec<i32> = env("CENTER", "0 0")
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        let (ox, oz) = (center[0], center[1]);
        let out = std::path::PathBuf::from(env("OUT", "../../cache/ours-worldgen"));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&root).unwrap();
        let mut names: Vec<String> =
            std::fs::read_dir(data.data_pack().root.join("minecraft/worldgen/biome"))
                .unwrap()
                .filter_map(|e| {
                    Some(format!(
                        "minecraft:{}",
                        e.ok()?.file_name().to_str()?.strip_suffix(".json")?
                    ))
                })
                .collect();
        names.sort();
        let biomes: HashMap<String, u32> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i as u32))
            .collect();
        let observed = |file: &str| {
            std::fs::read_to_string(root.join("observed").join(file))
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| serde_json::from_value::<Vec<i32>>(v["states"].clone()).ok())
        };
        let shapes = mc_tick::Shapes::new(
            &data,
            observed("fence_attach.json"),
            observed("conductor.json"),
        )
        .with_instruments(observed("instrument.json"))
        .with_support(observed("support.json"))
        .with_push(observed("push.json"));
        let generator = super::dimension(&data, seed, &biomes, dimension).unwrap();
        let features =
            super::dimension_features(&data, shapes, &biomes, &generator, dimension).unwrap();
        println!("not generated: {:?}", features.unsupported);
        let air = data.blocks.default_state("minecraft:air").unwrap();
        let pipeline = crate::pipeline::Pipeline::new(generator, features, seed, air, 0)
            .with_fast_lava(super::fast_lava(&data, dimension));
        pipeline.prepare();
        let region = out
            .join("dimensions")
            .join(dimension.id().replace(':', "/"))
            .join("region");
        let _ = std::fs::remove_dir_all(&region);
        std::fs::create_dir_all(&region).unwrap();
        let mut storage = mc_storage::RegionStorage::new(region);
        let storage_names = mc_storage::Names {
            blocks: &data.blocks,
            biomes: &names,
            data_version: data.version.world_version,
        };
        let started = std::time::Instant::now();
        for cz in oz - radius..=oz + radius {
            for cx in ox - radius..=ox + radius {
                let done = pipeline.finish(cx, cz, &Nothing);
                let tag =
                    mc_storage::encode_chunk(&done.chunk, None, &storage_names, true).unwrap();
                let mut bytes = Vec::new();
                mc_nbt::write_named("", &tag, &mut bytes).unwrap();
                storage.write_chunk(cx, cz, &bytes, 0).unwrap();
            }
        }
        println!(
            "{} chunks in {:.1} s into {}",
            (2 * radius + 1).pow(2),
            started.elapsed().as_secs_f64(),
            out.display()
        );
    }
}
