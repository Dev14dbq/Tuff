//! What a release needs besides the binary, built where Java is available
//! (CI): `fetch-server-jar` gets Mojang's jar the way `tuff` does at its
//! first start, and `reports-bundle` packs the generated tables that `tuff`
//! cannot make without Java into `tuff-reports-<version>.zip`.
//!
//! The bundle holds `reports/` from vanilla's data generator, the command
//! tree plus game rule defaults from `observed/commands.json`, and the block
//! state facts `cargo xtask observe` records ([`OBSERVED_FACTS`]): ids, names
//! and numbers per block state. No Mojang assets, no data pack, no jar, and
//! no recorded structure layouts (`monuments.json`, `mansions.json`).

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use mc_bootstrap::{jar, mojang};
use zip::write::SimpleFileOptions;

/// One number per block state each, from `cargo xtask observe <fact>`.
pub const OBSERVED_FACTS: [&str; 6] = [
    "fence_attach.json",
    "conductor.json",
    "opacity.json",
    "instrument.json",
    "support.json",
    "push.json",
];

fn version_arg(args: &[String]) -> Result<(String, Option<PathBuf>)> {
    let mut version = mc_protocol::VERSION_NAME.to_owned();
    let mut out = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--version" => version = it.next().context("--version needs a value")?.clone(),
            "--out" => out = Some(it.next().context("--out needs a folder")?.into()),
            other => bail!("unknown option `{other}`"),
        }
    }
    Ok((version, out))
}

/// `cargo xtask fetch-server-jar [--version <id>]`: Mojang's version JSON and
/// `server.jar` into `reference/<id>/`, checked by SHA1.
pub fn fetch_server_jar(args: &[String]) -> Result<()> {
    let (version, _) = version_arg(args)?;
    let dir = crate::workspace_root().join("reference").join(&version);
    let meta = mojang::version_json(&version)?;
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join(format!("{version}.json")),
        serde_json::to_string_pretty(&meta)?,
    )?;
    let (url, sha1, size) = mojang::server_download(&meta)?;
    let jar_path = dir.join("server.jar");
    if jar_path.is_file() && jar::sha1_of_file(&jar_path)? == sha1 {
        println!("{} is already there (SHA1 {sha1})", jar_path.display());
        return Ok(());
    }
    println!("downloading {url}");
    mojang::download_checked(&url, &sha1, size, &jar_path)?;
    println!("{} (SHA1 {sha1})", jar_path.display());
    Ok(())
}

/// `cargo xtask reports-bundle [--version <id>] [--out <dir>]`: packs
/// `cache/mcdata/<id>/reports`, `observed/commands.json` and the observed
/// block facts into `<dir>/tuff-reports-<id>.zip` (default `dist/`) with a
/// `.sha256` file.
pub fn reports_bundle(args: &[String]) -> Result<()> {
    let (version, out) = version_arg(args)?;
    let root = crate::workspace_root();
    let data = root.join("cache/mcdata").join(&version);
    let observed = data.join("observed/commands.json");
    ensure!(
        data.join("reports/blocks.json").is_file(),
        "no {}/reports (run cargo xtask extract-data)",
        data.display()
    );
    ensure!(
        observed.is_file(),
        "no {} (run cargo xtask observe-commands)",
        observed.display()
    );
    let facts: Vec<PathBuf> = OBSERVED_FACTS
        .iter()
        .map(|f| data.join("observed").join(f))
        .collect();
    for fact in &facts {
        ensure!(
            fact.is_file(),
            "no {} (run cargo xtask observe)",
            fact.display()
        );
    }
    let out = out.unwrap_or_else(|| root.join("dist"));
    fs::create_dir_all(&out)?;
    let name = mc_bootstrap::tables_name(&version);
    let path = out.join(&name);

    let mut files = Vec::new();
    collect(&data.join("reports"), &mut files)?;
    files.sort();
    files.push(observed);
    files.extend(facts);
    let mut zip = zip::ZipWriter::new(File::create(&path)?);
    // Fixed timestamps, so the same tables give the same bytes.
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    let mut buf = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(&data)?;
        let entry = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        zip.start_file(entry, options)?;
        buf.clear();
        File::open(file)?.read_to_end(&mut buf)?;
        zip.write_all(&buf)?;
    }
    zip.finish()?;
    let sha256 = jar::sha256_of_file(&path)?;
    fs::write(
        out.join(format!("{name}.sha256")),
        format!("{sha256}  {name}\n"),
    )?;
    println!(
        "{} files -> {} (SHA-256 {sha256})",
        files.len(),
        path.display()
    );
    Ok(())
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), out)?;
        } else {
            out.push(entry.path());
        }
    }
    Ok(())
}
