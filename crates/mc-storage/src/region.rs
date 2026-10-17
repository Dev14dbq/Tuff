//! Anvil region files (`r.<x>.<z>.mca`), 32×32 chunks each.
//!
//! Layout (public documentation, "Region file format"): 4 KiB sectors; sector 0
//! holds 1024 big-endian locations (3-byte sector offset + 1-byte sector
//! count), sector 1 holds 1024 timestamps. A chunk starts with a 4-byte length
//! (including the compression byte), a compression byte, then the data.
//! Compression bit 0x80 means the data lives in an external `c.<x>.<z>.mcc`.
//!
//! Writing puts a chunk into free sectors (never over its old ones), then
//! updates the header, so a crash in between leaves the previous version.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::read::{GzDecoder, ZlibDecoder};

pub const SECTOR: usize = 4096;
/// A location entry counts sectors in one byte; bigger chunks go external.
const MAX_SECTORS: usize = 255;
/// Compression bytes.
pub const GZIP: u8 = 1;
pub const ZLIB: u8 = 2;
pub const UNCOMPRESSED: u8 = 3;

#[derive(Debug)]
pub enum StorageError {
    Io(PathBuf, io::Error),
    Corrupt(PathBuf, String),
    UnsupportedCompression(PathBuf, u8),
    Nbt(PathBuf, mc_nbt::NbtError),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Io(path, err) => write!(f, "{}: {err}", path.display()),
            StorageError::Corrupt(path, what) => write!(f, "{}: corrupt: {what}", path.display()),
            StorageError::UnsupportedCompression(path, kind) => {
                write!(
                    f,
                    "{}: unsupported chunk compression {kind}",
                    path.display()
                )
            }
            StorageError::Nbt(path, err) => write!(f, "{}: {err}", path.display()),
        }
    }
}

impl std::error::Error for StorageError {}

pub type Result<T> = std::result::Result<T, StorageError>;

/// One region file, read fully into memory.
#[derive(Debug)]
pub struct RegionFile {
    path: PathBuf,
    data: Vec<u8>,
}

impl RegionFile {
    pub fn open(path: &Path) -> Result<Self> {
        let data = fs::read(path).map_err(|e| StorageError::Io(path.to_owned(), e))?;
        if !data.is_empty() && data.len() < 2 * SECTOR {
            return Err(StorageError::Corrupt(
                path.to_owned(),
                "shorter than the header".into(),
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            data,
        })
    }

    fn corrupt(&self, what: impl Into<String>) -> StorageError {
        StorageError::Corrupt(self.path.clone(), what.into())
    }

    /// Sector offset and count of chunk (`x`, `z`) inside the region (0..32).
    fn location(&self, x: usize, z: usize) -> (usize, usize) {
        if self.data.is_empty() {
            return (0, 0);
        }
        let i = 4 * (x + z * 32);
        let entry = u32::from_be_bytes(self.data[i..i + 4].try_into().expect("4 bytes"));
        ((entry >> 8) as usize, (entry & 0xFF) as usize)
    }

    /// Last save time (Unix seconds) of chunk (`x`, `z`), 0 if absent.
    pub fn timestamp(&self, x: usize, z: usize) -> u32 {
        if self.data.is_empty() {
            return 0;
        }
        let i = SECTOR + 4 * (x + z * 32);
        u32::from_be_bytes(self.data[i..i + 4].try_into().expect("4 bytes"))
    }

    /// Chunks present in this region, as local (x, z).
    pub fn chunks(&self) -> Vec<(usize, usize)> {
        (0..32 * 32)
            .map(|i| (i % 32, i / 32))
            .filter(|&(x, z)| self.location(x, z).0 != 0)
            .collect()
    }

    /// Uncompressed NBT bytes of chunk (`x`, `z`) (0..32 inside the region).
    pub fn chunk_bytes(&self, x: usize, z: usize) -> Result<Option<Vec<u8>>> {
        assert!(x < 32 && z < 32, "local chunk coordinates are 0..32");
        let (offset, sectors) = self.location(x, z);
        if offset == 0 || sectors == 0 {
            return Ok(None);
        }
        let start = offset * SECTOR;
        let header = self
            .data
            .get(start..start + 5)
            .ok_or_else(|| self.corrupt(format!("chunk {x},{z} points past the end")))?;
        let length = u32::from_be_bytes(header[..4].try_into().expect("4 bytes")) as usize;
        let compression = header[4];
        if length == 0 || length > sectors * SECTOR {
            return Err(self.corrupt(format!(
                "chunk {x},{z} has length {length} for {sectors} sectors"
            )));
        }
        let payload: Vec<u8> = if compression & 0x80 != 0 {
            let external = self.external_path(x, z);
            fs::read(&external).map_err(|e| StorageError::Io(external, e))?
        } else {
            self.data
                .get(start + 5..start + 4 + length)
                .ok_or_else(|| self.corrupt(format!("chunk {x},{z} is truncated")))?
                .to_vec()
        };
        decompress(&self.path, compression & 0x7F, &payload).map(Some)
    }

    /// An empty region (just the two header sectors) that will live at `path`.
    pub fn create(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            data: vec![0; 2 * SECTOR],
        }
    }

