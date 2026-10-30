//! Reading Mojang's `server.jar`: the bundler around the real server jar,
//! and copying selected entries out of a jar or zip.

use std::fs::{self, File};
use std::io::{self, BufReader, Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use zip::ZipArchive;

/// SHA1 of a file, as lowercase hex (what Mojang's version JSON lists).
pub fn sha1_of_file(path: &Path) -> Result<String> {
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

/// SHA-256 of a file, as lowercase hex (what our release checksums list).
pub fn sha256_of_file(path: &Path) -> Result<String> {
    use sha2::Digest as _;
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Since 1.18 `server.jar` is a bundler; the real server jar is stored inside
/// it under `META-INF/versions/`, listed in `META-INF/versions.list`.
pub fn read_bundled_server_jar(bundler: &Path, version: &str) -> Result<Vec<u8>> {
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

/// Extracts the files of the zip `bytes` whose names pass `filter` into
/// `dest`. Returns the number of files and bytes written.
pub fn extract_matching(
    bytes: &[u8],
    dest: &Path,
    filter: impl Fn(&str) -> bool,
) -> Result<(usize, u64)> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let (mut files, mut written) = (0, 0);
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
            .with_context(|| format!("unsafe path in archive: {name}"))?;
        let path = dest.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        written += io::copy(&mut entry, &mut File::create(&path)?)?;
        files += 1;
    }
    Ok((files, written))
}
