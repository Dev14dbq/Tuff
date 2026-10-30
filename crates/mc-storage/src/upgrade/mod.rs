//! Bringing data saved by older Minecraft versions up to 26.3, the way
//! vanilla's data fixers do.
//!
//! Every saved document carries a `DataVersion`. [`upgrade`] runs the fixes
//! newer than it, oldest first, then stamps the target version. Supported
//! saves are those from 1.18 on (data version [`OLDEST`]): 1.18 is where
//! chunks lost their `Level` wrapper and got the section and biome layout
//! 26.3 still uses. Older chunks need the 1.13 flattening, the 1.17 split of
//! entities into their own files and the 1.18 height change, which we do not
//! implement; open such a world in a vanilla 1.18 or newer first.
//!
//! [`files`] moves a world folder to the 26.3 layout (and Paper's split
//! dimension folders back into one world); [`world`] is `--forceUpgrade`.

mod block_entity;
mod chunk;
mod components;
mod entity;
pub mod files;
mod fixes;
mod item;
mod level;
mod nbt;
mod text;
mod walk;
pub mod world;

use mc_nbt::{Compound, Tag};

/// 1.18's data version; older saves are not upgraded.
pub const OLDEST: i32 = 2860;

/// 26.3's data version, what [`upgrade`] brings documents to when the caller
/// has no game data at hand.
pub const TARGET: i32 = 5023;

/// What kind of document is being upgraded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A chunk in `region/`.
    Chunk,
    /// A chunk's entities in `entities/`.
    EntityChunk,
    /// A chunk's points of interest in `poi/`.
    Poi,
    /// A player file.
    Player,
    /// level.dat (the root holding `Data`).
    Level,
    /// A saved data file (`{data, DataVersion}`).
    Saved(Saved),
}

/// Saved data files that have fixes of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    /// `chunks.dat`, now `chunk_tickets.dat`.
    Tickets,
    Raids,
    Scoreboard,
    RandomSequences,
    /// `map_<n>.dat`.
    Map,
    BossEvents,
    WorldBorder,
    /// Any other file; only text and items in it are fixed.
    Other,
}

/// What a fix may need to know about where a document lives.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// The dimension a chunk belongs to (`minecraft:overworld`...).
    pub dimension: &'a str,
}

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            dimension: "minecraft:overworld",
        }
    }
}

/// Why a document was left as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Saved before 1.18 (or with no `DataVersion` at all: before 1.9).
    TooOld(Option<i32>),
    /// Saved by a newer game than the target.
    TooNew(i32),
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsupported::TooOld(Some(v)) => write!(
                f,
                "saved with data version {v}, before 1.18 ({OLDEST}); open it in vanilla 1.18 or newer first"
            ),
            Unsupported::TooOld(None) => write!(
                f,
                "saved before 1.9 (no data version); open it in vanilla 1.18 or newer first"
            ),
            Unsupported::TooNew(v) => write!(f, "saved by a newer version (data version {v})"),
        }
    }
}

impl std::error::Error for Unsupported {}

/// The document's data version (level.dat keeps it in `Data`).
pub fn data_version(root: &Compound, kind: Kind) -> Option<i32> {
    let holder = match kind {
        Kind::Level => nbt::compound(root, "Data")?,
        _ => root,
    };
    match holder.get("DataVersion") {
        Some(Tag::Int(v)) => Some(*v),
        Some(other) => nbt::number(other).map(|v| v as i32),
        None => None,
    }
}

fn set_data_version(root: &mut Compound, kind: Kind, version: i32) {
    let holder = match kind {
        Kind::Level => nbt::child(root, "Data"),
        _ => root,
    };
    holder.insert("DataVersion", Tag::Int(version));
}

/// Brings `root` from its data version up to `target`. `Ok(false)` when it is
/// already there.
pub fn upgrade(
    root: &mut Compound,
    kind: Kind,
    ctx: &Context,
    target: i32,
) -> Result<bool, Unsupported> {
    let from = data_version(root, kind);
    match from {
        Some(v) if v > target => return Err(Unsupported::TooNew(v)),
        Some(v) if v == target => return Ok(false),
        Some(v) if v >= OLDEST => {}
        other => return Err(Unsupported::TooOld(other)),
    }
    let from = from.unwrap_or(OLDEST);
    run_between(root, kind, ctx, from, target);
    set_data_version(root, kind, target);
    Ok(true)
}

/// Runs the fixes newer than `from` up to and including `to`.
pub(crate) fn run_between(root: &mut Compound, kind: Kind, ctx: &Context, from: i32, to: i32) {
    for (version, apply) in fixes::FIXES {
        if *version > from && *version <= to {
            walk::run(root, kind, *apply, ctx);
        }
    }
}

/// Upgrades a chunk's NBT as read from a region file, in place. A tag that is
/// not a compound is left alone.
pub fn upgrade_tag(
    tag: &mut Tag,
    kind: Kind,
    ctx: &Context,
    target: i32,
) -> Result<bool, Unsupported> {
    match tag {
        Tag::Compound(root) => upgrade(root, kind, ctx, target),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests;
