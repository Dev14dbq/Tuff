//! `cargo xtask extract-data`: vanilla game data from the user's own `server.jar`.
//!
//! Nothing here is ever committed (`/cache/` is in `.gitignore`). Result layout:
//!
//! ```text
//! cache/mcdata/<version>/
//!   manifest.json  what was extracted from which jar, plus summary counts
//!   version.json   from the server jar: protocol and data versions
//!   data/          the built-in vanilla data pack (recipes, loot tables, worldgen, tags, ...)
//!   reports/       output of the built-in data generator (`--reports`): blocks, registries, packets, ...
//! ```

use std::fs::{self, File};
use std::io::{self, BufReader, Cursor, Read};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use zip::ZipArchive;

pub fn run(args: &[String]) -> Result<()> {
    let mut version = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => version = Some(args.next().context("--version needs a value")?.clone()),
            other => bail!("unknown option `{other}` for extract-data"),
        }
    }

    let root = crate::workspace_root();
    let version = match version {
        Some(version) => version,
        None => newest_reference_version(&root.join("reference"))?,
    };
    let reference = root.join("reference").join(&version);
    let server_jar = reference.join("server.jar");
    let meta = read_json(&reference.join(format!("{version}.json")))?;
    println!("extract-data: Minecraft {version}");

    let expected_sha1 = meta["downloads"]["server"]["sha1"]
        .as_str()
        .context("version JSON has no downloads.server.sha1")?;
    let sha1 = sha1_of_file(&server_jar)?;
    ensure!(
        sha1 == expected_sha1,
        "{} has SHA1 {sha1}, the version JSON expects {expected_sha1}",
        server_jar.display()
    );
    println!("  server.jar SHA1 ok ({sha1})");

    let required_java = meta["javaVersion"]["majorVersion"].as_u64().unwrap_or(21);
    let java = java_major_version()?;
    ensure!(
        java >= required_java,
        "Minecraft {version} needs Java {required_java}+, but `java` is {java}"
    );
    println!("  java {java} ok (needs {required_java}+)");

    // Build into a staging directory and swap it in at the end, so a failed run
    // never leaves a half-written cache behind.
    let cache = root.join("cache").join("mcdata");
    let out = cache.join(&version);
    let staging = cache.join(format!(".{version}.partial"));
    remove_dir_if_exists(&staging)?;
    fs::create_dir_all(&staging)?;

    let inner_jar = read_bundled_server_jar(&server_jar, &version)?;
    let (files, bytes) = extract_matching(&inner_jar, &staging, |name| {
        name == "version.json" || name.starts_with("data/")
    })?;
    println!(
        "  data pack: {files} files, {:.1} MB",
        bytes as f64 / 1_000_000.0
    );

    let work = staging.join(".work");
    run_data_generator(&server_jar, &work)?;
    fs::rename(
        work.join("generated").join("reports"),
        staging.join("reports"),
    )
    .context("the data generator produced no reports/ directory")?;
    fs::remove_dir_all(&work)?;
    println!("  reports generated");

    let summary = summarize(&staging)?;
    let manifest = json!({
        "version": version,
        "source": {
            "server_jar": format!("reference/{version}/server.jar"),
            "sha1": sha1,
        },
        "java": java,
        "extracted_at_unix": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        "summary": summary,
    });
    fs::write(
        staging.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;

    remove_dir_if_exists(&out)?;
    fs::rename(&staging, &out)?;
    println!("  -> {}", out.strip_prefix(&root).unwrap_or(&out).display());
    print_summary(&summary);
    Ok(())
}

/// The release in `reference/` with the latest `releaseTime` that has both a
/// version JSON and a `server.jar`.
fn newest_reference_version(reference: &Path) -> Result<String> {
    let mut newest: Option<(String, String)> = None;
    let entries =
        fs::read_dir(reference).with_context(|| format!("cannot read {}", reference.display()))?;
    for entry in entries {
        let dir = entry?.path();
        let Some(id) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let meta_path = dir.join(format!("{id}.json"));
        if !meta_path.is_file() || !dir.join("server.jar").is_file() {
            continue;
        }
        let release_time = read_json(&meta_path)?["releaseTime"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if newest.as_ref().is_none_or(|(time, _)| release_time > *time) {
            newest = Some((release_time, id.to_owned()));
        }
    }
    newest.map(|(_, id)| id).with_context(|| {
        format!(
            "no reference/<version>/ with <version>.json and server.jar in {}",
            reference.display()
        )
    })
}

fn sha1_of_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = sha1_smol::Sha1::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.digest().to_string())
}

fn java_major_version() -> Result<u64> {
    let output = Command::new("java")
        .arg("-version")
        .output()
        .context("cannot run `java`; install a JDK, e.g. `brew install --cask temurin` on macOS")?;
    // `java -version` prints e.g. `openjdk version "25.0.4.1" 2026-08-18` to stderr.
    let text = String::from_utf8_lossy(&output.stderr);
    let version = text
        .split('"')
        .nth(1)
        .with_context(|| format!("unexpected `java -version` output: {text}"))?;
    let mut parts = version.split(['.', '-', '+']);
    let first: u64 = parts.next().unwrap_or_default().parse()?;
    // Java 8 and older report themselves as 1.x.
    Ok(if first == 1 {
        parts.next().unwrap_or_default().parse()?
    } else {
        first
    })
}

