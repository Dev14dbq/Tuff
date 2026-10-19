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
use mc_protocol::packets::play::{HashedStack, ItemStack, dig};

use crate::blocks::{self, Placement, offset};
use crate::context::{Event, Pose, ServerContext};
use crate::level::{WorldLevel, to_pos};

/// How far a player reaches, from the eyes to the block centre. The protocol
/// documentation gives 6 for digging.
const REACH: f64 = 6.0;
const EYE_HEIGHT: f64 = 1.62;
const PLAYER_WIDTH: f64 = 0.6;
const PLAYER_HEIGHT: f64 = 1.8;
const CREATIVE: i32 = 1;
const SPECTATOR: i32 = 3;

/// Inventory slots as the client numbers them: 36..45 hotbar, 45 off hand.
#[derive(Clone, Debug)]
pub struct Hands {
    pub slots: Vec<Option<ItemStack>>,
    /// Selected hotbar slot, 0..9.
    pub selected: usize,
    /// Component data of stacks loaded from the player file, kept until the
    /// slot changes (the network form does not carry it yet).
    pub components: std::collections::HashMap<usize, mc_nbt::Tag>,
    /// The stack on the mouse cursor while the inventory is open.
    pub carried: Option<ItemStack>,
}

impl Default for Hands {
    fn default() -> Self {
        Self {
            slots: vec![None; 46],
            selected: 0,
            components: std::collections::HashMap::new(),
            carried: None,
        }
    }
}

impl Hands {
    pub fn main_hand(&self) -> Option<&ItemStack> {
        self.slots[36 + self.selected].as_ref()
    }

    /// A click in the inventory window, as the client reports its result
    /// (the server takes the client's word for now). Returns stacks thrown
    /// out of the window: Q over a slot (mode 4) or a click outside it
    /// (slot −999).
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        mode: i32,
        changed: &[(i16, Option<HashedStack>)],
        carried: Option<HashedStack>,
    ) -> Vec<ItemStack> {
        let mut thrown = Vec::new();
        if mode == 4
            && let Some(Some(stack)) = usize::try_from(slot).ok().and_then(|s| self.slots.get(s))
        {
            let count = if button == 1 { stack.count } else { 1 };
            thrown.push(ItemStack {
                count,
                ..stack.clone()
            });
        }
        if mode == 0
            && slot == -999
            && let Some(stack) = &self.carried
        {
            let count = if button == 0 { stack.count } else { 1 };
            thrown.push(ItemStack {
                count,
                ..stack.clone()
            });
        }
        let to_stack = |h: HashedStack| ItemStack {
            count: h.count,
            item: h.item,
            has_component_changes: h.has_component_changes,
        };
        for (slot, stack) in changed {
            let Some(index) = usize::try_from(*slot)
                .ok()
                .filter(|&i| i < self.slots.len())
            else {
                continue;
            };
            let new = stack.clone().map(to_stack);
            if new.as_ref().map(|s| s.item) != self.slots[index].as_ref().map(|s| s.item) {
                self.components.remove(&index);
            }
            self.slots[index] = new;
        }
        self.carried = carried.map(to_stack);
        thrown
    }

    /// Container slot of a hand.
    pub fn hand_slot(&self, hand: i32) -> usize {
        if hand == 1 { 45 } else { 36 + self.selected }
    }

    /// Puts items in: first onto matching stacks, then into empty slots
    /// (hotbar first, then the main inventory). Returns how many went in and
    /// the container slots that changed.
    pub fn insert(&mut self, item: i32, mut count: i32, max: i32) -> (i32, Vec<usize>) {
        let order: Vec<usize> = (36..45).chain(9..36).collect();
        let mut changed = Vec::new();
        let start = count;
        for &slot in &order {
            if count == 0 {
                break;
            }
            if let Some(stack) = &mut self.slots[slot]
                && stack.item == item
                && !stack.has_component_changes
                && stack.count < max
            {
                let n = (max - stack.count).min(count);
                stack.count += n;
                count -= n;
                changed.push(slot);
            }
        }
        for &slot in &order {
            if count == 0 {
                break;
            }
            if self.slots[slot].is_none() {
                self.components.remove(&slot);
                let n = max.min(count);
                self.slots[slot] = Some(ItemStack {
                    count: n,
                    item,
                    has_component_changes: false,
                });
                count -= n;
                changed.push(slot);
            }
        }
        (start - count, changed)
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
    /// Lines of text for the player (command feedback).
    pub messages: Vec<mc_nbt::Tag>,
    /// Items to drop into the world, at block positions.
    pub drops: Vec<(Position, crate::loot::Drop)>,
    /// A container the player opened.
    pub open: Option<(Position, crate::containers::Kind)>,
    /// The player opened a crafting table.
    pub open_crafting: bool,
}

