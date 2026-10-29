//! What the server watches a player's connection for (minecraft.wiki,
//! "Server.properties", "Game rule"): moving too far in one step (sent back,
//! unless `player_movement_check` is off, or `elytra_movement_check` while
//! gliding), floating in the air when `allow-flight` is off, chatting too
//! fast, and sending more packets a second than `rate-limit`. Also what
//! vanilla refuses outright: chat with characters it does not allow, moves
//! to a position that is not a number, creative drops faster than its
//! throttle, and more than one command suggestion request a tick.

use std::time::{Duration, Instant};

use crate::context::{Pose, ServerContext};

/// The squared distance one movement may cover, and while gliding.
const TOO_FAR: f64 = 100.0;
const TOO_FAR_GLIDING: f64 = 300.0;

/// Ticks a player may float before they are kicked for flying.
const FLOATING_TICKS: u32 = 80;

/// A tick, for the counters that run in ticks.
const TICK: Duration = Duration::from_millis(50);

fn ticks(d: Duration) -> u32 {
    (d.as_millis() / TICK.as_millis()).min(u128::from(u32::MAX)) as u32
}

/// A chat message or command vanilla kicks for
/// (`multiplayer.disconnect.illegal_characters`): `§`, control characters
/// or DEL anywhere in it.
pub fn illegal_chat(message: &str) -> bool {
    message
        .chars()
        .any(|c| c == '§' || c < ' ' || c == '\u{7f}')
}

/// A move vanilla kicks for (`multiplayer.disconnect.invalid_player_movement`,
/// or `invalid_vehicle_movement` for a vehicle): a coordinate that is not a
/// number, or a rotation that is not finite. Infinite coordinates are only
/// clamped by vanilla.
pub fn invalid_move(position: Option<[f64; 3]>, rotation: Option<[f32; 2]>) -> bool {
    position.is_some_and(|p| p.iter().any(|v| v.is_nan()))
        || rotation.is_some_and(|r| r.iter().any(|v| !v.is_finite()))
}

/// The kick (a translation key) vanilla answers `packet` with before doing
/// anything else with it, if any.
pub fn refused(packet: &mc_protocol::packets::play::Serverbound) -> Option<&'static str> {
    use mc_protocol::packets::play::Serverbound as P;
    let player = "multiplayer.disconnect.invalid_player_movement";
    match *packet {
        P::Chat(ref chat) if illegal_chat(&chat.message) => {
            Some("multiplayer.disconnect.illegal_characters")
        }
        P::ChatCommand { ref command } if illegal_chat(command) => {
            Some("multiplayer.disconnect.illegal_characters")
        }
        P::MovePlayerPos { x, y, z, .. } if invalid_move(Some([x, y, z]), None) => Some(player),
        P::MovePlayerPosRot {
            x,
            y,
            z,
            yaw,
            pitch,
            ..
        } if invalid_move(Some([x, y, z]), Some([yaw, pitch])) => Some(player),
        P::MovePlayerRot { yaw, pitch, .. } if invalid_move(None, Some([yaw, pitch])) => {
            Some(player)
        }
        P::MoveVehicle {
            x,
            y,
            z,
            yaw,
            pitch,
            ..
        } if invalid_move(Some([x, y, z]), Some([yaw, pitch])) => {
            Some("multiplayer.disconnect.invalid_vehicle_movement")
        }
        _ => None,
    }
}

/// A stack from the creative inventory vanilla takes: no more than its
/// stack size (its `max_stack_size` component, else the item's).
pub fn creative_stack_ok(ctx: &ServerContext, stack: &mc_protocol::components::ItemStack) -> bool {
    let max = match stack.components.get("minecraft:max_stack_size") {
        Some(mc_nbt::Tag::Int(max)) => *max,
        _ => ctx.items.max_stack(stack.item),
    };
    stack.count <= max
}

/// Inventory menu slots the creative inventory may set: vanilla's 1 to 45
/// (0 is the crafting result).
pub const CREATIVE_SLOTS: std::ops::RangeInclusive<i16> = 1..=45;

/// Vanilla's tick throttle: each event adds `step`, each tick takes one
/// away, and the count may not reach `threshold`.
#[derive(Debug)]
pub struct Throttle {
    step: u32,
    threshold: u32,
    count: u32,
    /// The tick the count was last brought up to date at.
    at: Option<Instant>,
}

impl Throttle {
    /// Chat and commands: 20 a message, kicked at 200 (vanilla's default of
    /// ten seconds' worth).
    pub fn chat() -> Self {
        Self::new(20, 200)
    }

    /// Items thrown out of the creative inventory: 20 a drop, ignored from
    /// 1480 on.
    pub fn creative_drops() -> Self {
        Self::new(20, 1480)
    }

    fn new(step: u32, threshold: u32) -> Self {
        Self {
            step,
            threshold,
            count: 0,
            at: None,
        }
    }

