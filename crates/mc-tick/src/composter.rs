//! Composters (minecraft.wiki, "Composter"): each item put in fills a
//! layer by its chance; the seventh layer turns into bone meal ready to
//! take a second later.

use crate::dir::BlockPos;
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

/// Ticks between the last layer and the bone meal being ready.
pub const READY_DELAY: i64 = 20;

fn layers(ctx: Ctx, state: u32) -> u32 {
    ctx.shapes
        .property(state, "level")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// An item went into the composter `state` at `p`; `success` is its roll.
/// The fill shows (world event 1500) either way.
pub fn add(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, success: bool) {
    let l = layers(ctx, state);
    if success && l < 7 {
        let new = ctx.shapes.with(state, "level", &(l + 1).to_string());
        set_block(level, ctx, p, new, Flags::ALL);
        if l + 1 == 7 {
            level.schedule_block_tick(p, ctx.shapes.block_index(state), READY_DELAY, 0);
        }
        level.sound("block.composter.fill_success", p);
    } else {
        level.sound("block.composter.fill", p);
    }
    level.level_event(1500, p, i32::from(success));
}

/// The composter at `p` is emptied of its bone meal.
pub fn empty(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    set_block(
        level,
        ctx,
        p,
        ctx.shapes.with(state, "level", "0"),
        Flags::ALL,
    );
    level.sound("block.composter.empty", p);
}

/// The composter's block tick: a full one becomes ready.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind
        || ctx.shapes.block_name(state) != Some("minecraft:composter")
        || layers(ctx, state) != 7
    {
        return;
    }
    set_block(
        level,
        ctx,
        p,
        ctx.shapes.with(state, "level", "8"),
        Flags::ALL,
    );
    level.sound("block.composter.ready", p);
}

/// The chance (percent) that an item fills a composter a level.
pub fn chance(name: &str) -> u32 {
    let n = name.strip_prefix("minecraft:").unwrap_or(name);
    if n.ends_with("_leaves") || n.ends_with("_sapling") {
        return 30;
    }
    match n {
        "kelp" | "dried_kelp" | "beetroot_seeds" | "melon_seeds" | "pumpkin_seeds"
        | "wheat_seeds" | "torchflower_seeds" | "pitcher_pod" | "short_grass" | "seagrass"
        | "sweet_berries" | "glow_berries" | "moss_carpet" | "pale_moss_carpet"
        | "hanging_roots" | "small_dripleaf" | "mangrove_propagule" | "cherry_sapling"
        | "short_dry_grass" | "tall_dry_grass" | "bush" | "firefly_bush" => 30,
        "cactus"
        | "dried_kelp_block"
        | "flowering_azalea_leaves"
        | "glow_lichen"
        | "melon_slice"
        | "nether_sprouts"
        | "sugar_cane"
        | "tall_grass"
        | "twisting_vines"
        | "vine"
        | "weeping_vines"
        | "sunflower_unused"
        | "leaf_litter"
        | "wildflowers" => 50,
        "apple"
        | "azalea"
        | "beetroot"
        | "big_dripleaf"
        | "carrot"
        | "cocoa_beans"
        | "fern"
        | "large_fern"
        | "lily_pad"
        | "melon"
        | "moss_block"
        | "pale_moss_block"
        | "brown_mushroom"
        | "red_mushroom"
        | "nether_wart"
        | "potato"
        | "pumpkin"
        | "carved_pumpkin"
        | "crimson_roots"
        | "warped_roots"
        | "sea_pickle"
        | "shroomlight"
        | "spore_blossom"
        | "wheat"
        | "crimson_fungus"
        | "warped_fungus"
        | "dandelion"
        | "poppy"
        | "blue_orchid"
        | "allium"
        | "azure_bluet"
        | "red_tulip"
        | "orange_tulip"
        | "white_tulip"
        | "pink_tulip"
        | "oxeye_daisy"
        | "cornflower"
        | "lily_of_the_valley"
        | "wither_rose"
        | "torchflower"
        | "open_eyeblossom"
        | "closed_eyeblossom"
        | "sunflower"
        | "lilac"
        | "rose_bush"
        | "peony"
        | "pitcher_plant"
        | "cherry_leaves_unused"
        | "pale_hanging_moss" => 65,
        "baked_potato"
        | "bread"
        | "cookie"
        | "flowering_azalea"
        | "hay_block"
        | "red_mushroom_block"
        | "brown_mushroom_block"
        | "mushroom_stem"
        | "nether_wart_block"
        | "warped_wart_block" => 85,
        "cake" | "pumpkin_pie" => 100,
        _ => 0,
    }
}
