//! Points of interest (minecraft.wiki, "Point of Interest"): the blocks
//! villagers (and bees, and portals) look for. Each one has tickets: a bed
//! or a workstation one, a bell 32 (everyone may meet at it), and a villager
//! that takes one holds it until it lets go (it dies, the job is lost, the
//! block is broken and the point with it).
//!
//! The points of a chunk are read when the chunk comes into memory and
//! checked against its blocks (blocks are the truth, tickets are kept from the
//! file), follow every block placed or broken, and are saved to
//! `poi/r.x.z.mca` as vanilla does: per chunk, `Sections` by section height,
//! each with `Valid` and the `Records` (`type`, `pos`, `free_tickets`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_storage::RegionStorage;
use mc_world::Chunk;
use tracing::warn;

use crate::context::ServerContext;
use crate::tick::lock;

pub const HOME: &str = "minecraft:home";
pub const MEETING: &str = "minecraft:meeting_point";
pub const BEEHIVE: &str = "minecraft:beehive";
pub const BEE_NEST: &str = "minecraft:bee_nest";
pub const NETHER_PORTAL: &str = "minecraft:nether_portal";
pub const LODESTONE: &str = "minecraft:lodestone";
pub const LIGHTNING_ROD: &str = "minecraft:lightning_rod";

/// Every type and the tickets one of its points has.
const KINDS: &[(&str, i32)] = &[
    (HOME, 1),
    (MEETING, 32),
    ("minecraft:armorer", 1),
    ("minecraft:butcher", 1),
    ("minecraft:cartographer", 1),
    ("minecraft:cleric", 1),
    ("minecraft:farmer", 1),
    ("minecraft:fisherman", 1),
    ("minecraft:fletcher", 1),
    ("minecraft:leatherworker", 1),
    ("minecraft:librarian", 1),
    ("minecraft:mason", 1),
    ("minecraft:shepherd", 1),
    ("minecraft:toolsmith", 1),
    ("minecraft:weaponsmith", 1),
    (BEEHIVE, 0),
    (BEE_NEST, 0),
    (NETHER_PORTAL, 0),
    (LODESTONE, 0),
    (LIGHTNING_ROD, 0),
];

/// Tickets a point of this type starts with.
/// Whether a point is a beehive or a bee nest.
pub fn is_hive_kind(kind: &str) -> bool {
    kind == BEEHIVE || kind == BEE_NEST
}

pub fn tickets_of(kind: &str) -> i32 {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(0, |&(_, t)| t)
}

/// The type a block name makes (a bed counts only where it is its head: see
/// [`Table`]).
fn kind_of_name(name: &str) -> Option<&'static str> {
    let kind = match name {
        "minecraft:bell" => MEETING,
        "minecraft:beehive" => BEEHIVE,
        "minecraft:bee_nest" => BEE_NEST,
        "minecraft:nether_portal" => NETHER_PORTAL,
        "minecraft:lodestone" => LODESTONE,
        "minecraft:lightning_rod" => LIGHTNING_ROD,
        n if n.ends_with("_bed") => HOME,
        n => {
            let profession = crate::trading::profession_for(n)?;
            return KINDS
                .iter()
                .find(|(k, _)| k.strip_prefix("minecraft:") == Some(profession))
                .map(|&(k, _)| k);
        }
    };
    Some(kind)
}

/// Whether a block may be a point of interest.
pub fn is_poi_block(name: &str) -> bool {
    kind_of_name(name).is_some()
}

/// The profession a job-site type gives (`minecraft:farmer` → `farmer`).
pub fn profession_of(kind: &str) -> Option<&'static str> {
    crate::trading::PROFESSIONS
        .iter()
        .copied()
        .find(|p| kind.strip_prefix("minecraft:") == Some(p))
}

/// Which block states are points: a type per state (an index into
/// [`KINDS`] plus one; 0 for none).
#[derive(Debug)]
struct Table(Vec<u8>);

