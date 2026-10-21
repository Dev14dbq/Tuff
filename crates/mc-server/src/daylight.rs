//! Daylight detectors (minecraft.wiki, "Daylight Detector"): once a second
//! each one sets its power from the sky light it sees, weakened by the
//! sun's angle; inverted ones give the darkness instead.

use mc_protocol::Position;

use crate::context::{Event, ServerContext};
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

/// The sun's angle as a share of a turn (0 at noon, 0.5 at midnight), from
/// the overworld clock.
fn time_of_day(day_time: i64) -> f64 {
    let d = (day_time as f64 / 24000.0 - 0.25).rem_euclid(1.0);
    let e = 0.5 - (d * std::f64::consts::PI).cos() / 2.0;
    (d * 2.0 + e) / 3.0
}

fn day_time(ctx: &ServerContext) -> i64 {
    let time = lock(&ctx.time);
    time.clocks
        .iter()
        .find(|c| c.name == "minecraft:overworld")
        .or(time.clocks.first())
        .map_or(time.age, |c| c.ticks)
}

/// How much the sky light is dimmed now, 0..11: night, rain and thunder.
fn sky_darken(ctx: &ServerContext) -> i32 {
    let (rain, thunder) = {
        let w = lock(&ctx.weather);
        (f64::from(w.rain_level), f64::from(w.thunder_level))
    };
    let d = 1.0 - rain * 5.0 / 16.0;
    let e = 1.0 - thunder * 5.0 / 16.0;
    let angle = time_of_day(day_time(ctx)) * std::f64::consts::TAU;
    let f = 0.5 + 2.0 * angle.cos().clamp(-0.25, 0.25);
    ((1.0 - f * d * e) * 11.0) as i32
}

/// The power a detector `inverted` or not at `p` gives now.
fn power(ctx: &ServerContext, p: Position, inverted: bool, darken: i32) -> u8 {
    let mut level = i32::from(ctx.world.sky_light(p)) - darken;
    if inverted {
        level = 15 - level;
    } else if level > 0 {
        let mut angle = time_of_day(day_time(ctx)) * std::f64::consts::TAU;
        let toward = if angle < std::f64::consts::PI {
            0.0
        } else {
            std::f64::consts::TAU
        };
        angle += (toward - angle) * 0.2;
        level = (f64::from(level) * angle.cos()).round() as i32;
    }
    level.clamp(0, 15) as u8
}

/// A detector exists at `p`: it gets updated.
pub fn register(ctx: &ServerContext, p: Position) {
    lock(&ctx.daylight).insert((p.x, p.y, p.z));
}

/// A detector block was set at `p`: it gets its block entity and updates.
pub fn placed(ctx: &ServerContext, p: Position) {
    register(ctx, p);
    let blocks = &ctx.data.blocks;
    if ctx
        .world
        .block_entity(p, blocks)
        .is_none_or(|be| be.id != "minecraft:daylight_detector")
    {
        ctx.world.set_block_entity(
            mc_world::BlockEntity {
                x: p.x,
                y: p.y,
                z: p.z,
                id: "minecraft:daylight_detector".into(),
                data: mc_nbt::Compound::new(),
            },
            blocks,
        );
    }
}

/// Sets the power of the detector at `p` (in `level`) from the sky now.
fn refresh(ctx: &ServerContext, level: &mut WorldLevel, p: Position, darken: i32) -> bool {
    use mc_tick::Level as _;
    let state = level.block(to_pos(p));
    if ctx.data.blocks.block_name(state) != Some("minecraft:daylight_detector") {
        return false;
    }
    let inverted = ctx.data.blocks.property(state, "inverted") == Some("true");
    let new = power(ctx, p, inverted, darken).to_string();
    if ctx.data.blocks.property(state, "power") != Some(new.as_str()) {
        level.set_block(p, ctx.shapes.with(state, "power", &new));
    }
    true
}

/// The detector at `p` was just switched: its power follows at once.
pub fn update(ctx: &ServerContext, p: Position) {
    register(ctx, p);
    let mut level = WorldLevel::new(ctx);
    refresh(ctx, &mut level, p, sky_darken(ctx));
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

/// Every 20 game ticks, every detector in a loaded chunk.
pub fn tick(ctx: &ServerContext, count: i64) {
    if count % 20 != 0 {
        return;
    }
    let all: Vec<_> = lock(&ctx.daylight).iter().copied().collect();
    if all.is_empty() {
        return;
    }
    let darken = sky_darken(ctx);
    let mut level = WorldLevel::new(ctx);
    for key in all {
        let p = Position::new(key.0, key.1, key.2);
        if !ctx.world.is_loaded(p) || !refresh(ctx, &mut level, p, darken) {
            lock(&ctx.daylight).remove(&key);
        }
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noon_is_zero_and_midnight_half() {
        assert!(time_of_day(6000).abs() < 1e-9);
        assert!((time_of_day(18000) - 0.5).abs() < 1e-9);
        assert!(time_of_day(12000) > 0.2 && time_of_day(12000) < 0.3);
    }
}
