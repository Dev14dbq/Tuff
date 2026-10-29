//! A world folder's files: the 26.x layout (25w37a, 4772) and level.dat.
//!
//! Vanilla runs this on every start, before anything else reads the world:
//! level.dat is brought to the current version, and a world from before
//! 4772 has its files moved:
//! - `region`, `entities`, `poi` of the Overworld, `DIM-1` and `DIM1` go to
//!   `dimensions/minecraft/{overworld,the_nether,the_end}/`;
//! - per-dimension saved data (`chunks.dat`, `raids.dat`, `world_border.dat`)
//!   goes to that dimension's `data/minecraft/`, world-wide data (scoreboard,
//!   maps, command storage, random sequences) to `data/minecraft/`;
//! - `playerdata`, `advancements`, `stats` go under `players/`;
//! - what level.dat kept of game rules, weather, the border, the dragon
//!   fight, the wandering trader, boss bars, scheduled events, world
//!   generation settings and clocks becomes saved data files of its own.
//!
//! Vanilla builds the new layout next to the old one and swaps it in; we move
//! in place, never over a file that is already there, and write level.dat
//! last, so a run that stops halfway is finished by the next one. Obsolete
//! files vanilla deletes (old village and structure data) are left.
//!
//! Paper keeps the Nether and the End in sibling worlds (`world_nether/DIM-1`,
//! `world_the_end/DIM1`, each with its own level.dat and Bukkit `uid.dat`).
//! [`fix_world`] moves those dimension folders into the main world first,
//! the way they sit in a vanilla world, and leaves the rest of the sibling
//! (its level.dat, `uid.dat`, `paper-world.yml`) where it is: vanilla reads
//! none of it. Paper's per-world game rules are lost with it; vanilla has
//! one set, the main world's.

use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};

use super::{Context, Kind, OLDEST, Saved, Unsupported, data_version, nbt, run_between, upgrade};

/// The data version of the 26.x layout.
pub const LAYOUT: i32 = 4772;

/// What [`fix_world`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// level.dat's data version before, if the world has a level.dat.
    pub from: Option<i32>,
    /// Files and folders moved, from → to.
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// Moves not made because the target was already there.
    pub kept: Vec<PathBuf>,
    /// Saved data files written from level.dat.
    pub extracted: Vec<PathBuf>,
    /// Paper dimension folders brought into the world.
    pub paper: Vec<PathBuf>,
}

/// Why [`fix_world`] stopped.
#[derive(Debug)]
pub enum FileError {
    Io(io::Error),
    Unsupported(Unsupported),
    Nbt(String),
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Io(e) => write!(f, "{e}"),
            FileError::Unsupported(u) => write!(f, "this world was {u}"),
            FileError::Nbt(e) => write!(f, "level.dat: {e}"),
        }
    }
}

impl std::error::Error for FileError {}

impl From<io::Error> for FileError {
    fn from(e: io::Error) -> Self {
        FileError::Io(e)
    }
}

pub fn read_gz(path: &Path) -> io::Result<Compound> {
    let raw = std::fs::read(path)?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(&raw[..]).read_to_end(&mut bytes)?;
    match mc_nbt::read_named(&bytes) {
        Ok((_, Tag::Compound(root), _)) => Ok(root),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "root is not a compound",
        )),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
    }
}

