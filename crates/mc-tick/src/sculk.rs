//! Sculk sensors and shriekers as blocks (minecraft.wiki, "Sculk Sensor",
//! "Calibrated Sculk Sensor", "Sculk Shrieker"): what a vibration that
//! arrived does to them, and their ticks after. Which vibration reaches
//! which sensor is the server's (`vibrations`).
//!
//! A sensor that a vibration reaches turns `active` with a power by its
//! distance (`max(1, 15 - floor(15 * distance / range))`) for 30 ticks (a
//! calibrated one 10), then sits in `cooldown` for 10 ticks before it hears
//! again. Its power goes out to every side, strongly into the block below.
//! A calibrated sensor gives none out of the side it takes its input from.
//! A shrieker shrieks for 90 ticks.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::redstone::update_neighbors_at;
use crate::updates::{Ctx, Flags, set_block};

pub const SENSOR: &str = "minecraft:sculk_sensor";
pub const CALIBRATED: &str = "minecraft:calibrated_sculk_sensor";
pub const SHRIEKER: &str = "minecraft:sculk_shrieker";

/// Ticks a sensor stays active.
pub const ACTIVE_TICKS: i64 = 30;
pub const CALIBRATED_ACTIVE_TICKS: i64 = 10;
pub const COOLDOWN_TICKS: i64 = 10;
pub const SHRIEK_TICKS: i64 = 90;

fn name<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes.block_name(state).unwrap_or("")
}

/// Whether the state is a sculk sensor of either kind.
pub fn is_sensor(ctx: Ctx, state: u32) -> bool {
    matches!(name(ctx, state), SENSOR | CALIBRATED)
}

pub fn is_calibrated(ctx: Ctx, state: u32) -> bool {
    name(ctx, state) == CALIBRATED
}

/// How far a sensor hears.
pub fn range(ctx: Ctx, state: u32) -> f64 {
    if is_calibrated(ctx, state) { 16.0 } else { 8.0 }
}

fn phase<'a>(ctx: Ctx<'a>, state: u32) -> &'a str {
    ctx.shapes
        .property(state, "sculk_sensor_phase")
        .unwrap_or("inactive")
}

/// Whether a sensor may take a vibration now.
pub fn listening(ctx: Ctx, state: u32) -> bool {
    is_sensor(ctx, state) && phase(ctx, state) == "inactive"
}

/// The power a vibration from `distance` blocks gives.
pub fn strength(distance: f64, range: f64) -> u8 {
    let lost = (15.0 * distance / range).floor() as i32;
    (15 - lost).max(1) as u8
}

fn waterlogged(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.property(state, "waterlogged") == Some("true")
}

/// Turns a sensor active with this power: it clicks (not under water), and
/// it is due to stop.
pub fn activate(level: &mut dyn Level, ctx: Ctx, p: BlockPos, power: u8) -> bool {
    let state = level.block(p);
    if !listening(ctx, state) {
        return false;
    }
    let s = ctx.shapes.with(state, "sculk_sensor_phase", "active");
    let s = ctx.shapes.with(s, "power", &power.to_string());
    set_block(level, ctx, p, s, Flags::ALL);
    update_neighbors_at(level, ctx, p);
    update_neighbors_at(level, ctx, p.offset(Dir::Down));
    let ticks = if is_calibrated(ctx, s) {
        CALIBRATED_ACTIVE_TICKS
    } else {
        ACTIVE_TICKS
    };
    level.schedule_block_tick(p, ctx.shapes.block_index(s), ticks, 0);
    if !waterlogged(ctx, s) {
        level.sound("block.sculk_sensor.clicking", p);
    }
    true
}

/// The power a sensor gives into its neighbour `toward`: weakly to every
/// side, but not out of a calibrated sensor's input.
pub fn power(ctx: Ctx, state: u32, toward: Dir) -> u8 {
    if !is_sensor(ctx, state) || phase(ctx, state) != "active" {
        return 0;
    }
    if is_calibrated(ctx, state) && input_side(ctx, state) == Some(toward) {
        return 0;
    }
    ctx.shapes
        .property(state, "power")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// The strong power a sensor drives into the block below.
pub fn strong_power(ctx: Ctx, state: u32, toward: Dir) -> u8 {
    if toward == Dir::Down {
        power(ctx, state, toward)
    } else {
        0
    }
}

/// Where a calibrated sensor takes its input from: the amethyst side, the
/// way it faces (back toward who placed it).
pub fn input_side(ctx: Ctx, state: u32) -> Option<Dir> {
    Some(match ctx.shapes.property(state, "facing")? {
        "north" => Dir::North,
        "south" => Dir::South,
        "west" => Dir::West,
        _ => Dir::East,
    })
}

/// The frequency a calibrated sensor listens for: the redstone it gets from
/// behind, 0 for any.
pub fn calibration(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> u8 {
    let Some(side) = input_side(ctx, state) else {
        return 0;
    };
    crate::redstone::signal_from(level, ctx, p, side)
}

/// Starts a shrieker's shriek; whether it was quiet.
pub fn shriek(level: &mut dyn Level, ctx: Ctx, p: BlockPos) -> bool {
    let state = level.block(p);
    if name(ctx, state) != SHRIEKER || ctx.shapes.property(state, "shrieking") == Some("true") {
        return false;
    }
    let s = ctx.shapes.with(state, "shrieking", "true");
    set_block(level, ctx, p, s, Flags::ALL);
    level.schedule_block_tick(p, ctx.shapes.block_index(s), SHRIEK_TICKS, 0);
    if !waterlogged(ctx, s) {
        level.sound("block.sculk_shrieker.shriek", p);
    }
    true
}

/// A sensor's or shrieker's due tick: active turns to cooldown, cooldown to
/// inactive; a shriek ends.
pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) != kind {
        return;
    }
    if name(ctx, state) == SHRIEKER {
        if ctx.shapes.property(state, "shrieking") == Some("true") {
            let s = ctx.shapes.with(state, "shrieking", "false");
            set_block(level, ctx, p, s, Flags::ALL);
        }
        return;
    }
    if !is_sensor(ctx, state) {
        return;
    }
    match phase(ctx, state) {
        "active" => {
            let s = ctx.shapes.with(state, "sculk_sensor_phase", "cooldown");
            let s = ctx.shapes.with(s, "power", "0");
            set_block(level, ctx, p, s, Flags::ALL);
            update_neighbors_at(level, ctx, p);
            update_neighbors_at(level, ctx, p.offset(Dir::Down));
            level.schedule_block_tick(p, kind, COOLDOWN_TICKS, 0);
            if !waterlogged(ctx, s) {
                level.sound("block.sculk_sensor.clicking_stop", p);
            }
        }
        "cooldown" => {
            let s = ctx.shapes.with(state, "sculk_sensor_phase", "inactive");
            set_block(level, ctx, p, s, Flags::ALL);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_falls_with_distance() {
        assert_eq!(strength(0.0, 8.0), 15);
        assert_eq!(strength(1.0, 8.0), 14);
        assert_eq!(strength(4.0, 8.0), 8);
        assert_eq!(strength(7.9, 8.0), 1);
        assert_eq!(strength(8.0, 8.0), 1);
        assert_eq!(strength(8.0, 16.0), 8);
    }
}