    fn decay(&mut self, now: Instant) {
        let at = *self.at.get_or_insert(now);
        let fallen = ticks(now.saturating_duration_since(at));
        self.count = self.count.saturating_sub(fallen);
        // Whole ticks only, so events closer than a tick still wear off.
        self.at = Some(at + TICK * fallen);
    }

    /// An event at `now` (counted first): whether the count is now at the
    /// threshold, which is what vanilla kicks for.
    pub fn said(&mut self, now: Instant) -> bool {
        self.decay(now);
        self.count = self.count.saturating_add(self.step);
        self.count >= self.threshold
    }

    /// An event at `now` that is let through (and counted) only while
    /// under the threshold.
    pub fn allow(&mut self, now: Instant) -> bool {
        self.decay(now);
        if self.count >= self.threshold {
            return false;
        }
        self.count = self.count.saturating_add(self.step);
        true
    }
}

/// Command suggestion requests: vanilla answers at most one a tick, and a
/// newer request replaces one still waiting, which then gets no answer.
#[derive(Debug, Default)]
pub struct Suggestions {
    waiting: Option<(i32, String)>,
    last: Option<Instant>,
}

impl Suggestions {
    pub fn request(&mut self, id: i32, text: String) {
        self.waiting = Some((id, text));
    }

    /// When the waiting request may be answered, if one is waiting.
    pub fn due(&self) -> Option<Instant> {
        self.waiting
            .as_ref()
            .map(|_| self.last.map_or_else(Instant::now, |last| last + TICK))
    }

    /// The request to answer now, if one is waiting and its turn has come.
    pub fn take(&mut self, now: Instant) -> Option<(i32, String)> {
        if self.last.is_some_and(|last| now < last + TICK) {
            return None;
        }
        let request = self.waiting.take()?;
        self.last = Some(now);
        Some(request)
    }
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
    fn ten_messages_at_once_are_spam_but_a_tick_apart_they_wear_off() {
        // Vanilla counts first and kicks once the count reaches 200.
        let t0 = Instant::now();
        let mut s = Throttle::chat();
        for _ in 0..9 {
            assert!(!s.said(t0));
        }
        assert!(s.said(t0));
        // A message a second never piles up.
        let mut s = Throttle::chat();
        for i in 0..100 {
            assert!(!s.said(t0 + Duration::from_secs(i)));
        }
        // Gaps shorter than a tick still add up to ticks.
        let mut s = Throttle::chat();
        s.said(t0);
        s.decay(t0 + Duration::from_millis(30));
        s.decay(t0 + Duration::from_millis(60));
        assert_eq!(s.count, 19);
    }

    #[test]
    fn creative_drops_stop_at_vanilla_threshold() {
        let t0 = Instant::now();
        let mut d = Throttle::creative_drops();
        // 1480 / 20 = 74 drops at once, then nothing until it wears off.
        assert_eq!((0..200).filter(|_| d.allow(t0)).count(), 74);
        // Then one a second.
        assert!(d.allow(t0 + TICK));
        assert!(!d.allow(t0 + TICK * 20));
        assert!(d.allow(t0 + TICK * 21));
    }

    #[test]
    fn chat_characters_vanilla_refuses() {
        assert!(!illegal_chat("hello, world! ünïcode ✓ 漢字"));
        for bad in ["§cred", "tab\there", "line\nbreak", "del\u{7f}", "\0"] {
            assert!(illegal_chat(bad), "{bad:?}");
        }
    }

    #[test]
    fn moves_that_are_not_numbers() {
        assert!(!invalid_move(Some([1.0, 2.0, 3.0]), Some([90.0, 0.0])));
        // Infinite positions are clamped by vanilla, not kicked for.
        assert!(!invalid_move(Some([f64::INFINITY, 0.0, 0.0]), None));
        assert!(invalid_move(Some([0.0, f64::NAN, 0.0]), None));
        assert!(invalid_move(None, Some([f32::INFINITY, 0.0])));
        assert!(invalid_move(None, Some([0.0, f32::NAN])));
    }

    #[test]
    fn one_suggestion_a_tick_and_the_newest_wins() {
        let t0 = Instant::now();
        let mut s = Suggestions::default();
        assert_eq!(s.due(), None);
        s.request(1, "/a".into());
        assert_eq!(s.take(t0), Some((1, "/a".into())));
        // Three more in the same tick: only the last is answered, a tick on.
        for (id, text) in [(2, "/b"), (3, "/c"), (4, "/d")] {
            s.request(id, text.into());
            assert_eq!(s.take(t0), None);
        }
        assert_eq!(s.due(), Some(t0 + TICK));
        assert_eq!(s.take(t0 + TICK), Some((4, "/d".into())));
        assert_eq!(s.take(t0 + TICK * 5), None);
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