/// Writes next to `path` and renames over it.
pub fn write_gz(path: &Path, root: &Compound) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = Vec::new();
    mc_nbt::write_named("", &Tag::Compound(root.clone()), &mut bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let tmp = path.with_extension("dat_tmp");
    {
        let file = std::fs::File::create(&tmp)?;
        let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        gz.write_all(&bytes)?;
        gz.finish()?.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// The dimension folders Paper keeps in sibling worlds, and where they
/// go in the main world: (sibling suffix, old folder, 26.x folder).
const PAPER: [(&str, &str, &str); 2] = [
    ("_nether", "DIM-1", "dimensions/minecraft/the_nether"),
    ("_the_end", "DIM1", "dimensions/minecraft/the_end"),
];

fn paper_dimensions(dir: &Path, report: &mut FileReport) -> io::Result<()> {
    let (Some(name), Some(parent)) = (dir.file_name().and_then(|n| n.to_str()), dir.parent())
    else {
        return Ok(());
    };
    for (suffix, old, modern) in PAPER {
        let sibling = parent.join(format!("{name}{suffix}"));
        if !sibling.is_dir() || dir.join(old).exists() || dir.join(modern).exists() {
            continue;
        }
        for folder in [old, modern] {
            let from = sibling.join(folder);
            if from.is_dir() {
                let to = dir.join(folder);
                if let Some(p) = to.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::rename(&from, &to)?;
                report.paper.push(from.clone());
                report.moved.push((from, to));
                break;
            }
        }
    }
    Ok(())
}

/// Moves `from` to `to` unless `to` is there already.
fn mv(from: &Path, to: &Path, report: &mut FileReport) -> io::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        report.kept.push(from.to_owned());
        return Ok(());
    }
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::rename(from, to)?;
    report.moved.push((from.to_owned(), to.to_owned()));
    Ok(())
}

/// Removes a folder left empty by the moves.
fn remove_if_empty(dir: &Path) {
    if std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none()) {
        let _ = std::fs::remove_dir(dir);
    }
}

/// Brings a saved data file to `target`, if it needs it.
fn upgrade_saved(path: &Path, saved: Saved, target: i32) {
    let Ok(mut root) = read_gz(path) else {
        return;
    };
    if matches!(
        upgrade(&mut root, Kind::Saved(saved), &Context::default(), target),
        Ok(true)
    ) {
        let _ = write_gz(path, &root);
    }
}

fn saved_kind(file: &str) -> Saved {
    match file {
        "chunks.dat" | "chunk_tickets.dat" => Saved::Tickets,
        "raids.dat" | "raids_end.dat" => Saved::Raids,
        "scoreboard.dat" => Saved::Scoreboard,
        "random_sequences.dat" => Saved::RandomSequences,
        "world_border.dat" => Saved::WorldBorder,
        "custom_boss_events.dat" => Saved::BossEvents,
        f if f.starts_with("map_") => Saved::Map,
        _ => Saved::Other,
    }
}

fn move_saved(from: &Path, to: &Path, target: i32, report: &mut FileReport) -> io::Result<()> {
    if from.is_file() {
        let name = from.file_name().and_then(|n| n.to_str()).unwrap_or("");
        upgrade_saved(from, saved_kind(name), target);
    }
    mv(from, to, report)
}

/// The moves of 4772.
fn layout(dir: &Path, target: i32, report: &mut FileReport) -> io::Result<()> {
    mv(
        &dir.join("resources.zip"),
        &dir.join("resourcepacks/resources.zip"),
        report,
    )?;
    // Custom dimensions were already under `dimensions/`.
    if let Ok(namespaces) = std::fs::read_dir(dir.join("dimensions")) {
        for ns in namespaces.flatten() {
            let Ok(dims) = std::fs::read_dir(ns.path()) else {
                continue;
            };
            for dim in dims.flatten() {
                let data = dim.path().join("data");
                for (from, to) in [
                    ("chunks.dat", "minecraft/chunk_tickets.dat"),
                    ("raids.dat", "minecraft/raids.dat"),
                    ("world_border.dat", "minecraft/world_border.dat"),
                ] {
                    move_saved(&data.join(from), &data.join(to), target, report)?;
                }
            }
        }
    }
    for (old, name) in [
        ("", "overworld"),
        ("DIM-1", "the_nether"),
        ("DIM1", "the_end"),
    ] {
        let old_dir = if old.is_empty() {
            dir.to_owned()
        } else {
            dir.join(old)
        };
        let new_dir = dir.join("dimensions/minecraft").join(name);
        let data = old_dir.join("data");
        let new_data = new_dir.join("data/minecraft");
        for (from, to) in [
            ("chunks.dat", "chunk_tickets.dat"),
            ("raids.dat", "raids.dat"),
            ("raids_end.dat", "raids.dat"),
            ("world_border.dat", "world_border.dat"),
        ] {
            move_saved(&data.join(from), &new_data.join(to), target, report)?;
        }
        for folder in ["region", "entities", "poi"] {
            mv(&old_dir.join(folder), &new_dir.join(folder), report)?;
        }
    }
    let data = dir.join("data");
    let mut world_files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&data) {
        for e in entries.flatten() {
            if let Some(name) = e.file_name().to_str() {
                world_files.push(name.to_owned());
            }
        }
    }
    for name in world_files {
        let to = match name.as_str() {
            "scoreboard.dat" | "stopwatches.dat" | "random_sequences.dat" => {
                Some(format!("minecraft/{name}"))
            }
            "idcounts.dat" => Some("minecraft/maps/last_id.dat".to_owned()),
            n => n
                .strip_prefix("map_")
                .and_then(|r| r.strip_suffix(".dat"))
                .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                .map(|id| format!("minecraft/maps/{id}.dat"))
                .or_else(|| {
                    n.strip_prefix("command_storage_")
                        .and_then(|r| r.strip_suffix(".dat"))
                        .filter(|ns| !ns.is_empty())
                        .map(|ns| format!("{ns}/command_storage.dat"))
                }),
        };
        if let Some(to) = to {
            move_saved(&data.join(&name), &data.join(to), target, report)?;
        }
    }
    for (from, to) in [
        ("advancements", "players/advancements"),
        ("playerdata", "players/data"),
        ("stats", "players/stats"),
    ] {
        mv(&dir.join(from), &dir.join(to), report)?;
    }
    for old in ["DIM-1", "DIM1"] {
        remove_if_empty(&dir.join(old).join("data"));
        remove_if_empty(&dir.join(old));
    }
    Ok(())
}