impl Table {
    fn build(ctx: &ServerContext) -> Self {
        let blocks = &ctx.data.blocks;
        let kinds: Vec<&str> = KINDS.iter().map(|&(k, _)| k).collect();
        Self(
            (0..blocks.state_count())
                .map(|s| {
                    let Some(kind) = blocks.block_name(s).and_then(kind_of_name) else {
                        return 0;
                    };
                    if kind == HOME && blocks.property(s, "part") != Some("head") {
                        return 0;
                    }
                    kinds
                        .iter()
                        .position(|k| *k == kind)
                        .map_or(0, |i| i as u8 + 1)
                })
                .collect(),
        )
    }

    fn get(&self, state: u32) -> Option<&'static str> {
        match self.0.get(state as usize) {
            Some(&i) if i > 0 => Some(KINDS[usize::from(i) - 1].0),
            _ => None,
        }
    }
}

/// One point of interest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: &'static str,
    pub free: i32,
}

type Pos = [i32; 3];

/// The points of interest of one dimension.
#[derive(Debug, Default)]
pub struct Poi {
    /// By x, z, y: a stretch in x is a range.
    records: BTreeMap<(i32, i32, i32), Record>,
    /// Chunks whose points are in memory, and those that need saving.
    loaded: HashSet<(i32, i32)>,
    dirty: HashSet<(i32, i32)>,
    files: Option<RegionStorage>,
    /// What chunks that left memory had, when there is no folder to keep it.
    kept: HashMap<(i32, i32), Vec<(Pos, Record)>>,
    table: OnceLock<Table>,
}

fn key(p: Pos) -> (i32, i32, i32) {
    (p[0], p[2], p[1])
}

fn pos_of(k: (i32, i32, i32)) -> Pos {
    [k.0, k.2, k.1]
}

fn chunk_of(p: Pos) -> (i32, i32) {
    (p[0] >> 4, p[2] >> 4)
}

impl Poi {
    pub fn new(dir: Option<std::path::PathBuf>) -> Self {
        Self {
            files: dir.map(RegionStorage::new),
            ..Self::default()
        }
    }

    pub fn get(&self, p: Pos) -> Option<Record> {
        self.records.get(&key(p)).copied()
    }

