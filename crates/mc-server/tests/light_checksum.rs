//! The light of newly generated chunks stays the same: a fixed seed, chunks
//! loaded in a fixed order, a checksum of every chunk's stored light. Needs
//! extracted data (`cargo xtask extract-data`); skipped otherwise.

use mc_server::{Config, ServerContext, find_data_dir};
use std::path::Path;

/// FNV-1a over everything a client is sent about light.
fn checksum(ctx: &ServerContext, chunks: &[(i32, i32)]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
        hash = (hash ^ 0xff).wrapping_mul(0x100_0000_01b3);
    };
    for &(x, z) in chunks {
        let light = ctx.world.light_data(x, z).expect("loaded");
        for mask in [
            &light.sky_mask,
            &light.block_mask,
            &light.empty_sky_mask,
            &light.empty_block_mask,
        ] {
            let words: Vec<u8> = mask.iter().flat_map(|w| w.to_le_bytes()).collect();
            eat(&words);
        }
        for array in light.sky.iter().chain(&light.block) {
            eat(array);
        }
    }
    hash
}

#[test]
fn generated_light_is_unchanged() {
    let Ok(data_dir) = find_data_dir(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!("skipped: no extracted data (cargo xtask extract-data)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("mc-rewrite-light-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut config = Config::new(data_dir.clone());
    config.world = Some(dir.clone());
    config.seed = Some(4242);
    let data = mc_data::McData::load(&data_dir).unwrap();
    let ctx = ServerContext::new(config, data).unwrap();
    // A 6×6 area loaded in a scrambled but fixed order, around a river/coast
    // and a plain area, so neighbours are lit before and after each other.
    let mut chunks: Vec<(i32, i32)> = (-3..3).flat_map(|x| (-3..3).map(move |z| (x, z))).collect();
    let mut seed: u64 = 0x5eed;
    for i in (1..chunks.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        chunks.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    for &(x, z) in &chunks {
        ctx.world.watch(x, z, &ctx.data.blocks, &|_| Vec::new());
    }
    let sum = checksum(&ctx, &chunks);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("light checksum {sum:#018x}");
    assert_eq!(sum, EXPECTED, "light of generated chunks changed");
}

/// Moves when world generation changes what the 6×6 chunks hold (a feature
/// that emits light, a structure placed differently), not only when the
/// light engine does: find the commit that moved it and say why in its
/// message before updating this.
const EXPECTED: u64 = 0xbab9_46fd_6464_be83;
