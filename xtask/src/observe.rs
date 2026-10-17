//! `cargo xtask observe attach`: learns from a vanilla server which faces of
//! each block state fences attach to, a fact the data reports do not carry.
//!
//! Every state is placed on a frozen superflat world with an oak fence on
//! each horizontal side; after the neighbour updates settle, each fence's
//! connection towards the block is read back through a bot. Results go to
//! `cache/mcdata/<version>/observed/fence_attach.json`: one number per state
//! id, bits 1 north, 2 east, 4 south, 8 west; 16 if the state did not stay
//! as placed (its own reaction to the fences, or support it lacked).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::parity::{Mirror, Server, VANILLA_PORT};
use crate::probe::{load_data, packet_ids};

const PER_RUN: usize = 1500;
const ROW: usize = 39;
const Y: i32 = -59;

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("attach") => attach(&args[1..]),
        _ => bail!("usage: observe attach [--first <state>] [--count <n>]"),
    }
}

fn cell(i: usize) -> (i32, i32) {
    (-76 + 4 * (i % ROW) as i32, -76 + 4 * (i / ROW) as i32)
}

fn attach(args: &[String]) -> Result<()> {
    let data = load_data(None)?;
    let total = data.blocks.state_count() as usize;
    let (mut first, mut count) = (0usize, total);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--first" => first = value()?.parse()?,
            "--count" => count = value()?.parse()?,
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let out_dir = root
        .join("cache/mcdata")
        .join(&data.version.name)
        .join("observed");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join("fence_attach.json");
    // Keep earlier results, so a long run can be done in parts.
    let mut results: Vec<i32> = std::fs::read_to_string(&out_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v["states"].clone()).ok())
        .filter(|v: &Vec<i32>| v.len() == total)
        .unwrap_or_else(|| vec![-1; total]);

    let end = (first + count).min(total);
    let mut start = first;
    while start < end {
        let batch: Vec<u32> = (start..(start + PER_RUN).min(end))
            .map(|s| s as u32)
            .filter(|&s| data.blocks.block_name(s) != Some("minecraft:moving_piston"))
            .collect();
        let began = Instant::now();
        let observed = observe_batch(&root, &data, &batch)?;
        for (state, bits) in observed {
            results[state as usize] = bits;
        }
        println!(
            "states {start}..{} done in {:.0} s",
            (start + PER_RUN).min(end),
            began.elapsed().as_secs_f64()
        );
        let json = serde_json::json!({
            "version": data.version.name,
            "what": "faces of each block state an oak fence attaches to: 1 north, 2 east, 4 south, 8 west; 16 = state changed after placing; -1 = not observed",
            "states": results,
        });
        std::fs::write(&out_path, serde_json::to_string(&json)?)?;
        start += PER_RUN;
    }
    println!("wrote {}", out_path.display());
    Ok(())
}

fn observe_batch(
    root: &std::path::Path,
    data: &mc_data::McData,
    states: &[u32],
) -> Result<Vec<(u32, i32)>> {
    let mut vanilla = Server::vanilla(root)?;
    let result = (|| -> Result<Vec<(u32, i32)>> {
        let mut mirror = Mirror::join(&format!("127.0.0.1:{VANILLA_PORT}"), packet_ids(data))?;
        for setup in [
            "gamerule random_tick_speed 0",
            "gamerule spawn_mobs false",
            "kill @e[type=!player]",
            "tp Parity 0 -40 0",
            "tick freeze",
            "fill -80 -60 -80 80 -60 80 stone",
        ] {
            vanilla.send(setup)?;
        }
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(3))?;
        for (i, &state) in states.iter().enumerate() {
            let (x, z) = cell(i);
            let text = data.blocks.describe(state).context("unknown state")?;
            vanilla.send(&format!(
                "setblock {x} {Y} {z} {}",
                text.trim_end_matches("[]")
            ))?;
        }
        for i in 0..states.len() {
            let (x, z) = cell(i);
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                vanilla.send(&format!("setblock {} {Y} {} oak_fence", x + dx, z + dz))?;
            }
        }
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(5))?;
        let connected = |state: Option<u32>, side: &str| {
            state.and_then(|s| data.blocks.property(s, side)) == Some("true")
        };
        Ok(states
            .iter()
            .enumerate()
            .map(|(i, &state)| {
                let (x, z) = cell(i);
                let mut bits = 0;
                // The fence north of the block reaches south to it, and so on.
                if connected(mirror.block(x, Y, z - 1), "south") {
                    bits |= 1;
                }
                if connected(mirror.block(x + 1, Y, z), "west") {
                    bits |= 2;
                }
                if connected(mirror.block(x, Y, z + 1), "north") {
                    bits |= 4;
                }
                if connected(mirror.block(x - 1, Y, z), "east") {
                    bits |= 8;
                }
                if mirror.block(x, Y, z) != Some(state) {
                    bits |= 16;
                }
                (state, bits)
            })
            .collect())
    })();
    vanilla.stop();
    result
}

fn wait_quiet(mirror: &mut Mirror, min: Duration) -> Result<()> {
    let start = Instant::now();
    let mut quiet = Instant::now();
    while start.elapsed() < min || quiet.elapsed() < Duration::from_millis(1500) {
        if mirror.pump()? {
            quiet = Instant::now();
        }
        if start.elapsed() > Duration::from_secs(120) {
            bail!("vanilla kept changing blocks");
        }
    }
    Ok(())
}
