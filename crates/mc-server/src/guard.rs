//! What the server watches a player's connection for (minecraft.wiki,
//! "Server.properties", "Game rule"): moving too far in one step (sent back,
//! unless `player_movement_check` is off, or `elytra_movement_check` while
//! gliding), floating in the air when `allow-flight` is off, chatting too
//! fast, and sending more packets a second than `rate-limit`.

use std::time::{Duration, Instant};

use crate::context::{Pose, ServerContext};

/// The squared distance one movement may cover, and while gliding.
const TOO_FAR: f64 = 100.0;
const TOO_FAR_GLIDING: f64 = 300.0;

/// Ticks a player may float before they are kicked for flying.
const FLOATING_TICKS: u32 = 80;

/// A tick, for the counters that run in ticks.
const TICK: Duration = Duration::from_millis(50);

/// What each chat message or command adds to the spam counter, and how
/// high it may go; it falls by one a tick.
const SPAM_STEP: u32 = 20;
const SPAM_LIMIT: u32 = 200;

fn ticks(d: Duration) -> u32 {
    (d.as_millis() / TICK.as_millis()).min(u128::from(u32::MAX)) as u32
}

/// The move from `before` to `after` is too far for one step: the player
/// is sent back.
pub fn too_far(ctx: &ServerContext, before: Pose, after: Pose, gliding: bool) -> bool {
    if !ctx.game_rule_bool("player_movement_check")
        || gliding && !ctx.game_rule_bool("elytra_movement_check")
    {
        return false;
    }
    let (dx, dy, dz) = (after.x - before.x, after.y - before.y, after.z - before.z);
    dx * dx + dy * dy + dz * dz > if gliding { TOO_FAR_GLIDING } else { TOO_FAR }
}

/// Nothing but air around the player's box (a sixteenth wider, reaching
/// a little more than half a block down).
pub fn nothing_around(ctx: &ServerContext, pose: Pose) -> bool {
    let lo = [pose.x - 0.3625, pose.y - 0.6125, pose.z - 0.3625];
    let hi = [pose.x + 0.3625, pose.y + 1.8625, pose.z + 0.3625];
    for x in lo[0].floor() as i32..=hi[0].floor() as i32 {
        for y in lo[1].floor() as i32..=hi[1].floor() as i32 {
            for z in lo[2].floor() as i32..=hi[2].floor() as i32 {
                let at = mc_protocol::Position::new(x, y, z);
                match ctx.world.block(at, &ctx.data.blocks) {
                    Some(s) if ctx.data.blocks.is_air(s) => {}
                    _ => return false,
                }
            }
        }
    }
    true
}

/// How long a player has been floating.
#[derive(Debug, Default)]
pub struct Floating {
    since: Option<Instant>,
}

impl Floating {
    /// The player's latest move left them floating or not; whether they
    /// have floated too long.
    pub fn moved(&mut self, floating: bool, now: Instant) -> bool {
        if !floating {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        ticks(now.duration_since(since)) > FLOATING_TICKS
    }
}

/// The chat spam counter.
#[derive(Debug, Default)]
pub struct Spam {
    count: u32,
    at: Option<Instant>,
}

impl Spam {
    /// A chat message or command: whether that is too many.
    pub fn said(&mut self, now: Instant) -> bool {
        let fallen = self.at.map_or(0, |at| ticks(now.duration_since(at)));
        self.count = self.count.saturating_sub(fallen) + SPAM_STEP;
        self.at = Some(now);
        self.count > SPAM_LIMIT
    }
}

/// Packets received this second.
#[derive(Debug)]
pub struct Rate {
    limit: u32,
    count: u32,
    since: Instant,
}

impl Rate {
    /// `limit` packets a second; 0 for no limit.
    pub fn new(limit: u32, now: Instant) -> Self {
        Self {
            limit,
            count: 0,
            since: now,
        }
    }

    /// A packet came in: whether the second's limit is passed.
    pub fn received(&mut self, now: Instant) -> bool {
        if self.limit == 0 {
            return false;
        }
        if now.duration_since(self.since) >= Duration::from_secs(1) {
            self.count = 0;
            self.since = now;
        }
        self.count += 1;
        self.count > self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, z: f64) -> Pose {
        Pose {
            x,
            y,
            z,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        }
    }

    #[test]
    fn ten_blocks_a_step_is_too_far_unless_gliding_or_the_rule_is_off() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let from = at(0.0, -60.0, 0.0);
        assert!(!too_far(&ctx, from, at(9.9, -60.0, 0.0), false));
        assert!(too_far(&ctx, from, at(10.1, -60.0, 0.0), false));
        assert!(!too_far(&ctx, from, at(15.0, -60.0, 0.0), true));
        assert!(too_far(&ctx, from, at(17.5, -60.0, 0.0), true));
        let rules = |name: &str, on: bool| {
            ctx.game_rules
                .lock()
                .unwrap()
                .set(name, crate::rules::Value::Bool(on));
        };
        rules("elytra_movement_check", false);
        assert!(!too_far(&ctx, from, at(17.5, -60.0, 0.0), true));
        assert!(too_far(&ctx, from, at(17.5, -60.0, 0.0), false));
        rules("player_movement_check", false);
        assert!(!too_far(&ctx, from, at(17.5, -60.0, 0.0), false));
    }

    #[test]
    fn floating_over_80_ticks_is_flying() {
        let mut f = Floating::default();
        let t0 = Instant::now();
        assert!(!f.moved(true, t0));
        assert!(!f.moved(true, t0 + TICK * 80));
        assert!(f.moved(true, t0 + TICK * 81));
        // Touching anything starts it over.
        assert!(!f.moved(false, t0 + TICK * 82));
        assert!(!f.moved(true, t0 + TICK * 100));
    }

    #[test]
    fn the_flat_world_has_air_above_and_ground_below() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        // The flat ground's top is at y -61.
        assert!(!nothing_around(&ctx, at(0.5, -60.0, 0.5)));
        assert!(nothing_around(&ctx, at(0.5, -50.0, 0.5)));
    }

    #[test]
    fn eleven_messages_at_once_are_spam_but_a_tick_apart_they_wear_off() {
        let t0 = Instant::now();
        let mut s = Spam::default();
        for _ in 0..10 {
            assert!(!s.said(t0));
        }
        assert!(s.said(t0));
        // A message a second never piles up.
        let mut s = Spam::default();
        for i in 0..100 {
            assert!(!s.said(t0 + Duration::from_secs(i)));
        }
    }

    #[test]
    fn more_packets_a_second_than_the_limit_is_too_many() {
        let t0 = Instant::now();
        let mut r = Rate::new(3, t0);
        assert!(!r.received(t0));
        assert!(!r.received(t0));
        assert!(!r.received(t0));
        assert!(r.received(t0));
        assert!(!r.received(t0 + Duration::from_secs(1)));
        let mut off = Rate::new(0, t0);
        assert!((0..1000).all(|_| !off.received(t0)));
    }
}
