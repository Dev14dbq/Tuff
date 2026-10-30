//! Chunks and entity chunks a world folder keeps from other versions: read
//! as 26.3 writes them, the way vanilla upgrades a document when it loads
//! it. One saved by a newer game is refused, and the file stays as it is.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use mc_data::Blocks;
use mc_nbt::Tag;
use mc_storage::upgrade::{Context, Kind, Unsupported, data_version, upgrade_tag};
use mc_storage::{Palettes, RegionStorage};
use mc_world::Chunk;
use tracing::{error, warn};

use super::{Anvil, HEIGHTMAPS};

/// Upgrade state of one dimension's folder.
#[derive(Debug)]
pub(super) struct Stored {
    dimension: String,
    data_version: i32,
    /// Old documents read as they are, reported so far.
    reported: AtomicU32,
    /// Region chunks saved by a newer game: never loaded, never written.
    refused: Mutex<HashSet<(i32, i32)>>,
}

/// A document saved by a newer game than this one; reading it would lose
/// what we do not know, and writing it would downgrade it.
#[derive(Debug, Clone, Copy)]
pub(super) struct TooNew(pub i32);

impl std::fmt::Display for TooNew {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "saved by a newer version (data version {}); left untouched",
            self.0
        )
    }
}

impl std::error::Error for TooNew {}

impl Stored {
    pub(super) fn new(dimension: String, data_version: i32) -> Self {
        Self {
            dimension,
            data_version,
            reported: AtomicU32::new(0),
            refused: Mutex::default(),
        }
    }

    /// Brings `tag` (the chunk or entity chunk at `x`, `z`) up to date in
    /// memory. A document too old to upgrade is used as it is; one from a
    /// newer game is refused, and a refused region chunk is remembered so
    /// that no save writes over it.
    pub(super) fn upgrade(&self, tag: &mut Tag, kind: Kind, x: i32, z: i32) -> Result<(), TooNew> {
        let ctx = Context {
            dimension: &self.dimension,
        };
        match upgrade_tag(tag, kind, &ctx, self.data_version) {
            Ok(_) => Ok(()),
            Err(Unsupported::TooNew(v)) => {
                if matches!(kind, Kind::Chunk) {
                    self.refused.lock().expect("refused lock").insert((x, z));
                }
                error!("{} {x},{z}: {}", what(kind), TooNew(v));
                Err(TooNew(v))
            }
            Err(why) => {
                if self.reported.fetch_add(1, Ordering::Relaxed) < 5 {
                    warn!("{} {x},{z} was {why}; read as it is", what(kind));
                }
                Ok(())
            }
        }
    }

    /// Whether the region chunk was refused and must not be written.
    pub(super) fn is_refused(&self, x: i32, z: i32) -> bool {
        self.refused.lock().expect("refused lock").contains(&(x, z))
    }

    /// What `storage` keeps at `x`, `z`, upgraded; [`TooNew`] (see
    /// [`refused`]) for a document a newer game saved.
    pub(super) fn read(
        &self,
        storage: &mut RegionStorage,
        kind: Kind,
        x: i32,
        z: i32,
    ) -> anyhow::Result<Option<Tag>> {
        if matches!(kind, Kind::Chunk) && self.is_refused(x, z) {
            return Err(TooNew(0).into());
        }
        let mut read = storage.read_chunk(x, z)?;
        if let Some(tag) = &mut read {
            self.upgrade(tag, kind, x, z)?;
        }
        Ok(read)
    }
}

/// Whether a [`Stored::read`] was refused: the file must be left as it is.
pub(super) fn refused(read: &anyhow::Result<Option<Tag>>) -> bool {
    read.as_ref().is_err_and(|e| e.is::<TooNew>())
}

fn what(kind: Kind) -> &'static str {
    match kind {
        Kind::EntityChunk => "entities of chunk",
        _ => "chunk",
    }
}

impl Anvil {
    /// Whether the folder has a finished chunk here (without decoding it).
    /// One from a newer game counts as finished, so nothing is generated
    /// over it.
    pub(super) fn has_full(&self, x: i32, z: i32) -> bool {
        let raw = self.storage.lock().expect("region storage").read_raw(x, z);
        let Ok(Some(raw)) = raw else {
            return false;
        };
        let Ok(Tag::Compound(root)) = raw.parse() else {
            return false;
        };
        if data_version(&root, Kind::Chunk).is_some_and(|v| v > self.stored.data_version) {
            let mut refused = self.stored.refused.lock().expect("refused lock");
            refused.insert((x, z));
            return true;
        }
        match root.get("Status") {
            Some(Tag::String(s)) => s == "minecraft:full" || s == "full",
            _ => false,
        }
    }

