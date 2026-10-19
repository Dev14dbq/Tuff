//! `cargo xtask gen-speed [--seed n] [--view-distance n] [--only vanilla|ours]`:
//! how fast vanilla and our server make a new world: time to start (the
//! spawn area) and to deliver a joining player's chunks, generated as they
//! are asked for. Both start from no world with the same seed.

use std::time::Instant;

use anyhow::{Context as _, Result, bail};

use crate::parity::Server;

pub fn run(args: &[String]) -> Result<()> {
    let mut seed: i64 = -1294266004;
    let mut view_distance: i8 = 10;
    let mut only: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seed" => seed = it.next().context("--seed")?.parse()?,
            "--view-distance" => view_distance = it.next().context("--view-distance")?.parse()?,
            "--only" => only = it.next().cloned(),
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let ids = crate::probe::load_packet_ids(None)?;
    let properties = |port: u16| {
        format!(
            "online-mode=false\nserver-ip=127.0.0.1\nserver-port={port}\nlevel-name=world\n\
             level-seed={seed}\nview-distance={view_distance}\nsimulation-distance={view_distance}\n\
             spawn-protection=0\npause-when-empty-seconds=0\nwhite-list=false\nsync-chunk-writes=false\n"
        )
    };
    if only.as_deref() != Some("ours") {
        let dir = root.join("cache/vanilla-server/26.3-speed");
        let _ = std::fs::remove_dir_all(dir.join("world"));
        Server::prepare(&root, &dir, &properties(25590))?;
        let started = Instant::now();
        let mut server = Server::start_java(&root, &dir)?;
        let up = started.elapsed();
        let r = crate::chunk_speed::measure("127.0.0.1:25590", ids.clone(), "Speed", view_distance);
        server.stop();
        println!("vanilla: started in {} ms; {}", up.as_millis(), r?);
    }
    if only.as_deref() != Some("vanilla") {
        let dir = root.join("cache/ours-speed");
        // Build first so the start time is the server's own.
        let status = std::process::Command::new("cargo")
            .args(["build", "--release", "-q", "-p", "mc-server"])
            .current_dir(&root)
            .status()?;
        anyhow::ensure!(status.success(), "building mc-server failed");
        let started = Instant::now();
        let mut server = Server::ours_in(&root, &dir, &properties(25591))?;
        let up = started.elapsed();
        let r = crate::chunk_speed::measure("127.0.0.1:25591", ids, "Speed", view_distance);
        server.stop();
        println!("ours:    started in {} ms; {}", up.as_millis(), r?);
    }
    Ok(())
}
