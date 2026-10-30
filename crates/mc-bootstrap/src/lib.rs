//! Game data for a server that has none yet, with no Java and no Rust
//! toolchain: `server.jar` comes from Mojang's own download service, the
//! built-in data pack and `version.json` are read out of it here, and the
//! tables vanilla's data generator writes (`reports/`), plus the command
//! tree, game rule defaults and per-state block facts a vanilla server was
//! seen to have (`observed/`), come from the Tuff release.
//!
//! Tuff never ships Mojang files: the jar is downloaded by the user's own
//! server from Mojang, and the release holds only generated number and name
//! tables. Result layout, the same as `cargo xtask extract-data` writes:
//!
//! ```text
//! <cache>/mcdata/<version>/
//!   manifest.json  where everything came from
//!   version.json   from the server jar: protocol and data versions
//!   data/          the built-in vanilla data pack
//!   assets/minecraft/lang/en_us.json  English text for the console
//!   reports/       data generator tables (from the release)
//!   observed/      command tree, game rules, block facts (from the release)
//! <cache>/mojang/<version>/
//!   <version>.json, server.jar  as downloaded, kept for the next run
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tracing::{info, warn};

pub mod jar;
pub mod mojang;

/// File name of the generated tables for a Minecraft version in a release.
pub fn tables_name(version: &str) -> String {
    format!("tuff-reports-{version}.zip")
}

/// Where the generated tables come from.
#[derive(Debug, Clone)]
pub enum Tables {
    /// A `tuff-reports-<version>.zip` on disk.
    File(PathBuf),
    /// Downloaded from a release: `<base>/tuff-reports-<version>.zip`,
    /// checked against `<base>/tuff-reports-<version>.zip.sha256`.
    Release { base: String },
}

/// What [`prepare`] makes.
#[derive(Debug, Clone)]
pub struct Prepare<'a> {
    /// Minecraft version (`26.3`).
    pub version: &'a str,
    /// Protocol number `version.json` must carry.
    pub protocol: i32,
    /// The `cache` folder next to the server.
    pub cache: &'a Path,
    pub tables: Tables,
}

/// Whether `dir` holds prepared data for `protocol`.
pub fn is_prepared(dir: &Path, protocol: i32) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("version.json")) else {
        return false;
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["protocol_version"].as_i64())
        == Some(i64::from(protocol))
        && dir.join("reports/blocks.json").is_file()
        && dir.join("data").is_dir()
}

/// Makes `<cache>/mcdata/<version>` and returns it. Built in a staging
/// folder and moved into place at the end, so a failed run leaves nothing
/// half-written.
pub fn prepare(p: &Prepare) -> Result<PathBuf> {
    let out = p.cache.join("mcdata").join(p.version);
    if is_prepared(&out, p.protocol) {
        return Ok(out);
    }
    info!(
        "No game data for Minecraft {} yet; preparing it in {}",
        p.version,
        out.display()
    );
    // The small tables first, so a release without them fails before the
    // jar download.
    let (tables, tables_source) = tables_zip(p)?;
    let (jar, jar_url, jar_sha1) = server_jar(p)?;

    let staging = p
        .cache
        .join("mcdata")
        .join(format!(".{}.partial", p.version));
    remove_dir_if_exists(&staging)?;
    fs::create_dir_all(&staging)?;
    let built = fill(p, &staging, &jar, &tables).and_then(|()| {
        let manifest = json!({
            "version": p.version,
            "source": {
                "server_jar": jar_url,
                "sha1": jar_sha1,
                "tables": tables_source,
                "tables_sha256": jar::sha256_of_file(&tables)?,
            },
            "prepared_at_unix": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        });
        fs::write(
            staging.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)? + "\n",
        )?;
        remove_dir_if_exists(&out)?;
        fs::rename(&staging, &out)?;
        Ok(())
    });
    if built.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    built?;
    info!("Game data for Minecraft {} is ready", p.version);
    Ok(out)
}

/// The data pack and `version.json` from the jar, the tables from the zip.
fn fill(p: &Prepare, staging: &Path, jar: &Path, tables: &Path) -> Result<()> {
    info!("Reading the data pack out of {}", jar.display());
    let inner = jar::read_bundled_server_jar(jar, p.version)?;
    let (files, _) = jar::extract_matching(&inner, staging, |name| {
        name == "version.json"
            || name.starts_with("data/")
            || name == "assets/minecraft/lang/en_us.json"
    })?;
    ensure!(
        files > 1000,
        "{} holds only {files} data files",
        jar.display()
    );
    ensure!(
        is_protocol(&staging.join("version.json"), p.protocol),
        "{} is not the server for protocol {}",
        jar.display(),
        p.protocol
    );
    let bytes = fs::read(tables).with_context(|| format!("reading {}", tables.display()))?;
    jar::extract_matching(&bytes, staging, |name| {
        name.starts_with("reports/")
            || name
                .strip_prefix("observed/")
                .is_some_and(|f| f.ends_with(".json") && !f.contains('/'))
    })?;
    ensure!(
        staging.join("reports/blocks.json").is_file()
            && staging.join("reports/registries.json").is_file(),
        "{} has no reports/",
        tables.display()
    );
    Ok(())
}

