//! Reading Mojang's `server.jar`: the bundler around the real server jar,
//! and copying selected entries out of a jar or zip.

use std::fs::{self, File};
use std::io::{self, BufReader, Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result, ensure};

/// Largest single file taken out of a jar or zip (the biggest vanilla
/// report is a few MB; the inner server jar is about 60 MB).
pub const MAX_ENTRY: u64 = 256 << 20;
/// Largest total taken out of one archive.
pub const MAX_TOTAL: u64 = 2 << 30;
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

/// SHA-256 of bytes in memory, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
        .take(MAX_ENTRY + 1)
        .read_to_end(&mut jar)?;
    ensure!(
        jar.len() as u64 <= MAX_ENTRY,
        "the server jar inside {} is larger than {MAX_ENTRY} bytes",
        bundler.display()
    );
    Ok(jar)
}

/// Extracts the files of the zip `bytes` whose names pass `filter` into
/// `dest`. Returns the number of files and bytes written. An entry over
/// [`MAX_ENTRY`] or a total over [`MAX_TOTAL`] (uncompressed) is an error.
pub fn extract_matching(
    bytes: &[u8],
    dest: &Path,
    filter: impl Fn(&str) -> bool,
) -> Result<(usize, u64)> {
    extract_limited(bytes, dest, filter, MAX_ENTRY, MAX_TOTAL)
}

fn extract_limited(
    bytes: &[u8],
    dest: &Path,
    filter: impl Fn(&str) -> bool,
    max_entry: u64,
    max_total: u64,
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
        ensure!(
            entry.size() <= max_entry,
            "{name} is {} bytes uncompressed, more than {max_entry}",
            entry.size()
        );
        // The declared size can lie; count what actually comes out.
        let copied = io::copy(
            &mut (&mut entry).take(max_entry + 1),
            &mut File::create(&path)?,
        )?;
        ensure!(
            copied <= max_entry,
            "{name} unpacks to more than {max_entry} bytes"
        );
        written += copied;
        ensure!(
            written <= max_total,
            "the archive unpacks to more than {max_total} bytes"
        );
        files += 1;
    }
    Ok((files, written))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn oversized_entries_and_totals_are_refused() {
        let mut out = Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        for name in ["a.json", "b.json"] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&[b'x'; 1000]).unwrap();
        }
        zip.finish().unwrap();
        let bytes = out.into_inner();
        let dir = std::env::temp_dir().join(format!("mc-bootstrap-bomb-{}", std::process::id()));
        assert!(extract_limited(&bytes, &dir, |_| true, 999, 10_000).is_err());
        assert!(extract_limited(&bytes, &dir, |_| true, 1000, 1500).is_err());
        assert_eq!(
            extract_limited(&bytes, &dir, |_| true, 1000, 2000).unwrap(),
            (2, 2000)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
