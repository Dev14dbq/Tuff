//! `cargo xtask worldgen-compare <vanilla world> <our world> [--radius n]
//! [--center x z] [--dimension nether|end]`:
//! compares two worlds chunk by chunk around 0, 0: how many blocks are the
//! same, which blocks differ most, and how often each block occurs in
//! each (features: trees, plants, ores).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Result, bail};
use mc_storage::chunk::{Decoded, Palettes, decode_chunk};

use crate::probe::load_data;

fn region(world: &Path) -> std::path::PathBuf {
    let d = world.join("dimensions/minecraft/overworld/region");
    if d.is_dir() { d } else { world.join("region") }
}

/// The region folder of another dimension, and its lowest y and height.
fn dimension_region(world: &Path, dimension: &str) -> Result<(std::path::PathBuf, i32, i32)> {
    Ok(match dimension {
        "overworld" => (region(world), -64, 384),
        "nether" | "the_nether" => (world.join("dimensions/minecraft/the_nether/region"), 0, 128),
        "end" | "the_end" => (world.join("dimensions/minecraft/the_end/region"), 0, 128),
        other => bail!("unknown dimension `{other}`"),
    })
}

pub fn run(args: &[String]) -> Result<()> {
    let mut paths = Vec::new();
    let mut radius = 4;
    let (mut ox, mut oz) = (0i32, 0i32);
    let mut dimension = String::from("overworld");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--radius" => radius = it.next().map_or(Ok(4), |v| v.parse())?,
            "--dimension" => dimension = it.next().cloned().unwrap_or_default(),
            "--center" => {
                ox = it.next().map_or(Ok(0), |v| v.parse())?;
                oz = it.next().map_or(Ok(0), |v| v.parse())?;
            }
            _ => paths.push(std::path::PathBuf::from(a)),
        }
    }
    let [theirs, ours] = &paths[..] else {
        bail!("usage: worldgen-compare <vanilla world> <our world> [--radius n]");
    };
    let data = load_data(None)?;
    let air = data.blocks.default_state("minecraft:air").unwrap_or(0);
    let lookup = |_: &str| Some(0u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &lookup,
        air,
        fallback_biome: 0,
        unresolved: std::cell::Cell::new(0),
    };
    let (region_a, min_y, height) = dimension_region(theirs, &dimension)?;
    let (region_b, ..) = dimension_region(ours, &dimension)?;
    let mut a = mc_storage::region::RegionStorage::new(region_a);
    let mut b = mc_storage::region::RegionStorage::new(region_b);
    let name = |s: u32| {
        data.blocks
            .block_name(s)
            .unwrap_or("?")
            .trim_start_matches("minecraft:")
            .to_owned()
    };
    let (mut same, mut total) = (0usize, 0usize);
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut pairs: HashMap<(String, String), usize> = HashMap::new();
    let mut chunks = 0;
    // `PAIR=vanilla,ours`: where that difference is, by y.
    let pair = std::env::var("PAIR").ok();
    let mut pair_y: BTreeMap<i32, usize> = BTreeMap::new();
    let mut pair_chunks: BTreeMap<(i32, i32), usize> = BTreeMap::new();
    for cz in oz - radius..=oz + radius {
        for cx in ox - radius..=ox + radius {
            let (Some(ta), Some(tb)) = (a.read_chunk(cx, cz)?, b.read_chunk(cx, cz)?) else {
                continue;
            };
            let (Ok(Decoded::Chunk(ca)), Ok(Decoded::Chunk(cb))) = (
                decode_chunk(&ta, min_y, height as u32, &palettes),
                decode_chunk(&tb, min_y, height as u32, &palettes),
            ) else {
                continue;
            };
            chunks += 1;
            for y in min_y..min_y + height {
                for lz in 0..16 {
                    for lx in 0..16 {
                        let (sa, sb) = (ca.block(lx, y, lz), cb.block(lx, y, lz));
                        let (na, nb) = (name(sa), name(sb));
                        total += 1;
                        if na == nb {
                            same += 1;
                        } else {
                            if pair.as_deref() == Some(&format!("{na},{nb}")) {
                                *pair_y.entry(y).or_default() += 1;
                                *pair_chunks.entry((cx, cz)).or_default() += 1;
                                if pair_y.values().sum::<usize>() <= 12 {
                                    println!(
                                        "  at {} {y} {}",
                                        cx * 16 + lx as i32,
                                        cz * 16 + lz as i32
                                    );
                                }
                            }
                            *pairs.entry((na.clone(), nb.clone())).or_default() += 1;
                        }
                        if na != "air" && na != "stone" && na != "deepslate" {
                            counts.entry(na).or_default().0 += 1;
                        }
                        if nb != "air" && nb != "stone" && nb != "deepslate" {
                            counts.entry(nb).or_default().1 += 1;
                        }
                    }
                }
            }
        }
    }
    println!(
        "{chunks} chunks: {same}/{total} blocks the same ({:.3}%)",
        same as f64 * 100.0 / total.max(1) as f64
    );
    if pair.is_some() {
        println!("by y: {pair_y:?}");
        let mut top: Vec<_> = pair_chunks.into_iter().collect();
        top.sort_by_key(|c| std::cmp::Reverse(c.1));
        println!("by chunk: {:?}", &top[..top.len().min(12)]);
    }
    let mut list: Vec<_> = pairs.into_iter().collect();
    list.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("most different (vanilla → ours):");
    for ((x, y), n) in list.iter().take(
        std::env::var("TOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
    ) {
        println!("{n:>9}  {x} → {y}");
    }
    println!("{:>30} {:>9} {:>9}", "block", "vanilla", "ours");
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by_key(|x| std::cmp::Reverse(x.1.0 + x.1.1));
    for (n, (va, vb)) in rows.iter().take(90) {
        let mark = if *va == 0 || *vb == 0 || (*va as f64 / *vb as f64 - 1.0).abs() > 0.25 {
            " <"
        } else {
            ""
        };
        println!("{n:>30} {va:>9} {vb:>9}{mark}");
    }
    Ok(())
}

