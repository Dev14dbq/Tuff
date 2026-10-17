//! Decodes worlds saved by a vanilla 26.3 server, if available under
//! cache/vanilla-server/26.3/: `world-normal` (zlib, the default) and
//! `world-lz4` (`region-file-compression=lz4`). See docs/specs/world-26.3.md.

use std::path::Path;

use mc_storage::{Decoded, Names, Palettes, RegionFile, RegionStorage, decode_chunk, encode_chunk};

#[test]
fn decodes_every_chunk_of_a_vanilla_world() {
    decode_world("world-normal", 256);
}

#[test]
fn decodes_every_chunk_of_an_lz4_world() {
    decode_world("world-lz4", 64);
}

/// Decodes, re-encodes and decodes again every chunk of a test world; at least
/// `min_full` chunks must be fully generated.
fn decode_world(name: &str, min_full: usize) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data_dir = root.join("cache/mcdata/26.3");
    let regions = root.join(format!(
        "cache/vanilla-server/26.3/{name}/dimensions/minecraft/overworld/region"
    ));
    if !data_dir.is_dir() || !regions.is_dir() {
        eprintln!("skipped: no extracted data or vanilla test world {name}");
        return;
    }
    let data = mc_data::McData::load(&data_dir).unwrap();
    let biomes = data
        .data_pack()
        .registry_entries("minecraft:worldgen/biome")
        .unwrap();
    let biome_id = |name: &str| biomes.iter().position(|b| b == name).map(|i| i as u32);
    let palettes = Palettes {
        blocks: &data.blocks,
        biome: &biome_id,
        air: data.blocks.default_state("minecraft:air").unwrap(),
        fallback_biome: biome_id("minecraft:plains").unwrap(),
        unresolved: Default::default(),
    };
    let bedrock = data.blocks.default_state("minecraft:bedrock").unwrap();
    let names = Names {
        blocks: &data.blocks,
        biomes: &biomes,
        data_version: 5023,
    };

    let mut storage = RegionStorage::new(&regions);
    let (mut full, mut partial, mut with_block_entities) = (0, 0, 0);
    for entry in std::fs::read_dir(&regions).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let mut parts = name
            .trim_start_matches("r.")
            .trim_end_matches(".mca")
            .split('.');
        let (rx, rz): (i32, i32) = (
            parts.next().unwrap().parse().unwrap(),
            parts.next().unwrap().parse().unwrap(),
        );
        for (lx, lz) in RegionFile::open(&path).unwrap().chunks() {
            let (x, z) = (rx * 32 + lx as i32, rz * 32 + lz as i32);
            let tag = storage.read_chunk(x, z).unwrap().unwrap();
            match decode_chunk(&tag, -64, 384, &palettes).unwrap() {
                Decoded::Chunk(chunk) => {
                    full += 1;
                    assert_eq!((chunk.x, chunk.z), (x, z));
                    assert_eq!(chunk.sections.len(), 24);
                    // Vanilla puts bedrock at the bottom of every overworld column.
                    let bottom_bedrock = (0..16)
                        .flat_map(|bx| (0..16).map(move |bz| (bx, bz)))
                        .filter(|&(bx, bz)| chunk.block(bx, -64, bz) == bedrock)
                        .count();
                    assert_eq!(bottom_bedrock, 256, "chunk {x},{z}");
                    assert!(chunk.heightmaps.iter().any(|(n, _)| n == "MOTION_BLOCKING"));
                    assert!(
                        chunk.light.iter().any(|l| l.sky.is_some()),
                        "sky light stored"
                    );
                    with_block_entities += usize::from(!chunk.block_entities.is_empty());
                    // Encoding on top of the stored NBT and decoding again
                    // gives the same chunk, and keeps what we do not model.
                    let encoded = encode_chunk(&chunk, Some(&tag), &names, false).unwrap();
                    let Decoded::Chunk(again) =
                        decode_chunk(&encoded, -64, 384, &palettes).unwrap()
                    else {
                        panic!("re-encoded chunk {x},{z} is not full");
                    };
                    assert!(*again == *chunk, "chunk {x},{z} changed in a round trip");
                    let (mc_nbt::Tag::Compound(before), mc_nbt::Tag::Compound(after)) =
                        (&tag, &encoded)
                    else {
                        unreachable!()
                    };
                    for (key, _) in before.iter() {
                        assert!(after.get(key).is_some(), "chunk {x},{z} lost {key}");
                    }
                    let mut bytes = Vec::new();
                    mc_nbt::write_named("", &encoded, &mut bytes).unwrap();
                }
                Decoded::NotFull(_) => partial += 1,
            }
        }
    }
    eprintln!(
        "{full} full chunks, {partial} partially generated, {with_block_entities} with block entities"
    );
    assert!(full >= min_full, "{name}: only {full} full chunks");
    assert_eq!(
        palettes.unresolved.get(),
        0,
        "every block and biome in the palettes resolves"
    );
}