    /// Whether the points of the chunk holding `p` are in memory (a point
    /// not there is gone for sure, rather than not read yet).
    pub fn knows(&self, p: Pos) -> bool {
        self.loaded.contains(&chunk_of(p))
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn put(&mut self, p: Pos, r: Record) {
        self.records.insert(key(p), r);
        self.dirty.insert(chunk_of(p));
    }

    fn remove(&mut self, p: Pos) -> Option<Record> {
        let r = self.records.remove(&key(p));
        if r.is_some() {
            self.dirty.insert(chunk_of(p));
        }
        r
    }

    /// Takes a ticket of the point; whether there was one.
    pub fn claim(&mut self, p: Pos) -> bool {
        match self.records.get_mut(&key(p)) {
            Some(r) if r.free > 0 => {
                r.free -= 1;
                self.dirty.insert(chunk_of(p));
                true
            }
            _ => false,
        }
    }

    /// Gives a ticket back (a point that is gone has none to get back).
    pub fn release(&mut self, p: Pos) {
        let max = self.get(p).map_or(0, |r| tickets_of(r.kind));
        if let Some(r) = self.records.get_mut(&key(p))
            && r.free < max
        {
            r.free += 1;
            self.dirty.insert(chunk_of(p));
        }
    }

    /// Points of a type satisfying `want` within `horizontal` blocks (a
    /// square) and `vertical` of `origin`, nearest first.
    pub fn near(
        &self,
        origin: Pos,
        horizontal: i32,
        vertical: i32,
        want: impl Fn(&Record) -> bool,
    ) -> Vec<(Pos, Record)> {
        let from = (origin[0] - horizontal, i32::MIN, i32::MIN);
        let to = (origin[0] + horizontal, i32::MAX, i32::MAX);
        let mut found: Vec<(i64, Pos, Record)> = self
            .records
            .range(from..=to)
            .filter(|(k, r)| {
                (k.1 - origin[2]).abs() <= horizontal
                    && (k.2 - origin[1]).abs() <= vertical
                    && want(r)
            })
            .map(|(k, r)| {
                let p = pos_of(*k);
                let d = (0..3).map(|i| i64::from(p[i] - origin[i]).pow(2)).sum();
                (d, p, *r)
            })
            .collect();
        found.sort_by_key(|f| f.0);
        found.into_iter().map(|(_, p, r)| (p, r)).collect()
    }

    /// The nearest point of the type with a ticket to spare, and takes it.
    pub fn take_nearest(
        &mut self,
        origin: Pos,
        horizontal: i32,
        vertical: i32,
        want: impl Fn(&str) -> bool,
    ) -> Option<(Pos, &'static str)> {
        let (p, r) = self
            .near(origin, horizontal, vertical, |r| r.free > 0 && want(r.kind))
            .into_iter()
            .next()?;
        self.claim(p).then_some((p, r.kind))
    }

    /// The points of the type anywhere in memory, nearest to `origin` first.
    pub fn of_kind(&self, origin: Pos, kind: &str) -> Vec<Pos> {
        self.near(origin, i32::MAX / 2, i32::MAX / 2, |r| r.kind == kind)
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    fn table(&self, ctx: &ServerContext) -> &Table {
        self.table.get_or_init(|| Table::build(ctx))
    }

    // ---- files ----

    /// What is stored for a chunk.
    fn stored(&mut self, c: (i32, i32)) -> Vec<(Pos, Record)> {
        let Some(files) = &mut self.files else {
            return self.kept.get(&c).cloned().unwrap_or_default();
        };
        match files.read_chunk(c.0, c.1) {
            Ok(Some(Tag::Compound(root))) => read_records(&root),
            Ok(_) => Vec::new(),
            Err(err) => {
                warn!("points of interest of chunk {},{}: {err:#}", c.0, c.1);
                Vec::new()
            }
        }
    }

    fn records_of(&self, c: (i32, i32)) -> Vec<(Pos, Record)> {
        self.records
            .range((c.0 * 16, c.1 * 16, i32::MIN)..=(c.0 * 16 + 15, c.1 * 16 + 15, i32::MAX))
            .filter(|(k, _)| (k.1 >> 4) == c.1)
            .map(|(k, r)| (pos_of(*k), *r))
            .collect()
    }

    fn write(&mut self, c: (i32, i32), data_version: i32) {
        let records = self.records_of(c);
        let Some(files) = &mut self.files else {
            self.kept.insert(c, records);
            return;
        };
        let Some(bytes) = write_records(&records, data_version) else {
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);
        if let Err(err) = std::fs::create_dir_all(files.dir()) {
            warn!("creating {}: {err}", files.dir().display());
        }
        if let Err(err) = files.write_chunk(c.0, c.1, &bytes, now) {
            warn!(
                "saving points of interest of chunk {},{}: {err:#}",
                c.0, c.1
            );
        }
    }
}

fn read_records(root: &Compound) -> Vec<(Pos, Record)> {
    let Some(Tag::Compound(sections)) = root.get("Sections") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (_, section) in sections.iter() {
        let Tag::Compound(section) = section else {
            continue;
        };
        let Some(Tag::List(list)) = section.get("Records") else {
            continue;
        };
        for r in list {
            let Tag::Compound(r) = r else { continue };
            let (Some(Tag::String(kind)), Some(Tag::IntArray(p))) = (r.get("type"), r.get("pos"))
            else {
                continue;
            };
            let Some(&(kind, max)) = KINDS.iter().find(|(k, _)| k == kind) else {
                continue;
            };
            if p.len() != 3 {
                continue;
            }
            let free = match r.get("free_tickets") {
                Some(Tag::Int(f)) => (*f).clamp(0, max),
                _ => max,
            };
            out.push(([p[0], p[1], p[2]], Record { kind, free }));
        }
    }
    out
}

fn write_records(records: &[(Pos, Record)], data_version: i32) -> Option<Vec<u8>> {
    let mut by_section: BTreeMap<i32, Vec<Tag>> = BTreeMap::new();
    for (p, r) in records {
        let mut c = Compound::new();
        c.insert("type", Tag::String(r.kind.to_owned()));
        c.insert("pos", Tag::IntArray(p.to_vec()));
        c.insert("free_tickets", Tag::Int(r.free));
        by_section
            .entry(p[1] >> 4)
            .or_default()
            .push(Tag::Compound(c));
    }
    let mut sections = Compound::new();
    for (y, list) in by_section {
        let mut s = Compound::new();
        s.insert("Valid", Tag::Byte(1));
        s.insert("Records", Tag::List(list));
        sections.insert(y.to_string(), Tag::Compound(s));
    }
    let mut root = Compound::new();
    root.insert("DataVersion", Tag::Int(data_version));
    root.insert("Sections", Tag::Compound(sections));
    let mut bytes = Vec::new();
    mc_nbt::write_named("", &Tag::Compound(root), &mut bytes).ok()?;
    Some(bytes)
}

/// The points in a chunk's blocks.
fn scan(chunk: &Chunk, table: &Table) -> Vec<(Pos, &'static str)> {
    let mut out = Vec::new();
    for (i, section) in chunk.sections.iter().enumerate() {
        if !section.blocks.may_contain(|s| table.get(s).is_some()) {
            continue;
        }
        let base = chunk.min_y + 16 * i as i32;
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    if let Some(kind) = table.get(section.block(x, y, z)) {
                        out.push((
                            [
                                chunk.x * 16 + x as i32,
                                base + y as i32,
                                chunk.z * 16 + z as i32,
                            ],
                            kind,
                        ));
                    }
                }
            }
        }
    }
    out
}