/// `cargo xtask worldgen-logs <vanilla> <ours> <cx> <cz>`: tree logs in one
/// chunk of each world.
pub fn logs(args: &[String]) -> Result<()> {
    let data = load_data(None)?;
    let air = data.blocks.default_state("minecraft:air").unwrap_or(0);
    let lookup = |_: &str| Some(0u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &lookup,
        air,
        fallback_biome: 0,
        unresolved: std::cell::Cell::new(0),
    };
    let (cx, cz): (i32, i32) = (args[2].parse()?, args[3].parse()?);
    for w in &args[..2] {
        let mut s = mc_storage::region::RegionStorage::new(region(Path::new(w)));
        let Some(t) = s.read_chunk(cx, cz)? else {
            continue;
        };
        let Ok(Decoded::Chunk(c)) = decode_chunk(&t, -64, 384, &palettes) else {
            continue;
        };
        let mut cols: BTreeMap<(usize, usize), Vec<(i32, String)>> = BTreeMap::new();
        let want = std::env::var("BLOCK").unwrap_or_else(|_| "_log".into());
        for y in -64..320 {
            for lz in 0..16 {
                for lx in 0..16 {
                    let n = data
                        .blocks
                        .describe(c.block(lx, y, lz))
                        .unwrap_or("?")
                        .to_owned();
                    if n.contains(&want) {
                        cols.entry((lx, lz))
                            .or_default()
                            .push((y, n.trim_start_matches("minecraft:").to_owned()));
                    }
                }
            }
        }
        println!("== {w}");
        for ((x, z), list) in cols {
            let ys: Vec<String> = list.iter().map(|(y, n)| format!("{y}:{n}")).collect();
            println!("  {x:>2},{z:>2}: {}", ys.join(" "));
        }
    }
    Ok(())
}