/// Who is acting.
#[derive(Clone, Copy, Debug)]
pub struct Actor {
    pub uuid: u128,
    pub entity_id: i32,
    pub game_mode: i32,
    pub pose: Pose,
    /// Sneaking players place blocks against containers and levers instead
    /// of using them.
    pub sneaking: bool,
}

impl Actor {
    /// The server console: nobody in the world.
    pub fn console() -> Self {
        Self {
            uuid: 0,
            entity_id: 0,
            game_mode: CREATIVE,
            sneaking: false,
            pose: Pose {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
                on_ground: true,
            },
        }
    }
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
    let mut outcome = Outcome {
        ack: Some(sequence),
        ..Outcome::default()
    };
    let (world, blocks) = (&ctx.world, &ctx.data.blocks);
    let Some(state) = world.block(position, blocks) else {
        return outcome;
    };
    // Spectators do not dig; creative breaks at once; survival breaks when
    // the client says it finished, or at once for blocks that break
    // instantly (dig times are not checked yet).
    let breaks = match (status, actor.game_mode) {
        (_, SPECTATOR) => false,
        (dig::START, CREATIVE) => true,
        (dig::START, _) => blocks::breaks_instantly(&ctx.data, &ctx.rules, state),
        (dig::FINISH, mode) => mode != CREATIVE,
        _ => return Outcome::default(),
    };
    if !breaks || !in_reach(&actor.pose, position) {
        return outcome;
    }
    if blocks.is_air(state) {
        return outcome;
    }
    let block_at = |p: Position| world.block(p, blocks);
    // The other half of a door or bed goes with the shape updates.
    let left = blocks::break_block(&ctx.data, &ctx.rules, position, state, &block_at)[0].1;
    let mut level = WorldLevel::new(ctx);
    if !level.set_block(position, left) {
        outcome.updates.push((position, state));
        return outcome;
    }
    outcome.updates.push((position, left));
    if blocks
        .block_name(state)
        .and_then(crate::containers::kind_of)
        .is_some()
    {
        let (items, _) = crate::containers::broken(ctx, position);
        for stack in items {
            outcome.drops.push((
                position,
                crate::loot::Drop {
                    item: stack.item,
                    count: stack.count,
                },
            ));
        }
        outcome.events.push(Event::ContainerGone { position });
    }
    if actor.game_mode != CREATIVE
        && let Some(name) = blocks.block_name(state)
    {
        let props = |k: &str| blocks.property(state, k).map(str::to_owned);
        for drop in ctx.loot.drops(name, &props) {
            outcome.drops.push((position, drop));
        }
    }
    outcome.events.push(Event::BlockBroken {
        by: actor.uuid,
        position,
        state,
    });
    // Everyone gets every change, the breaker included (vanilla sends the
    // broken block to the breaker twice: as the answer and with the rest).
    outcome.events.push(Event::BlocksChanged {
        by: 0,
        changes: level.into_changes(),
    });
    outcome
}