fn is_protocol(version_json: &Path, protocol: i32) -> bool {
    fs::read_to_string(version_json)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["protocol_version"].as_i64())
        == Some(i64::from(protocol))
}

/// `server.jar` for the version: the one kept from an earlier run if its
/// SHA1 still matches, else a fresh download from Mojang.
fn server_jar(p: &Prepare) -> Result<(PathBuf, String, String)> {
    let dir = p.cache.join("mojang").join(p.version);
    let meta_path = dir.join(format!("{}.json", p.version));
    let jar = dir.join("server.jar");
    let meta = match fs::read(&meta_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    {
        Some(meta) => meta,
        None => {
            info!(
                "Looking up Minecraft {} in Mojang's version manifest ({})",
                p.version,
                mojang::VERSION_MANIFEST
            );
            let meta = mojang::version_json(p.version)?;
            fs::create_dir_all(&dir)?;
            fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
            meta
        }
    };
    let (url, sha1, size) = mojang::server_download(&meta)?;
    if jar.is_file() && jar::sha1_of_file(&jar)? == sha1 {
        return Ok((jar, url, sha1));
    }
    info!(
        "Downloading Minecraft {} server.jar ({:.1} MB) from {url}: Tuff reads the vanilla data pack from it and ships none",
        p.version,
        size as f64 / 1_000_000.0
    );
    mojang::download_checked(&url, &sha1, size, &jar)?;
    info!("server.jar SHA1 ok ({sha1}), saved as {}", jar.display());
    Ok((jar, url, sha1))
}

/// The tables zip on disk, and where it came from.
fn tables_zip(p: &Prepare) -> Result<(PathBuf, String)> {
    match &p.tables {
        Tables::File(path) => {
            ensure!(path.is_file(), "no {}", path.display());
            warn!(
                "Using generated tables from {} without a checksum (SHA-256 {}); it should be the file from a Tuff release archive",
                path.display(),
                jar::sha256_of_file(path)?
            );
            Ok((path.clone(), path.display().to_string()))
        }
        Tables::Release { base } => {
            let name = tables_name(p.version);
            let dest = p.cache.join("mojang").join(p.version).join(&name);
            let url = format!("{base}/{name}");
            let sums = mojang::get_bytes(&format!("{url}.sha256"), 4096)
                .with_context(|| format!("no {name} in this release; pass --reports <zip>"))?;
            let expected = String::from_utf8_lossy(&sums)
                .split_whitespace()
                .next()
                .map(str::to_ascii_lowercase)
                .context("empty checksum file")?;
            if dest.is_file() && jar::sha256_of_file(&dest)? == expected {
                return Ok((dest, url));
            }
            info!(
                "Downloading generated tables (block, registry, packet and command ids) from {url}"
            );
            let bytes = mojang::get_bytes(&url, 256 << 20)?;
            let got = jar::sha256_hex(&bytes);
            if got != expected {
                bail!("{url} has SHA-256 {got}, the release lists {expected}");
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            let part = dest.with_extension("part");
            fs::write(&part, &bytes)?;
            fs::rename(&part, &dest)?;
            Ok((dest, url))
        }
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for (name, body) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn extraction_keeps_only_matching_entries_and_refuses_escapes() {
        let dir = std::env::temp_dir().join(format!("mc-bootstrap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let bytes = zip_of(&[
            ("reports/blocks.json", b"{}"),
            ("assets/minecraft/textures/a.png", b"x"),
        ]);
        let (files, _) =
            jar::extract_matching(&bytes, &dir, |n| n.starts_with("reports/")).unwrap();
        assert_eq!(files, 1);
        assert!(dir.join("reports/blocks.json").is_file());
        assert!(!dir.join("assets").exists());

        let evil = zip_of(&[("../escape.json", b"{}")]);
        assert!(jar::extract_matching(&evil, &dir, |_| true).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_mojang_hosts_are_downloaded_from() {
        let meta = json!({"downloads": {"server": {
            "url": "https://example.com/server.jar", "sha1": "00", "size": 1
        }}});
        assert!(mojang::server_download(&meta).is_err());
        let meta = json!({"downloads": {"server": {
            "url": "https://piston-data.mojang.com/v1/objects/00/server.jar", "sha1": "00", "size": 1
        }}});
        assert!(mojang::server_download(&meta).is_ok());
        let meta = json!({"downloads": {"server": {
            "url": "https://piston-data.mojang.com/v1/objects/00/server.jar", "sha1": "00"
        }}});
        assert!(mojang::server_download(&meta).is_err(), "size is required");
    }
}