/// `cargo xtask worldgen-map <world> <radius> <out.ppm>`: top blocks of
/// the chunks around 0, 0, coloured roughly.
pub fn map(args: &[String]) -> Result<()> {
    let data = load_data(None)?;
    let air = data.blocks.default_state("minecraft:air").unwrap_or(0);
    let lookup = |_: &str| Some(0u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &lookup,
        air,
        fallback_biome: 0,
        unresolved: std::cell::Cell::new(0),
    };
    let radius: i32 = args[1].parse()?;
    let (ox, oz): (i32, i32) = match (args.get(3), args.get(4)) {
        (Some(a), Some(b)) => (a.parse()?, b.parse()?),
        _ => (0, 0),
    };
    let size = (radius * 2 + 1) * 16;
    let mut img = vec![0u8; (size * size * 3) as usize];
    let mut s = mc_storage::region::RegionStorage::new(region(Path::new(&args[0])));
    for cz in oz - radius..=oz + radius {
        for cx in ox - radius..=ox + radius {
            let Some(t) = s.read_chunk(cx, cz)? else {
                continue;
            };
            let Ok(Decoded::Chunk(c)) = decode_chunk(&t, -64, 384, &palettes) else {
                continue;
            };
            for lz in 0..16 {
                for lx in 0..16 {
                    for y in (-64..320).rev() {
                        let st = c.block(lx, y, lz);
                        if data.blocks.is_air(st) {
                            continue;
                        }
                        let n = data
                            .blocks
                            .block_name(st)
                            .unwrap_or("?")
                            .trim_start_matches("minecraft:");
                        let col: [u8; 3] = match n {
                            "grass_block" => [95, 159, 53],
                            "water" => [52, 90, 210],
                            "sand" | "sandstone" => [219, 207, 163],
                            "stone" | "andesite" | "diorite" | "granite" | "gravel" => {
                                [125, 125, 125]
                            }
                            "snow" | "snow_block" => [250, 250, 250],
                            "ice" | "packed_ice" => [160, 190, 250],
                            "dirt" | "coarse_dirt" | "podzol" => [134, 96, 67],
                            "birch_leaves" => [130, 170, 70],
                            _ if n.ends_with("_leaves") => [40, 110, 30],
                            _ if n.ends_with("_log") => [100, 70, 40],
                            "short_grass" | "tall_grass" | "fern" | "bush" => [80, 180, 50],
                            "leaf_litter" => [170, 110, 40],
                            "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant" => [40, 140, 120],
                            _ => [230, 60, 200],
                        };
                        let shade = ((y - 40).clamp(0, 120) as f32 / 120.0) * 0.6 + 0.6;
                        let (ix, iz) = (
                            (cx - ox + radius) * 16 + lx as i32,
                            (cz - oz + radius) * 16 + lz as i32,
                        );
                        let i = ((iz * size + ix) * 3) as usize;
                        for k in 0..3 {
                            img[i + k] = (f32::from(col[k]) * shade).min(255.0) as u8;
                        }
                        break;
                    }
                }
            }
        }
    }
    let mut out = format!("P6\n{size} {size}\n255\n").into_bytes();
    out.extend_from_slice(&img);
    std::fs::write(&args[2], out)?;
    Ok(())
}

/// `cargo xtask worldgen-find <world> <block> <radius> [cx cz]`: where a block is,
/// grouped by chunk (count and bounds).
pub fn find(args: &[String]) -> Result<()> {
    let data = load_data(None)?;
    let air = data.blocks.default_state("minecraft:air").unwrap_or(0);
    let lookup = |_: &str| Some(0u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &lookup,
        air,
        fallback_biome: 0,
        unresolved: std::cell::Cell::new(0),
    };
    let want = &args[1];
    let radius: i32 = args[2].parse()?;
    let (ox, oz): (i32, i32) = match (args.get(3), args.get(4)) {
        (Some(a), Some(b)) => (a.parse()?, b.parse()?),
        _ => (0, 0),
    };
    let mut s = mc_storage::region::RegionStorage::new(region(Path::new(&args[0])));
    for cz in oz - radius..=oz + radius {
        for cx in ox - radius..=ox + radius {
            let Some(t) = s.read_chunk(cx, cz)? else {
                continue;
            };
            let Ok(Decoded::Chunk(c)) = decode_chunk(&t, -64, 384, &palettes) else {
                continue;
            };
            let mut n = 0;
            let mut lo = (i32::MAX, i32::MAX, i32::MAX);
            let mut hi = (i32::MIN, i32::MIN, i32::MIN);
            for y in -64..320 {
                for lz in 0..16 {
                    for lx in 0..16 {
                        let name = data.blocks.block_name(c.block(lx, y, lz)).unwrap_or("?");
                        if name.trim_start_matches("minecraft:") == want {
                            n += 1;
                            let (x, z) = (cx * 16 + lx as i32, cz * 16 + lz as i32);
                            lo = (lo.0.min(x), lo.1.min(y), lo.2.min(z));
                            hi = (hi.0.max(x), hi.1.max(y), hi.2.max(z));
                        }
                    }
                }
            }
            if n > 0 {
                println!("chunk {cx},{cz}: {n} from {lo:?} to {hi:?}");
            }
        }
    }
    Ok(())
}

