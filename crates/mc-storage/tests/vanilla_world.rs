//! Decodes a world saved by a vanilla 26.3 server, if one is available at
//! cache/vanilla-server/26.3/world-normal (see docs/specs/protocol-26.3.md).

use std::path::Path;

use mc_storage::{Decoded, Palettes, RegionFile, RegionStorage, decode_chunk};

#[test]
fn decodes_every_chunk_of_a_vanilla_world() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let data_dir = root.join("cache/mcdata/26.3");
    let regions =
        root.join("cache/vanilla-server/26.3/world-normal/dimensions/minecraft/overworld/region");
    if !data_dir.is_dir() || !regions.is_dir() {
        eprintln!("skipped: no extracted data or vanilla test world");
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
                }
                Decoded::NotFull(_) => partial += 1,
            }
        }
    }
    eprintln!(
        "{full} full chunks, {partial} partially generated, {with_block_entities} with block entities"
    );
    assert!(
        full >= 256,
        "the 16×16 force-loaded area is fully generated"
    );
    assert_eq!(
        palettes.unresolved.get(),
        0,
        "every block and biome in the palettes resolves"
    );
}
