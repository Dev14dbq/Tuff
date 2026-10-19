//! `cargo xtask observe-structures [--seed <n>] [--monuments <n>] [--mansions <n>]`:
//! records ocean monuments and woodland mansions as a vanilla server builds
//! them, for our generator to build the same way.
//!
//! A vanilla world with that seed is asked where the nearest of each is
//! from points far apart (`locate structure`), the chunks around each are
//! force-loaded and saved, and then read back:
//! - a mansion keeps the list of its pieces in its start chunk (template,
//!   position, rotation, mirror): `observed/mansions.json`;
//! - a monument keeps only its outline, so its blocks are read inside it
//!   (prismarine, lanterns, sponges, gold, water; terrain left there is
//!   left alone), and the pillar columns under it: `observed/monuments.json`.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use mc_nbt::Tag;
use mc_storage::chunk::{Decoded, Palettes, decode_chunk};
use serde_json::{Value, json};

use crate::parity::Server;
use crate::probe::load_data;

const PORT: u16 = 25575;

pub fn run(args: &[String]) -> Result<()> {
    let mut seed = -1_294_266_004i64;
    let (mut monuments, mut mansions) = (8usize, 5usize);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--seed" => seed = value()?.parse()?,
            "--monuments" => monuments = value()?.parse()?,
            "--mansions" => mansions = value()?.parse()?,
            other => anyhow::bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let dir = root.join(format!("cache/vanilla-server/26.3-structures/{seed}"));
    let world = dir.join("world");
    let _ = std::fs::remove_dir_all(&world);
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-seed={seed}\nlevel-type=minecraft\\:normal\ngenerate-structures=true\n\
         view-distance=2\nsimulation-distance=2\nspawn-protection=0\n\
         pause-when-empty-seconds=0\nenable-status=true\nsync-chunk-writes=false\n"
    );
    Server::prepare(&root, &dir, &properties)?;
    let mut server = Server::start_java(&root, &dir)?;
    let found = (|| -> Result<_> {
        let monument_at = find(&mut server, "monument", monuments, 1600)?;
        let mansion_at = find(&mut server, "mansion", mansions, 9000)?;
        // Monuments reach 29 blocks from their chunk's corner; one more
        // chunk around for the ground under the pillars.
        for &(cx, cz) in &monument_at {
            load(&mut server, cx - 3, cz - 3, cx + 2, cz + 2)?;
        }
        for &(cx, cz) in &mansion_at {
            load(&mut server, cx, cz, cx, cz)?;
        }
        server.command("save-all flush", false)?;
        server.wait_for("", "Saved the game", Duration::from_secs(120))?;
        Ok((monument_at, mansion_at))
    })();
    server.stop();
    let (monument_at, mansion_at) = found?;
    let out = root.join("cache/mcdata/26.3/observed");
    std::fs::create_dir_all(&out)?;
    let mansions: Vec<Value> = mansion_at
        .iter()
        .filter_map(|&(cx, cz)| mansion(&world, cx, cz).transpose())
        .collect::<Result<_>>()?;
    std::fs::write(
        out.join("mansions.json"),
        serde_json::to_string(&json!({ "seed": seed, "mansions": mansions }))?,
    )?;
    let mut monuments: Vec<Value> = monument_at
        .iter()
        .filter_map(|&(cx, cz)| monument(&world, cx, cz).transpose())
        .collect::<Result<_>>()?;
    share_pillars(&mut monuments);
    std::fs::write(
        out.join("monuments.json"),
        serde_json::to_string(&json!({ "seed": seed, "monuments": monuments }))?,
    )?;
    println!(
        "{} mansions, {} monuments in {}",
        mansions.len(),
        monuments.len(),
        out.display()
    );
    Ok(())
}

/// Start chunks of up to `n` of a structure, the nearest to points `step`
/// blocks apart in a spiral around 0, 0.
fn find(server: &mut Server, name: &str, n: usize, step: i32) -> Result<Vec<(i32, i32)>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let points = [
        (0, 0),
        (1, 0),
        (0, 1),
        (-1, 0),
        (0, -1),
        (1, 1),
        (-1, 1),
        (-1, -1),
        (1, -1),
        (2, 0),
        (0, 2),
        (-2, 0),
        (0, -2),
        (2, 2),
        (-2, -2),
    ];
    for (i, j) in points {
        if out.len() >= n {
            break;
        }
        let (x, z) = (i * step, j * step);
        let line = server.wait_for(
            &format!("execute positioned {x} 0 {z} run locate structure minecraft:{name}"),
            "nearest minecraft:",
            Duration::from_secs(120),
        )?;
        // "The nearest minecraft:monument is at [688, ~, 240] (728 blocks away)"
        let inside = line
            .split_once('[')
            .and_then(|(_, r)| r.split_once(']'))
            .context("locate answer")?
            .0;
        let parts: Vec<&str> = inside.split(", ").collect();
        let (bx, bz): (i32, i32) = (parts[0].parse()?, parts[2].parse()?);
        let c = (bx >> 4, bz >> 4);
        if seen.insert(c) {
            println!("  {name} at chunk {}, {}", c.0, c.1);
            out.push(c);
        }
    }
    Ok(out)
}

