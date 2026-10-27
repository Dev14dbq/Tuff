//! `cargo xtask flat`: a superflat world saved by vanilla, served and
//! changed by us, and back.
//!
//! 1. vanilla makes a flat world (`level-type=flat`) and a joining player's
//!    chunks, and saves;
//! 2. vanilla serves that world; our server serves a copy that still has its
//!    chunks, and a copy with the chunks deleted (so ours reads the layers
//!    from `world_gen_settings.dat` and makes every chunk): the chunk
//!    packets of all three must be identical;
//! 3. our server, on the copy without chunks, places a gold block and
//!    saves; vanilla opens that folder and finds the block in the chunk we
//!    made.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};

use crate::mcc::copy_dir;
use crate::parity::Server;

const PORT: u16 = 25604;
const GOLD: &str = "1 -60 1";

pub fn run(_args: &[String]) -> Result<()> {
    let root = crate::workspace_root();
    let vanilla = root.join("cache/vanilla-server/26.3-flat");
    let ours = root.join("cache/ours-flat");
    // The world as vanilla left it: every run below starts from a copy, as
    // vanilla rewrites chunks every time it stops.
    let base = root.join("cache/flat-base");
    let scratch = root.join("cache/flat-dumps");
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-type=minecraft\\:flat\nview-distance=6\nsimulation-distance=2\n\
         spawn-protection=0\npause-when-empty-seconds=0\nenable-status=true\nwhite-list=false\n"
    );
    let addr = format!("127.0.0.1:{PORT}");
    let _ = std::fs::remove_dir_all(&scratch);

    // 1. Vanilla makes the world.
    let _ = std::fs::remove_dir_all(vanilla.join("world"));
    Server::prepare(&root, &vanilla, &properties)?;
    let mut server = Server::start_java(&root, &vanilla)?;
    let made = (|| -> Result<()> {
        let ids = crate::probe::load_packet_ids(None)?;
        crate::chunk_speed::measure(&addr, ids, "Maker", 4)?;
        server.wait_for("save-all flush", "Saved the game", Duration::from_secs(60))?;
        Ok(())
    })();
    server.stop();
    made?;
    let kept =
        std::fs::read_dir(vanilla.join("world/dimensions/minecraft/overworld/region"))?.count();
    ensure!(kept > 0, "vanilla saved no region files");
    let _ = std::fs::remove_dir_all(&base);
    copy_dir(&vanilla.join("world"), &base)?;
    println!("1. vanilla made a flat world ({kept} region files)");

    // 2. Chunk packets: vanilla, ours with chunks, ours making them.
    let world = base.clone();
    dump_vanilla(
        &root,
        &vanilla,
        &base,
        &properties,
        &addr,
        &scratch.join("vanilla"),
    )?;
    dump_ours(
        &root,
        &properties,
        &addr,
        |dir| copy_dir(&world, &dir.join("world")),
        &scratch.join("ours-read"),
    )?;
    dump_ours(
        &root,
        &properties,
        &addr,
        |dir| {
            copy_dir(&world, &dir.join("world"))?;
            let region = dir.join("world/dimensions/minecraft/overworld");
            for folder in ["region", "entities", "poi"] {
                let _ = std::fs::remove_dir_all(region.join(folder));
            }
            Ok(())
        },
        &scratch.join("ours-made"),
    )?;
    // Vanilla against itself on the same files: what is noise in vanilla.
    dump_vanilla(
        &root,
        &vanilla,
        &base,
        &properties,
        &addr,
        &scratch.join("vanilla-again"),
    )?;
    let (noise, _) = diff(&scratch.join("vanilla"), &scratch.join("vanilla-again"))?;
    println!("2. vanilla against vanilla again: {noise}");
    let (read, read_ok) = diff(&scratch.join("vanilla"), &scratch.join("ours-read"))?;
    println!("   ours serving vanilla's chunks: {read}");
    let (made, made_ok) = diff(&scratch.join("vanilla"), &scratch.join("ours-made"))?;
    println!("   ours making the chunks:        {made}");

    // 3. Ours builds in a world it made; vanilla opens it.
    let mut server = Server::ours_in_with(&root, &ours, &properties, &|dir| {
        copy_dir(&world, &dir.join("world"))?;
        let region = dir.join("world/dimensions/minecraft/overworld");
        for folder in ["region", "entities", "poi"] {
            let _ = std::fs::remove_dir_all(region.join(folder));
        }
        Ok(())
    })?;
    let placed = server
        .wait_for(
            &format!("setblock {GOLD} minecraft:gold_block"),
            "Changed the block",
            Duration::from_secs(30),
        )
        .map(|_| ());
    server.stop();
    placed?;
    let _ = std::fs::remove_dir_all(vanilla.join("world"));
    copy_dir(&ours.join("world"), &vanilla.join("world"))?;
    let mut server = Server::start_java(&root, &vanilla)?;
    let found = (|| -> Result<String> {
        server.command("forceload add 0 0", false)?;
        std::thread::sleep(Duration::from_secs(2));
        server.wait_for(
            &format!("execute if block {GOLD} minecraft:gold_block"),
            "Test",
            Duration::from_secs(30),
        )
    })();
    server.stop();
    let found = found?;
    ensure!(
        found.contains("passed"),
        "vanilla did not find the gold block: {found}"
    );
    println!("3. ours built in the world it made; vanilla finds the gold block");

    ensure!(read_ok && made_ok, "chunk packets differ");
    Ok(())
}

