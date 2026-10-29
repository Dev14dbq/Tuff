//! Rewrites a world folder the way vanilla `--forceUpgrade` does: every chunk
//! that loads is saved again in the format this server writes.
//!
//! The read is [`mc_storage::RegionFile::chunk_bytes`] plus an NBT parse (what
//! [`mc_storage::RegionStorage::read_chunk`] does). The write is
//! [`mc_storage::RegionFile::write_chunk`], which always stores zlib. A chunk
//! that does not decompress, or whose NBT does not parse, is counted and its
//! bytes stay as they were. A region file that does not open is left on disk.
//!
//! `entities/` and `poi/` sit next to `region/` and use the same region files,
//! so they are rewritten the same way. Chunk NBT is not passed through
//! [`mc_storage::decode_chunk`] / [`mc_storage::encode_chunk`]: that codec needs
//! the block and biome registries and the dimension's height, and this function
//! only has the world folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use mc_storage::RegionFile;

/// What [`upgrade_world`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Region files that opened (`region`, `entities`, and `poi`).
    pub regions: usize,
    /// Chunks that loaded and were written back.
    pub chunks_rewritten: usize,
    /// Chunks inside an opened region that did not load.
    pub chunks_left: usize,
}

/// Loads every stored chunk under `world_dir` and writes it back.
///
/// Overworld region files are `region/` or
/// `dimensions/minecraft/overworld/region`. The nether is `DIM-1/region` or
/// `dimensions/minecraft/the_nether/region`, the end `DIM1/region` or
/// `dimensions/minecraft/the_end/region`. Paper keeps the nether and the end
/// in the sibling folders `<name>_nether` and `<name>_the_end` (their `DIM-1`
/// / `DIM1` and `dimensions/minecraft/...` paths too). `entities` and `poi`
/// next to each of those are included when the directory is there.
pub fn upgrade_world(world_dir: &std::path::Path) -> anyhow::Result<Report> {
    if !world_dir.is_dir() {
        bail!("{} is not a world folder", world_dir.display());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    let mut report = Report::default();
    for dir in storage_dirs(world_dir) {
        upgrade_dir(&dir, now, &mut report)?;
    }
    Ok(report)
}

/// The world folder plus Paper's `<name>_nether` and `<name>_the_end`.
fn roots(world_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![world_dir.to_path_buf()];
    let Some(name) = world_dir.file_name().and_then(|name| name.to_str()) else {
        return roots;
    };
    let Some(parent) = world_dir.parent() else {
        return roots;
    };
    for suffix in ["_nether", "_the_end"] {
        let sibling = parent.join(format!("{name}{suffix}"));
        if sibling.is_dir() {
            roots.push(sibling);
        }
    }
    roots
}

/// Folders that can hold `region`, `entities`, and `poi` for one world root.
fn dimension_bases(root: &Path) -> [PathBuf; 6] {
    [
        root.to_path_buf(),
        root.join("dimensions/minecraft/overworld"),
        root.join("DIM-1"),
        root.join("dimensions/minecraft/the_nether"),
        root.join("DIM1"),
        root.join("dimensions/minecraft/the_end"),
    ]
}

fn storage_dirs(world_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in roots(world_dir) {
        for base in dimension_bases(&root) {
            for kind in ["region", "entities", "poi"] {
                let dir = base.join(kind);
                if !dir.is_dir() {
                    continue;
                }
                let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
                if seen.insert(key) {
                    out.push(dir);
                }
            }
        }
    }
    out
}

fn upgrade_dir(dir: &Path, now: u32, report: &mut Report) -> anyhow::Result<()> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry
            .with_context(|| format!("reading {}", dir.display()))?
            .path();
        if path.extension().is_some_and(|ext| ext == "mca") {
            files.push(path);
        }
    }
    files.sort();
    for path in files {
        upgrade_file(&path, now, report)?;
    }
    Ok(())
}

