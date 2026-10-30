//! Jukeboxes (minecraft.wiki, "Jukebox"): a disc plays once, for the length
//! its song has in the `jukebox_song` registry, and the jukebox gives a
//! redstone signal of 15 while it does. The disc stays in until it is
//! taken out; a comparator reads the song's `comparator_output` all the
//! while. Hoppers and droppers put discs in; a hopper below takes the disc
//! out once the signal is off. The block entity keeps the disc as
//! `RecordItem` and `ticks_since_song_started` while a song plays.

use mc_data::Blocks as Table;
use mc_nbt::{Compound, Tag};

use super::{Blocks, Effects, ItemStack, Items};
use crate::dir::{BlockPos, Dir};

pub const ID: &str = "minecraft:jukebox";
const TICKS_KEY: &str = "ticks_since_song_started";

/// What the `jukebox_song` registry says about a song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Song {
    /// Its place in the registry, as clients were told.
    pub index: i32,
    /// How long it plays, in ticks.
    pub length: i64,
    /// What a comparator reads.
    pub comparator: u8,
}

/// Songs and the jukeboxes playing them (vanilla `JukeboxSongPlayer`: the
/// song, its length and the ticks since it started).
pub trait Jukeboxes {
    /// The song of a music disc item (`minecraft:music_disc_13`).
    fn song(&self, disc: &str) -> Option<Song>;
    /// The song playing at `p`: its length and the ticks it has played.
    fn playing(&self, p: BlockPos) -> Option<(i64, i64)>;
    /// The song at `p` plays on from `ticks` of `length`, or (`None`) stops.
    fn set_playing(&mut self, p: BlockPos, song: Option<(i64, i64)>);
}

/// The disc in the jukebox at `p`: its item name.
pub fn record(host: &impl Blocks, p: BlockPos) -> Option<String> {
    let data = host.block_entity(p)?;
    match data.get("RecordItem") {
        Some(Tag::Compound(c)) => match c.get("id") {
            Some(Tag::String(id)) => Some(id.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `Some(has_record)` when the block at `p` is a jukebox.
fn is_jukebox(host: &impl Blocks, blocks: &Table, p: BlockPos) -> Option<bool> {
    let state = host.block(p)?;
    (blocks.block_name(state)? == ID).then(|| blocks.property(state, "has_record") == Some("true"))
}

fn centre_top(p: BlockPos) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 1.2,
        f64::from(p.z) + 0.5,
    ]
}

fn write_ticks(host: &mut impl Blocks, p: BlockPos, ticks: Option<i64>) {
    host.edit_block_entity(p, None, &mut |data| match ticks {
        Some(t) => {
            data.insert(TICKS_KEY, Tag::Long(t));
        }
        None => {
            data.remove(TICKS_KEY);
        }
    });
}

/// The disc in the jukebox at `p` starts to play: clients are told, and the
/// jukebox gives its signal (the caller sets `has_record`, which lets the
/// neighbours know).
pub fn start<H: Blocks + Effects + Jukeboxes>(host: &mut H, p: BlockPos) {
    let Some(song) = record(host, p).and_then(|d| host.song(&d)) else {
        return;
    };
    host.set_playing(p, Some((song.length, 0)));
    write_ticks(host, p, Some(0));
    host.level_event(p, 1010, song.index);
}

/// The music at `p` stops, as it does when the disc comes out or the
/// jukebox breaks. Clients are told either way.
pub fn stop<H: Blocks + Effects + Jukeboxes>(host: &mut H, p: BlockPos) {
    host.set_playing(p, None);
    write_ticks(host, p, None);
    host.level_event(p, 1011, 0);
}

/// A jukebox block entity was loaded: if its song was playing, it goes on.
pub fn wake<H: Blocks + Jukeboxes>(host: &mut H, p: BlockPos) {
    let Some(data) = host.block_entity(p) else {
        return;
    };
    let Some(Tag::Long(ticks)) = data.get(TICKS_KEY) else {
        return;
    };
    if let Some(song) = record(host, p).and_then(|d| host.song(&d)) {
        host.set_playing(p, Some((song.length, *ticks)));
    }
}

/// One tick of the jukebox at `p`, whose song of `length` had played
/// `ticks`.
pub fn tick<H>(host: &mut H, blocks: &Table, p: BlockPos, length: i64, ticks: i64)
where
    H: Blocks + Effects + Jukeboxes,
{
    // Gone, or emptied without the music being stopped.
    if is_jukebox(host, blocks, p) != Some(true) {
        host.set_playing(p, None);
        return;
    }
    let ticks = ticks + 1;
    if ticks >= length {
        // The song is over; the disc stays.
        host.set_playing(p, None);
        write_ticks(host, p, None);
        host.level_event(p, 1011, 0);
        // The neighbours of the jukebox (and theirs) look again at the signal.
        let mut around = [p; 7];
        for (a, d) in around[1..].iter_mut().zip(Dir::NC_ORDER) {
            *a = p.offset(d);
        }
        host.update_neighbors(&around);
        return;
    }
    if let Some((length, _)) = host.playing(p) {
        host.set_playing(p, Some((length, ticks)));
    }
    if ticks % 20 == 0 {
        write_ticks(host, p, Some(ticks));
        // A note of a random colour.
        let colour = (host.random() >> 11) as f64 / (1u64 << 53) as f64;
        host.particles(
            "minecraft:note",
            centre_top(p),
            0.0,
            colour as f32 / 24.0,
            0,
        );
    }
}

/// A disc put in by a hopper or a dropper: `false` if the jukebox is full
/// or the item is not a disc.
pub fn insert<H: Blocks + Items + Effects + Jukeboxes>(
    host: &mut H,
    blocks: &Table,
    p: BlockPos,
    stack: &ItemStack,
) -> bool {
    if is_jukebox(host, blocks, p) != Some(false) {
        return false;
    }
    let Some(name) = host.name(stack.item) else {
        return false;
    };
    if host.song(name).is_none() {
        return false;
    }
    let mut item = Compound::new();
    item.insert("id", Tag::String(name.to_owned()));
    item.insert("count", Tag::Int(1));
    let mut fresh = Compound::new();
    fresh.insert("RecordItem", Tag::Compound(item));
    host.edit_block_entity(p, Some(ID), &mut |data| *data = fresh.clone());
    start(host, p);
    set_has_record(host, blocks, p, true);
    true
}

/// The disc a hopper took out of the jukebox at `p`.
pub fn take<H: Blocks + Items + Effects + Jukeboxes>(
    host: &mut H,
    blocks: &Table,
    p: BlockPos,
) -> Option<ItemStack> {
    let disc = record(host, p)?;
    let item = host.id(&disc)?;
    stop(host, p);
    host.edit_block_entity(p, Some(ID), &mut |data| *data = Compound::new());
    set_has_record(host, blocks, p, false);
    Some(ItemStack::new(item, 1))
}

fn set_has_record(host: &mut (impl Blocks + Effects), blocks: &Table, p: BlockPos, has: bool) {
    let Some(state) = host.block(p) else {
        return;
    };
    let value = if has { "true" } else { "false" };
    let new = blocks
        .with_property(state, "has_record", value)
        .unwrap_or(state);
    host.set_block(p, new);
}
