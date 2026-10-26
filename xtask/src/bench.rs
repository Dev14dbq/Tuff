//! `cargo xtask bench`: the baseline table. Runs fixed scenarios on vanilla
//! (`reference/26.3/server.jar`), on vanilla with Lithium and C2ME
//! (`modded`) and on our server, each from a new world, and prints a
//! Markdown table for docs/BENCHMARKS.md:
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
//! every 5 s during the measured window; a run's numbers are the median of
//! those P50s and P99s. RAM is the memory footprint at the end (see
//! `rss_mb`).
//!
//! Fair runs (see `quiet`): every server gets the same `--cores` (JVM
//! `-XX:ActiveProcessorCount`, our `MC_THREADS` / `TOKIO_WORKER_THREADS`)
//! and 2 GB of heap; a run starts only on a quiet machine (no Minecraft
//! client, on AC power, other processes under `--background` cores) and is
//! redone when the background got busy during it. `--rounds` runs every
//! scenario that many times, the sides taking turns in a different order
//! each round; the table shows the median and the range.
//!
//! The `modded` side is vanilla under Fabric with Lithium and C2ME, from
//! `tools/fabric/26.3` (`fabric-server-launch.jar` and `mods/`).
//!
//! With `--jfr`, one more round runs the Java servers under their `/jfr`
//! profiler (its numbers stay out of the table: the profiler costs time);
//! each recording is kept as `cache/jfr/<scenario>-<side>.jfr` and
//! summarized (see `jfr_summary`; `--summarize <file.jfr>` redoes that).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};

use crate::parity::Server;
use crate::quiet::{self, Background, Watch};

const SEED: i64 = -1294266004;
const VANILLA_PORT: u16 = 25600;
const OURS_PORT: u16 = 25601;
const MODDED_PORT: u16 = 25602;
const VANILLA_DIR: &str = "cache/vanilla-server/26.3-bench";
const MODDED_DIR: &str = "cache/vanilla-server/26.3-bench-modded";
const FABRIC: &str = "tools/fabric/26.3";
const ALL: &str = "gen10,gen32,bots,idle,hoppers,redstone,entities";

