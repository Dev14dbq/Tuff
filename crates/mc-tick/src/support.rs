//! Blocks that need something to hold them up (minecraft.wiki: "Torch",
//! "Rail", "Redstone Dust", "Button", "Flower", "Sugar Cane", "Cactus"...):
//! what each needs, and whether it has it. Plants read the `supports_*`
//! block tags; how well a block's faces hold things is observed from
//! vanilla (`observe support`: torch, rail and button on top, lantern and
//! button below; fence attachment for the sides).
//!
//! Observed on the parity bench: a command places a block even where it
//! cannot stay; it breaks when an update reaches it. Attached blocks
//! (torches, buttons, dust, rails, signs...) look only at updates from the
//! side that holds them, plants at updates from any side, and cactus, sugar
//! cane and bamboo wait for a block tick. Candles never break.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::updates::{Ctx, Flags, set_block};

/// Observed support bits (see `observed/support.json`).
pub const TOP_CENTER: u8 = 1;
pub const TOP_RIGID: u8 = 2;
pub const TOP_FULL: u8 = 4;
pub const BOTTOM_CENTER: u8 = 8;
pub const BOTTOM_FULL: u8 = 16;

/// When a block looks at its support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Never breaks for lack of support.
    Never,
    /// On updates from this side.
    From(Dir),
    /// On updates from any side.
    AnySide,
    /// A block tick after an update.
    Tick,
}

/// How the block `state` checks its support.
pub fn check(ctx: Ctx, state: u32) -> Check {
    let Some(full) = ctx.shapes.block_name(state) else {
        return Check::Never;
    };
    let name = full.trim_start_matches("minecraft:");
    let upper = ctx.shapes.property(state, "half") == Some("upper");
    let attached = || facing(ctx, state).map_or(Check::Never, |f| Check::From(f.opposite()));
    match name {
        "torch" | "soul_torch" | "redstone_torch" | "copper_torch" | "redstone_wire"
        | "repeater" | "comparator" | "rail" | "powered_rail" | "detector_rail"
        | "activator_rail" | "snow" | "candle_cake" | "cake" => Check::From(Dir::Down),
        "wall_torch"
        | "soul_wall_torch"
        | "redstone_wall_torch"
        | "copper_wall_torch"
        | "ladder"
        | "tripwire_hook" => attached(),
        "lever" => face_check(ctx, state),
        "cactus" | "sugar_cane" | "bamboo" => Check::Tick,
        "cocoa" => facing(ctx, state).map_or(Check::Never, Check::From),
        "candle" => Check::Never,
        "piston_head" => facing(ctx, state).map_or(Check::Never, |f| Check::From(f.opposite())),
        "lantern" | "soul_lantern" => lantern_check(ctx, state),
        _ if name.ends_with("copper_lantern") => lantern_check(ctx, state),
        _ if name.ends_with("_candle") => Check::Never,
        _ if name.ends_with("_button") => face_check(ctx, state),
        _ if name.ends_with("_pressure_plate") || name.ends_with("_carpet") => {
            Check::From(Dir::Down)
        }
        _ if name.ends_with("_door") => {
            if upper {
                Check::Never
            } else {
                Check::From(Dir::Down)
            }
        }
        _ if name.ends_with("_wall_sign")
            || name.ends_with("_wall_banner")
            || name.ends_with("_wall_hanging_sign")
            || name.ends_with("_wall_fan") =>
        {
            attached()
        }
        _ if name.ends_with("_hanging_sign") => Check::From(Dir::Up),
        _ if name.ends_with("_sign") || name.ends_with("_banner") => Check::From(Dir::Down),
        _ => {
            if is_plant(ctx, state, name) {
                if upper { Check::Never } else { Check::AnySide }
            } else if is_coral(ctx, state, name) {
                Check::From(Dir::Down)
            } else {
                Check::Never
            }
        }
    }
}