fn load(server: &mut Server, x0: i32, z0: i32, x1: i32, z1: i32) -> Result<()> {
    server.command(
        &format!(
            "forceload add {} {} {} {}",
            x0 * 16,
            z0 * 16,
            x1 * 16 + 15,
            z1 * 16 + 15
        ),
        false,
    )?;
    let chunks = ((x1 - x0 + 1) * (z1 - z0 + 1)) as u64;
    std::thread::sleep(Duration::from_secs(4 + chunks / 3));
    server.command("forceload remove all", false)?;
    Ok(())
}

fn start(world: &Path, cx: i32, cz: i32, id: &str) -> Result<Option<mc_nbt::Compound>> {
    let mut s = mc_storage::region::RegionStorage::new(region(world));
    let Some(Tag::Compound(chunk)) = s.read_chunk(cx, cz)? else {
        return Ok(None);
    };
    let Some(Tag::Compound(structures)) = chunk.get("structures") else {
        return Ok(None);
    };
    let Some(Tag::Compound(starts)) = structures.get("starts") else {
        return Ok(None);
    };
    Ok(match starts.get(id) {
        Some(Tag::Compound(c)) => Some(c.clone()),
        _ => None,
    })
}

fn region(world: &Path) -> std::path::PathBuf {
    let d = world.join("dimensions/minecraft/overworld/region");
    if d.is_dir() { d } else { world.join("region") }
}

fn int(c: &mc_nbt::Compound, k: &str) -> i32 {
    match c.get(k) {
        Some(Tag::Int(v)) => *v,
        _ => 0,
    }
}

fn string<'a>(c: &'a mc_nbt::Compound, k: &str) -> &'a str {
    match c.get(k) {
        Some(Tag::String(v)) => v,
        _ => "",
    }
}