/// What a scenario run gives: what was measured and its numbers.
struct Run {
    what: String,
    metrics: Vec<(&'static str, f64)>,
}

struct Options {
    bots: usize,
    entities: usize,
    seconds: u64,
    cores: usize,
}

pub fn run(args: &[String]) -> Result<()> {
    let mut only = "vanilla,modded,ours".to_owned();
    let mut scenarios = ALL.to_owned();
    let mut options = Options {
        bots: 50,
        entities: 2000,
        seconds: 60,
        cores: 4,
    };
    let mut rounds = 3usize;
    let mut background = 1.0f64;
    let mut noisy = false;
    let mut jfr = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--only" => only = value()?.clone(),
            "--scenarios" => scenarios = value()?.clone(),
            "--bots" => options.bots = value()?.parse()?,
            "--entities" => options.entities = value()?.parse()?,
            "--seconds" => options.seconds = value()?.parse()?,
            "--cores" => options.cores = value()?.parse()?,
            "--rounds" => rounds = value()?.parse::<usize>()?.max(1),
            "--background" => background = value()?.parse()?,
            "--noisy" => noisy = true,
            "--jfr" => jfr = true,
            "--summarize" => {
                println!("{}", jfr_summary(std::path::Path::new(value()?))?);
                return Ok(());
            }
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let sides: Vec<&str> = only
        .split(',')
        .map(|side| match side {
            "vanilla" | "modded" | "ours" => Ok(side),
            other => bail!("--only vanilla,modded,ours, not `{other}`"),
        })
        .collect::<Result<_>>()?;
    let scenarios: Vec<&str> = scenarios.split(',').collect();
    if let Some(bad) = scenarios.iter().find(|s| !ALL.split(',').any(|a| a == **s)) {
        bail!("unknown scenario `{bad}` (one of {ALL})");
    }
    if sides.contains(&"ours") {
        // Build first so the compiler is not the background of a run.
        let status = std::process::Command::new("cargo")
            .args(["build", "--release", "-q", "-p", "mc-server"])
            .current_dir(&root)
            .status()?;
        anyhow::ensure!(status.success(), "building mc-server failed");
    }
    let guard = quiet::Guard::new(background, noisy)?;
    eprintln!("bench: {}", guard.conditions(options.cores));

    let mut results: BTreeMap<(usize, usize), Vec<(Run, Background)>> = BTreeMap::new();
    let extra = usize::from(jfr);
    for round in 0..rounds + extra {
        let profile = round == rounds;
        for (si, &scenario) in scenarios.iter().enumerate() {
            for k in 0..sides.len() {
                let di = (k + round) % sides.len();
                let side = sides[di];
                if profile && side == "ours" {
                    continue;
                }
                let label = if profile {
                    "jfr".to_owned()
                } else {
                    format!("{}/{rounds}", round + 1)
                };
                // A run is redone (twice at most) when the background was
                // busy on average during it; then it is kept, marked.
                let mut attempt = 0;
                let (run, bg) = loop {
                    guard.wait()?;
                    eprintln!("bench: [{label}] {scenario} on {side}");
                    let watch = Watch::start(&guard);
                    let run = run_scenario(&root, side, scenario, &options, profile, &watch);
                    let bg = watch.stop();
                    match run {
                        Ok(run) if bg.mean <= background || noisy || attempt == 2 => {
                            break (run, bg);
                        }
                        Ok(_) => {
                            eprintln!(
                                "bench: background averaged {:.2} cores (at worst {}), running again",
                                bg.mean, bg.who
                            );
                            attempt += 1;
                        }
                        Err(err) => {
                            eprintln!("bench: {scenario} on {side}: error: {err:#}");
                            let run = Run {
                                what: format!("error: {err:#}"),
                                metrics: Vec::new(),
                            };
                            break (run, bg);
                        }
                    }
                };
                eprintln!(
                    "bench: [{label}] {scenario} on {side}: {} (at the peak: {})",
                    describe(&[(&run, &bg)], background),
                    bg.who
                );
                if !profile {
                    results.entry((si, di)).or_default().push((run, bg));
                }
            }
        }
    }
    println!("{}\n", guard.conditions(options.cores));
    println!("| scenario | server | result |\n|---|---|---|");
    for ((si, di), runs) in &results {
        let runs: Vec<(&Run, &Background)> = runs.iter().map(|(r, b)| (r, b)).collect();
        println!(
            "| {} | {} | {} |",
            scenarios[*si],
            sides[*di],
            describe(&runs, background)
        );
    }
    Ok(())
}

/// One table cell: what was measured, each number as the median over the
/// runs with its range, and the background (the highest run mean and the
/// highest one-second peak, in cores).
fn describe(runs: &[(&Run, &Background)], background: f64) -> String {
    let ok: Vec<&Run> = runs
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| !r.metrics.is_empty())
        .collect();
    let Some(first) = ok.first() else {
        return runs.first().map_or(String::new(), |(r, _)| r.what.clone());
    };
    let mut parts = Vec::new();
    for (i, (name, _)) in first.metrics.iter().enumerate() {
        let mut values: Vec<f64> = ok
            .iter()
            .filter_map(|r| r.metrics.get(i))
            .map(|m| m.1)
            .collect();
        values.sort_by(f64::total_cmp);
        let digits = usize::from(name.starts_with("MSPT"));
        let median = values[values.len() / 2];
        let (lo, hi) = (values[0], values[values.len() - 1]);
        parts.push(if values.len() > 1 {
            format!("{name} {median:.digits$} [{lo:.digits$}–{hi:.digits$}]")
        } else {
            format!("{name} {median:.digits$}")
        });
    }
    let mean = runs.iter().map(|(_, b)| b.mean).fold(0.0, f64::max);
    let peak = runs.iter().map(|(_, b)| b.peak).fold(0.0, f64::max);
    let errors = runs.len() - ok.len();
    let mut cell = format!(
        "{}: {}; background ≤ {mean:.2} cores (peak {peak:.1})",
        first.what,
        parts.join(", ")
    );
    if ok.len() > 1 {
        cell += &format!(", {} runs", ok.len());
    }
    if mean > background {
        cell += " ⚠ noisy";
    }
    if errors > 0 {
        cell += &format!(", {errors} failed");
    }
    cell
}