fn face_check(ctx: Ctx, state: u32) -> Check {
    match ctx.shapes.property(state, "face") {
        Some("floor") => Check::From(Dir::Down),
        Some("ceiling") => Check::From(Dir::Up),
        _ => facing(ctx, state).map_or(Check::Never, |f| Check::From(f.opposite())),
    }
}

fn lantern_check(ctx: Ctx, state: u32) -> Check {
    if ctx.shapes.property(state, "hanging") == Some("true") {
        Check::From(Dir::Up)
    } else {
        Check::From(Dir::Down)
    }
}

fn is_coral(ctx: Ctx, state: u32, name: &str) -> bool {
    ctx.shapes.in_tag(state, "minecraft:corals")
        || ctx.shapes.in_tag(state, "minecraft:coral_plants")
        || name.ends_with("_coral_fan")
        || name.ends_with("coral")
}

/// Plants that break as soon as their ground is wrong.
fn is_plant(ctx: Ctx, state: u32, name: &str) -> bool {
    ctx.shapes.in_tag(state, "minecraft:saplings")
        || ctx.shapes.in_tag(state, "minecraft:small_flowers")
        || ctx.shapes.in_tag(state, "minecraft:flowers")
        || ctx.shapes.in_tag(state, "minecraft:crops")
        || matches!(
            name,
            "short_grass"
                | "tall_grass"
                | "fern"
                | "large_fern"
                | "sweet_berry_bush"
                | "bush"
                | "firefly_bush"
                | "pink_petals"
                | "wildflowers"
                | "dead_bush"
                | "short_dry_grass"
                | "tall_dry_grass"
                | "lily_pad"
                | "nether_wart"
                | "wither_rose"
                | "crimson_fungus"
                | "warped_fungus"
                | "crimson_roots"
                | "warped_roots"
                | "nether_sprouts"
                | "azalea"
                | "flowering_azalea"
                | "small_dripleaf"
                | "brown_mushroom"
                | "red_mushroom"
                | "melon_stem"
                | "pumpkin_stem"
                | "attached_melon_stem"
                | "attached_pumpkin_stem"
                | "bamboo_sapling"
                | "cactus_flower"
        )
}

fn facing(ctx: Ctx, state: u32) -> Option<Dir> {
    ctx.shapes
        .property(state, "facing")
        .and_then(Dir::from_name)
}