fn upgrade_file(path: &Path, now: u32, report: &mut Report) -> anyhow::Result<()> {
    // A file that does not open is not a region we can rewrite. Leave it.
    let mut region = match RegionFile::open(path) {
        Ok(region) => region,
        Err(err) => {
            tracing::warn!("not upgrading {} (left on disk): {err}", path.display());
            return Ok(());
        }
    };
    let present = region.chunks();
    report.regions += 1;
    for (x, z) in present {
        let bytes = match region.chunk_bytes(x, z) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                tracing::warn!(
                    "chunk {x},{z} in {} did not load; left as stored",
                    path.display()
                );
                report.chunks_left += 1;
                continue;
            }
            Err(err) => {
                tracing::warn!(
                    "chunk {x},{z} in {} did not load ({err}); left as stored",
                    path.display()
                );
                report.chunks_left += 1;
                continue;
            }
        };
        if let Err(err) = mc_nbt::read_named(&bytes) {
            tracing::warn!(
                "chunk {x},{z} in {} did not load ({err}); left as stored",
                path.display()
            );
            report.chunks_left += 1;
            continue;
        }
        region
            .write_chunk(x, z, &bytes, now)
            .with_context(|| format!("rewriting chunk {x},{z} in {}", path.display()))?;
        report.chunks_rewritten += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("mc-upgrade-{name}-{}-{nanos}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn block_chunk(x: i32, z: i32, block: &str) -> mc_nbt::Tag {
        let mut states = mc_nbt::Compound::new();
        states.insert(
            "palette",
            mc_nbt::Tag::List(vec![mc_nbt::Tag::String(block.to_owned())]),
        );
        let mut section = mc_nbt::Compound::new();
        section.insert("Y", mc_nbt::Tag::Byte(0));
        section.insert("block_states", mc_nbt::Tag::Compound(states));
        let mut root = mc_nbt::Compound::new();
        root.insert("DataVersion", mc_nbt::Tag::Int(5023));
        root.insert("xPos", mc_nbt::Tag::Int(x));
        root.insert("zPos", mc_nbt::Tag::Int(z));
        root.insert("Status", mc_nbt::Tag::String("minecraft:full".into()));
        root.insert(
            "sections",
            mc_nbt::Tag::List(vec![mc_nbt::Tag::Compound(section)]),
        );
        mc_nbt::Tag::Compound(root)
    }

    fn palette_block(tag: &mc_nbt::Tag) -> &str {
        let mc_nbt::Tag::Compound(root) = tag else {
            panic!("chunk root is not a compound");
        };
        let Some(mc_nbt::Tag::List(sections)) = root.get("sections") else {
            panic!("chunk has no sections");
        };
        let Some(mc_nbt::Tag::Compound(section)) = sections.first() else {
            panic!("chunk has no section");
        };
        let Some(mc_nbt::Tag::Compound(states)) = section.get("block_states") else {
            panic!("section has no block_states");
        };
        let Some(mc_nbt::Tag::List(palette)) = states.get("palette") else {
            panic!("block_states has no palette");
        };
        let Some(mc_nbt::Tag::String(name)) = palette.first() else {
            panic!("palette is empty");
        };
        name
    }

    fn write_chunk(dir: &Path, x: i32, z: i32, tag: &mc_nbt::Tag) {
        std::fs::create_dir_all(dir).unwrap();
        let mut bytes = Vec::new();
        mc_nbt::write_named("", tag, &mut bytes).unwrap();
        mc_storage::RegionStorage::new(dir)
            .write_chunk(x, z, &bytes, 1)
            .unwrap();
    }

    fn read_chunk(dir: &Path, x: i32, z: i32) -> mc_nbt::Tag {
        mc_storage::RegionStorage::new(dir)
            .read_chunk(x, z)
            .unwrap()
            .unwrap_or_else(|| panic!("no chunk {x},{z} in {}", dir.display()))
    }

    #[test]
    fn a_chunk_keeps_its_block_and_a_second_upgrade_is_safe() {
        let scratch = Scratch::new("block");
        let world = scratch.0.join("world");
        let region = world.join("region");
        let tag = block_chunk(-1, 33, "minecraft:stone");
        write_chunk(&region, -1, 33, &tag);

        let report = upgrade_world(&world).unwrap();
        assert_eq!(
            report,
            Report {
                regions: 1,
                chunks_rewritten: 1,
                chunks_left: 0,
            }
        );
        let read = read_chunk(&region, -1, 33);
        assert_eq!(palette_block(&read), "minecraft:stone");
        assert_eq!(read, tag);
        assert!(!world.join("DIM-1").exists());
        assert!(!world.join("dimensions").exists());
        assert!(!scratch.0.join("world_nether").exists());

        let again = upgrade_world(&world).unwrap();
        assert_eq!(again.chunks_rewritten, 1);
        assert_eq!(again.chunks_left, 0);
        let read = read_chunk(&region, -1, 33);
        assert_eq!(palette_block(&read), "minecraft:stone");
        assert_eq!(read, tag);
    }

    #[test]
    fn nether_end_paper_entities_and_poi_are_rewritten() {
        let scratch = Scratch::new("dims");
        let world = scratch.0.join("world");
        let places = [
            world.join("DIM-1/region"),
            world.join("DIM1/region"),
            world.join("dimensions/minecraft/overworld/region"),
            world.join("dimensions/minecraft/the_nether/region"),
            world.join("dimensions/minecraft/the_end/region"),
            scratch.0.join("world_nether/DIM-1/region"),
            scratch
                .0
                .join("world_nether/dimensions/minecraft/the_nether/region"),
            scratch.0.join("world_the_end/DIM1/region"),
            scratch
                .0
                .join("world_the_end/dimensions/minecraft/the_end/region"),
            world.join("entities"),
            world.join("poi"),
            world.join("DIM-1/entities"),
            world.join("DIM-1/poi"),
        ];
        let tag = block_chunk(0, 0, "minecraft:stone");
        for place in &places {
            write_chunk(place, 0, 0, &tag);
        }

        let report = upgrade_world(&world).unwrap();
        assert_eq!(report.chunks_left, 0);
        assert_eq!(report.regions, places.len());
        assert_eq!(report.chunks_rewritten, places.len());
        for place in &places {
            assert_eq!(palette_block(&read_chunk(place, 0, 0)), "minecraft:stone");
        }

        let again = upgrade_world(&world).unwrap();
        assert_eq!(again.chunks_left, 0);
        assert_eq!(again.chunks_rewritten, places.len());
        for place in &places {
            assert_eq!(palette_block(&read_chunk(place, 0, 0)), "minecraft:stone");
        }
    }

    #[test]
    fn a_chunk_that_does_not_load_is_left_and_a_bad_file_is_not_deleted() {
        let scratch = Scratch::new("bad");
        let world = scratch.0.join("world");
        let region = world.join("region");
        let tag = block_chunk(0, 0, "minecraft:stone");
        write_chunk(&region, 0, 0, &tag);

        let region_file = region.join("r.0.0.mca");
        let sector = mc_storage::region::SECTOR;
        let mut data = std::fs::read(&region_file).unwrap();
        let start = data.len().div_ceil(sector) * sector;
        data.resize(start + sector, 0);
        let payload = b"NOTZLIB!";
        let length = (payload.len() + 1) as u32;
        data[start..start + 4].copy_from_slice(&length.to_be_bytes());
        data[start + 4] = mc_storage::region::ZLIB;
        data[start + 5..start + 5 + payload.len()].copy_from_slice(payload);
        let offset = u32::try_from(start / sector).unwrap();
        // Local (2, 0), one sector. The stone chunk stays at local (0, 0).
        let index = 4 * 2;
        let location = (offset << 8) | 1;
        data[index..index + 4].copy_from_slice(&location.to_be_bytes());
        std::fs::write(&region_file, &data).unwrap();
        let marker_at = data
            .windows(payload.len())
            .position(|window| window == payload)
            .unwrap();

        let short = region.join("r.9.9.mca");
        let short_bytes = [1_u8, 2, 3, 4, 5];
        std::fs::write(&short, short_bytes).unwrap();

        let report = upgrade_world(&world).unwrap();
        assert_eq!(
            report,
            Report {
                regions: 1,
                chunks_rewritten: 1,
                chunks_left: 1,
            }
        );
        assert_eq!(palette_block(&read_chunk(&region, 0, 0)), "minecraft:stone");
        let after = std::fs::read(&region_file).unwrap();
        assert_eq!(&after[marker_at..marker_at + payload.len()], payload);
        assert_eq!(std::fs::read(&short).unwrap(), short_bytes);
        assert!(short.is_file());
    }

    #[test]
    fn a_missing_world_folder_is_an_error() {
        let missing =
            std::env::temp_dir().join(format!("mc-upgrade-missing-{}-absent", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(upgrade_world(&missing).is_err());
    }
}