fn run_scenario(
    root: &std::path::Path,
    side: &str,
    scenario: &str,
    options: &Options,
    profile: bool,
    watch: &Watch,
) -> Result<Run> {
    match scenario {
        "gen10" => generation(root, side, 10, options, watch),
        "gen32" => generation(root, side, 32, options, watch),
        "bots" => with_bots(root, side, options, profile, watch),
        _ => contraption(root, side, scenario, options, profile, watch),
    }
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

/// Starts `side` on a new world with `cores` threads' worth of parallelism,
/// and tells `watch` which process is the server.
fn start(
    root: &std::path::Path,
    side: &str,
    properties: &str,
    cores: usize,
    watch: &Watch,
) -> Result<(Server, String)> {
    let addr = format!("127.0.0.1:{}", port(side));
    let jvm = vec![format!("-XX:ActiveProcessorCount={cores}")];
    let server = match side {
        "vanilla" => {
            let dir = root.join(VANILLA_DIR);
            let _ = std::fs::remove_dir_all(dir.join("world"));
            Server::prepare(root, &dir, properties)?;
            Server::start_jar(&root.join("reference/26.3/server.jar"), &dir, &jvm)?
        }
        "modded" => {
            let dir = root.join(MODDED_DIR);
            let _ = std::fs::remove_dir_all(dir.join("world"));
            Server::prepare(root, &dir, properties)?;
            let fabric = root.join(FABRIC);
            let _ = std::fs::remove_dir_all(dir.join("mods"));
            std::fs::create_dir_all(dir.join("mods"))?;
            for mod_jar in std::fs::read_dir(fabric.join("mods"))
                .with_context(|| format!("no {FABRIC}/mods"))?
            {
                let mod_jar = mod_jar?.path();
                std::fs::copy(
                    &mod_jar,
                    dir.join("mods").join(mod_jar.file_name().unwrap()),
                )?;
            }
            // The launcher takes the user's own server.jar instead of
            // downloading one.
            let server_jar = root.join("reference/26.3/server.jar");
            std::fs::write(
                dir.join("fabric-server-launcher.properties"),
                format!("serverJar={}\n", server_jar.display()),
            )?;
            Server::start_jar(&fabric.join("fabric-server-launch.jar"), &dir, &jvm)?
        }
        _ => Server::ours_in_env(
            root,
            &root.join("cache/ours-bench"),
            properties,
            &[
                ("MC_THREADS", cores.to_string()),
                ("TOKIO_WORKER_THREADS", cores.to_string()),
            ],
        )?,
    };
    watch.ignore(server.pid());
    Ok((server, addr))
}

fn port(side: &str) -> u16 {
    match side {
        "vanilla" => VANILLA_PORT,
        "modded" => MODDED_PORT,
        _ => OURS_PORT,
    }
}

fn generation(
    root: &std::path::Path,
    side: &str,
    view_distance: u8,
    options: &Options,
    watch: &Watch,
) -> Result<Run> {
    let (mut server, addr) = start(
        root,
        side,
        &properties(port(side), false, view_distance),
        options.cores,
        watch,
    )?;
    let ids = crate::probe::load_packet_ids(None)?;
    let result = crate::chunk_speed::measure(&addr, ids, "Bench", view_distance as i8);
    let rss = rss_mb(server.pid());
    server.stop();
    let result = result?;
    // "... all 1234 ms (567 chunks/s)": keep the rate and the total time.
    let number = |s: Option<&str>| s.map_or(f64::NAN, ms);
    let rate = number(
        result
            .rsplit_once('(')
            .and_then(|(_, r)| r.strip_suffix(" chunks/s)")),
    );
    let all = number(between(&result, ", all ", " ms"));
    let chunks = number(between(&result, "ms, ", " chunks"));
    Ok(Run {
        what: format!("view distance {view_distance}"),
        metrics: vec![
            ("chunks/s", rate),
            ("chunks", chunks),
            ("ms", all),
            ("RAM MB", rss as f64),
        ],
    })
}

fn with_bots(
    root: &std::path::Path,
    side: &str,
    options: &Options,
    profile: bool,
    watch: &Watch,
) -> Result<Run> {
    let (mut server, addr) = start(
        root,
        side,
        &properties(port(side), false, 10),
        options.cores,
        watch,
    )?;
    let warmup = 30;
    let bot_args: Vec<String> = [
        "--addr",
        &addr,
        "--count",
        &options.bots.to_string(),
        "--seconds",
        &(options.seconds + warmup + 5).to_string(),
        "--view-distance",
        "10",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let bots = std::thread::spawn(move || crate::bots::run(&bot_args));
    std::thread::sleep(Duration::from_secs(warmup));
    let seconds = options.seconds;
    let mspt = profiled(root, &mut server, side, "bots", profile, |s| {
        sample(s, seconds)
    });
    let rss = rss_mb(server.pid());
    let bots = bots.join().expect("bots thread");
    server.stop();
    let mut metrics = mspt?;
    metrics.push(("RAM MB", rss as f64));
    let what = match bots {
        Ok(()) => format!("{} bots", options.bots),
        Err(err) => format!("{} bots ({err:#})", options.bots),
    };
    Ok(Run { what, metrics })
}

fn contraption(
    root: &std::path::Path,
    side: &str,
    scenario: &str,
    options: &Options,
    profile: bool,
    watch: &Watch,
) -> Result<Run> {
    let (mut server, _) = start(
        root,
        side,
        &properties(port(side), true, 4),
        options.cores,
        watch,
    )?;
    let seconds = options.seconds;
    let result = build(&mut server, scenario, options.entities).and_then(|what| {
        std::thread::sleep(Duration::from_secs(20));
        let mut metrics = profiled(root, &mut server, side, scenario, profile, |s| {
            sample(s, seconds)
        })?;
        metrics.push(("RAM MB", rss_mb(server.pid()) as f64));
        Ok(Run { what, metrics })
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

/// Runs `measure`, on the Java sides with `jfr` under their `/jfr`
/// profiler, and keeps the recording as `cache/jfr/<scenario>-<side>.jfr`.
fn profiled(
    root: &std::path::Path,
    server: &mut Server,
    side: &str,
    scenario: &str,
    jfr: bool,
    measure: impl FnOnce(&mut Server) -> Result<Vec<(&'static str, f64)>>,
) -> Result<Vec<(&'static str, f64)>> {
    let dir = match side {
        "vanilla" => VANILLA_DIR,
        "modded" => MODDED_DIR,
        _ => return measure(server),
    };
    if !jfr {
        return measure(server);
    }
    let debug = root.join(dir).join("debug");
    let _ = std::fs::remove_dir_all(&debug);
    server.wait_for("jfr start", "profiling", Duration::from_secs(30))?;
    let result = measure(server);
    server.wait_for("jfr stop", "profiling", Duration::from_secs(60))?;
    // The dump is written after the reply; wait for the file to settle.
    let mut recording = None;
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(500));
        recording = std::fs::read_dir(&debug)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "jfr"));
        if recording.is_some() {
            break;
        }
    }
    let recording = recording.context("no .jfr recording in debug/")?;
    std::thread::sleep(Duration::from_secs(2));
    let kept = root
        .join("cache/jfr")
        .join(format!("{scenario}-{side}.jfr"));
    std::fs::create_dir_all(kept.parent().unwrap())?;
    std::fs::rename(&recording, &kept)?;
    eprintln!("bench: {scenario}: recording {}", kept.display());
    eprintln!("{}", jfr_summary(&kept)?);
    result
}

/// The execution samples of the server thread (the tick) in a recording:
/// the 15 vanilla methods most often on top (the first `net.minecraft` frame
/// of the stack, so JDK and fastutil leaves count for their caller) and the
/// 15 most often anywhere on the stack.
fn jfr_summary(recording: &std::path::Path) -> Result<String> {
    let out = std::process::Command::new(jdk_tool("jfr"))
        .args(["print", "--json", "--stack-depth", "128", "--events"])
        .arg("jdk.ExecutionSample")
        .arg(recording)
        .output()
        .context("running jfr print")?;
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("jfr print output")?;
    let events = json["recording"]["events"]
        .as_array()
        .context("no events")?;
    let mut top: std::collections::HashMap<String, usize> = Default::default();
    let mut inclusive: std::collections::HashMap<String, usize> = Default::default();
    let mut samples = 0;
    for event in events {
        let values = &event["values"];
        if values["sampledThread"]["javaName"] != "Server thread" {
            continue;
        }
        samples += 1;
        let frames = values["stackTrace"]["frames"].as_array();
        let mut seen = std::collections::HashSet::new();
        let mut first = true;
        for frame in frames.into_iter().flatten() {
            let class = frame["method"]["type"]["name"].as_str().unwrap_or("");
            if !class.starts_with("net/minecraft/") {
                continue;
            }
            let class = class.trim_start_matches("net/minecraft/").replace('/', ".");
            let method = format!("{class}.{}", frame["method"]["name"].as_str().unwrap_or(""));
            if first {
                *top.entry(method.clone()).or_default() += 1;
                first = false;
            }
            if seen.insert(method.clone()) {
                *inclusive.entry(method).or_default() += 1;
            }
        }
    }
    let table = |map: &std::collections::HashMap<String, usize>, title: &str| {
        let mut rows: Vec<_> = map.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let mut s = format!("| {title} | % |\n|---|---|\n");
        for (method, n) in rows.into_iter().take(15) {
            s += &format!(
                "| `{method}` | {:.1} |\n",
                100.0 * *n as f64 / samples.max(1) as f64
            );
        }
        s
    };
    Ok(format!(
        "{samples} samples of the server thread\n\n{}\n{}",
        table(&top, "top vanilla frame"),
        table(&inclusive, "on the stack")
    ))
}

/// A JDK tool next to `java`: from `JAVA_HOME`, or `/usr/libexec/java_home`
/// on macOS, or the bare name on `PATH`.
fn jdk_tool(name: &str) -> std::path::PathBuf {
    let home = std::env::var_os("JAVA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let out = std::process::Command::new("/usr/libexec/java_home")
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().into())
        });
    match home {
        Some(home) if home.join("bin").join(name).is_file() => home.join("bin").join(name),
        _ => name.into(),
    }
}

/// Median P50 and P99 (and the mean of the average) over `tick query`
/// replies every 5 s for `seconds`.
fn sample(server: &mut Server, seconds: u64) -> Result<Vec<(&'static str, f64)>> {
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
    Ok(vec![
        ("MSPT p50", median(&mut p50)),
        ("MSPT p99", median(&mut p99)),
        ("MSPT mean", mean),
    ])
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
