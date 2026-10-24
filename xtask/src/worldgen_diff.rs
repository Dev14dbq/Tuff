//! `cargo xtask worldgen-diff [--seed <n> | --seeds 1,2,3 | --seed-count <n>]
//! [--radius <chunks>] [--dimension nether|end]`: compares our generator with
//! the vanilla reference world (`worldgen-ref`): biomes per 4×4×4 cell, and
//! blocks too with `WORLDGEN_BLOCKS` set. Several seeds end in one table.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use mc_storage::chunk::{Decoded, Palettes, decode_chunk};
use mc_worldgen::climate::{BiomeList, Climate, DataDir};
use mc_worldgen::density::Builder;
use mc_worldgen::generator::{BiomeSource, Dimension};

use crate::probe::load_data;
use crate::worldgen_seeds::{self, Row};

pub fn run(args: &[String]) -> Result<()> {
    let (mut seed, mut radius) = (12345i64, 3i32);
    let mut dimension = Dimension::Overworld;
    let (mut ox, mut oz) = (0i32, 0i32);
    let (mut list, mut count) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--seed" => seed = value()?.parse()?,
            "--seeds" => list = Some(value()?),
            "--seed-count" => count = Some(value()?.parse()?),
            "--radius" => radius = value()?.parse()?,
            "--center" => {
                ox = value()?.parse()?;
                oz = it.next().context("--center needs x and z")?.parse()?;
            }
            "--dimension" => {
                let name = value()?;
                dimension =
                    Dimension::parse(&name).with_context(|| format!("no dimension {name}"))?;
            }
            other => bail!("unknown option `{other}`"),
        }
    }
    let seeds = worldgen_seeds::seeds(seed, list.as_deref(), count)?;
    worldgen_seeds::each(&seeds, |seed| compare(seed, radius, dimension, (ox, oz)))?;
    Ok(())
}