    /// Stores chunk (`x`, `z`) as zlib-compressed `nbt` with timestamp `time`
    /// (Unix seconds), writing the region file to disk.
    pub fn write_chunk(&mut self, x: usize, z: usize, nbt: &[u8], time: u32) -> Result<()> {
        assert!(x < 32 && z < 32, "local chunk coordinates are 0..32");
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(nbt)
            .and_then(|()| encoder.flush())
            .map_err(|e| StorageError::Io(self.path.clone(), e))?;
        let compressed = encoder
            .finish()
            .map_err(|e| StorageError::Io(self.path.clone(), e))?;
        if self.data.is_empty() {
            self.data = vec![0; 2 * SECTOR];
        }

        // Body: length (compression byte included), compression byte, data.
        let mut body = Vec::with_capacity(compressed.len() + 5);
        let external = self.external_path(x, z);
        if (compressed.len() + 5).div_ceil(SECTOR) > MAX_SECTORS {
            fs::write(&external, &compressed).map_err(|e| StorageError::Io(external.clone(), e))?;
            body.extend_from_slice(&1u32.to_be_bytes());
            body.push(ZLIB | 0x80);
        } else {
            body.extend_from_slice(&(compressed.len() as u32 + 1).to_be_bytes());
            body.push(ZLIB);
            body.extend_from_slice(&compressed);
            if external.exists() {
                let _ = fs::remove_file(&external);
            }
        }
        let sectors = body.len().div_ceil(SECTOR);
        body.resize(sectors * SECTOR, 0);

        let start = self.free_run(sectors);
        let end = (start + sectors) * SECTOR;
        if self.data.len() < end {
            self.data.resize(end, 0);
        }
        self.data[start * SECTOR..end].copy_from_slice(&body);
        let i = 4 * (x + z * 32);
        let location = ((start as u32) << 8) | sectors as u32;
        self.data[i..i + 4].copy_from_slice(&location.to_be_bytes());
        self.data[SECTOR + i..SECTOR + i + 4].copy_from_slice(&time.to_be_bytes());

        // Data first, then the header that points at it.
        let io = |e| StorageError::Io(self.path.clone(), e);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.path)
            .map_err(io)?;
        let on_disk = file.metadata().map_err(io)?.len() as usize;
        if on_disk < 2 * SECTOR {
            file.write_all(&self.data[..2 * SECTOR]).map_err(io)?;
        }
        file.seek(SeekFrom::Start((start * SECTOR) as u64))
            .map_err(io)?;
        file.write_all(&self.data[start * SECTOR..end])
            .map_err(io)?;
        file.sync_data().map_err(io)?;
        file.seek(SeekFrom::Start(i as u64)).map_err(io)?;
        file.write_all(&self.data[i..i + 4]).map_err(io)?;
        file.seek(SeekFrom::Start((SECTOR + i) as u64))
            .map_err(io)?;
        file.write_all(&self.data[SECTOR + i..SECTOR + i + 4])
            .map_err(io)?;
        Ok(())
    }

    /// First run of `count` sectors used by no chunk (the one being replaced
    /// included, so its old copy survives until the header changes).
    fn free_run(&self, count: usize) -> usize {
        let total = self.data.len().div_ceil(SECTOR);
        let mut used = vec![false; total.max(2)];
        used[0] = true;
        used[1] = true;
        for cz in 0..32 {
            for cx in 0..32 {
                let (offset, sectors) = self.location(cx, cz);
                if offset == 0 {
                    continue;
                }
                for s in offset..offset + sectors {
                    if s < used.len() {
                        used[s] = true;
                    }
                }
            }
        }
        let mut run = 0;
        for (sector, &taken) in used.iter().enumerate() {
            run = if taken { 0 } else { run + 1 };
            if run == count {
                return sector + 1 - count;
            }
        }
        // Append, after any free sectors at the end.
        total - run
    }

    /// `c.<chunk x>.<chunk z>.mcc` next to the region, for oversized chunks.
    fn external_path(&self, x: usize, z: usize) -> PathBuf {
        let (rx, rz) = region_coords(&self.path).unwrap_or((0, 0));
        let name = format!("c.{}.{}.mcc", rx * 32 + x as i32, rz * 32 + z as i32);
        self.path.with_file_name(name)
    }
}

