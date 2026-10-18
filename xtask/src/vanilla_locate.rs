//! `cargo xtask vanilla-locate --seed <n> <structure>...`: asks a vanilla
//! server (a new world with that seed) where the nearest of each structure
//! is from 0, 0, to compare structure placement with ours.

use std::time::Duration;

use anyhow::{Context, Result};

use crate::parity::Server;

const PORT: u16 = 25574;

pub fn run(args: &[String]) -> Result<()> {
    let mut seed = 0i64;
    let mut names = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seed" => seed = it.next().context("--seed needs a value")?.parse()?,
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
    Server::prepare(&root, &dir, &properties)?;
    let mut server = Server::start_java(&root, &dir)?;
    for n in &names {
        let id = if n.contains(':') {
            n.clone()
        } else {
            format!("minecraft:{n}")
        };
        server.command(
            &format!("execute positioned 0 0 0 run locate structure {id}"),
            false,
        )?;
        std::thread::sleep(Duration::from_secs(4));
        server.command("", true)?;
    }
    server.stop();
    Ok(())
}