/// Use Item On: places the held block item, if it fits.
pub fn use_item_on(
    ctx: &ServerContext,
    actor: Actor,
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
    if face >= 6 || !in_reach(&actor.pose, clicked) || actor.game_mode == SPECTATOR {
        return outcome;
    }
    let held: Option<ItemStack> = ctx
        .inventories
        .lock()
        .expect("inventories lock")
        .get(&actor.uuid)
        .and_then(|h| h.held(hand).cloned());
    let held = held.as_ref();
    let (world, blocks) = (&ctx.world, &ctx.data.blocks);
    let block_at = |p: Position| world.block(p, blocks);
    let use_blocks = !(actor.sneaking && held.is_some());
    // Crafting tables open their grid.
    if use_blocks
        && block_at(clicked).and_then(|s| blocks.block_name(s)) == Some("minecraft:crafting_table")
    {
        outcome.open_crafting = true;
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
    // Containers open.
    if use_blocks
        && let Some(kind) = block_at(clicked)
            .and_then(|s| blocks.block_name(s))
            .and_then(crate::containers::kind_of)
    {
        outcome.open = Some((clicked, kind));
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
    // Levers, doors, repeaters... react to being used instead of having
    // something placed against them.
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    if use_blocks && mc_tick::use_block(&mut level, logic, to_pos(clicked)) {
        if let Some(state) = block_at(clicked) {
            outcome.updates.push((clicked, state));
        }
        outcome.events.push(Event::BlocksChanged {
            by: 0,
            changes: level.into_changes(),
        });
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
    drop(level);
    // The client predicted something at both; tell it what is really there.
    let refuse = |mut outcome: Outcome| {
        for p in [clicked, offset(clicked, face)] {
            if let Some(state) = block_at(p) {
                outcome.updates.push((p, state));
            }
        }
        outcome
    };
    let Some(block) = held.and_then(|item| ctx.rules.block_for_item(item.item)) else {
        return refuse(outcome);
    };
    let placement = Placement {
        clicked,
        face,
        cursor,
        yaw: actor.pose.yaw,
        pitch: actor.pose.pitch,
        sneaking: actor.sneaking,
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

    let mut level = WorldLevel::new(ctx);
    // Nothing to hold it up there: not placed.
    let unsupported = placed
        .blocks
        .iter()
        .any(|&(p, state)| !mc_tick::support::can_survive(&level, level.logic(), to_pos(p), state));
    if unsupported {
        return refuse(outcome);
    }
    let mut placed_any = false;
    for &(p, state) in &placed.blocks {
        // Connections and shape from the neighbours, as the block is placed.
        let state = mc_tick::placement_state(&level, level.logic(), to_pos(p), state);
        let state = mc_tick::redstone::player_placement(&level, level.logic(), to_pos(p), state);
        placed_any |= level.set_block(p, state);
    }
    if !placed_any {
        return refuse(outcome);
    }
    let changes = level.into_changes();
    // Containers get their (empty) block entity.
    for &(p, state) in &placed.blocks {
        crate::containers::placed(ctx, p, state);
    }
    // The answer: the clicked block, then the placed ones.
    for p in std::iter::once(clicked).chain(placed.blocks.iter().map(|&(p, _)| p)) {
        if let Some(state) = block_at(p)
            && !outcome.updates.iter().any(|&(q, _)| q == p)
        {
            outcome.updates.push((p, state));
        }
    }
    outcome.events.push(Event::BlocksChanged { by: 0, changes });
    // Survival and adventure use up the block.
    if actor.game_mode != CREATIVE {
        let mut inventories = ctx.inventories.lock().expect("inventories lock");
        if let Some(hands) = inventories.get_mut(&actor.uuid) {
            let slot = hands.hand_slot(hand);
            if let Some(stack) = &mut hands.slots[slot] {
                stack.count -= 1;
                if stack.count <= 0 {
                    hands.slots[slot] = None;
                    hands.components.remove(&slot);
                }
            }
            outcome.events.push(Event::InventorySlot {
                uuid: actor.uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
    }
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