fn children(c: &mc_nbt::Compound) -> Vec<&mc_nbt::Compound> {
    match c.get("Children") {
        Some(Tag::List(l)) => l
            .iter()
            .filter_map(|t| match t {
                Tag::Compound(c) => Some(c),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn bbox(c: &mc_nbt::Compound) -> [i32; 6] {
    match c.get("BB") {
        Some(Tag::IntArray(v)) if v.len() == 6 => [v[0], v[1], v[2], v[3], v[4], v[5]],
        _ => [0; 6],
    }
}

fn mansion(world: &Path, cx: i32, cz: i32) -> Result<Option<Value>> {
    let Some(s) = start(world, cx, cz, "minecraft:mansion")? else {
        return Ok(None);
    };
    let pieces: Vec<Value> = children(&s)
        .into_iter()
        .map(|p| {
            json!({
                "template": string(p, "Template"),
                "pos": [int(p, "TPX"), int(p, "TPY"), int(p, "TPZ")],
                "rotation": string(p, "Rot"),
                "mirror": string(p, "Mi"),
                "bbox": bbox(p),
            })
        })
        .collect();
    Ok(Some(json!({ "chunk": [cx, cz], "pieces": pieces })))
}

const MONUMENT_BLOCKS: [&str; 7] = [
    "minecraft:prismarine",
    "minecraft:prismarine_bricks",
    "minecraft:dark_prismarine",
    "minecraft:sea_lantern",
    "minecraft:gold_block",
    "minecraft:wet_sponge",
    "minecraft:water",
];

/// Plants the features grew in the monument's water afterwards.
const GROWN: [&str; 5] = [
    "minecraft:kelp",
    "minecraft:kelp_plant",
    "minecraft:seagrass",
    "minecraft:tall_seagrass",
    "minecraft:bubble_column",
];

fn monument(world: &Path, cx: i32, cz: i32) -> Result<Option<Value>> {
    let Some(s) = start(world, cx, cz, "minecraft:monument")? else {
        return Ok(None);
    };
    let Some(building) = children(&s).into_iter().next() else {
        return Ok(None);
    };
    let bb = bbox(building);
    let facing = int(building, "O");
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
    let mut storage = mc_storage::region::RegionStorage::new(region(world));
    let mut chunks = HashMap::new();
    let mut block = |x: i32, y: i32, z: i32| -> Result<String> {
        let key = (x >> 4, z >> 4);
        if let std::collections::hash_map::Entry::Vacant(e) = chunks.entry(key) {
            e.insert(match storage.read_chunk(key.0, key.1)? {
                Some(t) => match decode_chunk(&t, -64, 384, &palettes) {
                    Ok(Decoded::Chunk(c)) => Some(c),
                    _ => None,
                },
                None => None,
            });
        }
        Ok(match &chunks[&key] {
            Some(c) => data
                .blocks
                .describe(c.block((x & 15) as usize, y, (z & 15) as usize))
                .unwrap_or("?")
                .to_owned(),
            None => "?".to_owned(),
        })
    };
    let mut palette: Vec<String> = Vec::new();
    let mut index: HashMap<String, i64> = HashMap::new();
    let mut id = |name: &str| -> i64 {
        *index.entry(name.to_owned()).or_insert_with(|| {
            palette.push(name.to_owned());
            palette.len() as i64 - 1
        })
    };
    let water = "minecraft:water[level=0]";
    let mut blocks = Vec::new();
    let mut skipped = 0;
    for y in bb[1]..=bb[4] {
        for z in bb[2]..=bb[5] {
            for x in bb[0]..=bb[3] {
                let name = block(x, y, z)?;
                let base = name.split('[').next().unwrap_or("");
                if GROWN.contains(&base) {
                    blocks.push(id(water));
                } else if MONUMENT_BLOCKS.contains(&base) {
                    blocks.push(id(&name));
                } else {
                    skipped += 1;
                    blocks.push(-1);
                }
            }
        }
    }
    let mut pillars = Vec::new();
    for z in bb[2]..=bb[5] {
        for x in bb[0]..=bb[3] {
            let name = block(x, bb[1] - 1, z)?;
            let base = name.split('[').next().unwrap_or("");
            if MONUMENT_BLOCKS[..6].contains(&base) {
                pillars.push(json!([x - bb[0], z - bb[2], id(&name)]));
            }
        }
    }
    println!(
        "  monument at chunk {cx}, {cz}: {} pillar columns, {skipped} terrain blocks left",
        pillars.len()
    );
    Ok(Some(json!({
        "chunk": [cx, cz],
        "bbox": bb,
        "facing": facing,
        "palette": palette,
        "blocks": blocks,
        "pillars": pillars,
    })))
}

const SIDE: i32 = 58;
const TALL: i32 = 23;

/// Where `x, z` of a monument lands after mirroring (`mirror`) and `turns`
/// quarter turns.
fn turn(x: i32, z: i32, turns: u8, mirror: bool) -> (i32, i32) {
    let (mut x, mut z) = (if mirror { SIDE - 1 - x } else { x }, z);
    for _ in 0..turns {
        (x, z) = (SIDE - 1 - z, x);
    }
    (x, z)
}

fn names(m: &Value) -> Vec<String> {
    let palette: Vec<&str> = m["palette"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    m["blocks"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| match v.as_i64() {
                    Some(i) if i >= 0 => palette[i as usize]
                        .split('[')
                        .next()
                        .unwrap_or("")
                        .to_owned(),
                    _ => String::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pillars reach the sea floor wherever it is lower than the monument, so
/// each monument only shows those over deep water. The building is the same
/// in all of them: each is turned onto the first, the pillars of all are
/// put together and handed back to each.
fn share_pillars(monuments: &mut [Value]) {
    let Some(first) = monuments.first() else {
        return;
    };
    let base = names(first);
    let at = |x: i32, y: i32, z: i32| ((y * SIDE + z) * SIDE + x) as usize;
    let mut fits = Vec::new();
    for m in monuments.iter() {
        let own = names(m);
        let mut best = (0usize, 0u8, false);
        for turns in 0..4 {
            for mirror in [false, true] {
                let mut same = 0;
                for y in 0..TALL {
                    for z in 0..SIDE {
                        for x in 0..SIDE {
                            let (bx, bz) = turn(x, z, turns, mirror);
                            if own[at(x, y, z)] == base[at(bx, y, bz)] {
                                same += 1;
                            }
                        }
                    }
                }
                if same > best.0 {
                    best = (same, turns, mirror);
                }
            }
        }
        fits.push((best.1, best.2));
    }
    let mut all = BTreeSet::new();
    for (m, &(turns, mirror)) in monuments.iter().zip(&fits) {
        for p in m["pillars"].as_array().into_iter().flatten() {
            let (x, z) = (
                p[0].as_i64().unwrap_or(0) as i32,
                p[1].as_i64().unwrap_or(0) as i32,
            );
            all.insert(turn(x, z, turns, mirror));
        }
    }
    for (m, &(turns, mirror)) in monuments.iter_mut().zip(&fits) {
        let bricks = "minecraft:prismarine_bricks[]";
        let palette = m["palette"].as_array_mut().expect("palette");
        let id = match palette.iter().position(|v| v.as_str() == Some(bricks)) {
            Some(i) => i,
            None => {
                palette.push(json!(bricks));
                palette.len() - 1
            }
        };
        let mut pillars = Vec::new();
        for z in 0..SIDE {
            for x in 0..SIDE {
                if all.contains(&turn(x, z, turns, mirror)) {
                    pillars.push(json!([x, z, id]));
                }
            }
        }
        m["pillars"] = json!(pillars);
    }
    println!("  {} pillar columns under each monument", all.len());
}