/// Catches the points up with the world: chunks that came into memory are
/// read (and their points checked against the blocks), blocks that changed
/// since make or lose their point.
pub fn sync(ctx: &ServerContext) {
    let log = ctx.world.take_poi_log();
    if log.loaded.is_empty() && log.changed.is_empty() {
        return;
    }
    let mut poi = lock(&ctx.poi);
    for (cx, cz) in log.loaded {
        if poi.loaded.contains(&(cx, cz)) {
            continue;
        }
        let found = {
            let table = poi.table(ctx);
            ctx.world.peek_chunk(cx, cz, |chunk| scan(chunk, table))
        };
        let Some(found) = found else {
            continue;
        };
        let stored = poi.stored((cx, cz));
        let mut changed = false;
        let mut kept = Vec::new();
        for (p, kind) in &found {
            let max = tickets_of(kind);
            let free = stored
                .iter()
                .find(|(q, r)| q == p && r.kind == *kind)
                .map_or_else(
                    || {
                        changed = true;
                        max
                    },
                    |(_, r)| r.free.clamp(0, max),
                );
            kept.push((*p, Record { kind, free }));
        }
        // A point whose block is gone is dropped.
        changed |= stored
            .iter()
            .any(|(q, _)| !found.iter().any(|(p, _)| p == q));
        for (p, r) in kept {
            poi.records.insert(key(p), r);
        }
        poi.loaded.insert((cx, cz));
        if changed {
            poi.dirty.insert((cx, cz));
        }
    }
    for p in log.changed {
        let p = [p.x, p.y, p.z];
        let Some(state) = ctx.world.block_if_loaded(Position::new(p[0], p[1], p[2])) else {
            continue;
        };
        let kind = poi.table(ctx).get(state);
        match (kind, poi.get(p)) {
            (Some(k), Some(r)) if r.kind == k => {}
            (Some(k), _) => poi.put(
                p,
                Record {
                    kind: k,
                    free: tickets_of(k),
                },
            ),
            (None, Some(_)) => {
                poi.remove(p);
            }
            (None, None) => {}
        }
    }
}

