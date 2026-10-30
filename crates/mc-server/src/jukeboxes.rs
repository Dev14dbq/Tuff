//! Jukeboxes on the server: the rules are in
//! `mc_tick::block_entity::jukebox`; this reads songs from the
//! `jukebox_song` registry and keeps the jukeboxes with a song playing.

use std::collections::HashMap;

use mc_protocol::Position;
#[cfg(test)]
use mc_protocol::packets::play::ItemStack;
use mc_tick::BlockPos;
use mc_tick::block_entity::jukebox;

use crate::block_entities::Host;
use crate::context::ServerContext;
use crate::level::to_pos;
use crate::tick::lock;

pub use mc_tick::block_entity::jukebox::Song;

/// Jukeboxes with a song playing: their position, the song's length in
/// ticks and the ticks played.
pub type Playing = HashMap<(i32, i32, i32), (i64, i64)>;

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
#[cfg(test)]
fn record(ctx: &ServerContext, p: Position) -> Option<String> {
    jukebox::record(&Host::new(ctx), to_pos(p))
}

/// Whether the jukebox at `p` has a song playing (it gives a signal).
pub fn playing(ctx: &ServerContext, p: Position) -> bool {
    lock(&ctx.jukeboxes).contains_key(&(p.x, p.y, p.z))
}

/// The disc in the jukebox at `p` starts to play (see `jukebox::start`).
pub fn start(ctx: &ServerContext, p: Position) {
    jukebox::start(&mut Host::new(ctx), to_pos(p));
}

/// The music at `p` stops, as it does when the disc comes out or the
/// jukebox breaks. Clients are told either way.
pub fn stop(ctx: &ServerContext, p: Position) {
    jukebox::stop(&mut Host::new(ctx), to_pos(p));
}

/// A jukebox block entity was loaded: if its song was playing, it goes on.
pub fn wake(ctx: &ServerContext, p: Position) {
    jukebox::wake(&mut Host::new(ctx), to_pos(p));
}

/// One tick of every jukebox with a song playing.
pub fn tick(ctx: &ServerContext) {
    let active: Vec<_> = lock(&ctx.jukeboxes).iter().map(|(k, v)| (*k, *v)).collect();
    let mut host = Host::new(ctx);
    for (key, (length, ticks)) in active {
        let p = BlockPos::new(key.0, key.1, key.2);
        jukebox::tick(&mut host, &ctx.data.blocks, p, length, ticks);
    }
}

/// A disc put in by a hopper or a dropper: `false` if the jukebox is full
/// or the item is not a disc.
#[cfg(test)]
fn insert(ctx: &ServerContext, p: Position, stack: &ItemStack) -> bool {
    jukebox::insert(&mut Host::new(ctx), &ctx.data.blocks, to_pos(p), stack)
}

/// The disc a hopper took out of the jukebox at `p`.
#[cfg(test)]
fn take(ctx: &ServerContext, p: Position) -> Option<ItemStack> {
    jukebox::take(&mut Host::new(ctx), &ctx.data.blocks, to_pos(p))
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
