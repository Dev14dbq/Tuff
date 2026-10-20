//! `cargo xtask bench`: the baseline table. Runs fixed scenarios on vanilla
//! (`reference/26.3/server.jar`) and on our server, each from a new world,
//! and prints a Markdown table for docs/BENCHMARKS.md:
//!
//! - `gen10`, `gen32`: a player joins a new world (fixed seed) with view
//!   distance 10 / 32; chunks per second until the whole view has arrived.
//! - `bots`: N bots (`xtask bots`) walk around a new world, view distance 10.
//! - `idle`: the superflat world below with its 128×128 field forceloaded and
//!   nothing built, the floor for the next three.
//! - `hoppers`: a superflat world with a forceloaded 128×128 layer of hoppers
//!   holding items, passing them east.
//! - `redstone`: 64×64 observer pairs facing each other (2-tick clocks) under
//!   a layer of redstone dust they power.
//! - `entities`: N cows in a fenced, forceloaded 128×128 field.
//!
//! MSPT comes from `tick query` (percentiles of the last 100 ticks), asked
//! every 5 s during the measured window; the table shows the median of those
//! P50s and P99s. RAM is the memory footprint at the end (see `rss_mb`); vanilla
//! runs with `-Xmx2G` as in the parity tools.

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::parity::Server;

const SEED: i64 = -1294266004;
const VANILLA_PORT: u16 = 25600;
const OURS_PORT: u16 = 25601;
const ALL: &str = "gen10,gen32,bots,idle,hoppers,redstone,entities";

pub fn run(args: &[String]) -> Result<()> {
    let mut only: Option<String> = None;
    let mut scenarios = ALL.to_owned();
    let mut bots = 50usize;
    let mut entities = 2000usize;
    let mut seconds = 60u64;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--only" => only = Some(value()?.clone()),
            "--scenarios" => scenarios = value()?.clone(),
            "--bots" => bots = value()?.parse()?,
            "--entities" => entities = value()?.parse()?,
            "--seconds" => seconds = value()?.parse()?,
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let sides: Vec<&str> = match only.as_deref() {
        Some(side @ ("vanilla" | "ours")) => vec![side],
        Some(other) => bail!("--only vanilla|ours, not `{other}`"),
        None => vec!["vanilla", "ours"],
    };
    let mut rows = Vec::new();
    for scenario in scenarios.split(',') {
        for &side in &sides {
            eprintln!("bench: {scenario} on {side}");
            let row = match scenario {
                "gen10" => generation(&root, side, 10),
                "gen32" => generation(&root, side, 32),
                "bots" => with_bots(&root, side, bots, seconds),
                "idle" | "hoppers" | "redstone" | "entities" => {
                    contraption(&root, side, scenario, entities, seconds)
                }
                other => bail!("unknown scenario `{other}` (one of {ALL})"),
            };
            let row = row.unwrap_or_else(|err| format!("error: {err:#}"));
            eprintln!("bench: {scenario} on {side}: {row}");
            rows.push(format!("| {scenario} | {side} | {row} |"));
        }
    }
    println!("| scenario | server | result |\n|---|---|---|");
    for row in rows {
        println!("{row}");
    }
    Ok(())
}

fn properties(port: u16, flat: bool, view_distance: u8) -> String {
    let level = if flat {
        "level-type=minecraft\\:flat\ngenerate-structures=false\n"
    } else {
        "level-type=minecraft\\:normal\n"
    };
    let simulation = view_distance.min(10);
    format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={port}\nlevel-name=world\n\
         level-seed={SEED}\n{level}view-distance={view_distance}\n\
         simulation-distance={simulation}\nspawn-protection=0\npause-when-empty-seconds=0\n\
         white-list=false\nsync-chunk-writes=false\nenable-status=true\nmax-players=1000\nallow-flight=true\n"
    )
}

/// Starts `side` on a new world.
fn start(root: &std::path::Path, side: &str, properties: &str) -> Result<(Server, String)> {
    Ok(if side == "vanilla" {
        let dir = root.join("cache/vanilla-server/26.3-bench");
        let _ = std::fs::remove_dir_all(dir.join("world"));
        Server::prepare(root, &dir, properties)?;
        (
            Server::start_java(root, &dir)?,
            format!("127.0.0.1:{VANILLA_PORT}"),
        )
    } else {
        (
            Server::ours_in(root, &root.join("cache/ours-bench"), properties)?,
            format!("127.0.0.1:{OURS_PORT}"),
        )
    })
}

