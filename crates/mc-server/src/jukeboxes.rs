//! Jukeboxes (minecraft.wiki, "Jukebox"): a disc plays once, for the length
//! its song has in the `jukebox_song` registry, and the jukebox gives a
//! redstone signal of 15 while it does. The disc stays in until it is
//! taken out; a comparator reads the song's `comparator_output` all the
//! while. Hoppers and droppers put discs in; a hopper below takes the disc
//! out once the signal is off. The block entity keeps the disc as
//! `RecordItem` and `ticks_since_song_started` while a song plays.

use std::collections::HashMap;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::tick::lock;

/// Jukeboxes with a song playing: their position, the song's length in
/// ticks and the ticks played.
pub type Playing = HashMap<(i32, i32, i32), (i64, i64)>;

const ID: &str = "minecraft:jukebox";
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

/// The song of a music disc item (`minecraft:music_disc_13`).
pub fn song_of(ctx: &ServerContext, item: &str) -> Option<Song> {
    let id = ctx.items.get(ctx.items.id(item)?)?.jukebox_song.clone()?;
    let pack = ctx.data.data_pack();
    let index = pack
        .registry_entries("minecraft:jukebox_song")
        .ok()?
        .iter()
        .position(|e| *e == id)?;
    let file = pack
        .root
        .join("minecraft/jukebox_song")
        .join(format!("{}.json", id.trim_start_matches("minecraft:")));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    Some(Song {
        index: index as i32,
        length: (json["length_in_seconds"].as_f64()? * 20.0).ceil() as i64,
        comparator: json["comparator_output"].as_u64().unwrap_or(0).min(15) as u8,
    })
}