/// Saved data file at `path` holding `data`.
fn write_saved(path: &Path, data: Tag, version: i32, report: &mut FileReport) -> io::Result<()> {
    let mut root = Compound::new();
    root.insert("data", data);
    root.insert("DataVersion", Tag::Int(version));
    write_gz(path, &root)?;
    report.extracted.push(path.to_owned());
    Ok(())
}

/// 4772: what level.dat kept that now has a file of its own.
fn split_level(
    dir: &Path,
    data: &mut Compound,
    target: i32,
    report: &mut FileReport,
) -> io::Result<()> {
    let dims = dir.join("dimensions/minecraft");
    let world = dir.join("data/minecraft");
    if let Some(fight) = data.remove("dragon_fight") {
        let path = dims.join("the_end/data/minecraft/ender_dragon_fight.dat");
        write_saved(&path, fight, LAYOUT, report)?;
    }
    if let Some(Tag::Compound(mut player)) = data.remove("Player") {
        let from = data_version(&player, Kind::Player).unwrap_or(OLDEST);
        run_between(&mut player, Kind::Player, &Context::default(), from, target);
        player.insert("DataVersion", Tag::Int(target));
        let uuid = match player.get("UUID") {
            Some(uuid @ Tag::IntArray(_)) => uuid.clone(),
            _ => {
                let path = dir.join("players/data/00000000-0000-0000-0000-000000000000.dat");
                write_gz(&path, &player)?;
                report.extracted.push(path);
                Tag::IntArray(vec![0; 4])
            }
        };
        data.insert("singleplayer_uuid", uuid);
    }
    for (key, file) in [
        ("wandering_trader_migration_data", "wandering_trader.dat"),
        ("CustomBossEvents", "custom_boss_events.dat"),
        ("weather_data", "weather.dat"),
        ("scheduled_events", "scheduled_events.dat"),
    ] {
        if let Some(value) = data.remove(key) {
            write_saved(&world.join(file), value, LAYOUT, report)?;
        }
    }
    if let Some(Tag::Compound(border)) = data.remove("world_border") {
        for (dim, scale) in [("overworld", 1.0), ("the_nether", 8.0), ("the_end", 1.0)] {
            let mut b = border.clone();
            for k in ["center_x", "center_z"] {
                b.insert(k, Tag::Double(nbt::num(&border, k).unwrap_or(0.0) / scale));
            }
            let path = dims.join(dim).join("data/minecraft/world_border.dat");
            write_saved(&path, Tag::Compound(b), LAYOUT, report)?;
        }
    }
    if let Some(rules) = data.remove("game_rules") {
        write_saved(&world.join("game_rules.dat"), rules, LAYOUT, report)?;
    }
    if let Some(settings) = data.remove("world_gen_settings") {
        write_saved(
            &world.join("world_gen_settings.dat"),
            settings,
            LAYOUT,
            report,
        )?;
    }
    if let Some(clocks) = data.remove("world_clocks") {
        write_saved(&world.join("world_clocks.dat"), clocks, LAYOUT, report)?;
    }
    Ok(())
}