/// Whether the block at `p` holding `state` stays where it is.
pub fn can_survive(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let Some(full) = ctx.shapes.block_name(state) else {
        return true;
    };
    let name = full.trim_start_matches("minecraft:");
    let below = level.block(p.offset(Dir::Down));
    let above = level.block(p.offset(Dir::Up));
    let holds = |s: u32, bits: u8| ctx.shapes.support(s) & bits != 0;
    let on = |tag: &str| ctx.shapes.in_tag(below, tag);
    // The face of the neighbour in direction `d` that looks back at us.
    let side = |d: Dir| ctx.shapes.face_attaches(level.block(p.offset(d)), d);
    let attached_side = || facing(ctx, state).is_some_and(|f| side(f.opposite()));
    let lower = ctx.shapes.property(state, "half") != Some("upper");
    let water_at = |q: BlockPos| {
        let s = level.block(q);
        ctx.shapes.block_name(s) == Some("minecraft:water")
            || ctx.shapes.property(s, "waterlogged") == Some("true")
    };
    match name {
        "torch" | "soul_torch" | "redstone_torch" | "copper_torch" => holds(below, TOP_CENTER),
        "wall_torch" | "soul_wall_torch" | "redstone_wall_torch" | "copper_wall_torch" => {
            attached_side()
        }
        "lever" => face_attached(ctx, state, below, above, &side),
        _ if name.ends_with("_button") => face_attached(ctx, state, below, above, &side),
        "redstone_wire" => {
            holds(below, TOP_FULL) || ctx.shapes.block_name(below) == Some("minecraft:hopper")
        }
        "repeater" | "comparator" => holds(below, TOP_RIGID),
        "rail" | "powered_rail" | "detector_rail" | "activator_rail" => holds(below, TOP_RIGID),
        _ if name.ends_with("_pressure_plate") => holds(below, TOP_RIGID | TOP_CENTER),
        _ if name.ends_with("_door") => !lower || holds(below, TOP_FULL),
        _ if name.ends_with("_carpet") => !ctx.shapes.is_air(below),
        _ if name.ends_with("_wall_sign")
            || name.ends_with("_wall_banner")
            || name.ends_with("_wall_hanging_sign")
            || name == "ladder"
            || name == "tripwire_hook"
            || name.ends_with("_wall_fan") =>
        {
            attached_side()
        }
        _ if name.ends_with("_hanging_sign") => holds(above, BOTTOM_CENTER | BOTTOM_FULL),
        _ if name.ends_with("_sign") || name.ends_with("_banner") => holds(below, TOP_CENTER),
        "candle" => true,
        // A head lives on its extended piston.
        "piston_head" => facing(ctx, state).is_some_and(|f| {
            let base = level.block(p.offset(f.opposite()));
            matches!(
                ctx.shapes.block_name(base),
                Some("minecraft:piston" | "minecraft:sticky_piston")
            ) && ctx.shapes.property(base, "extended") == Some("true")
                && ctx.shapes.property(base, "facing") == Some(f.name())
        }),
        _ if name.ends_with("_candle") => true,
        "lantern" | "soul_lantern" => lantern(ctx, state, below, above),
        _ if name.ends_with("copper_lantern") => lantern(ctx, state, below, above),
        "snow" => {
            !on("minecraft:cannot_support_snow_layer")
                && (on("minecraft:support_override_snow_layer")
                    || holds(below, TOP_FULL)
                    || (ctx.shapes.block_name(below) == Some("minecraft:snow")
                        && ctx.shapes.property(below, "layers") == Some("8")))
        }
        "cactus" => {
            (on("minecraft:supports_cactus") || ctx.shapes.block_name(below) == Some(full))
                && Dir::HORIZONTAL.iter().all(|&d| {
                    // Nothing solid beside it.
                    let n = level.block(p.offset(d));
                    ctx.shapes.is_air(n) || !ctx.shapes.face_attaches(n, d)
                })
        }
        "cactus_flower" => {
            on("minecraft:support_override_cactus_flower") || holds(below, TOP_CENTER)
        }
        "sugar_cane" => {
            ctx.shapes.block_name(below) == Some(full)
                || (on("minecraft:supports_sugar_cane")
                    && Dir::HORIZONTAL.iter().any(|&d| {
                        let q = p.offset(Dir::Down).offset(d);
                        water_at(q)
                            || ctx
                                .shapes
                                .in_tag(level.block(q), "minecraft:supports_sugar_cane_adjacently")
                    }))
        }
        "bamboo" | "bamboo_sapling" => {
            on("minecraft:supports_bamboo")
                || matches!(
                    ctx.shapes.block_name(below),
                    Some("minecraft:bamboo" | "minecraft:bamboo_sapling")
                )
        }
        "lily_pad" => water_at(p.offset(Dir::Down)) || on("minecraft:supports_lily_pad"),
        "nether_wart" => on("minecraft:supports_nether_wart"),
        "wither_rose" => on("minecraft:supports_wither_rose"),
        "crimson_fungus" => on("minecraft:supports_crimson_fungus"),
        "warped_fungus" => on("minecraft:supports_warped_fungus"),
        "crimson_roots" => on("minecraft:supports_crimson_roots"),
        "warped_roots" => on("minecraft:supports_warped_roots"),
        "nether_sprouts" => on("minecraft:supports_nether_sprouts"),
        "azalea" | "flowering_azalea" => on("minecraft:supports_azalea"),
        "small_dripleaf" => !lower || on("minecraft:supports_small_dripleaf"),
        "big_dripleaf" => {
            on("minecraft:supports_big_dripleaf")
                || matches!(
                    ctx.shapes.block_name(below),
                    Some("minecraft:big_dripleaf_stem" | "minecraft:big_dripleaf")
                )
        }
        "melon_stem" | "pumpkin_stem" | "attached_melon_stem" | "attached_pumpkin_stem" => {
            on("minecraft:supports_stem_crops")
        }
        // Dark enough (under 13) on a full block, or on mycelium and the like.
        "brown_mushroom" | "red_mushroom" => {
            on("minecraft:overrides_mushroom_light_requirement")
                || (holds(below, TOP_FULL) && level.brightness(p) < 13)
        }
        "dead_bush" | "short_dry_grass" | "tall_dry_grass" => {
            !lower || on("minecraft:supports_dry_vegetation")
        }
        "cocoa" => facing(ctx, state).is_some_and(|f| {
            ctx.shapes
                .in_tag(level.block(p.offset(f)), "minecraft:supports_cocoa")
        }),
        _ if ctx.shapes.in_tag(state, "minecraft:crops") => on("minecraft:supports_crops"),
        _ if ctx.shapes.in_tag(state, "minecraft:corals")
            || ctx.shapes.in_tag(state, "minecraft:coral_plants")
            || name.ends_with("_coral_fan")
            || name.ends_with("coral") =>
        {
            holds(below, TOP_FULL)
        }
        _ if ctx.shapes.in_tag(state, "minecraft:saplings")
            || ctx.shapes.in_tag(state, "minecraft:small_flowers")
            || ctx.shapes.in_tag(state, "minecraft:flowers")
            || matches!(
                name,
                "short_grass"
                    | "tall_grass"
                    | "fern"
                    | "large_fern"
                    | "sweet_berry_bush"
                    | "bush"
                    | "firefly_bush"
                    | "pink_petals"
                    | "wildflowers"
            ) =>
        {
            !lower || on("minecraft:supports_vegetation")
        }
        _ => true,
    }
}