/// `cargo xtask worldgen-slice <vanilla> <ours> <dimension> <x> <y> <z>
/// [radius] [x|z]`: one layer of blocks around a position in both worlds,
/// side by side, a letter per block (horizontal, or vertical along x or z).
pub fn slice(args: &[String]) -> Result<()> {
    let [theirs, ours, dimension, x, y, z, rest @ ..] = args else {
        bail!("usage: worldgen-slice <vanilla> <ours> <dimension> <x> <y> <z> [radius] [x|z]");
    };
    let (x, y, z): (i32, i32, i32) = (x.parse()?, y.parse()?, z.parse()?);
    let radius: i32 = rest.first().map_or(Ok(6), |v| v.parse())?;
    let vertical = rest.get(1).map(String::as_str);
    let data = load_data(None)?;
    let air = data.blocks.default_state("minecraft:air").unwrap_or(0);
    let lookup = |_: &str| Some(0u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &lookup,
        air,
        fallback_biome: 0,
        unresolved: std::cell::Cell::new(0),
    };
    let mut worlds = Vec::new();
    for w in [theirs, ours] {
        let (dir, min_y, height) = dimension_region(Path::new(w), dimension)?;
        worlds.push((mc_storage::region::RegionStorage::new(dir), min_y, height));
    }
    let mut chunks: HashMap<(usize, i32, i32), Option<mc_world::Chunk>> = HashMap::new();
    let mut legend: BTreeMap<String, char> = BTreeMap::new();
    let letters: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    let mut block_at = |w: usize, bx: i32, by: i32, bz: i32| -> Result<char> {
        let (storage, min_y, height) = &mut worlds[w];
        let key = (w, bx >> 4, bz >> 4);
        #[allow(clippy::map_entry)]
        if !chunks.contains_key(&key) {
            let chunk = match storage.read_chunk(bx >> 4, bz >> 4)? {
                Some(t) => match decode_chunk(&t, *min_y, *height as u32, &palettes) {
                    Ok(Decoded::Chunk(c)) => Some(*c),
                    _ => None,
                },
                None => None,
            };
            chunks.insert(key, chunk);
        }
        let Some(chunk) = &chunks[&key] else {
            return Ok('?');
        };
        let name = data
            .blocks
            .block_name(chunk.block((bx & 15) as usize, by, (bz & 15) as usize))
            .unwrap_or("?")
            .trim_start_matches("minecraft:")
            .to_owned();
        if name == "air" || name == "cave_air" {
            return Ok('.');
        }
        let n = legend.len();
        Ok(*legend.entry(name).or_insert(letters[n % letters.len()]))
    };
    for row in -radius..=radius {
        let mut line = String::new();
        for w in 0..2 {
            for col in -radius..=radius {
                let (bx, by, bz) = match vertical {
                    Some("x") => (x + col, y - row, z),
                    Some("z") => (x, y - row, z + col),
                    _ => (x + col, y, z + row),
                };
                line.push(block_at(w, bx, by, bz)?);
            }
            line.push_str("   ");
        }
        println!("{line}");
    }
    let legend: Vec<String> = legend.iter().map(|(n, c)| format!("{c}={n}")).collect();
    println!("{}", legend.join(" "));
    Ok(())
}