/// The disc in the jukebox at `p`: its item name.
pub fn record(ctx: &ServerContext, p: Position) -> Option<String> {
    let be = ctx.world.block_entity(p, &ctx.data.blocks)?;
    match be.data.get("RecordItem") {
        Some(Tag::Compound(c)) => match c.get("id") {
            Some(Tag::String(id)) => Some(id.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn is_jukebox(ctx: &ServerContext, p: Position) -> Option<bool> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    (blocks.block_name(state)? == ID).then(|| blocks.property(state, "has_record") == Some("true"))
}

/// Whether the jukebox at `p` has a song playing (it gives a signal).
pub fn playing(ctx: &ServerContext, p: Position) -> bool {
    lock(&ctx.jukeboxes).contains_key(&(p.x, p.y, p.z))
}

fn centre_top(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 1.2,
        f64::from(p.z) + 0.5,
    ]
}

fn write_ticks(ctx: &ServerContext, p: Position, ticks: Option<i64>) {
    let blocks = &ctx.data.blocks;
    let Some(mut be) = ctx.world.block_entity(p, blocks) else {
        return;
    };
    match ticks {
        Some(t) => {
            be.data.insert(TICKS_KEY, Tag::Long(t));
        }
        None => {
            be.data.remove(TICKS_KEY);
        }
    }
    ctx.world.set_block_entity(be, blocks);
}

/// The disc in the jukebox at `p` starts to play: clients are told, and the
/// jukebox gives its signal (the caller sets `has_record`, which lets the
/// neighbours know).
pub fn start(ctx: &ServerContext, p: Position) {
    let Some(song) = record(ctx, p).and_then(|d| song_of(ctx, &d)) else {
        return;
    };
    lock(&ctx.jukeboxes).insert((p.x, p.y, p.z), (song.length, 0));
    write_ticks(ctx, p, Some(0));
    fx::level_event(
        ctx,
        1010,
        [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
        song.index,
    );
}

/// The music at `p` stops, as it does when the disc comes out or the
/// jukebox breaks. Clients are told either way.
pub fn stop(ctx: &ServerContext, p: Position) {
    lock(&ctx.jukeboxes).remove(&(p.x, p.y, p.z));
    write_ticks(ctx, p, None);
    fx::level_event(
        ctx,
        1011,
        [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
        0,
    );
}

/// A jukebox block entity was loaded: if its song was playing, it goes on.
pub fn wake(ctx: &ServerContext, p: Position) {
    let Some(be) = ctx.world.block_entity(p, &ctx.data.blocks) else {
        return;
    };
    let Some(Tag::Long(ticks)) = be.data.get(TICKS_KEY) else {
        return;
    };
    if let Some(song) = record(ctx, p).and_then(|d| song_of(ctx, &d)) {
        lock(&ctx.jukeboxes).insert((p.x, p.y, p.z), (song.length, *ticks));
    }
}

/// Lets the neighbours of `p` (and theirs) look again at the signal.
fn refresh(ctx: &ServerContext, p: Position) {
    let mut level = crate::level::WorldLevel::new(ctx);
    let logic = level.logic();
    let at = crate::level::to_pos(p);
    mc_tick::redstone::update_neighbors_at(&mut level, logic, at);
    for d in mc_tick::Dir::NC_ORDER {
        mc_tick::redstone::update_neighbors_at(&mut level, logic, at.offset(d));
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

/// One tick of every jukebox with a song playing.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<_> = lock(&ctx.jukeboxes).iter().map(|(k, v)| (*k, *v)).collect();
    for (key, (length, ticks)) in active {
        let p = Position::new(key.0, key.1, key.2);
        // Gone, or emptied without the music being stopped.
        if is_jukebox(ctx, p) != Some(true) {
            lock(&ctx.jukeboxes).remove(&key);
            continue;
        }
        let ticks = ticks + 1;
        if ticks >= length {
            // The song is over; the disc stays.
            lock(&ctx.jukeboxes).remove(&key);
            write_ticks(ctx, p, None);
            fx::level_event(
                ctx,
                1011,
                [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
                0,
            );
            refresh(ctx, p);
            continue;
        }
        if let Some(entry) = lock(&ctx.jukeboxes).get_mut(&key) {
            entry.1 = ticks;
        }
        if ticks % 20 == 0 {
            write_ticks(ctx, p, Some(ticks));
            fx::particles(
                ctx,
                "minecraft:note",
                centre_top(p),
                0.0,
                crate::hits::rnd() as f32 / 24.0,
                0,
            );
        }
    }
}

/// A disc put in by a hopper or a dropper: `false` if the jukebox is full
/// or the item is not a disc.
pub fn insert(ctx: &ServerContext, p: Position, stack: &ItemStack) -> bool {
    if is_jukebox(ctx, p) != Some(false) {
        return false;
    }
    let Some(name) = ctx.items.name(stack.item) else {
        return false;
    };
    if song_of(ctx, name).is_none() {
        return false;
    }
    let blocks = &ctx.data.blocks;
    let mut item = Compound::new();
    item.insert("id", Tag::String(name.to_owned()));
    item.insert("count", Tag::Int(1));
    let mut data = Compound::new();
    data.insert("RecordItem", Tag::Compound(item));
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: ID.into(),
            data,
        },
        blocks,
    );
    start(ctx, p);
    set_has_record(ctx, p, true);
    true
}

/// The disc a hopper took out of the jukebox at `p`.
pub fn take(ctx: &ServerContext, p: Position) -> Option<ItemStack> {
    let disc = record(ctx, p)?;
    let item = ctx.items.id(&disc)?;
    stop(ctx, p);
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: ID.into(),
            data: Compound::new(),
        },
        &ctx.data.blocks,
    );
    set_has_record(ctx, p, false);
    Some(ItemStack::new(item, 1))
}

fn set_has_record(ctx: &ServerContext, p: Position, has: bool) {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return;
    };
    let mut level = crate::level::WorldLevel::new(ctx);
    level.set_block(
        p,
        ctx.shapes
            .with(state, "has_record", if has { "true" } else { "false" }),
    );
    let changes = level.into_changes();
    let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::flat;

    fn put(ctx: &ServerContext, p: Position, name: &str, props: &[(&str, &str)]) {
        let state = ctx.data.blocks.state_id(name, props).unwrap();
        crate::level::WorldLevel::new(ctx).set_block(p, state);
    }

    fn pass(ctx: &ServerContext, n: usize) {
        for _ in 0..n {
            lock(&ctx.time).age += 1;
            crate::tick::run_scheduled_ticks(ctx);
            tick(ctx);
        }
    }

    fn disc(ctx: &ServerContext, name: &str) -> ItemStack {
        ItemStack::new(ctx.items.id(name).unwrap(), 1)
    }

    fn state_text(ctx: &ServerContext, p: Position) -> String {
        let blocks = &ctx.data.blocks;
        blocks
            .describe(ctx.world.block(p, blocks).unwrap())
            .unwrap()
            .to_owned()
    }

    #[test]
    fn songs_have_the_registry_s_length_index_and_comparator_output() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let song = song_of(&ctx, "minecraft:music_disc_13").unwrap();
        // 178 seconds, and a comparator reads 1 for it.
        assert_eq!((song.length, song.comparator), (3560, 1));
        let cat = song_of(&ctx, "minecraft:music_disc_cat").unwrap();
        assert_eq!(cat.comparator, 2);
        assert_ne!(song.index, cat.index);
        assert!(song_of(&ctx, "minecraft:stick").is_none());
    }

    #[test]
    fn a_song_plays_once_signals_while_it_does_and_ends_with_the_disc_in() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let jukebox = Position::new(10, -60, 10);
        let dust = Position::new(11, -60, 10);
        put(
            &ctx,
            jukebox,
            "minecraft:jukebox",
            &[("has_record", "false")],
        );
        put(&ctx, dust, "minecraft:redstone_wire", &[]);
        assert!(insert(
            &ctx,
            jukebox,
            &disc(&ctx, "minecraft:music_disc_13")
        ));
        assert!(playing(&ctx, jukebox));
        // A second disc has no room.
        assert!(!insert(
            &ctx,
            jukebox,
            &disc(&ctx, "minecraft:music_disc_cat")
        ));
        pass(&ctx, 2);
        assert!(
            state_text(&ctx, dust).contains("power=15"),
            "{}",
            state_text(&ctx, dust)
        );
        // The song is 3560 ticks long.
        pass(&ctx, 3400);
        assert!(playing(&ctx, jukebox));
        pass(&ctx, 200);
        assert!(!playing(&ctx, jukebox));
        assert!(
            state_text(&ctx, dust).contains("power=0"),
            "{}",
            state_text(&ctx, dust)
        );
        // The disc is still in, and a comparator would still read it.
        assert!(state_text(&ctx, jukebox).contains("has_record=true"));
        assert_eq!(
            record(&ctx, jukebox).as_deref(),
            Some("minecraft:music_disc_13")
        );
        assert_eq!(crate::blockuse::comparator_signal(&ctx, jukebox), Some(1));
    }

    #[test]
    fn taking_the_disc_out_stops_the_song_and_the_signal() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let jukebox = Position::new(20, -60, 10);
        let dust = Position::new(21, -60, 10);
        put(
            &ctx,
            jukebox,
            "minecraft:jukebox",
            &[("has_record", "false")],
        );
        put(&ctx, dust, "minecraft:redstone_wire", &[]);
        assert!(insert(
            &ctx,
            jukebox,
            &disc(&ctx, "minecraft:music_disc_cat")
        ));
        pass(&ctx, 2);
        assert!(state_text(&ctx, dust).contains("power=15"));
        let taken = take(&ctx, jukebox).expect("the disc");
        assert_eq!(ctx.items.name(taken.item), Some("minecraft:music_disc_cat"));
        assert!(!playing(&ctx, jukebox));
        pass(&ctx, 2);
        assert!(state_text(&ctx, dust).contains("power=0"));
        assert!(take(&ctx, jukebox).is_none());
        assert_eq!(crate::blockuse::comparator_signal(&ctx, jukebox), Some(0));
    }

    #[test]
    fn a_song_playing_when_the_chunk_was_saved_goes_on_where_it_was() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let jukebox = Position::new(50, -60, 10);
        put(
            &ctx,
            jukebox,
            "minecraft:jukebox",
            &[("has_record", "false")],
        );
        assert!(insert(
            &ctx,
            jukebox,
            &disc(&ctx, "minecraft:music_disc_13")
        ));
        pass(&ctx, 45);
        // The block entity keeps the ticks (every second); the playing set
        // is memory only.
        lock(&ctx.jukeboxes).clear();
        wake(&ctx, jukebox);
        let (length, ticks) = lock(&ctx.jukeboxes)[&(50, -60, 10)];
        assert_eq!((length, ticks), (3560, 40));
    }

    #[test]
    fn only_music_discs_go_into_a_jukebox() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let jukebox = Position::new(30, -60, 10);
        put(
            &ctx,
            jukebox,
            "minecraft:jukebox",
            &[("has_record", "false")],
        );
        assert!(!insert(&ctx, jukebox, &disc(&ctx, "minecraft:stick")));
        assert!(!playing(&ctx, jukebox));
    }

    #[test]
    fn a_hopper_feeds_a_jukebox_and_collects_the_disc_when_the_song_is_over() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let jukebox = Position::new(40, -60, 10);
        put(
            &ctx,
            jukebox,
            "minecraft:jukebox",
            &[("has_record", "false")],
        );
        // A hopper above, pointing down into the jukebox, holding a disc.
        let feeder = Position::new(40, -59, 10);
        put(
            &ctx,
            feeder,
            "minecraft:hopper",
            &[("facing", "down"), ("enabled", "true")],
        );
        containers_put(&ctx, feeder, disc(&ctx, "minecraft:music_disc_13"));
        // A hopper under it, pulling.
        let catcher = Position::new(40, -61, 10);
        put(
            &ctx,
            catcher,
            "minecraft:hopper",
            &[("facing", "south"), ("enabled", "true")],
        );
        crate::hoppers::register(&ctx, feeder);
        crate::hoppers::register(&ctx, catcher);
        pass(&ctx, 20);
        assert!(playing(&ctx, jukebox), "the hopper put the disc in");
        // The signal locks the hopper below while the song plays.
        pass(&ctx, 200);
        assert!(playing(&ctx, jukebox));
        assert_eq!(
            record(&ctx, jukebox).as_deref(),
            Some("minecraft:music_disc_13")
        );
        // Once it is over, the hopper below is free to take the disc.
        pass(&ctx, 3500);
        assert!(!playing(&ctx, jukebox));
        assert!(record(&ctx, jukebox).is_none(), "the disc was taken");
        assert!(state_text(&ctx, jukebox).contains("has_record=false"));
        let held = crate::containers::slots(
            &ctx,
            catcher,
            crate::containers::kind_of("minecraft:hopper").unwrap(),
        );
        assert!(
            held.iter()
                .flatten()
                .any(|s| ctx.items.name(s.item) == Some("minecraft:music_disc_13"))
        );
    }

    fn containers_put(ctx: &ServerContext, p: Position, stack: ItemStack) {
        crate::containers::put(
            ctx,
            p,
            crate::containers::kind_of("minecraft:hopper").unwrap(),
            &[(0, Some(stack))],
        );
    }
}