/// Levers and buttons: on a floor, a ceiling or a wall.
fn face_attached(ctx: Ctx, state: u32, below: u32, above: u32, side: &dyn Fn(Dir) -> bool) -> bool {
    match ctx.shapes.property(state, "face") {
        Some("floor") => ctx.shapes.support(below) & TOP_FULL != 0,
        Some("ceiling") => ctx.shapes.support(above) & BOTTOM_FULL != 0,
        _ => facing(ctx, state).is_some_and(|f| side(f.opposite())),
    }
}

fn lantern(ctx: Ctx, state: u32, below: u32, above: u32) -> bool {
    if ctx.shapes.property(state, "hanging") == Some("true") {
        ctx.shapes.support(above) & (BOTTOM_CENTER | BOTTOM_FULL) != 0
    } else {
        ctx.shapes.support(below) & TOP_CENTER != 0
    }
}

fn is_rail(name: &str) -> bool {
    matches!(
        name,
        "minecraft:rail"
            | "minecraft:powered_rail"
            | "minecraft:detector_rail"
            | "minecraft:activator_rail"
    )
}

fn destroy(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    level.destroyed(p, state);
    set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
}

/// A block was just put at `p` (a different block from before). Dust and
/// powered, detector and activator rails break at once without support;
/// cactus, sugar cane and bamboo look next tick.
pub fn placed(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let name = ctx.shapes.block_name(state).unwrap_or("");
    let at_once = matches!(
        name,
        "minecraft:redstone_wire"
            | "minecraft:powered_rail"
            | "minecraft:detector_rail"
            | "minecraft:activator_rail"
    );
    if at_once && !can_survive(level, ctx, p, state) {
        destroy(level, ctx, p, state);
    } else if check(ctx, state) == Check::Tick {
        let kind = ctx.shapes.block_index(state);
        if !level.is_block_tick_scheduled(p, kind) {
            level.schedule_block_tick(p, kind, 1, 0);
        }
    }
}

/// A neighbour of `p` changed: rails without support break. Returns whether
/// the block broke.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let state = level.block(p);
    let name = ctx.shapes.block_name(state).unwrap_or("");
    if is_rail(name) && !can_survive(level, ctx, p, state) {
        destroy(level, ctx, p, state);
        return true;
    }
    false
}
