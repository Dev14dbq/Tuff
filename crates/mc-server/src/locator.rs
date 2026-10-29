//! The locator bar (minecraft.wiki, "Locator Bar"). Other players in the
//! same dimension are dots on it, unless `locator_bar` is off, they are
//! spectating or dead, or they stand farther than the two can send and
//! receive (`waypoint_transmit_range`, `waypoint_receive_range`). A colour
//! or style from `/waypoint` replaces the team colour.

use std::collections::HashMap;

use mc_protocol::packets::play::Waypoint;

use crate::commands::gear;
use crate::context::{Pose, ServerContext};
use crate::scoreboard::COLORS;
use crate::tick::lock;

const STYLE: &str = "minecraft:default";
const FAR: f64 = 60_000_000.0;

/// What the viewer should be told about `other`, and the block we remember
/// so a step inside the same block is not sent again. `force` resends a
/// dot whose block did not change (a new colour).
pub fn offer(
    ctx: &ServerContext,
    viewer: u128,
    other: u128,
    pose: Pose,
    bars: &mut HashMap<u128, [i32; 3]>,
    force: bool,
) -> Option<Waypoint> {
    if viewer == other {
        return None;
    }
    let block = [
        pose.x.floor() as i32,
        pose.y.floor() as i32,
        pose.z.floor() as i32,
    ];
    if !shown(ctx, viewer, other, pose) {
        return forget(other, bars);
    }
    let known = bars.get(&other).copied();
    let operation = match known {
        None => 0,
        Some(at) if !force && at == block => return None,
        Some(_) => 2,
    };
    bars.insert(other, block);
    Some(mark(ctx, other, operation, Some(block)))
}

/// Drop a dot the viewer was shown.
pub fn forget(other: u128, bars: &mut HashMap<u128, [i32; 3]>) -> Option<Waypoint> {
    bars.remove(&other).map(|_| Waypoint {
        operation: 1,
        uuid: other,
        style: STYLE.to_owned(),
        color: None,
        pos: None,
    })
}

fn shown(ctx: &ServerContext, viewer: u128, other: u128, pose: Pose) -> bool {
    if !ctx.game_rule_bool("locator_bar") {
        return false;
    }
    let viewer_at = {
        let players = lock(&ctx.players);
        let Some(them) = players.get(&other) else {
            return false;
        };
        if them.game_mode == 3 || them.dimension != ctx.world.dimension {
            return false;
        }
        players.get(&viewer).and_then(|p| p.pose)
    };
    if lock(&ctx.vitals).get(&other).is_some_and(|v| v.dead) {
        return false;
    }
    let Some(mine) = viewer_at else {
        return false;
    };
    let reach = gear::number(ctx, other, "waypoint_transmit_range").min(gear::number(
        ctx,
        viewer,
        "waypoint_receive_range",
    ));
    if reach <= 0.0 {
        return false;
    }
    let (dx, dy, dz) = (pose.x - mine.x, pose.y - mine.y, pose.z - mine.z);
    dx * dx + dy * dy + dz * dz <= reach * reach || reach >= FAR
}

fn mark(ctx: &ServerContext, other: u128, operation: i32, pos: Option<[i32; 3]>) -> Waypoint {
    let (color, style) = icon(ctx, other);
    Waypoint {
        operation,
        uuid: other,
        style,
        color,
        pos,
    }
}

fn icon(ctx: &ServerContext, uuid: u128) -> (Option<[u8; 3]>, String) {
    let (set_color, set_style) = lock(&ctx.rare).waypoint(uuid);
    let style = set_style
        .map(|s| {
            if s.contains(':') {
                s
            } else {
                format!("minecraft:{s}")
            }
        })
        .unwrap_or_else(|| STYLE.to_owned());
    if let Some(c) = set_color {
        return (parse_color(&c), style);
    }
    let name = lock(&ctx.players)
        .get(&uuid)
        .map(|p| p.profile.name.clone())
        .unwrap_or_default();
    let team = lock(&ctx.scoreboard)
        .team_of(&name)
        .and_then(|t| t.color)
        .and_then(|i| COLORS.get(i))
        .copied();
    (team.and_then(parse_color), style)
}

fn parse_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#').unwrap_or(match text {
        "black" => "000000",
        "dark_blue" => "0000AA",
        "dark_green" => "00AA00",
        "dark_aqua" => "00AAAA",
        "dark_red" => "AA0000",
        "dark_purple" => "AA00AA",
        "gold" => "FFAA00",
        "gray" => "AAAAAA",
        "dark_gray" => "555555",
        "blue" => "5555FF",
        "green" => "55FF55",
        "aqua" => "55FFFF",
        "red" => "FF5555",
        "light_purple" => "FF55FF",
        "yellow" => "FFFF55",
        "white" => "FFFFFF",
        _ => return None,
    });
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Pose;
    use crate::rules::Value;
    use crate::testkit::{flat, join};
    use mc_protocol::Encode;

    fn pose_at(x: f64, z: f64) -> Pose {
        Pose {
            x,
            y: -60.0,
            z,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        }
    }

    #[test]
    fn a_player_is_tracked_until_the_bar_is_off() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, 1, 10, [0.5, -60.0, 0.5], 0);
        join(&ctx, 2, 11, [4.5, -60.0, 4.5], 0);
        let mut bars = HashMap::new();
        let first = offer(&ctx, 1, 2, pose_at(4.5, 4.5), &mut bars, false).expect("track");
        assert_eq!(first.operation, 0);
        assert_eq!(bars.get(&2), Some(&[4, -60, 4]));
        let mut bytes = Vec::new();
        first.encode(&mut bytes);
        assert_eq!(bytes[0], 0, "track");
        assert!(offer(&ctx, 1, 2, pose_at(4.5, 4.5), &mut bars, false).is_none());
        let moved = offer(&ctx, 1, 2, pose_at(6.5, 4.5), &mut bars, false).expect("update");
        assert_eq!(moved.operation, 2);

        lock(&ctx.game_rules).set("locator_bar", Value::Bool(false));
        let gone = offer(&ctx, 1, 2, pose_at(6.5, 4.5), &mut bars, false).expect("untrack");
        assert_eq!(gone.operation, 1);
        assert!(bars.is_empty());
    }

    #[test]
    fn a_spectator_is_not_on_the_bar() {
        let Some(ctx) = flat() else {
            return;
        };
        join(&ctx, 1, 10, [0.5, -60.0, 0.5], 0);
        join(&ctx, 2, 11, [4.5, -60.0, 4.5], 3);
        let mut bars = HashMap::new();
        assert!(offer(&ctx, 1, 2, pose_at(4.5, 4.5), &mut bars, false).is_none());
    }
}