fn dump_vanilla(
    root: &Path,
    dir: &Path,
    base: &Path,
    properties: &str,
    addr: &str,
    out: &Path,
) -> Result<()> {
    let _ = std::fs::remove_dir_all(dir.join("world"));
    Server::prepare(root, dir, properties)?;
    copy_dir(base, &dir.join("world"))?;
    let mut server = Server::start_java(root, dir)?;
    server.command("tick freeze", false)?;
    let dumped = probe(addr, out);
    server.stop();
    dumped
}

fn dump_ours(
    root: &Path,
    properties: &str,
    addr: &str,
    setup: impl Fn(&Path) -> Result<()>,
    out: &Path,
) -> Result<()> {
    let mut server = Server::ours_in_with(root, &root.join("cache/ours-flat"), properties, &setup)?;
    let dumped = probe(addr, out);
    server.stop();
    dumped
}

/// Joins as a client and saves the chunk packets of the first batches.
fn probe(addr: &str, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    run_xtask(&[
        "probe",
        "--addr",
        addr,
        "--batches",
        "30",
        "--dump",
        out.to_str().context("path")?,
    ])
    .map(|_| ())
}

/// What `chunk-diff` says of two dumps: the summary, and whether every
/// difference is the one thing vanilla itself varies in.
///
/// Vanilla sends the sky-light array of the section below the world (bit 0)
/// for some chunks and not for others, and not the same ones from one run
/// to the next on identical files (`vanilla` against `vanilla-again` shows
/// it). We never send it. A chunk that differs only so counts as identical;
/// anything else (blocks, biomes, heightmaps, any other light, block
/// entities) is a real difference.
fn diff(a: &Path, b: &Path) -> Result<(String, bool)> {
    let out = run_xtask(&[
        "chunk-diff",
        a.to_str().context("path")?,
        b.to_str().context("path")?,
    ])?;
    let last = out
        .lines()
        .rev()
        .find(|l| l.contains("common chunks identical"))
        .context("no chunk-diff summary")?;
    let (same, common) = last
        .split_once('/')
        .and_then(|(same, rest)| Some((same.trim(), rest.split_whitespace().next()?)))
        .context("chunk-diff summary")?;
    ensure!(common != "0", "no chunks in common");
    let varies = |line: &str| {
        line.ends_with("): sky light sections [0, 1, 2] vs [1, 2]")
            || line.ends_with("): sky light sections [1, 2] vs [0, 1, 2]")
    };
    let differing: Vec<&str> = out.lines().filter(|l| l.starts_with("chunk (")).collect();
    let real: Vec<&str> = differing.iter().copied().filter(|l| !varies(l)).collect();
    for line in &real {
        println!("   {line}");
    }
    let text = if differing.is_empty() {
        format!("all {common} common chunks identical")
    } else {
        format!(
            "{same} of {common} common chunks identical, {} differ only in the below-world sky array",
            differing.len() - real.len()
        )
    };
    Ok((text, real.is_empty()))
}

fn run_xtask(args: &[&str]) -> Result<String> {
    let out = Command::new(std::env::current_exe()?)
        .args(args)
        .output()
        .context("running xtask")?;
    if !out.status.success() {
        bail!(
            "xtask {}: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
