//! Mojang's public download service, as the official launcher uses it: the
//! version manifest lists every version with the URL and SHA1 of its version
//! JSON, and the version JSON gives the URL, SHA1 and size of `server.jar`.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

pub const VERSION_MANIFEST: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// Hosts a download may come from: Mojang's own, nothing a JSON could
/// redirect us to elsewhere.
const MOJANG_HOSTS: [&str; 2] = [
    "https://piston-meta.mojang.com/",
    "https://piston-data.mojang.com/",
];

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(Duration::from_secs(600)))
        .build()
        .into()
}

fn mojang_url(url: &str) -> Result<&str> {
    ensure!(
        MOJANG_HOSTS.iter().any(|host| url.starts_with(host)),
        "refusing to download from {url}: not a Mojang host"
    );
    Ok(url)
}

/// The body of `url` as bytes, up to `limit`.
pub fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>> {
    let response = agent()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {url}"))?;
    ensure!(
        bytes.len() as u64 <= limit,
        "{url} is larger than {limit} bytes"
    );
    Ok(bytes)
}

fn sha1_hex(bytes: &[u8]) -> String {
    sha1_smol::Sha1::from(bytes).digest().to_string()
}

/// The version JSON of `version` (`26.3`), checked against the SHA1 the
/// manifest gives for it.
pub fn version_json(version: &str) -> Result<Value> {
    let manifest: Value = serde_json::from_slice(&get_bytes(VERSION_MANIFEST, 16 << 20)?)
        .context("version manifest is not JSON")?;
    let entry = manifest["versions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["id"] == version)
        .with_context(|| format!("Mojang's version manifest has no version {version}"))?;
    let url = mojang_url(entry["url"].as_str().context("manifest entry has no url")?)?;
    let bytes = get_bytes(url, 16 << 20)?;
    if let Some(expected) = entry["sha1"].as_str() {
        let sha1 = sha1_hex(&bytes);
        ensure!(
            sha1 == expected,
            "{url} has SHA1 {sha1}, the manifest expects {expected}"
        );
    }
    serde_json::from_slice(&bytes).with_context(|| format!("{url} is not JSON"))
}

/// URL, SHA1 and size of the server jar named by a version JSON.
pub fn server_download(meta: &Value) -> Result<(String, String, u64)> {
    let server = &meta["downloads"]["server"];
    let url = mojang_url(
        server["url"]
            .as_str()
            .context("version JSON has no server download")?,
    )?;
    let sha1 = server["sha1"]
        .as_str()
        .context("server download has no sha1")?;
    let size = server["size"].as_u64().unwrap_or(0);
    Ok((url.to_owned(), sha1.to_owned(), size))
}

/// Downloads `url` to `dest`, checking its SHA1; the file appears only once
/// it is complete and correct.
pub fn download_checked(url: &str, sha1: &str, size: u64, dest: &Path) -> Result<()> {
    let url = mojang_url(url)?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let part = dest.with_extension("part");
    let response = agent()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = response.into_body().into_reader();
    let mut file = File::create(&part).with_context(|| format!("creating {}", part.display()))?;
    let mut hasher = sha1_smol::Sha1::new();
    let mut buf = vec![0; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {url}")),
        };
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        total += n as u64;
        if size > 0 && total > size {
            bail!("{url} is larger than the {size} bytes Mojang lists");
        }
    }
    file.sync_all()?;
    drop(file);
    let got = hasher.digest().to_string();
    if got != sha1 {
        let _ = fs::remove_file(&part);
        bail!("{url} has SHA1 {got}, Mojang lists {sha1}");
    }
    fs::rename(&part, dest)?;
    Ok(())
}
