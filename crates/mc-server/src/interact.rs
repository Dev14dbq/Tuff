//! A player's hands: digging and placing blocks, and what to answer.
//!
//! Observed from vanilla 26.3 (`cargo xtask blocks`), creative mode:
//! - accepted placement: the placer gets Block Update for the clicked block and
//!   the new block(s), then Acknowledge Block Change; others get the update and
//!   two arm swings;
//! - refused placement (occupied, inside a player): the placer gets the real
//!   states of the clicked block and the one next to it, then the ack;
//! - out of reach: only the ack;
//! - breaking: the breaker gets the update and the ack; others get the World
//!   Event "block broken" (particles, sound), then the update.

use mc_protocol::Position;
use mc_protocol::packets::play::{ItemStack, dig};

use crate::blocks::{self, Placement, offset};
use crate::context::{Event, Pose, ServerContext};

/// How far a player reaches, from the eyes to the block centre. The protocol
/// documentation gives 6 for digging.
const REACH: f64 = 6.0;
const EYE_HEIGHT: f64 = 1.62;
const PLAYER_WIDTH: f64 = 0.6;
const PLAYER_HEIGHT: f64 = 1.8;
const CREATIVE: i32 = 1;

/// Inventory slots as the client numbers them: 36..45 hotbar, 45 off hand.
pub struct Hands {
    pub slots: Vec<Option<ItemStack>>,
    /// Selected hotbar slot, 0..9.
    pub selected: usize,
}

impl Default for Hands {
    fn default() -> Self {
        Self {
            slots: vec![None; 46],
            selected: 0,
        }
    }
}

impl Hands {
    pub fn main_hand(&self) -> Option<&ItemStack> {
        self.slots[36 + self.selected].as_ref()
    }

    /// 0 main hand, 1 off hand.
    pub fn held(&self, hand: i32) -> Option<&ItemStack> {
        if hand == 1 {
            self.slots[45].as_ref()
        } else {
            self.main_hand()
        }
    }
}

/// What to send back to the player and what everyone else should hear.
#[derive(Debug, Default)]
pub struct Outcome {
    pub updates: Vec<(Position, u32)>,
    pub ack: Option<i32>,
    pub events: Vec<Event>,
}

/// Who is acting.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
    pub uuid: u128,
    pub entity_id: i32,
    pub game_mode: i32,
    pub pose: Pose,
}

fn in_reach(pose: &Pose, p: Position) -> bool {
    let (dx, dy, dz) = (
        f64::from(p.x) + 0.5 - pose.x,
        f64::from(p.y) + 0.5 - (pose.y + EYE_HEIGHT),
        f64::from(p.z) + 0.5 - pose.z,
    );
    dx * dx + dy * dy + dz * dz <= REACH * REACH
}

fn intersects(pose: &Pose, p: Position) -> bool {
    let half = PLAYER_WIDTH / 2.0;
    let (bx, by, bz) = (f64::from(p.x), f64::from(p.y), f64::from(p.z));
    pose.x - half < bx + 1.0
        && pose.x + half > bx
        && pose.y < by + 1.0
        && pose.y + PLAYER_HEIGHT > by
        && pose.z - half < bz + 1.0
        && pose.z + half > bz
}

/// Player Action. Creative players break on "start digging"; anyone on
/// "finish digging" (dig times are not checked yet).
pub fn dig(
    ctx: &ServerContext,
    actor: Actor,
    status: i32,
    position: Position,
    sequence: i32,
) -> Outcome {
    let breaks = match status {
        dig::START => actor.game_mode == CREATIVE,
        dig::FINISH => true,
        _ => return Outcome::default(),
    };
    let mut outcome = Outcome {
        ack: Some(sequence),
        ..Outcome::default()
    };
    if !breaks || !in_reach(&actor.pose, position) {
        return outcome;
    }
    let (world, blocks) = (&ctx.world, &ctx.data.blocks);
    let Some(state) = world.block(position, blocks) else {
        return outcome;
    };
    if blocks.is_air(state) {
        return outcome;
    }
    let block_at = |p: Position| world.block(p, blocks);
    let mut changes = Vec::new();
    for (p, new) in blocks::break_block(&ctx.data, &ctx.rules, position, state, &block_at) {
        if world.set_block(p, new, blocks, &ctx.rules).is_some() {
            changes.push((p, new));
        }
    }
    if changes.is_empty() {
        outcome.updates.push((position, state));
        return outcome;
    }
    outcome.updates.clone_from(&changes);
    outcome.events.push(Event::BlockBroken {
        by: actor.uuid,
        position,
        state,
    });
    outcome.events.push(Event::BlocksChanged {
        by: actor.uuid,
        changes,
    });
    outcome
}

