//! `cargo xtask chunk-diff <dump a> <dump b>`: compares Chunk Data packets
//! saved by `probe --dump` from two servers (e.g. vanilla and ours serving the
//! same world). Sections are decoded to block state and biome ids, so palette
//! order and bit width do not matter, only content. Heightmaps, light arrays
//! and block entity positions are compared too.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mc_protocol::Reader;
use mc_world::BitStorage;

#[derive(Debug, PartialEq)]
struct ChunkContent {
    blocks: Vec<Vec<u32>>,
    biomes: Vec<Vec<u32>>,
    heightmaps: BTreeMap<i32, Vec<i64>>,
    sky: BTreeMap<usize, Vec<u8>>,
    block_light: BTreeMap<usize, Vec<u8>>,
    /// Position, type, and the keys of the data the client gets.
    block_entities: Vec<(u8, i16, i32, String)>,
}

pub fn run(args: &[String]) -> Result<()> {
    let [a, b] = args else {
        bail!("usage: chunk-diff <dump dir a> <dump dir b>");
    };
    let (a, b) = (load(Path::new(a))?, load(Path::new(b))?);
    let common: Vec<_> = a.keys().filter(|k| b.contains_key(k)).collect();
    println!(
        "{} chunks in a, {} in b, {} in both",
        a.len(),
        b.len(),
        common.len()
    );
    let mut identical = 0;
    for key in &common {
        let (x, y) = (&a[key], &b[key]);
        let mut diffs = Vec::new();
        for (i, (sa, sb)) in x.blocks.iter().zip(&y.blocks).enumerate() {
            let differing: Vec<usize> = (0..sa.len()).filter(|&j| sa[j] != sb[j]).collect();
            if let Some(&first) = differing.first() {
                let (bx, by, bz) = (first & 15, first >> 8, (first >> 4) & 15);
                diffs.push(format!(
                    "section {i}: {} blocks differ (first at {bx},{by},{bz}: state {} vs {})",
                    differing.len(),
                    sa[first],
                    sb[first]
                ));
            }
        }
        if x.blocks.len() != y.blocks.len() {
            diffs.push(format!("{} vs {} sections", x.blocks.len(), y.blocks.len()));
        }
        if x.biomes != y.biomes {
            diffs.push("biomes differ".into());
        }
        if x.heightmaps != y.heightmaps {
            diffs.push(format!(
                "heightmaps {:?} vs {:?}",
                x.heightmaps.keys().collect::<Vec<_>>(),
                y.heightmaps.keys().collect::<Vec<_>>()
            ));
        }
        if x.sky != y.sky {
            diffs.push(format!(
                "sky light sections {:?} vs {:?}",
                x.sky.keys().collect::<Vec<_>>(),
                y.sky.keys().collect::<Vec<_>>()
            ));
        }
        if x.block_light != y.block_light {
            diffs.push(format!(
                "block light sections {:?} vs {:?}",
                x.block_light.keys().collect::<Vec<_>>(),
                y.block_light.keys().collect::<Vec<_>>()
            ));
        }
        if x.block_entities != y.block_entities {
            diffs.push(format!(
                "block entities {:?} vs {:?}",
                x.block_entities, y.block_entities
            ));
        }
        if diffs.is_empty() {
            identical += 1;
        } else {
            println!("chunk {key:?}: {}", diffs.join("; "));
        }
    }
    println!("{identical}/{} common chunks identical", common.len());
    Ok(())
}

fn load(dir: &Path) -> Result<BTreeMap<(i32, i32), ChunkContent>> {
    let mut chunks = BTreeMap::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let path = entry?.path();
        if !path
            .to_string_lossy()
            .ends_with("level_chunk_with_light.bin")
        {
            continue;
        }
        let body = std::fs::read(&path)?;
        let (key, content) = decode(&body).with_context(|| path.display().to_string())?;
        chunks.insert(key, content);
    }
    Ok(chunks)
}

fn describe_keys(tag: Option<&mc_nbt::Tag>) -> String {
    match tag {
        None => "-".into(),
        Some(mc_nbt::Tag::Compound(c)) => {
            let mut keys: Vec<&str> = c.iter().map(|(k, _)| k).collect();
            keys.sort_unstable();
            format!("{{{}}}", keys.join(","))
        }
        Some(other) => format!("{other:?}"),
    }
}

fn container(
    r: &mut Reader,
    entries: usize,
    indirect: std::ops::RangeInclusive<u8>,
) -> Result<Vec<u32>> {
    let bits = r.u8()?;
    if bits == 0 {
        return Ok(vec![r.varint()? as u32; entries]);
    }
    let palette: Option<Vec<u32>> = if indirect.contains(&bits) {
        Some(
            (0..r.length_prefix()?)
                .map(|_| r.varint().map(|v| v as u32))
                .collect::<Result<_, _>>()?,
        )
    } else {
        None
    };
    let words = BitStorage::word_count(u32::from(bits), entries);
    let longs: Vec<i64> = (0..words).map(|_| r.i64()).collect::<Result<_, _>>()?;
    let storage =
        BitStorage::from_words(u32::from(bits), entries, longs).context("bad data array")?;
    (0..entries)
        .map(|i| {
            let v = storage.get(i);
            match &palette {
                Some(p) => p.get(v as usize).copied().context("index past the palette"),
                None => Ok(v),
            }
        })
        .collect()
}

fn decode(body: &[u8]) -> Result<((i32, i32), ChunkContent)> {
    let mut r = Reader::new(body);
    let key = (r.i32()?, r.i32()?);
    let mut heightmaps = BTreeMap::new();
    for _ in 0..r.length_prefix()? {
        let kind = r.varint()?;
        let words: Vec<i64> = (0..r.length_prefix()?)
            .map(|_| r.i64())
            .collect::<Result<_, _>>()?;
        heightmaps.insert(kind, words);
    }
    let data = r.byte_array(1 << 21)?.to_vec();
    let mut s = Reader::new(&data);
    let (mut blocks, mut biomes) = (Vec::new(), Vec::new());
    while s.remaining() > 0 {
        s.i16()?;
        s.i16()?;
        blocks.push(container(&mut s, 4096, 4..=8)?);
        biomes.push(container(&mut s, 64, 1..=3)?);
    }
    let mut block_entities = Vec::new();
    for _ in 0..r.length_prefix()? {
        let (xz, y, kind) = (r.u8()?, r.i16()?, r.varint()?);
        let data = r.nbt_optional()?;
        block_entities.push((xz, y, kind, describe_keys(data.as_ref())));
    }
    block_entities.sort_unstable();
    let masks: Vec<Vec<u8>> = (0..4)
        .map(|_| r.byte_array(1024).map(<[u8]>::to_vec))
        .collect::<Result<_, _>>()?;
    let bits = |mask: &[u8]| -> Vec<usize> {
        (0..mask.len() * 8)
            .filter(|&i| mask[i / 8] & (1 << (i % 8)) != 0)
            .collect()
    };
    let mut arrays = |mask: &[u8]| -> Result<BTreeMap<usize, Vec<u8>>> {
        let count = r.length_prefix()?;
        let sections = bits(mask);
        if sections.len() != count {
            bail!("mask has {} bits but {count} arrays follow", sections.len());
        }
        sections
            .into_iter()
            .map(|section| Ok((section, r.byte_array(2048)?.to_vec())))
            .collect()
    };
    let sky = arrays(&masks[0])?;
    let block_light = arrays(&masks[1])?;
    Ok((
        key,
        ChunkContent {
            blocks,
            biomes,
            heightmaps,
            sky,
            block_light,
            block_entities,
        },
    ))
}