fn region_coords(path: &Path) -> Option<(i32, i32)> {
    let name = path.file_name()?.to_str()?;
    let mut parts = name.strip_prefix("r.")?.strip_suffix(".mca")?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn decompress(path: &Path, compression: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let read = match compression {
        1 => GzDecoder::new(payload).read_to_end(&mut out),
        2 => ZlibDecoder::new(payload).read_to_end(&mut out),
        3 => {
            out.extend_from_slice(payload);
            Ok(out.len())
        }
        // 4 = LZ4 (lz4-java block stream), 127 = custom: not supported yet.
        other => return Err(StorageError::UnsupportedCompression(path.to_owned(), other)),
    };
    read.map_err(|e| StorageError::Io(path.to_owned(), e))?;
    Ok(out)
}

/// The region files of one dimension (`.../region/`), opened on demand.
#[derive(Debug)]
pub struct RegionStorage {
    dir: PathBuf,
    open: HashMap<(i32, i32), Option<RegionFile>>,
}

impl RegionStorage {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            open: HashMap::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn region(&mut self, rx: i32, rz: i32) -> Result<&mut Option<RegionFile>> {
        Ok(match self.open.entry((rx, rz)) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let path = self.dir.join(format!("r.{rx}.{rz}.mca"));
                let file = if path.is_file() {
                    Some(RegionFile::open(&path)?)
                } else {
                    None
                };
                e.insert(file)
            }
        })
    }

    /// Writes chunk (`x`, `z`) (world chunk coordinates) as uncompressed NBT
    /// bytes, creating the region file if needed.
    pub fn write_chunk(&mut self, x: i32, z: i32, nbt: &[u8], time: u32) -> Result<()> {
        let (rx, rz) = (x.div_euclid(32), z.div_euclid(32));
        let dir = self.dir.clone();
        let region = self.region(rx, rz)?;
        let region =
            region.get_or_insert_with(|| RegionFile::create(&dir.join(format!("r.{rx}.{rz}.mca"))));
        region.write_chunk(
            x.rem_euclid(32) as usize,
            z.rem_euclid(32) as usize,
            nbt,
            time,
        )
    }

    /// The parsed NBT root of chunk (`x`, `z`) in world chunk coordinates.
    pub fn read_chunk(&mut self, x: i32, z: i32) -> Result<Option<mc_nbt::Tag>> {
        let (rx, rz) = (x.div_euclid(32), z.div_euclid(32));
        let Some(region) = self.region(rx, rz)? else {
            return Ok(None);
        };
        let Some(bytes) =
            region.chunk_bytes(x.rem_euclid(32) as usize, z.rem_euclid(32) as usize)?
        else {
            return Ok(None);
        };
        let (_, tag, _) =
            mc_nbt::read_named(&bytes).map_err(|e| StorageError::Nbt(region.path.clone(), e))?;
        Ok(Some(tag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a region with one zlib chunk at local (3, 5).
    fn region_with_one_chunk(nbt: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(nbt).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut data = vec![0u8; 2 * SECTOR];
        let index = 4 * (3 + 5 * 32);
        let sectors = (compressed.len() + 5).div_ceil(SECTOR);
        data[index..index + 4].copy_from_slice(&((2u32 << 8) | sectors as u32).to_be_bytes());
        data[SECTOR + index..SECTOR + index + 4].copy_from_slice(&1_700_000_000u32.to_be_bytes());
        data.extend_from_slice(&((compressed.len() + 1) as u32).to_be_bytes());
        data.push(2);
        data.extend_from_slice(&compressed);
        data.resize(data.len().div_ceil(SECTOR) * SECTOR, 0);
        data
    }

    #[test]
    fn reads_a_zlib_chunk() {
        let dir = std::env::temp_dir().join(format!("mc-storage-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.0.0.mca");
        let mut root = mc_nbt::Compound::new();
        root.insert("DataVersion", mc_nbt::Tag::Int(5023));
        let mut nbt = Vec::new();
        mc_nbt::write_named("", &mc_nbt::Tag::Compound(root.clone()), &mut nbt).unwrap();
        fs::write(&path, region_with_one_chunk(&nbt)).unwrap();

        let region = RegionFile::open(&path).unwrap();
        assert_eq!(region.chunks(), vec![(3, 5)]);
        assert_eq!(region.timestamp(3, 5), 1_700_000_000);
        assert_eq!(region.chunk_bytes(3, 5).unwrap().unwrap(), nbt);
        assert_eq!(region.chunk_bytes(0, 0).unwrap(), None);

        let mut storage = RegionStorage::new(&dir);
        assert_eq!(
            storage.read_chunk(3, 5).unwrap(),
            Some(mc_nbt::Tag::Compound(root))
        );
        assert_eq!(
            storage.read_chunk(-1, -1).unwrap(),
            None,
            "missing region file"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn written_chunks_read_back_and_keep_old_copies_until_replaced() {
        let dir = std::env::temp_dir().join(format!("mc-storage-write-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let tag = |n: i32, filler: usize| {
            let mut root = mc_nbt::Compound::new();
            root.insert("DataVersion", mc_nbt::Tag::Int(n));
            // Random-looking bytes so the chunk does not compress to nothing.
            let noise: Vec<i8> = (0..filler).map(|i| (i * 7919 % 251) as i8).collect();
            root.insert("noise", mc_nbt::Tag::ByteArray(noise));
            let mut nbt = Vec::new();
            mc_nbt::write_named("", &mc_nbt::Tag::Compound(root.clone()), &mut nbt).unwrap();
            (mc_nbt::Tag::Compound(root), nbt)
        };
        let mut storage = RegionStorage::new(&dir);
        let (_, small_nbt) = tag(1, 100);
        storage.write_chunk(-1, 33, &small_nbt, 7).unwrap();
        let (big, big_nbt) = tag(2, 20_000);
        storage.write_chunk(-1, 33, &big_nbt, 8).unwrap();
        let (other, other_nbt) = tag(3, 10);
        storage.write_chunk(-32, 32, &other_nbt, 9).unwrap();

        // A fresh reader sees what is on disk.
        let mut fresh = RegionStorage::new(&dir);
        assert_eq!(fresh.read_chunk(-1, 33).unwrap(), Some(big));
        assert_eq!(fresh.read_chunk(-32, 32).unwrap(), Some(other));
        let region = RegionFile::open(&dir.join("r.-1.1.mca")).unwrap();
        assert_eq!(region.timestamp(31, 1), 8);
        assert_eq!(region.chunks(), vec![(0, 0), (31, 1)]);
        // The big chunk did not overwrite the small one's sector; the third
        // chunk reused it.
        assert_eq!(region.location(0, 0).0, 2);
        assert_eq!(region.location(31, 1).0, 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn region_coordinates_from_names() {
        assert_eq!(region_coords(Path::new("/w/r.-1.2.mca")), Some((-1, 2)));
        assert_eq!(region_coords(Path::new("r.x.mca")), None);
    }
}
