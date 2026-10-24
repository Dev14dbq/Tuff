//! Turtle eggs and frogspawn hatch (minecraft.wiki, "Turtle Egg" and
//! "Frogspawn"). A turtle egg on sand cracks on random ticks, quickly
//! around dusk and now and then at other hours, and the third crack hatches
//! the whole clutch. Frogspawn hatches 3 to 10 minutes after it is placed.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::rng::Rng;
use crate::updates::{Ctx, Flags, set_block};

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes
        .block_name(state)
        .unwrap_or("")
        .trim_start_matches("minecraft:")
}

fn int(ctx: Ctx, state: u32, property: &str) -> u32 {
    ctx.shapes
        .property(state, property)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The sun's turn as the fraction vanilla calls the time of day: 0 at noon.
fn time_of_day(day_time: i64) -> f64 {
    let d = (day_time.rem_euclid(24000) as f64 / 24000.0 - 0.25).rem_euclid(1.0);
    let e = 0.5 - (d * std::f64::consts::PI).cos() / 2.0;
    (d * 2.0 + e) / 3.0
}

/// Whether the hatch level may move now: for a short while after sunset
/// always, otherwise one random tick in 500.
fn may_progress(day_time: i64, rng: &mut Rng) -> bool {
    let f = time_of_day(day_time);
    (f > 0.65 && f < 0.69) || rng.below(500) == 0
}

fn on_sand(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    matches!(
        name(ctx, level.block(p.offset(Dir::Down))),
        "sand" | "red_sand" | "suspicious_sand"
    )
}

/// A random tick of a turtle egg.
pub fn egg_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32, rng: &mut Rng) {
    if !may_progress(level.day_time(), rng) || !on_sand(level, ctx, p) {
        return;
    }
    let hatch = int(ctx, state, "hatch");
    if hatch < 2 {
        level.sound("entity.turtle.egg_crack", p);
        let next = ctx.shapes.with(state, "hatch", &(hatch + 1).to_string());
        set_block(level, ctx, p, next, Flags::NONE);
    } else {
        level.sound("entity.turtle.egg_hatch", p);
        let eggs = int(ctx, state, "eggs").max(1);
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
        level.hatch(p, "minecraft:turtle", eggs as i32);
    }
}

/// Something stepped on the egg at `p`: one egg of the clutch breaks, and
/// the last one takes the block with it. False when there is no egg.
pub fn trample_egg(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let state = level.block(p);
    if name(ctx, state) != "turtle_egg" {
        return false;
    }
    level.sound("block.turtle_egg.break", p);
    let eggs = int(ctx, state, "eggs");
    if eggs > 1 {
        let next = ctx.shapes.with(state, "eggs", &(eggs - 1).to_string());
        set_block(level, ctx, p, next, Flags::NONE);
    } else {
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
    }
    true
}

/// Frogspawn placed at `p` asks for its tick 3600 to 11999 ticks later.
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, old: u32, state: u32) {
    if ctx.shapes.block_index(old) == ctx.shapes.block_index(state)
        || name(ctx, state) != "frogspawn"
    {
        return;
    }
    let mut rng = Rng::at(p, level.game_time());
    let kind = ctx.shapes.block_index(state);
    level.schedule_block_tick(p, kind, 3600 + i64::from(rng.below(8400)), 0);
}

/// A due tick of frogspawn: tadpoles come out.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind || name(ctx, state) != "frogspawn" {
        return;
    }
    let mut rng = Rng::at(p, level.game_time());
    level.sound("block.frogspawn.hatch", p);
    set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
    level.hatch(p, "minecraft:tadpole", 2 + rng.below(4) as i32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hatching_window_is_around_dawn_of_the_night() {
        // Noon is 0, midnight a half, the window sits before sunrise.
        assert!(time_of_day(6000) < 0.01);
        assert!((time_of_day(18000) - 0.5).abs() < 0.01);
        let f = time_of_day(21800);
        assert!(f > 0.65 && f < 0.69, "{f}");
        let f = time_of_day(12000);
        assert!(f < 0.25, "{f}");
    }
}