/// Writes the points of the chunks that changed.
pub fn save(ctx: &ServerContext) {
    sync(ctx);
    let mut poi = lock(&ctx.poi);
    let dirty: Vec<(i32, i32)> = poi.dirty.drain().collect();
    for c in dirty {
        poi.write(c, ctx.world.data_version());
    }
}

/// Chunks left memory: their points are written and forgotten (read again
/// when they come back).
pub fn unloaded(ctx: &ServerContext, chunks: &[(i32, i32)]) {
    sync(ctx);
    let mut poi = lock(&ctx.poi);
    for &c in chunks {
        if poi.dirty.remove(&c) || poi.files.is_none() {
            poi.write(c, ctx.world.data_version());
        }
        if poi.loaded.remove(&c) {
            let gone: Vec<_> = poi.records_of(c);
            for (p, _) in gone {
                poi.records.remove(&key(p));
            }
        }
    }
}

/// Takes a ticket at `p`, and whether it was free.
#[cfg(test)]
pub fn claim(ctx: &ServerContext, p: Pos) -> bool {
    lock(&ctx.poi).claim(p)
}

/// Gives the ticket at `p` back.
pub fn release(ctx: &ServerContext, p: Pos) {
    lock(&ctx.poi).release(p);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32, z: i32) -> Position {
        Position::new(x, y, z)
    }

    fn set(ctx: &ServerContext, p: [i32; 3], name: &str) {
        crate::testkit::set(ctx, p, name);
    }

    fn bed(ctx: &ServerContext, foot: [i32; 3], head: [i32; 3]) {
        let b = &ctx.data.blocks;
        let s = b.default_state("minecraft:red_bed").unwrap();
        let s = b.with_property(s, "facing", "east").unwrap();
        let (f, h) = (
            b.with_property(s, "part", "foot").unwrap(),
            b.with_property(s, "part", "head").unwrap(),
        );
        ctx.world
            .set_block(at(foot[0], foot[1], foot[2]), f, b, &ctx.rules)
            .unwrap();
        ctx.world
            .set_block(at(head[0], head[1], head[2]), h, b, &ctx.rules)
            .unwrap();
    }

    #[test]
    fn blocks_make_points_with_the_tickets_of_their_type() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        bed(&ctx, [6, -60, 3], [7, -60, 3]);
        set(&ctx, [9, -60, 3], "minecraft:bell");
        set(&ctx, [11, -60, 3], "minecraft:composter");
        set(&ctx, [13, -60, 3], "minecraft:stone");
        sync(&ctx);
        let poi = lock(&ctx.poi);
        // Only the bed's head is a home.
        assert_eq!(poi.get([6, -60, 3]), None);
        assert_eq!(
            poi.get([7, -60, 3]),
            Some(Record {
                kind: HOME,
                free: 1
            })
        );
        assert_eq!(
            poi.get([9, -60, 3]),
            Some(Record {
                kind: MEETING,
                free: 32
            })
        );
        assert_eq!(
            poi.get([11, -60, 3]),
            Some(Record {
                kind: "minecraft:farmer",
                free: 1
            })
        );
        assert_eq!(poi.get([13, -60, 3]), None);
    }

    #[test]
    fn a_ticket_goes_to_one_taker_and_comes_back_when_let_go() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        set(&ctx, [11, -60, 3], "minecraft:barrel");
        sync(&ctx);
        assert!(claim(&ctx, [11, -60, 3]));
        assert!(!claim(&ctx, [11, -60, 3]));
        assert!(
            lock(&ctx.poi)
                .take_nearest([0, -60, 0], 48, 8, |_| true)
                .is_none()
        );
        release(&ctx, [11, -60, 3]);
        // Releasing twice does not make a second ticket.
        release(&ctx, [11, -60, 3]);
        assert_eq!(lock(&ctx.poi).get([11, -60, 3]).unwrap().free, 1);
        let (p, kind) = lock(&ctx.poi)
            .take_nearest([0, -60, 0], 48, 8, |k| profession_of(k).is_some())
            .unwrap();
        assert_eq!((p, kind), ([11, -60, 3], "minecraft:fisherman"));
        // Broken, the point is gone with its tickets.
        set(&ctx, [11, -60, 3], "minecraft:air");
        sync(&ctx);
        assert_eq!(lock(&ctx.poi).get([11, -60, 3]), None);
        assert!(!claim(&ctx, [11, -60, 3]));
    }

    #[test]
    fn the_nearest_free_point_is_taken_first() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        set(&ctx, [20, -60, 0], "minecraft:lectern");
        set(&ctx, [5, -60, 0], "minecraft:lectern");
        set(&ctx, [5, -60, 70], "minecraft:lectern");
        sync(&ctx);
        let mut poi = lock(&ctx.poi);
        let first = poi.take_nearest([0, -60, 0], 48, 8, |_| true).unwrap().0;
        let second = poi.take_nearest([0, -60, 0], 48, 8, |_| true).unwrap().0;
        assert_eq!((first, second), ([5, -60, 0], [20, -60, 0]));
        // The third is too far.
        assert!(poi.take_nearest([0, -60, 0], 48, 8, |_| true).is_none());
    }

    #[test]
    fn points_are_saved_in_vanilla_form_and_rebuilt_from_the_chunk() {
        let dir = std::env::temp_dir().join(format!("mc-poi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let open = || {
            let mut config = crate::Config::new(data.clone());
            config.world = Some(dir.clone());
            config.seed = Some(12345);
            mc_data::McData::load(&data)
                .ok()
                .and_then(|d| ServerContext::new(config, d).ok())
        };
        let Some(ctx) = open() else {
            eprintln!("skipped: no game data");
            return;
        };
        let home = [40, 100, 40];
        let b = &ctx.data.blocks;
        let _ = ctx.world.block(at(40, 100, 40), b);
        bed(&ctx, [39, 100, 40], home);
        set(&ctx, [44, 100, 40], "minecraft:bell");
        sync(&ctx);
        assert!(claim(&ctx, home));
        crate::save(&ctx);
        let poi_dir = dir.join("dimensions/minecraft/overworld/poi");
        let file = poi_dir.join("r.0.0.mca");
        assert!(file.exists());
        drop(ctx);

        // Opened again, the chunk's points come back with the claimed bed
        // still claimed.
        let ctx = open().unwrap();
        let _ = ctx.world.block(at(40, 100, 40), &ctx.data.blocks);
        sync(&ctx);
        {
            let poi = lock(&ctx.poi);
            assert_eq!(
                poi.get(home),
                Some(Record {
                    kind: HOME,
                    free: 0
                })
            );
            assert_eq!(poi.get([44, 100, 40]).map(|r| r.free), Some(32));
        }
        // The file as the format has it.
        let raw = lock(&ctx.poi)
            .files
            .as_mut()
            .unwrap()
            .read_chunk(2, 2)
            .unwrap();
        let Some(Tag::Compound(root)) = raw else {
            panic!()
        };
        let Some(Tag::Compound(sections)) = root.get("Sections") else {
            panic!()
        };
        let Some(Tag::Compound(s)) = sections.get("6") else {
            panic!("{sections:?}")
        };
        assert_eq!(s.get("Valid"), Some(&Tag::Byte(1)));
        let Some(Tag::List(records)) = s.get("Records") else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        drop(ctx);

        // Without the file the points are rebuilt from the blocks, with all
        // their tickets.
        std::fs::remove_dir_all(&poi_dir).unwrap();
        let ctx = open().unwrap();
        let _ = ctx.world.block(at(40, 100, 40), &ctx.data.blocks);
        sync(&ctx);
        assert_eq!(
            lock(&ctx.poi).get(home),
            Some(Record {
                kind: HOME,
                free: 1
            })
        );
        drop(ctx);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