    /// A chunk saved halfway through generation (`minecraft:features`).
    pub(super) fn stored_proto(
        &self,
        x: i32,
        z: i32,
        blocks: &Blocks,
        min_y: i32,
        height: u32,
    ) -> Option<Chunk> {
        let raw = self
            .storage
            .lock()
            .expect("region storage")
            .read_raw(x, z)
            .ok()??;
        let mut tag = raw.parse().ok()?;
        self.stored.upgrade(&mut tag, Kind::Chunk, x, z).ok()?;
        let biome = |name: &str| self.biome_ids.get(name).copied();
        let palettes = Palettes {
            blocks,
            biome: &biome,
            air: self.air,
            fallback_biome: self.fallback_biome,
            unresolved: Default::default(),
        };
        let (status, chunk) =
            mc_storage::decode_with_status(&tag, min_y, height, &palettes).ok()?;
        (status == "minecraft:features").then_some(*chunk)
    }

    /// Computes the heightmaps a stored chunk lacks (`--eraseCache` drops
    /// them, as do some upgrade steps); vanilla does the same on load.
    pub(super) fn fill_heightmaps(&self, chunk: &mut Chunk) {
        let counts = match (&self.generation, &self.flat) {
            (Some(g), _) => &g.counts,
            (None, Some(f)) => &f.counts,
            (None, None) => return,
        };
        for (i, name) in HEIGHTMAPS.iter().enumerate() {
            if chunk.heightmaps.iter().any(|(n, _)| n == name) {
                continue;
            }
            let words =
                chunk.heightmap(|s| counts.get(s as usize).is_some_and(|b| b & (1 << i) != 0));
            chunk.heightmaps.push(((*name).to_owned(), words));
        }
    }
}

#[cfg(test)]
mod tests {
    use mc_nbt::Tag;
    use mc_protocol::Position;
    use mc_storage::RegionStorage;
    use mc_storage::upgrade::TARGET;

    use crate::context::ServerContext;

    fn open(folder: &std::path::Path) -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let mut config = crate::Config::new(dir);
        config.world = Some(folder.to_owned());
        config.seed = Some(1);
        Some(ServerContext::new(config, data).unwrap())
    }

    /// A chunk and a player file saved by a newer game are not loaded, and
    /// saving the world or the player never writes over them.
    #[test]
    fn files_from_a_newer_version_are_left_untouched() {
        let folder = std::env::temp_dir().join(format!("mc-newer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let Some(ctx) = open(&folder) else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let stone = blocks.default_state("minecraft:stone").unwrap();
        for x in [0, 16] {
            ctx.world
                .set_block(Position::new(x, 100, 0), stone, blocks, &ctx.rules);
        }
        assert!(ctx.world.save(blocks).chunks >= 2);
        drop(ctx);

        let regions = folder.join("dimensions/minecraft/overworld/region");
        let mut storage = RegionStorage::new(&regions);
        let Some(Tag::Compound(mut root)) = storage.read_chunk(0, 0).unwrap() else {
            panic!("no chunk 0,0");
        };
        root.insert("DataVersion", Tag::Int(TARGET + 1));
        root.insert("FromTheFuture", Tag::Byte(1));
        let mut bytes = Vec::new();
        mc_nbt::write_named("", &Tag::Compound(root.clone()), &mut bytes).unwrap();
        storage.write_chunk(0, 0, &bytes, 1).unwrap();
        let player = folder.join("players/data/00000000-0000-0000-0000-000000000001.dat");
        let mut newer = mc_nbt::Compound::new();
        newer.insert("DataVersion", Tag::Int(TARGET + 1));
        mc_storage::upgrade::files::write_gz(&player, &newer).unwrap();
        let player_bytes = std::fs::read(&player).unwrap();

        let ctx = open(&folder).unwrap();
        let blocks = &ctx.data.blocks;
        let air = blocks.default_state("minecraft:air").unwrap();
        for x in [0, 16] {
            ctx.world
                .set_block(Position::new(x, 100, 0), air, blocks, &ctx.rules);
        }
        ctx.world.save(blocks);
        let store = ctx.player_files.as_ref().unwrap();
        assert!(store.load(1).is_err(), "the newer player file is refused");
        drop(ctx);

        let mut storage = RegionStorage::new(&regions);
        assert_eq!(
            storage.read_chunk(0, 0).unwrap(),
            Some(Tag::Compound(root)),
            "the newer chunk is kept as it was"
        );
        let Some(Tag::Compound(saved)) = storage.read_chunk(1, 0).unwrap() else {
            panic!("no chunk 1,0");
        };
        assert_eq!(saved.get("DataVersion"), Some(&Tag::Int(TARGET)));
        assert_eq!(std::fs::read(&player).unwrap(), player_bytes);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