fn port(side: &str) -> u16 {
    if side == "vanilla" {
        VANILLA_PORT
    } else {
        OURS_PORT
    }
}

fn generation(root: &std::path::Path, side: &str, view_distance: u8) -> Result<String> {
    let (mut server, addr) = start(root, side, &properties(port(side), false, view_distance))?;
    let ids = crate::probe::load_packet_ids(None)?;
    let result = crate::chunk_speed::measure(&addr, ids, "Bench", view_distance as i8);
    let rss = rss_mb(server.pid());
    server.stop();
    let result = result?;
    // "... all 1234 ms (567 chunks/s)": keep the rate and the total time.
    let rate = result
        .rsplit_once('(')
        .and_then(|(_, r)| r.strip_suffix(" chunks/s)"))
        .unwrap_or("?");
    let all = between(&result, ", all ", " ms").unwrap_or("?");
    let chunks = between(&result, "ms, ", " chunks").unwrap_or("?");
    Ok(format!(
        "{rate} chunks/s ({chunks} chunks in {all} ms), RAM {rss} MB"
    ))
}

fn with_bots(root: &std::path::Path, side: &str, count: usize, seconds: u64) -> Result<String> {
    let (mut server, addr) = start(root, side, &properties(port(side), false, 10))?;
    let warmup = 30;
    let bot_args: Vec<String> = [
        "--addr",
        &addr,
        "--count",
        &count.to_string(),
        "--seconds",
        &(seconds + warmup + 5).to_string(),
        "--view-distance",
        "10",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let bots = std::thread::spawn(move || crate::bots::run(&bot_args));
    std::thread::sleep(Duration::from_secs(warmup));
    let mspt = sample(&mut server, seconds);
    let rss = rss_mb(server.pid());
    let bots = bots.join().expect("bots thread");
    server.stop();
    let mspt = mspt?;
    let bots = match bots {
        Ok(()) => String::new(),
        Err(err) => format!(" ({err:#})"),
    };
    Ok(format!("{count} bots: {mspt}, RAM {rss} MB{bots}"))
}

fn contraption(
    root: &std::path::Path,
    side: &str,
    scenario: &str,
    entities: usize,
    seconds: u64,
) -> Result<String> {
    let (mut server, _) = start(root, side, &properties(port(side), true, 4))?;
    let result = build(&mut server, scenario, entities).and_then(|what| {
        std::thread::sleep(Duration::from_secs(20));
        let mspt = sample(&mut server, seconds)?;
        Ok(format!("{what}: {mspt}, RAM {} MB", rss_mb(server.pid())))
    });
    server.stop();
    result
}

/// Builds the scenario's contraption with console commands on a superflat
/// world (grass at y = -61) and says what was built.
fn build(server: &mut Server, scenario: &str, entities: usize) -> Result<String> {
    server.command("gamerule doMobSpawning false", false)?;
    server.command("gamerule spawn_mobs false", false)?;
    server.command("forceload add 0 0 127 127", false)?;
    let y = -60;
    match scenario {
        "idle" => Ok("empty field".into()),
        "hoppers" => {
            fill(
                server,
                &format!(
                    "fill 0 {y} 0 127 {y} 127 minecraft:hopper[facing=east]\
                     {{Items:[{{Slot:0b,id:\"minecraft:cobblestone\",count:64}}]}}"
                ),
            )?;
            Ok("16384 hoppers".into())
        }
        "redstone" => {
            fill(
                server,
                &format!("fill 0 {y} 0 63 {y} 63 minecraft:observer[facing=east]"),
            )?;
            fill(
                server,
                &format!("fill 0 {} 0 63 {} 63 minecraft:redstone_wire", y + 1, y + 1),
            )?;
            for x in (1..64).step_by(2) {
                fill(
                    server,
                    &format!("fill {x} {y} 0 {x} {y} 63 minecraft:observer[facing=west]"),
                )?;
            }
            Ok("2048 observer clocks + 4096 dust".into())
        }
        "entities" => {
            for (a, b, c, d) in [
                (-1, -1, 128, -1),
                (-1, 128, 128, 128),
                (-1, -1, -1, 128),
                (128, -1, 128, 128),
            ] {
                fill(
                    server,
                    &format!("fill {a} {y} {b} {c} {} {d} minecraft:glass", y + 2),
                )?;
            }
            let mut state = 0x2545_f491_4f6c_dd1du64;
            let mut next = |n: u64| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state % n
            };
            for _ in 0..entities {
                let (x, z) = (next(126) + 1, next(126) + 1);
                server.send(&format!("summon minecraft:cow {x} {y} {z}"))?;
            }
            // Wait until every summon has been run.
            server.wait_for("seed", "Seed", Duration::from_secs(120))?;
            Ok(format!("{entities} cows"))
        }
        _ => unreachable!(),
    }
}

