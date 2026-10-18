//! `cargo xtask worldgen-ref [--seed <n>] [--radius <chunks>]`: has a
//! vanilla server generate a world to compare our generator with. A data
//! pack in the world takes every feature and carver out of every biome (and
//! structures are off), so what is left is the terrain shape, aquifers, ore
//! veins, surface and biomes. The chunks around 0, 0 are force-loaded, then
//! the world is saved to `cache/vanilla-server/26.3-worldgen/<seed>/world`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::parity::Server;

const PORT: u16 = 25573;

pub fn run(args: &[String]) -> Result<()> {
    let (mut seed, mut radius) = (12345i64, 7i32);
    let mut features = false;
    let mut structures = false;
    let (mut cx, mut cz) = (0i32, 0i32);
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
            "--features" => features = true,
            "--structures" => structures = true,
            "--center" => {
                cx = value()?.parse()?;
                cz = it.next().context("--center needs x and z")?.parse()?;
            }
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let data_root = root.join("cache/mcdata/26.3/data/minecraft/worldgen/biome");
    let dir = root.join(format!(
        "cache/vanilla-server/26.3-worldgen/{seed}{}{}",
        if features { "-features" } else { "" },
        if structures { "-structures" } else { "" }
    ));
    let world = dir.join("world");
    let _ = std::fs::remove_dir_all(&world);
    // The data pack: every biome without features and carvers.
    let pack = world.join("datapacks/terrain");
    let biomes = pack.join("data/minecraft/worldgen/biome");
    std::fs::create_dir_all(&biomes)?;
    std::fs::write(
        pack.join("pack.mcmeta"),
        r#"{"pack":{"description":"terrain only","min_format":121,"max_format":121}}"#,
    )?;
    for entry in std::fs::read_dir(&data_root)? {
        let path = entry?.path();
        let mut biome: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        if !features {
            biome["features"] = serde_json::json!([]);
        }
        biome["carvers"] = serde_json::json!([]);
        std::fs::write(
            biomes.join(path.file_name().context("biome file")?),
            serde_json::to_string(&biome)?,
        )?;
    }
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-seed={seed}\nlevel-type=minecraft\\:normal\ngenerate-structures={structures}\n\
         initial-enabled-packs=vanilla,file/terrain\nview-distance=4\nsimulation-distance=4\n\
         spawn-protection=0\npause-when-empty-seconds=0\nenable-status=true\n\
         sync-chunk-writes=false\n"
    );
    Server::prepare(&root, &dir, &properties)?;
    let mut server = Server::start_java(&root, &dir)?;
    let result = (|| -> Result<()> {
        let (x0, x1) = ((cx - radius) * 16, (cx + radius) * 16 + 15);
        let (z0, z1) = ((cz - radius) * 16, (cz + radius) * 16 + 15);
        server.command(&format!("forceload add {x0} {z0} {x1} {z1}"), true)?;
        // Let the chunks generate.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20 + (radius as u64) * 6) {
            std::thread::sleep(Duration::from_secs(2));
            server.command("", false)?;
        }
        server.command("save-all flush", true)?;
        std::thread::sleep(Duration::from_secs(5));
        server.command("", true)?;
        Ok(())
    })();
    server.stop();
    result?;
    println!("world in {}", world.display());
    Ok(())
}
