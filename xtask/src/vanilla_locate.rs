//! `cargo xtask vanilla-locate --seed <n> <structure>...`: asks a vanilla
//! server (a new world with that seed) where the nearest of each structure
//! is from 0, 0 (or `name@x,z`), to compare structure placement with ours;
//! `--ours` asks our server the same; `--dimension the_nether` asks there.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::parity::Server;

const PORT: u16 = 25574;

pub fn run(args: &[String]) -> Result<()> {
    let mut seed = 0i64;
    let mut names = Vec::new();
    let mut ours = false;
    let mut dimension = String::from("minecraft:overworld");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seed" => seed = it.next().context("--seed needs a value")?.parse()?,
            "--ours" => ours = true,
            "--dimension" => {
                let d = it.next().context("--dimension needs a value")?;
                dimension = if d.contains(':') {
                    d.to_owned()
                } else {
                    format!("minecraft:{d}")
                };
            }
            n => names.push(n.to_owned()),
        }
    }
    let root = crate::workspace_root();
    let dir = root.join(format!("cache/vanilla-server/26.3-locate/{seed}"));
    let _ = std::fs::remove_dir_all(dir.join("world"));
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-seed={seed}\nlevel-type=minecraft\\:normal\ngenerate-structures=true\n\
         view-distance=2\nsimulation-distance=2\npause-when-empty-seconds=0\n\
         enable-status=true\nsync-chunk-writes=false\n"
    );
    let mut server = if ours {
        Server::ours_in(
            &root,
            &root.join(format!("cache/ours-locate/{seed}")),
            &properties,
        )?
    } else {
        Server::prepare(&root, &dir, &properties)?;
        Server::start_java(&root, &dir)?
    };
    for n in &names {
        // `name@x,z` asks from that block instead of 0, 0.
        let (n, at) = n.split_once('@').unwrap_or((n, "0,0"));
        let (ax, az) = at.split_once(',').context("name@x,z")?;
        let id = if n.contains(':') {
            n.to_owned()
        } else {
            format!("minecraft:{n}")
        };
        // Locating can take a while; wait for the answer.
        match server.wait_for(
            &format!("execute in {dimension} positioned {ax} 0 {az} run locate structure {id}"),
            "nearest",
            Duration::from_secs(120),
        ) {
            Ok(line) => println!("{}", line.trim()),
            Err(e) => println!("{id}: {e:#}"),
        }
    }
    server.stop();
    Ok(())
}