/// Runs a `fill`, retrying while the forceloaded chunks are still loading.
fn fill(server: &mut Server, command: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    server.drain()?;
    server.send(command)?;
    server.drain().ok();
    while Instant::now() < deadline {
        let Some(line) = server.next_line(Duration::from_secs(5)) else {
            continue;
        };
        if line.contains("filled") {
            return Ok(());
        }
        if line.contains("not loaded") {
            std::thread::sleep(Duration::from_secs(1));
            server.send(command)?;
            continue;
        }
        let error = [
            "Unknown",
            "Incorrect",
            "Expected",
            "Invalid",
            "too many",
            "No blocks",
        ];
        if error.iter().any(|e| line.contains(e)) {
            bail!("`{}`: {line}", command.split(' ').next().unwrap_or(command));
        }
    }
    bail!("no reply to fill")
}

/// Median P50 and P99 (and the mean of the average) over `tick query`
/// replies every 5 s for `seconds`.
fn sample(server: &mut Server, seconds: u64) -> Result<String> {
    let (mut p50, mut p99, mut avg) = (Vec::new(), Vec::new(), Vec::new());
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_secs(5));
        server.drain()?;
        // "Average time per tick: 1.2ms (Target: 50.0ms)", maybe without
        // its start (the second line of one console message).
        let line = server.wait_for("tick query", "ms (Target", Duration::from_secs(30))?;
        let before = line.split("ms (Target").next().unwrap_or("");
        avg.push(ms(before.rsplit([' ', ':']).next().unwrap_or("")));
        let line = server.wait_for("", "Percentiles", Duration::from_secs(10))?;
        p50.push(ms(between(&line, "P50: ", "ms").unwrap_or("")));
        p99.push(ms(between(&line, "P99: ", "ms").unwrap_or("")));
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
    };
    let mean = avg.iter().sum::<f64>() / avg.len().max(1) as f64;
    Ok(format!(
        "MSPT mean {mean:.1}, p50 {:.1}, p99 {:.1}",
        median(&mut p50),
        median(&mut p99)
    ))
}

fn ms(s: &str) -> f64 {
    s.trim().replace(',', ".").parse().unwrap_or(f64::NAN)
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = s.find(start)? + start.len();
    let to = s[from..].find(end)?;
    Some(&s[from..from + to])
}

/// Memory of a process in MB: on macOS the footprint `top` shows (it counts
/// compressed pages, which the resident set leaves out under memory
/// pressure), elsewhere the resident set size.
fn rss_mb(pid: u32) -> u64 {
    let pid = pid.to_string();
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    if cfg!(target_os = "macos") {
        let out = run("top", &["-l", "1", "-pid", &pid, "-stats", "mem"]);
        let last = out
            .lines()
            .last()
            .unwrap_or("")
            .trim()
            .trim_end_matches(['+', '-']);
        let (number, unit) = last.split_at(last.len().saturating_sub(1));
        let number: f64 = number.parse().unwrap_or(0.0);
        return match unit {
            "G" => number * 1024.0,
            "M" => number,
            "K" => number / 1024.0,
            _ => 0.0,
        } as u64;
    }
    run("ps", &["-o", "rss=", "-p", &pid])
        .trim()
        .parse::<u64>()
        .map_or(0, |kb| kb / 1024)
}