/// Compares our generator with the reference world of one seed.
fn compare(seed: i64, radius: i32, dimension: Dimension, center: (i32, i32)) -> Result<Row> {
    let (ox, oz) = center;
    let root = crate::workspace_root();
    let data = load_data(None)?;
    let data_dir = root.join("cache/mcdata").join(&data.version.name);
    let source = DataDir(data_dir.clone());
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(data_dir.join(format!(
            "data/minecraft/worldgen/noise_settings/{}.json",
            dimension.settings().trim_start_matches("minecraft:")
        )))?)?;
    let legacy = settings["legacy_random_source"].as_bool().unwrap_or(false);
    let (min_y, height) = (
        settings["noise"]["min_y"].as_i64().unwrap_or(-64) as i32,
        settings["noise"]["height"].as_i64().unwrap_or(384) as i32,
    );
    let mut builder = Builder::new(&source, seed, legacy);
    let climate = Climate::from_router(&mut builder, &settings["noise_router"])
        .map_err(anyhow::Error::msg)?;
    let biome_source = match dimension.preset() {
        Some(preset) => BiomeSource::MultiNoise(serde_json::from_str(&std::fs::read_to_string(
            data_dir.join(format!("reports/biome_parameters/minecraft/{preset}.json")),
        )?)?),
        None => BiomeSource::TheEnd,
    };
    let list = match &biome_source {
        BiomeSource::MultiNoise(parameters) => BiomeList::from_json(parameters),
        BiomeSource::TheEnd => BiomeList::from_json(&serde_json::Value::Null),
    };
    let mut names: Vec<String> = std::fs::read_dir(data_dir.join("data/minecraft/worldgen/biome"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .map(|n| format!("minecraft:{}", n.trim_end_matches(".json")))
        })
        .collect();
    names.sort();
    let index: HashMap<String, u32> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), i as u32))
        .collect();
    let suffix = match dimension {
        Dimension::Overworld => "",
        Dimension::Nether => "-nether",
        Dimension::End => "-end",
    };
    let region = root.join(format!(
        "cache/vanilla-server/26.3-worldgen/{seed}{suffix}{}/world/dimensions/{}/region",
        if (ox, oz) == (0, 0) {
            String::new()
        } else {
            format!("-at-{ox}-{oz}")
        },
        dimension.id().replace(':', "/")
    ));
    let mut storage = mc_storage::region::RegionStorage::new(region);
    let mut read = 0usize;
    let air = data.blocks.default_state("minecraft:air").unwrap_or(0);
    let lookup = |n: &str| index.get(n).copied();
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &lookup,
        air,
        fallback_biome: 0,
        unresolved: std::cell::Cell::new(0),
    };
    let (mut same, mut total) = (0usize, 0usize);
    let mut wrong: HashMap<(String, String), usize> = HashMap::new();
    let started = std::time::Instant::now();
    let mut per_chunk: Vec<(i32, i32, usize)> = Vec::new();
    let terrain = std::env::var_os("WORLDGEN_TERRAIN").is_some();
    let full = std::env::var_os("WORLDGEN_BLOCKS").is_some();
    struct GameNames<'a> {
        blocks: &'a mc_data::Blocks,
        biomes: &'a HashMap<String, u32>,
    }
    impl mc_worldgen::materials::Names for GameNames<'_> {
        fn state(&self, v: &serde_json::Value) -> Option<u32> {
            match v {
                serde_json::Value::String(n) => {
                    self.blocks.default_state(&mc_worldgen::density::full_id(n))
                }
                serde_json::Value::Object(o) => {
                    let name = mc_worldgen::density::full_id(o.get("Name")?.as_str()?);
                    let mut state = self.blocks.default_state(&name)?;
                    if let Some(serde_json::Value::Object(props)) = o.get("Properties") {
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
    let game_names = GameNames {
        blocks: &data.blocks,
        biomes: &index,
    };
    let mut generator = if full || dimension != Dimension::Overworld {
        let state = |n: &str| data.blocks.default_state(n).unwrap_or(0);
        Some(
            mc_worldgen::generator::NoiseGenerator::new(
                &source,
                seed,
                &settings,
                &biome_source,
                &game_names,
                index.clone(),
                mc_worldgen::generator::Blocks {
                    air: state("minecraft:air"),
                    stone: state("minecraft:stone"),
                    water: state("minecraft:water"),
                    lava: state("minecraft:lava"),
                },
            )
            .map_err(anyhow::Error::msg)?,
        )
    } else {
        None
    };
    // The reference world has no carvers (unless made with `--carvers`).
    if std::env::var_os("WORLDGEN_CARVERS").is_none()
        && let Some(g) = &mut generator
    {
        g.clear_carvers();
    }
    let mut block_pairs: HashMap<(String, String), usize> = HashMap::new();
    let (mut blocks_same, mut blocks_total) = (0usize, 0usize);
    // Differences per band of 8 y (`WORLDGEN_BY_Y`).
    let mut by_y = vec![0usize; (height / 8) as usize];
    let final_density = builder
        .build(&settings["noise_router"]["final_density"])
        .map_err(anyhow::Error::msg)?;
    if terrain && dimension != Dimension::Overworld {
        bail!("WORLDGEN_TERRAIN is for the overworld (its aquifers)");
    }
    let aq = if dimension == Dimension::Overworld {
        &settings["aquifers"]
    } else {
        &settings_overworld(&data_dir)?["aquifers"]
    };
    let functions = mc_worldgen::aquifer::Functions {
        barrier: builder.build(&aq["barrier"]).map_err(anyhow::Error::msg)?,
        floodedness: builder
            .build(&aq["fluid_level_floodedness"])
            .map_err(anyhow::Error::msg)?,
        spread: builder
            .build(&aq["fluid_level_spread"])
            .map_err(anyhow::Error::msg)?,
        lava: builder.build(&aq["lava"]).map_err(anyhow::Error::msg)?,
        exclusion: builder
            .build(&aq["exclusion"])
            .map_err(anyhow::Error::msg)?,
        surface: builder
            .build(&aq["surface_level"])
            .map_err(anyhow::Error::msg)?,
    };
    let aquifer_random = {
        use mc_worldgen::random::RandomSource as _;
        builder
            .factory()
            .from_hash_of("minecraft:aquifer")
            .fork_positional()
    };
    let global = mc_worldgen::aquifer::Global {
        sea_level: 63,
        sea: mc_worldgen::aquifer::Fluid::Water,
        lava_level: -54,
    };
    if std::env::var_os("WORLDGEN_DENSITY").is_some() {
        let base = builder
            .named("minecraft:nether/base_3d_noise")
            .map_err(anyhow::Error::msg)?;
        for y in (min_y..min_y + height).step_by(8) {
            println!(
                "y {y}: final {:.4} base {:.4}",
                final_density.eval(0, y, 0),
                base.eval(0, y, 0)
            );
        }
    }
    let mut kinds: HashMap<(char, char), usize> = HashMap::new();
    let (mut solid_same, mut solid_total, mut too_much, mut too_little) =
        (0usize, 0usize, 0usize, 0usize);
    for cx in ox - radius..=ox + radius {
        for cz in oz - radius..=oz + radius {
            let Some(tag) = storage.read_chunk(cx, cz)? else {
                continue;
            };
            let Decoded::Chunk(chunk) = decode_chunk(&tag, min_y, height as u32, &palettes)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?
            else {
                continue;
            };
            let before = total - same;
            read += 1;
            if let Some(generator) = generator.as_ref().filter(|_| full) {
                let ours = generator.generate(cx, cz);
                if std::env::var_os("WORLDGEN_COLUMN").is_some() && (cx, cz) == (ox, oz) {
                    // The corner column, bottom up: s solid, . air, ~ fluid.
                    let kind = |s: u32| match data.blocks.block_name(s).unwrap_or("") {
                        "minecraft:air" | "minecraft:cave_air" => '.',
                        "minecraft:water" | "minecraft:lava" => '~',
                        _ => 's',
                    };
                    let column = |f: &dyn Fn(i32) -> u32| -> String {
                        (min_y..min_y + height).map(|y| kind(f(y))).collect()
                    };
                    println!("vanilla {}", column(&|y| chunk.block(0, y, 0)));
                    println!(
                        "ours    {}",
                        column(&|y| ours.blocks[((y - min_y) * 256) as usize])
                    );
                }
                for y in min_y..min_y + height {
                    for lz in 0..16 {
                        for lx in 0..16 {
                            let theirs = chunk.block(lx, y, lz);
                            let mine = ours.blocks
                                [(((y - min_y) * 16 + lz as i32) * 16 + lx as i32) as usize];
                            blocks_total += 1;
                            if theirs == mine {
                                blocks_same += 1;
                            } else {
                                let name = |s: u32| {
                                    data.blocks
                                        .describe(s)
                                        .unwrap_or("?")
                                        .trim_start_matches("minecraft:")
                                        .replace("[]", "")
                                };
                                *block_pairs.entry((name(theirs), name(mine))).or_default() += 1;
                                by_y[((y - min_y) / 8) as usize] += 1;
                            }
                        }
                    }
                }
            }
            if terrain && !full {
                let mut ctx = mc_worldgen::density::Ctx::new(cx * 16, cz * 16, -64, 384);
                let mut aquifer = mc_worldgen::aquifer::Aquifer::new(
                    &functions,
                    global,
                    aquifer_random,
                    cx,
                    cz,
                    -64,
                    384,
                );
                for y in -64..320 {
                    for lz in 0..16 {
                        for lx in 0..16 {
                            let (x, z) = (cx * 16 + lx, cz * 16 + lz);
                            let density = final_density.eval_in(x, y, z, &mut ctx);
                            let ours = match aquifer.substance(x, y, z, density, &mut ctx) {
                                None => 's',
                                Some(mc_worldgen::aquifer::Fluid::Air) => 'a',
                                Some(mc_worldgen::aquifer::Fluid::Water) => 'w',
                                Some(mc_worldgen::aquifer::Fluid::Lava) => 'l',
                            };
                            let s = chunk.block(lx as usize, y, lz as usize);
                            let name = data.blocks.block_name(s).unwrap_or("");
                            let theirs = match name {
                                "minecraft:air" | "minecraft:cave_air" => 'a',
                                "minecraft:water" => 'w',
                                "minecraft:lava" => 'l',
                                _ if data.blocks.property(s, "waterlogged") == Some("true") => 'w',
                                _ => 's',
                            };
                            solid_total += 1;
                            if ours == theirs {
                                solid_same += 1;
                            } else {
                                if ours == 's' {
                                    too_much += 1;
                                } else if theirs == 's' {
                                    too_little += 1;
                                }
                                *kinds.entry((theirs, ours)).or_default() += 1;
                            }
                        }
                    }
                }
                continue;
            }
            if std::env::var_os("WORLDGEN_SURFACE").is_some() {
                // Our surface where depth crosses 0, against vanilla's top
                // solid block, at the chunk's corner column.
                let (x, z) = (cx * 16, cz * 16);
                let mut theirs = -64;
                for y in (-64..320).rev() {
                    let s = chunk.block(0, y, 0);
                    let name = data.blocks.block_name(s).unwrap_or("");
                    if name != "minecraft:air" && name != "minecraft:water" {
                        theirs = y;
                        break;
                    }
                }
                let offset = depth_offset(&climate, x, z);
                let ours = (1.5 + offset) * 128.0 - 64.0;
                println!(
                    "chunk {cx} {cz}: vanilla top {theirs}, ours depth-0 at {ours:.1} (offset {offset:.4})"
                );
            }
            for (s, section) in chunk.sections.iter().enumerate() {
                for i in 0..64 {
                    let (lx, ly, lz) = (i & 3, (i >> 4) & 3, (i >> 2) & 3);
                    let theirs = names[section.biomes.get(i) as usize].clone();
                    let qy = (min_y / 4) + (s as i32) * 4 + ly as i32;
                    let (qx, qz) = (cx * 4 + lx as i32, cz * 4 + lz as i32);
                    let target = climate.sample(qx, qy, qz);
                    let ours = match &generator {
                        Some(g) => names[g.noise_biome(qx, qy, qz) as usize].clone(),
                        None => list.find(target).to_owned(),
                    };
                    total += 1;
                    if ours == theirs {
                        same += 1;
                    } else {
                        let n = wrong.entry((theirs.clone(), ours.clone())).or_default();
                        *n += 1;
                        if *n <= 2 && std::env::var_os("WORLDGEN_CELLS").is_some() {
                            println!(
                                "cell q({}, {qy}, {}): {target:?} vanilla {theirs} d={} | ours {ours} d={}",
                                cx * 4 + lx as i32,
                                cz * 4 + lz as i32,
                                list.distance_to(target, &theirs),
                                list.distance_to(target, &ours)
                            );
                        }
                    }
                }
            }
            per_chunk.push((cx, cz, total - same - before));
        }
    }
    if read == 0 {
        bail!("the reference world has no chunk around {ox}, {oz} (run worldgen-ref first)");
    }
    if std::env::var_os("WORLDGEN_MAP").is_some() {
        for cz in oz - radius..=oz + radius {
            let row: String = (ox - radius..=ox + radius)
                .map(|cx| {
                    let n = per_chunk
                        .iter()
                        .find(|c| c.0 == cx && c.1 == cz)
                        .map_or(0, |c| c.2);
                    match n {
                        0 => '.',
                        1..=50 => '1',
                        51..=300 => '2',
                        _ => '#',
                    }
                })
                .collect();
            println!("{row}");
        }
    }
    if full {
        println!(
            "blocks: {blocks_same}/{blocks_total} the same ({:.3}%), {:.1} s",
            100.0 * blocks_same as f64 / blocks_total.max(1) as f64,
            started.elapsed().as_secs_f64()
        );
        if std::env::var_os("WORLDGEN_BY_Y").is_some() {
            for (i, n) in by_y.iter().enumerate() {
                println!("  y {:4}..: {n}", min_y + i as i32 * 8);
            }
        }
        let mut pairs: Vec<_> = block_pairs.into_iter().collect();
        pairs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((theirs, ours), n) in pairs.into_iter().take(25) {
            println!("  {n:8}  vanilla {theirs} | ours {ours}");
        }
    } else if terrain {
        println!(
            "terrain: {solid_same}/{solid_total} blocks solid alike ({:.3}%), {too_much} solid only in ours, {too_little} only in vanilla, {:.1} s",
            100.0 * solid_same as f64 / solid_total.max(1) as f64,
            started.elapsed().as_secs_f64()
        );
        let mut kinds: Vec<_> = kinds.into_iter().collect();
        kinds.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        for ((theirs, ours), n) in kinds {
            println!("  {n:8}  vanilla {theirs} | ours {ours}");
        }
        // Terrain mode compares what is solid, not the biomes.
        return Ok(Row {
            blocks: Some((solid_same, solid_total)),
            ..Row::default()
        });
    }
    println!(
        "biomes: {same}/{total} cells match ({:.1}%) in {:.1} s",
        100.0 * same as f64 / total.max(1) as f64,
        started.elapsed().as_secs_f64()
    );
    let mut wrong: Vec<_> = wrong.into_iter().collect();
    wrong.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for ((theirs, ours), n) in wrong.into_iter().take(12) {
        println!("  {n:6}  vanilla {theirs} | ours {ours}");
    }
    Ok(Row {
        blocks: full.then_some((blocks_same, blocks_total)),
        biomes: Some((same, total)),
        ..Row::default()
    })
}

/// `offset` at a column: depth minus its height gradient.
fn depth_offset(climate: &Climate, x: i32, z: i32) -> f64 {
    let y = 0;
    let gradient = 1.5 - 3.0 * f64::from(y + 64) / 384.0;
    climate.depth.eval(x, y, z) - gradient
}

/// The overworld's noise settings (for the aquifer functions the terrain
/// check builds).
fn settings_overworld(data_dir: &std::path::Path) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(
        data_dir.join("data/minecraft/worldgen/noise_settings/overworld.json"),
    )?)?)
}
