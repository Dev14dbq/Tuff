//! `cargo run --release -p mc-server --example genmap -- <world dir> <seed>
//! <radius in chunks> <out.ppm>`: generates the chunks around 0, 0 through
//! the server's pipeline and draws the top block of every column, with a
//! count of what the features put down.

use std::collections::HashMap;
use std::io::Write as _;

use mc_protocol::Position;
use mc_server::{Config, Server};

fn color(name: &str) -> [u8; 3] {
    let n = name.trim_start_matches("minecraft:");
    match n {
        "grass_block" => [95, 159, 53],
        "water" => [52, 90, 210],
        "sand" | "sandstone" => [219, 207, 163],
        "red_sand" => [190, 102, 33],
        "stone" | "andesite" | "diorite" | "granite" | "tuff" => [125, 125, 125],
        "gravel" => [140, 130, 126],
        "snow" | "snow_block" | "powder_snow" => [250, 250, 250],
        "ice" | "packed_ice" | "blue_ice" => [160, 190, 250],
        "dirt" | "coarse_dirt" | "rooted_dirt" => [134, 96, 67],
        "podzol" => [91, 63, 24],
        "mycelium" => [111, 99, 105],
        "clay" => [160, 166, 179],
        "terracotta"
        | "orange_terracotta"
        | "red_terracotta"
        | "yellow_terracotta"
        | "white_terracotta"
        | "brown_terracotta"
        | "light_gray_terracotta" => [180, 100, 70],
        "lava" => [230, 90, 0],
        "mud" => [60, 57, 60],
        _ if n.ends_with("_leaves") => [40, 110, 30],
        _ if n.ends_with("_log") || n.ends_with("_wood") => [100, 70, 40],
        _ if n.contains("flower")
            || n.contains("tulip")
            || n == "poppy"
            || n == "dandelion"
            || n == "cornflower"
            || n == "azure_bluet"
            || n == "oxeye_daisy"
            || n == "allium" =>
        {
            [230, 60, 200]
        }
        "short_grass" | "tall_grass" | "fern" | "large_fern" | "bush" => [80, 180, 50],
        "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant" => [40, 140, 120],
        _ => [200, 0, 0],
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let world = std::path::PathBuf::from(&args[1]);
    let seed: i64 = args[2].parse()?;
    let radius: i32 = args[3].parse()?;
    let out = &args[4];
    let (ox, oz): (i32, i32) = match (args.get(5), args.get(6)) {
        (Some(a), Some(b)) => (a.parse()?, b.parse()?),
        _ => (0, 0),
    };
    let data_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let mut config = Config::new(data_dir);
    config.port = 0;
    config.online_mode = false;
    config.world = Some(world);
    config.seed = Some(seed);
    let server = Server::bind(config).await?;
    let ctx = server.context().clone();
    let blocks = &ctx.data.blocks;
    let started = std::time::Instant::now();
    let size = (radius * 2 + 1) * 16;
    let mut img = vec![0u8; (size * size * 3) as usize];
    let mut counts: HashMap<String, usize> = HashMap::new();
    let min_y = ctx.world.min_y;
    let top = min_y + ctx.world.height as i32;
    let mut chunks = 0;
    for cz in oz - radius..=oz + radius {
        for cx in ox - radius..=ox + radius {
            chunks += 1;
            for lz in 0..16 {
                for lx in 0..16 {
                    let (x, z) = (cx * 16 + lx, cz * 16 + lz);
                    let mut drawn = false;
                    for y in (min_y..top).rev() {
                        let s = ctx.world.block(Position { x, y, z }, blocks).unwrap_or(0);
                        if blocks.is_air(s) {
                            continue;
                        }
                        let name = blocks.block_name(s).unwrap_or("?");
                        if !drawn {
                            let c = color(name);
                            let shade = ((y - 40).clamp(0, 120) as f32 / 120.0) * 0.6 + 0.6;
                            let (ix, iz) = (x - (ox - radius) * 16, z - (oz - radius) * 16);
                            let i = ((iz * size + ix) * 3) as usize;
                            for k in 0..3 {
                                img[i + k] = (f32::from(c[k]) * shade).min(255.0) as u8;
                            }
                            drawn = true;
                        }
                        if y > 40 || name.ends_with("_ore") {
                            *counts.entry(name.to_owned()).or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    let saved = ctx.world.save(blocks);
    println!("saved {} chunks", saved.chunks);
    let mut f = std::fs::File::create(out)?;
    write!(f, "P6\n{size} {size}\n255\n")?;
    f.write_all(&img)?;
    println!(
        "{chunks} chunks in {:.1} s",
        started.elapsed().as_secs_f32()
    );
    let mut list: Vec<_> = counts.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1));
    for (n, c) in list.iter().take(70) {
        println!("{c:>9} {n}");
    }
    Ok(())
}
