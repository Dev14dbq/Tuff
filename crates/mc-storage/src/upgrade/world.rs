//! `--forceUpgrade`: every chunk, entity and point-of-interest region, and
//! every player file, brought to the current version and written back.
//!
//! Like vanilla, a chunk is only written when it was older than the target
//! (or `--eraseCache` removed something from it). With `erase_cache`,
//! heightmaps and light are dropped so they are computed again on load. A
//! chunk that does not load, or is too old or too new to upgrade, is counted
//! and its bytes stay as they were; a region file that does not open is left
//! on disk.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};

use super::{Context, Kind, upgrade};
use crate::RegionFile;

/// What [`upgrade_world`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Region files that opened (`region`, `entities`, and `poi`).
    pub regions: usize,
    /// Chunks written back.
    pub chunks_rewritten: usize,
    /// Chunks that were current already.
    pub chunks_current: usize,
    /// Chunks inside an opened region that did not load or could not be
    /// upgraded.
    pub chunks_left: usize,
    /// Player files written back.
    pub players: usize,
}

/// One folder of region files and what is in it.
struct Folder {
    dir: PathBuf,
    kind: Kind,
    dimension: &'static str,
}

/// Folders that can hold a dimension: (folder, dimension), old layout
/// included in case the file fix could not move something.
fn dimension_bases(root: &Path) -> [(PathBuf, &'static str); 6] {
    [
        (
            root.join("dimensions/minecraft/overworld"),
            "minecraft:overworld",
        ),
        (root.to_path_buf(), "minecraft:overworld"),
        (
            root.join("dimensions/minecraft/the_nether"),
            "minecraft:the_nether",
        ),
        (root.join("DIM-1"), "minecraft:the_nether"),
        (
            root.join("dimensions/minecraft/the_end"),
            "minecraft:the_end",
        ),
        (root.join("DIM1"), "minecraft:the_end"),
    ]
}

fn folders(world_dir: &Path) -> Vec<Folder> {
    let mut roots = vec![world_dir.to_path_buf()];
    if let (Some(name), Some(parent)) = (
        world_dir.file_name().and_then(|n| n.to_str()),
        world_dir.parent(),
    ) {
        for suffix in ["_nether", "_the_end"] {
            let sibling = parent.join(format!("{name}{suffix}"));
            if sibling.is_dir() {
                roots.push(sibling);
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        for (base, dimension) in dimension_bases(&root) {
            for (sub, kind) in [
                ("region", Kind::Chunk),
                ("entities", Kind::EntityChunk),
                ("poi", Kind::Poi),
            ] {
                let dir = base.join(sub);
                if !dir.is_dir() {
                    continue;
                }
                let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
                if seen.insert(key) {
                    out.push(Folder {
                        dir,
                        kind,
                        dimension,
                    });
                }
            }
        }
    }
    out
}

/// Drops what `--eraseCache` makes the game compute again; whether there was
/// any.
fn erase_cache(root: &mut Compound) -> bool {
    let mut changed = root.remove("Heightmaps").is_some();
    changed |= root.remove("isLightOn").is_some();
    if let Some(Tag::List(sections)) = root.get_mut("sections") {
        for s in sections {
            if let Tag::Compound(s) = s {
                changed |= s.remove("BlockLight").is_some();
                changed |= s.remove("SkyLight").is_some();
            }
        }
    }
    changed
}

/// Upgrades everything stored under `world_dir` to `target`. `note` gets one
/// line per thing left as it was.
pub fn upgrade_world(
    world_dir: &Path,
    target: i32,
    erase: bool,
    note: &mut dyn FnMut(String),
) -> std::io::Result<Report> {
    if !world_dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not a world folder", world_dir.display()),
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    let mut report = Report::default();
    for folder in folders(world_dir) {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&folder.dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "mca"))
            .collect();
        files.sort();
        for path in files {
            upgrade_file(&path, &folder, target, erase, now, &mut report, note)?;
        }
    }
    for dir in ["players/data", "playerdata"] {
        let Ok(entries) = std::fs::read_dir(world_dir.join(dir)) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|ext| ext != "dat") {
                continue;
            }
            let Ok(mut root) = super::files::read_gz(&path) else {
                note(format!(
                    "player file {} did not load; left as stored",
                    path.display()
                ));
                continue;
            };
            match upgrade(&mut root, Kind::Player, &Context::default(), target) {
                Ok(true) => {
                    super::files::write_gz(&path, &root)?;
                    report.players += 1;
                }
                Ok(false) => {}
                Err(why) => note(format!(
                    "player file {} was {why}; left as stored",
                    path.display()
                )),
            }
        }
    }
    Ok(report)
}

fn upgrade_file(
    path: &Path,
    folder: &Folder,
    target: i32,
    erase: bool,
    now: u32,
    report: &mut Report,
    note: &mut dyn FnMut(String),
) -> std::io::Result<()> {
    let mut region = match RegionFile::open(path) {
        Ok(region) => region,
        Err(err) => {
            note(format!(
                "not upgrading {} (left on disk): {err}",
                path.display()
            ));
            return Ok(());
        }
    };
    report.regions += 1;
    let ctx = Context {
        dimension: folder.dimension,
    };
    for (x, z) in region.chunks() {
        let bytes = match region.chunk_bytes(x, z) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                note(format!(
                    "chunk {x},{z} in {} did not load; left as stored",
                    path.display()
                ));
                report.chunks_left += 1;
                continue;
            }
            Err(err) => {
                note(format!(
                    "chunk {x},{z} in {} did not load ({err}); left as stored",
                    path.display()
                ));
                report.chunks_left += 1;
                continue;
            }
        };
        let mut root = match mc_nbt::read_named(&bytes) {
            Ok((_, Tag::Compound(root), _)) => root,
            Ok(_) | Err(_) => {
                note(format!(
                    "chunk {x},{z} in {} did not load (bad NBT); left as stored",
                    path.display()
                ));
                report.chunks_left += 1;
                continue;
            }
        };
        let mut changed = match upgrade(&mut root, folder.kind, &ctx, target) {
            Ok(changed) => changed,
            Err(why) => {
                note(format!(
                    "chunk {x},{z} in {} was {why}; left as stored",
                    path.display()
                ));
                report.chunks_left += 1;
                continue;
            }
        };
        if erase && folder.kind == Kind::Chunk {
            changed |= erase_cache(&mut root);
        }
        if !changed {
            report.chunks_current += 1;
            continue;
        }
        let mut out = Vec::new();
        mc_nbt::write_named("", &Tag::Compound(root), &mut out)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        region.write_chunk(x, z, &out, now).map_err(|e| {
            std::io::Error::other(format!(
                "rewriting chunk {x},{z} in {}: {e}",
                path.display()
            ))
        })?;
        report.chunks_rewritten += 1;
    }
    Ok(())
}