/// Since 1.18 `server.jar` is a bundler; the real server jar is stored inside it
/// under `META-INF/versions/`, listed in `META-INF/versions.list`.
fn read_bundled_server_jar(bundler: &Path, version: &str) -> Result<Vec<u8>> {
    let file = File::open(bundler).with_context(|| format!("cannot open {}", bundler.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(file))?;

    let mut list = String::new();
    archive
        .by_name("META-INF/versions.list")
        .context("server.jar has no META-INF/versions.list (not a bundler jar, pre-1.18?)")?
        .read_to_string(&mut list)?;
    // Each line: `<sha256>\t<id>\t<path relative to META-INF/versions/>`.
    let entries: Vec<(&str, &str)> = list
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let _sha256 = fields.next()?;
            Some((fields.next()?, fields.next()?))
        })
        .collect();
    let path = match entries.as_slice() {
        [(_, path)] => *path,
        _ => entries
            .iter()
            .find(|(id, _)| *id == version)
            .map(|(_, path)| *path)
            .with_context(|| format!("versions.list has no entry for {version}"))?,
    };

    let mut jar = Vec::new();
    archive
        .by_name(&format!("META-INF/versions/{path}"))?
        .read_to_end(&mut jar)?;
    Ok(jar)
}

/// Extracts the files of `jar` whose names pass `filter` into `dest`.
/// Returns the number of files and bytes written.
fn extract_matching(
    jar: &[u8],
    dest: &Path,
    filter: impl Fn(&str) -> bool,
) -> Result<(usize, u64)> {
    let mut archive = ZipArchive::new(Cursor::new(jar))?;
    let (mut files, mut bytes) = (0, 0);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name()?.into_owned();
        if !filter(&name) {
            continue;
        }
        let relative = entry
            .enclosed_name()
            .with_context(|| format!("unsafe path in jar: {name}"))?;
        let path = dest.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        bytes += io::copy(&mut entry, &mut File::create(&path)?)?;
        files += 1;
    }
    Ok((files, bytes))
}

/// Runs Mojang's built-in data generator. The bundler unpacks its libraries into
/// the current directory, so it runs inside `work`.
fn run_data_generator(server_jar: &Path, work: &Path) -> Result<()> {
    fs::create_dir_all(work)?;
    let log_path = work.join("generator.log");
    let log = File::create(&log_path)?;
    let status = Command::new("java")
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .arg("-jar")
        .arg(server_jar)
        .args(["--reports", "--output", "generated"])
        .current_dir(work)
        .stdout(log.try_clone()?)
        .stderr(log)
        .status()
        .context("cannot start the data generator")?;
    if !status.success() {
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        bail!(
            "the data generator failed ({status}); last lines of {}:\n{tail}",
            log_path.display()
        );
    }
    Ok(())
}

/// Counts what was extracted and checks the invariants later code relies on.
fn summarize(dir: &Path) -> Result<Value> {
    let version = read_json(&dir.join("version.json"))?;

    let blocks = read_json(&dir.join("reports").join("blocks.json"))?;
    let blocks = blocks.as_object().context("blocks.json is not an object")?;
    let mut state_ids = Vec::new();
    for (name, block) in blocks {
        let states = block["states"]
            .as_array()
            .with_context(|| format!("block {name} has no states"))?;
        for state in states {
            state_ids.push(state["id"].as_u64().context("block state without id")?);
        }
    }
    state_ids.sort_unstable();
    ensure!(
        state_ids.iter().enumerate().all(|(i, &id)| id == i as u64),
        "block state ids are not exactly 0..{}",
        state_ids.len()
    );

    let registries = read_json(&dir.join("reports").join("registries.json"))?;
    let registries = registries
        .as_object()
        .context("registries.json is not an object")?;
    let entries = |key: &str| {
        registries
            .get(key)
            .and_then(|registry| registry["entries"].as_object())
            .map_or(0, Map::len)
    };

    // packets.json: phase -> direction -> packet id -> { protocol_id }.
    let packets_path = dir.join("reports").join("packets.json");
    let mut packets = Map::new();
    if packets_path.is_file()
        && let Some(phases) = read_json(&packets_path)?.as_object()
    {
        for (phase, directions) in phases {
            let counts: Map<String, Value> = directions
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(direction, list)| {
                    Some((direction.clone(), json!(list.as_object()?.len())))
                })
                .collect();
            packets.insert(phase.clone(), Value::Object(counts));
        }
    }

    Ok(json!({
        "name": version["name"],
        "protocol_version": version["protocol_version"],
        "world_version": version["world_version"],
        "blocks": blocks.len(),
        "block_states": state_ids.len(),
        "registries": registries.len(),
        "items": entries("minecraft:item"),
        "entity_types": entries("minecraft:entity_type"),
        "packets": packets,
        "data_pack_files": count_files(&dir.join("data"))?,
    }))
}

fn print_summary(summary: &Value) {
    println!(
        "  version {} · protocol {} · world {}",
        summary["name"], summary["protocol_version"], summary["world_version"]
    );
    println!(
        "  {} blocks, {} block states, {} registries, {} items, {} entity types, {} data pack files",
        summary["blocks"],
        summary["block_states"],
        summary["registries"],
        summary["items"],
        summary["entity_types"],
        summary["data_pack_files"]
    );
    if let Some(phases) = summary["packets"].as_object() {
        let phases: Vec<String> = phases
            .iter()
            .map(|(phase, directions)| {
                let counts: Vec<String> = directions
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(direction, count)| format!("{direction} {count}"))
                    .collect();
                format!("{phase} ({})", counts.join(", "))
            })
            .collect();
        println!("  packets: {}", phases.join("; "));
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("cannot parse {}", path.display()))
}

fn count_files(dir: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        count += if entry.file_type()?.is_dir() {
            count_files(&entry.path())?
        } else {
            1
        };
    }
    Ok(count)
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.into()),
        _ => Ok(()),
    }
}
