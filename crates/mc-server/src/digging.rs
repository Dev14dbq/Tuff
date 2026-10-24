//! How long survival players take to break blocks, and the check that a
//! "finished digging" comes after that long (minecraft.wiki "Breaking").
//!
//! The client says when it is done; the server keeps what it saw when the
//! digging started and refuses a finish that comes too early, as vanilla
//! does: the progress made so far must pass 70%.

use mc_protocol::Position;
use serde_json::Value;

use crate::context::{Pose, ServerContext};
use crate::interact::Actor;
use crate::tick::lock;

const EYE_HEIGHT: f64 = 1.62;
/// Progress a finish must have reached (the progress of the ticks since the
/// start, counting the tick of the finish itself).
const NEEDED: f64 = 0.7;

/// A block a player started digging.
#[derive(Clone, Copy, Debug)]
pub struct Dig {
    pub position: Position,
    /// World age when the digging started.
    pub started: i64,
    /// Hotbar/inventory slot and item held then (0 for an empty hand).
    pub slot: usize,
    pub item: i32,
}

/// Everything that goes into how fast a block breaks.
#[derive(Clone, Copy, Debug)]
pub struct Factors {
    pub hardness: f32,
    /// The tool's speed on the block (1 for a hand).
    pub tool_speed: f64,
    /// The `mining_efficiency` attribute, which counts if the tool is faster
    /// than a hand.
    pub efficiency: f64,
    /// Haste or conduit power level, mining fatigue level.
    pub haste: i32,
    pub fatigue: i32,
    /// The `block_break_speed` attribute.
    pub break_speed: f64,
    /// The `submerged_mining_speed` attribute if the eyes are in water.
    pub submerged: Option<f64>,
    pub on_ground: bool,
    /// The block gives its drops (and breaks at the fast rate) with this tool.
    pub right_tool: bool,
}

/// How much of the block one tick of digging breaks (1 or more: at once).
pub fn progress_per_tick(f: &Factors) -> f64 {
    if f.hardness < 0.0 {
        return 0.0;
    }
    let mut speed = f.tool_speed;
    if speed > 1.0 {
        speed += f.efficiency;
    }
    if f.haste > 0 {
        speed *= 1.0 + 0.2 * f64::from(f.haste);
    }
    if f.fatigue > 0 {
        speed *= 0.3_f64.powi(f.fatigue.min(4));
    }
    speed *= f.break_speed;
    if let Some(under) = f.submerged {
        speed *= under;
    }
    if !f.on_ground {
        speed /= 5.0;
    }
    if f.hardness == 0.0 {
        return 1.0;
    }
    speed / f64::from(f.hardness) / if f.right_tool { 30.0 } else { 100.0 }
}

/// Ticks of digging a block takes (0: at once); `None` if it never breaks.
pub fn ticks_to_break(f: &Factors) -> Option<u32> {
    let p = progress_per_tick(f);
    if p >= 1.0 {
        Some(0)
    } else if p <= 0.0 {
        None
    } else {
        Some((1.0 / p).ceil() as u32)
    }
}

/// Whether a finish `ticks` after the start comes late enough.
pub fn finished_in_time(per_tick: f64, ticks: i64) -> bool {
    per_tick * (ticks.max(0) + 1) as f64 > NEEDED
}

fn attribute(ctx: &ServerContext, uuid: u128, name: &str) -> f64 {
    let Some(snap) = crate::commands::gear::snapshot(ctx, uuid, name) else {
        return 0.0;
    };
    let sum = |op: i32| -> f64 {
        snap.modifiers
            .iter()
            .filter(|m| m.operation == op)
            .map(|m| m.amount)
            .sum()
    };
    let add = snap.base + sum(0);
    let mut v = add + snap.base * sum(1);
    for m in snap.modifiers.iter().filter(|m| m.operation == 2) {
        v *= 1.0 + m.amount;
    }
    v
}

/// The tool component's rule for the block: its speed and whether it is the
/// right tool for drops.
fn tool_rule(ctx: &ServerContext, item: i32, block: &str) -> (f64, Option<bool>) {
    let Some(tool) = ctx
        .items
        .get(item)
        .and_then(|i| i.defaults.get("minecraft:tool"))
    else {
        return (1.0, None);
    };
    let default = tool["default_mining_speed"].as_f64().unwrap_or(1.0);
    let (mut speed, mut correct) = (None, None);
    for rule in tool["rules"].as_array().into_iter().flatten() {
        if !crate::enchant_effects::is(ctx, "minecraft:block", &rule["blocks"], block) {
            continue;
        }
        if speed.is_none() {
            speed = rule["speed"].as_f64();
        }
        if correct.is_none() {
            correct = rule["correct_for_drops"].as_bool();
        }
    }
    (speed.unwrap_or(default), correct)
}

fn level(ctx: &ServerContext, uuid: u128, names: &[&str]) -> i32 {
    lock(&ctx.vitals).get(&uuid).map_or(0, |v| {
        names
            .iter()
            .filter_map(|n| v.effects.active.get(*n))
            .map(|i| i.amplifier + 1)
            .max()
            .unwrap_or(0)
    })
}

fn held_item(ctx: &ServerContext, uuid: u128) -> (usize, i32) {
    crate::held::held(ctx, uuid, 0).map_or((0, 0), |h| (h.slot, h.stack.item))
}

