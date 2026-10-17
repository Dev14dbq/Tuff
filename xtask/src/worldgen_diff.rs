//! `cargo xtask worldgen-diff [--seed <n>] [--radius <chunks>]`: compares
//! our generator with the vanilla reference world (`worldgen-ref`): biomes
//! per 4×4×4 cell for now.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use mc_storage::chunk::{Decoded, Palettes, decode_chunk};
use mc_worldgen::climate::{BiomeList, Climate, DataDir};
use mc_worldgen::density::Builder;

use crate::probe::load_data;

pub fn run(args: &[String]) -> Result<()> {
    let (mut seed, mut radius) = (12345i64, 3i32);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--seed" => seed = value()?.parse()?,
            "--radius" => radius = value()?.parse()?,
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let data = load_data(None)?;
    let data_dir = root.join("cache/mcdata").join(&data.version.name);
    let source = DataDir(data_dir.clone());
    let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
        data_dir.join("data/minecraft/worldgen/noise_settings/overworld.json"),
    )?)?;
    let mut builder = Builder::new(&source, seed, false);
    let climate = Climate::from_router(&mut builder, &settings["noise_router"])
        .map_err(anyhow::Error::msg)?;
    let list = BiomeList::from_json(&serde_json::from_str(&std::fs::read_to_string(
        data_dir.join("reports/biome_parameters/minecraft/overworld.json"),
    )?)?);
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
    let region = root.join(format!(
        "cache/vanilla-server/26.3-worldgen/{seed}/world/dimensions/minecraft/overworld/region"
    ));
    let mut storage = mc_storage::region::RegionStorage::new(region);
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
    for cx in -radius..=radius {
        for cz in -radius..=radius {
            let Some(tag) = storage.read_chunk(cx, cz)? else {
                continue;
            };
            let Decoded::Chunk(chunk) =
                decode_chunk(&tag, -64, 384, &palettes).map_err(|e| anyhow::anyhow!("{e:?}"))?
            else {
                continue;
            };
            let before = total - same;
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
                    let qy = (-64 / 4) + (s as i32) * 4 + ly as i32;
                    let target = climate.sample(cx * 4 + lx as i32, qy, cz * 4 + lz as i32);
                    let ours = list.find(target).to_owned();
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
    if std::env::var_os("WORLDGEN_MAP").is_some() {
        for cz in -radius..=radius {
            let row: String = (-radius..=radius)
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
    Ok(())
}

/// `offset` at a column: depth minus its height gradient.
fn depth_offset(climate: &Climate, x: i32, z: i32) -> f64 {
    let y = 0;
    let gradient = 1.5 - 3.0 * f64::from(y + 64) / 384.0;
    climate.depth.eval(x, y, z) - gradient
}