/// Use Item On: places the held block item, if it fits.
#[allow(clippy::too_many_arguments)]
pub fn use_item_on(
    ctx: &ServerContext,
    actor: Actor,
    hands: &Hands,
    hand: i32,
    clicked: Position,
    face: i32,
    cursor: [f32; 3],
    sequence: i32,
) -> Outcome {
    let mut outcome = Outcome {
        ack: Some(sequence),
        ..Outcome::default()
    };
    let Ok(face) = usize::try_from(face) else {
        return outcome;
    };
    if face >= 6 || !in_reach(&actor.pose, clicked) {
        return outcome;
    }
    let (world, blocks) = (&ctx.world, &ctx.data.blocks);
    let block_at = |p: Position| world.block(p, blocks);
    // The client predicted something at both; tell it what is really there.
    let refuse = |mut outcome: Outcome| {
        for p in [clicked, offset(clicked, face)] {
            if let Some(state) = block_at(p) {
                outcome.updates.push((p, state));
            }
        }
        outcome
    };
    let Some(block) = hands
        .held(hand)
        .and_then(|item| ctx.rules.block_for_item(item.item))
    else {
        return refuse(outcome);
    };
    let placement = Placement {
        clicked,
        face,
        cursor,
        yaw: actor.pose.yaw,
        pitch: actor.pose.pitch,
    };
    let Some(placed) = blocks::place(&ctx.data, &ctx.rules, block, placement, &block_at) else {
        return refuse(outcome);
    };

    // A solid block may not go where a player stands.
    let others: Vec<Pose> = ctx
        .players
        .lock()
        .expect("players lock")
        .values()
        .filter(|p| p.profile.uuid != actor.uuid)
        .filter_map(|p| p.pose)
        .collect();
    let blocked = placed.blocks.iter().any(|&(p, state)| {
        ctx.rules.blocks_motion(state)
            && std::iter::once(&actor.pose)
                .chain(&others)
                .any(|pose| intersects(pose, p))
    });
    if blocked {
        return refuse(outcome);
    }

    let mut changes = Vec::new();
    for &(p, state) in &placed.blocks {
        if world.set_block(p, state, blocks, &ctx.rules).is_some() {
            changes.push((p, state));
        }
    }
    if changes.is_empty() {
        return refuse(outcome);
    }
    if let Some(state) = block_at(clicked)
        && !changes.iter().any(|&(p, _)| p == clicked)
    {
        outcome.updates.push((clicked, state));
    }
    outcome.updates.extend_from_slice(&changes);
    outcome.events.push(Event::BlocksChanged {
        by: actor.uuid,
        changes,
    });
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 2,
    });
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(x: f64, y: f64, z: f64) -> Pose {
        Pose {
            x,
            y,
            z,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        }
    }

    #[test]
    fn player_box() {
        let me = pose(-9.5, -60.0, 10.5);
        assert!(intersects(&me, Position::new(-10, -60, 10)));
        assert!(intersects(&me, Position::new(-10, -59, 10)));
        assert!(!intersects(&me, Position::new(-10, -61, 10)));
        assert!(!intersects(&me, Position::new(-8, -60, 10)));
        assert!(!intersects(&me, Position::new(-10, -58, 10)));
    }

    #[test]
    fn reach() {
        let me = pose(0.5, 0.0, 0.5);
        assert!(in_reach(&me, Position::new(2, -1, 0)));
        assert!(in_reach(&me, Position::new(5, 1, 0)));
        assert!(!in_reach(&me, Position::new(30, -1, 0)));
    }
}