/// 4773: generated structures of a namespace sit in `structure/`.
fn generated_structures(dir: &Path, report: &mut FileReport) -> io::Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir.join("generated")) {
        for e in entries.flatten() {
            mv(
                &e.path().join("structures"),
                &e.path().join("structure"),
                report,
            )?;
        }
    }
    Ok(())
}

/// 4899: hardcore worlds once played in single player let spectators
/// generate chunks again.
fn hardcore_spectators(dir: &Path, data: &Compound) {
    let hardcore = nbt::compound(data, "difficulty_settings")
        .and_then(|d| nbt::boolean(d, "hardcore"))
        .unwrap_or(false);
    if data.get("singleplayer_uuid").is_none() || !hardcore {
        return;
    }
    let path = dir.join("data/minecraft/game_rules.dat");
    if let Ok(mut root) = read_gz(&path) {
        nbt::child(&mut root, "data")
            .insert("minecraft:spectators_generate_chunks", nbt::flag(true));
        let _ = write_gz(&path, &root);
    }
}

/// Brings `dir`'s level.dat and file layout to `target` (named
/// `version_name`), as vanilla does at startup. A folder without level.dat
/// is a new world and is left alone, Paper folders aside.
pub fn fix_world(dir: &Path, target: i32, version_name: &str) -> Result<FileReport, FileError> {
    let mut report = FileReport::default();
    paper_dimensions(dir, &mut report)?;
    let level_path = dir.join("level.dat");
    if !level_path.is_file() {
        return Ok(report);
    }
    let mut root = read_gz(&level_path)?;
    let from = data_version(&root, Kind::Level);
    report.from = from;
    let from = match from {
        Some(v) if v > target => return Err(FileError::Unsupported(Unsupported::TooNew(v))),
        Some(v) if v == target => return Ok(report),
        Some(v) if v >= OLDEST => v,
        other => return Err(FileError::Unsupported(Unsupported::TooOld(other))),
    };
    let ctx = Context::default();
    if from < LAYOUT {
        run_between(&mut root, Kind::Level, &ctx, from, LAYOUT - 1);
        layout(dir, target, &mut report)?;
        let data = nbt::child(&mut root, "Data");
        split_level(dir, data, target, &mut report)?;
        for path in report.extracted.clone() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".dat") && !path.starts_with(dir.join("players")) {
                upgrade_saved(&path, saved_kind(name), target);
            }
        }
    }
    if from < 4773 {
        generated_structures(dir, &mut report)?;
    }
    run_between(&mut root, Kind::Level, &ctx, from.max(LAYOUT - 1), target);
    let data = nbt::child(&mut root, "Data");
    if from < 4899 {
        hardcore_spectators(dir, data);
    }
    data.insert("DataVersion", Tag::Int(target));
    // What vanilla appends when it next saves level.dat.
    let history = nbt::list(data, "version_history").to_vec();
    if history.last() != Some(&Tag::Int(target)) {
        let mut history = history;
        history.push(Tag::Int(target));
        data.insert("version_history", Tag::List(history));
    }
    let version = nbt::child(data, "Version");
    version.insert("Id", Tag::Int(target));
    version.insert("Name", Tag::String(version_name.to_owned()));
    std::fs::copy(&level_path, dir.join("level.dat_old"))?;
    write_gz(&level_path, &root)?;
    Ok(report)
}

/// Brings a player file to `target`; `Ok(false)` when it was current.
pub fn upgrade_player(root: &mut Compound, target: i32) -> Result<bool, Unsupported> {
    upgrade(root, Kind::Player, &Context::default(), target)
}