/// What goes into breaking `state` for this player now; `None` for blocks
/// the table does not know.
pub fn factors(ctx: &ServerContext, uuid: u128, pose: &Pose, state: u32) -> Option<Factors> {
    let name = ctx.data.blocks.block_name(state)?;
    let hardness = crate::hardness::hardness(name)?;
    let (_, item) = held_item(ctx, uuid);
    let (tool_speed, correct) = tool_rule(ctx, item, name);
    let is = |tag: &str| {
        crate::enchant_effects::is(
            ctx,
            "minecraft:block",
            &Value::String(format!("#minecraft:{tag}")),
            name,
        )
    };
    let needs = crate::hardness::needs_tool(
        name,
        is("needs_stone_tool") || is("needs_iron_tool") || is("needs_diamond_tool"),
        is("mineable/pickaxe"),
    );
    let in_water = crate::hits::in_water(ctx, [pose.x, pose.y + EYE_HEIGHT, pose.z]);
    Some(Factors {
        hardness,
        tool_speed,
        efficiency: attribute(ctx, uuid, "mining_efficiency"),
        haste: level(ctx, uuid, &["haste", "conduit_power"]),
        fatigue: level(ctx, uuid, &["mining_fatigue"]),
        break_speed: attribute(ctx, uuid, "block_break_speed"),
        submerged: in_water.then(|| attribute(ctx, uuid, "submerged_mining_speed")),
        on_ground: pose.on_ground,
        right_tool: !needs || correct == Some(true),
    })
}

/// A survival player starts digging: remember where and when.
pub fn start(ctx: &ServerContext, actor: &Actor, position: Position) {
    let (slot, item) = held_item(ctx, actor.uuid);
    let started = lock(&ctx.time).age;
    lock(&ctx.digs).insert(
        actor.uuid,
        Dig {
            position,
            started,
            slot,
            item,
        },
    );
}

/// The digging stops (cancelled, finished, or the player left).
pub fn stop(ctx: &ServerContext, uuid: u128) {
    lock(&ctx.digs).remove(&uuid);
}

/// Whether a survival player's "finished digging" is believed. Blocks the
/// table does not know are.
pub fn may_finish(ctx: &ServerContext, actor: &Actor, position: Position, state: u32) -> bool {
    let Some(f) = factors(ctx, actor.uuid, &actor.pose, state) else {
        return true;
    };
    let dig = lock(&ctx.digs).get(&actor.uuid).copied();
    let Some(dig) = dig else {
        tracing::debug!("dig: finish at {position:?} without a start, refused");
        return false;
    };
    let (slot, item) = held_item(ctx, actor.uuid);
    let now = lock(&ctx.time).age;
    let per_tick = progress_per_tick(&f);
    let ok = dig.position == position
        && (dig.slot, dig.item) == (slot, item)
        && finished_in_time(per_tick, now - dig.started);
    if !ok {
        tracing::debug!(
            "dig: finish at {position:?} after {} ticks refused (needs {:?}, progress {per_tick:.4}/tick)",
            now - dig.started,
            ticks_to_break(&f),
        );
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hand(hardness: f32) -> Factors {
        Factors {
            hardness,
            tool_speed: 1.0,
            efficiency: 0.0,
            haste: 0,
            fatigue: 0,
            break_speed: 1.0,
            submerged: None,
            on_ground: true,
            right_tool: true,
        }
    }

    #[test]
    fn dirt_by_hand_takes_15_ticks() {
        // wiki: dirt, no tool, 0.75 s.
        assert_eq!(ticks_to_break(&hand(0.5)), Some(15));
    }

    #[test]
    fn stone_by_hand_is_slow_and_needs_the_tool() {
        let f = Factors {
            right_tool: false,
            ..hand(1.5)
        };
        // 1.5 * 100 = 150 ticks, 7.5 s.
        assert_eq!(ticks_to_break(&f), Some(150));
    }

    #[test]
    fn a_pickaxe_speeds_up_stone() {
        // wiki: stone with a wooden pickaxe 0.75 s, diamond 0.3 s, iron 0.4 s.
        let with = |speed| Factors {
            tool_speed: speed,
            ..hand(1.5)
        };
        assert_eq!(ticks_to_break(&with(2.0)), Some(23));
        assert_eq!(ticks_to_break(&with(6.0)), Some(8));
        assert_eq!(ticks_to_break(&with(8.0)), Some(6));
    }

    #[test]
    fn efficiency_haste_water_and_air_change_the_time() {
        let base = Factors {
            tool_speed: 6.0,
            ..hand(1.5)
        };
        let eff = Factors {
            efficiency: 26.0,
            ..base
        };
        // Iron-class pickaxe with Efficiency V: two ticks.
        assert_eq!(ticks_to_break(&eff), Some(2));
        let slow = |f: Factors| ticks_to_break(&f).unwrap();
        assert!(slow(Factors { haste: 2, ..base }) < slow(base));
        assert!(slow(Factors { fatigue: 1, ..base }) > slow(base));
        assert_eq!(
            slow(Factors {
                on_ground: false,
                ..base
            }),
            // five times the ticks, rounded up
            (5.0 * 1.5 * 30.0 / 6.0_f64).ceil() as u32
        );
        assert_eq!(
            slow(Factors {
                submerged: Some(0.2),
                ..base
            }),
            slow(Factors {
                on_ground: false,
                ..base
            })
        );
    }

    #[test]
    fn bedrock_never_breaks() {
        assert_eq!(ticks_to_break(&hand(-1.0)), None);
    }

    #[test]
    fn a_finish_needs_most_of_the_progress() {
        let per_tick = progress_per_tick(&Factors {
            right_tool: false,
            ..hand(1.5)
        });
        assert!(!finished_in_time(per_tick, 0));
        assert!(!finished_in_time(per_tick, 50));
        assert!(finished_in_time(per_tick, 105));
        assert!(finished_in_time(1.0, 0));
    }
}
