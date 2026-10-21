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
    /// (the server takes the client's word for now). The client describes
    /// stacks by component hashes, so their damage comes from the stacks
    /// known before the click: these slots, the cursor and `known` (an open
    /// window's). Returns stacks thrown out of the window: Q over a slot
    /// (mode 4) or a click outside it (slot −999).
    pub fn click(
        &mut self,
        slot: i16,
        button: i8,
        mode: i32,
        changed: &[(i16, Option<HashedStack>)],
        carried: Option<HashedStack>,
        known: &[ItemStack],
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
        let before: Vec<ItemStack> = self
            .slots
            .iter()
            .chain(std::iter::once(&self.carried))
            .flatten()
            .filter(|s| s.has_changes())
            .chain(known.iter().filter(|s| s.has_changes()))
            .cloned()
            .collect();
        for (slot, stack) in changed {
            let Some(index) = usize::try_from(*slot)
                .ok()
                .filter(|&i| i < self.slots.len())
            else {
                continue;
            };
            let new = stack.as_ref().map(|h| h.resolve(&before));
            if new.as_ref().map(|s| s.item) != self.slots[index].as_ref().map(|s| s.item) {
                self.components.remove(&index);
            }
            self.slots[index] = new;
        }
        self.carried = carried.map(|h| h.resolve(&before));
        thrown
    }

    /// Stacks with component changes, for resolving a window click's hashes.
    pub fn changed_stacks(&self) -> Vec<ItemStack> {
        self.slots
            .iter()
            .chain(std::iter::once(&self.carried))
            .flatten()
            .filter(|s| s.has_changes())
            .cloned()
            .collect()
    }

    /// Pick block (middle click) for `item`: selects it in the hotbar,
    /// brings it there from the inventory, or (in creative) makes one.
    /// Returns the container slots that changed; the selected slot may
    /// change too.
    pub fn pick(&mut self, item: i32, creative: bool) -> Vec<usize> {
        let holds = |s: &Option<ItemStack>| s.as_ref().is_some_and(|s| s.item == item);
        if let Some(i) = (36..45).find(|&i| holds(&self.slots[i])) {
            self.selected = i - 36;
            return Vec::new();
        }
        let target = 36 + self.suitable_hotbar_slot();
        if let Some(from) = (9..36).find(|&i| holds(&self.slots[i])) {
            self.slots.swap(from, target);
            let (a, b) = (
                self.components.remove(&from),
                self.components.remove(&target),
            );
            if let Some(c) = a {
                self.components.insert(target, c);
            }
            if let Some(c) = b {
                self.components.insert(from, c);
            }
            self.selected = target - 36;
            return vec![from, target];
        }
        if !creative {
            return Vec::new();
        }
        let mut changed = vec![target];
        // What was there moves into the inventory when there is room.
        if self.slots[target].is_some()
            && let Some(free) = (9..36).find(|&i| self.slots[i].is_none())
        {
            self.slots[free] = self.slots[target].take();
            if let Some(c) = self.components.remove(&target) {
                self.components.insert(free, c);
            }
            changed.push(free);
        }
        self.components.remove(&target);
        self.slots[target] = Some(ItemStack {
            count: 1,
            item,
            damage: 0,
            has_component_changes: false,
        });
        self.selected = target - 36;
        changed
    }

    /// Where a picked item goes: the first empty hotbar slot from the
    /// selected one on, else the first without enchantments, else the
    /// selected one.
    fn suitable_hotbar_slot(&self) -> usize {
        let at = |i: usize| (self.selected + i) % 9;
        if let Some(i) = (0..9).map(at).find(|&i| self.slots[36 + i].is_none()) {
            return i;
        }
        let enchanted = |slot: usize| match self.components.get(&slot) {
            Some(mc_nbt::Tag::Compound(c)) => c.get("minecraft:enchantments").is_some(),
            _ => false,
        };
        (0..9)
            .map(at)
            .find(|&i| !enchanted(36 + i))
            .unwrap_or(self.selected)
    }

    /// Container slot of a hand.
    pub fn hand_slot(&self, hand: i32) -> usize {
        if hand == 1 { 45 } else { 36 + self.selected }
    }

    /// Puts items in: first onto matching stacks, then into empty slots
    /// (hotbar first, then the main inventory). Returns how many went in and
    /// the container slots that changed.
    pub fn insert(&mut self, item: i32, count: i32, max: i32) -> (i32, Vec<usize>) {
        self.insert_stack(&ItemStack::new(item, count), max)
    }

    /// [`Hands::insert`] for a stack that may be damaged: it only joins
    /// stacks with the same damage.
    pub fn insert_stack(&mut self, stack: &ItemStack, max: i32) -> (i32, Vec<usize>) {
        let order: Vec<usize> = (36..45).chain(9..36).collect();
        let mut changed = Vec::new();
        let mut count = stack.count;
        if !stack.has_component_changes {
            for &slot in &order {
                if count == 0 {
                    break;
                }
                if let Some(s) = &mut self.slots[slot]
                    && s.item == stack.item
                    && s.damage == stack.damage
                    && !s.has_component_changes
                    && s.count < max
                {
                    let n = (max - s.count).min(count);
                    s.count += n;
                    count -= n;
                    changed.push(slot);
                }
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
                    ..stack.clone()
                });
                count -= n;
                changed.push(slot);
            }
        }
        (stack.count - count, changed)
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
    /// The player placed a sign: its editor opens.
    pub open_sign: Option<Position>,
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
    face: i8,
    sequence: i32,
) -> Outcome {
    let mut outcome = Outcome {
        ack: Some(sequence),
        ..Outcome::default()
    };
    if status == 5 {
        crate::projectiles::release(ctx, actor);
        return outcome;
    }
    let (world, blocks) = (&ctx.world, &ctx.data.blocks);
    // Hitting a block puts out fire on the face hit.
    if status == dig::START
        && actor.game_mode != SPECTATOR
        && let Ok(face @ 0..=5) = usize::try_from(face)
    {
        let at = offset(position, face);
        let fire = world.block(at, blocks).is_some_and(|s| {
            matches!(
                blocks.block_name(s),
                Some("minecraft:fire" | "minecraft:soul_fire")
            )
        });
        if fire {
            let mut level = WorldLevel::new(ctx);
            level.set_block(at, ctx.shapes.air());
            let changes = level.into_changes();
            outcome.updates.extend(changes.iter().copied());
            outcome.events.push(Event::BlocksChanged { by: 0, changes });
            return outcome;
        }
    }
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
    // Unstable TNT lights when it is broken (not in creative).
    if actor.game_mode != CREATIVE
        && blocks.block_name(state) == Some("minecraft:tnt")
        && blocks.property(state, "unstable") == Some("true")
        && ctx.game_rule_bool("minecraft:tnt_explodes")
    {
        let tnt =
            crate::explosion::primed(ctx, position, crate::explosion::TNT_FUSE, Some(actor.uuid));
        crate::explosion::primed_sound(ctx, &tnt);
        let _ = ctx.events.send(Event::EntitySpawned(tnt.clone()));
        ctx.entities.lock().expect("entities lock").push(tnt);
    }
    if blocks
        .block_name(state)
        .and_then(crate::containers::kind_of)
        .is_some()
    {
        if matches!(
            blocks.block_name(state),
            Some("minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker")
        ) && actor.game_mode != CREATIVE
        {
            let points = crate::furnaces::collect_xp(ctx, position);
            crate::xp::spawn(ctx, centre(position), points);
        }
        let (items, _) = crate::containers::broken(ctx, position);
        for stack in items {
            outcome.drops.push((
                position,
                crate::loot::Drop {
                    item: stack.item,
                    count: stack.count,
                    components: crate::items::damage_components(stack.damage),
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
        if !silk_touch(ctx, actor.uuid) {
            crate::xp::spawn(ctx, centre(position), crate::xp::block_xp(name));
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

fn centre(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

/// Whether the main hand holds a Silk Touch tool.
fn silk_touch(ctx: &ServerContext, uuid: u128) -> bool {
    let inventories = ctx.inventories.lock().expect("inventories lock");
    inventories.get(&uuid).is_some_and(|h| {
        let Some(mc_nbt::Tag::Compound(c)) = h.components.get(&(36 + h.selected)) else {
            return false;
        };
        let Some(mc_nbt::Tag::Compound(e)) = c.get("minecraft:enchantments") else {
            return false;
        };
        e.get("minecraft:silk_touch").is_some()
            || matches!(e.get("levels"), Some(mc_nbt::Tag::Compound(l)) if l.get("minecraft:silk_touch").is_some())
    })
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
    // Flint and steel and fire charges light fire.
    let held_name = held.and_then(|item| ctx.items.name(item.item));
    // An eye of ender goes into an end portal frame; the twelfth makes the
    // portal.
    if held_name == Some("minecraft:ender_eye")
        && let Some(state) = block_at(clicked)
        && blocks.block_name(state) == Some("minecraft:end_portal_frame")
        && blocks.property(state, "eye") == Some("false")
    {
        use mc_tick::Level as _;
        let mut level = WorldLevel::new(ctx);
        level.set(to_pos(clicked), ctx.shapes.with(state, "eye", "true"));
        crate::portals::light_end_portal(&mut level, to_pos(clicked));
        let changes = level.into_changes();
        outcome.updates.extend(changes.iter().copied());
        outcome.events.push(Event::BlocksChanged { by: 0, changes });
        if actor.game_mode != CREATIVE {
            replace_held(ctx, &mut outcome, actor, hand, None);
        }
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
    // Spawn eggs of villagers and wandering traders.
    if let Some(egg @ ("minecraft:villager_spawn_egg" | "minecraft:wandering_trader_spawn_egg")) =
        held_name
    {
        let at = offset(clicked, face);
        let kind = egg.trim_end_matches("_spawn_egg");
        if let Some(entity) = crate::trading::new_merchant(
            ctx,
            kind,
            [
                f64::from(at.x) + 0.5,
                f64::from(at.y),
                f64::from(at.z) + 0.5,
            ],
        ) {
            crate::trading::spawn(ctx, entity);
            if actor.game_mode != CREATIVE {
                replace_held(ctx, &mut outcome, actor, hand, None);
            }
        }
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return refuse(outcome);
    }
    if let Some(name) = held_name
        && use_blocks
        && (crate::decor::place(ctx, &mut outcome, actor, hand, name, clicked, face)
            || crate::vehicles::place_cart(ctx, &mut outcome, actor, hand, name, clicked)
            || crate::projectiles::use_on_block(
                ctx,
                &mut outcome,
                actor,
                hand,
                name,
                clicked,
                cursor,
            ))
    {
        return outcome;
    }
    if let Some(tool @ ("minecraft:flint_and_steel" | "minecraft:fire_charge")) = held_name {
        // TNT turns into primed TNT instead.
        let tnt = use_blocks
            && block_at(clicked).and_then(|s| blocks.block_name(s)) == Some("minecraft:tnt");
        let mut level = WorldLevel::new(ctx);
        if tnt && crate::explosion::ignite(ctx, &mut level, clicked, Some(actor.uuid)) {
            let changes = level.into_changes();
            outcome.updates.extend(changes.iter().copied());
            outcome.events.push(Event::BlocksChanged { by: 0, changes });
        } else {
            drop(level);
            if !light(ctx, &mut outcome, clicked, face) {
                return refuse(outcome);
            }
        }
        let pitch = if tool == "minecraft:fire_charge" {
            (crate::util::random_u64() >> 40) as f32 / (1u64 << 24) as f32 * 0.4 + 1.0
        } else {
            (crate::util::random_u64() >> 40) as f32 / (1u64 << 24) as f32 * 0.4 + 0.8
        };
        outcome.events.push(Event::WorldSound {
            pos: [
                f64::from(clicked.x) + 0.5,
                f64::from(clicked.y) + 0.5,
                f64::from(clicked.z) + 0.5,
            ],
            sound: if tool == "minecraft:fire_charge" {
                "minecraft:item.firecharge.use"
            } else {
                "minecraft:item.flintandsteel.use"
            },
            category: 4,
            volume: 1.0,
            pitch,
        });
        if tool == "minecraft:fire_charge" {
            if actor.game_mode != CREATIVE {
                replace_held(ctx, &mut outcome, actor, hand, None);
            }
        } else {
            wear_held(ctx, &mut outcome, actor, hand, 1);
        }
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
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
        .filter(|p| p.dimension == ctx.world.dimension)
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
        if let Some(id) = ctx.data.blocks.block_name(state).and_then(sign_entity) {
            ctx.world.set_block_entity(
                mc_world::BlockEntity {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    id: id.to_owned(),
                    data: blank_sign(),
                },
                &ctx.data.blocks,
            );
            outcome.open_sign = Some(p);
        }
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

/// Lights the clicked campfire or candle, or sets fire against `face`.
/// Returns whether anything caught.
fn light(ctx: &ServerContext, outcome: &mut Outcome, clicked: Position, face: usize) -> bool {
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    let state = ctx.world.block(clicked, &ctx.data.blocks).unwrap_or(0);
    let name = ctx.data.blocks.block_name(state).unwrap_or("");
    let lightable =
        (name.ends_with("campfire") || name.ends_with("candle") || name.ends_with("candle_cake"))
            && ctx.data.blocks.property(state, "lit") == Some("false")
            && ctx.data.blocks.property(state, "waterlogged") != Some("true");
    let (at, new) = if lightable {
        (clicked, ctx.shapes.with(state, "lit", "true"))
    } else {
        let at = offset(clicked, face);
        let Some(fire) = mc_tick::fire::can_light(&level, logic, to_pos(at)) else {
            return false;
        };
        (at, fire)
    };
    if !level.set_block(at, new) {
        return false;
    }
    let changes = level.into_changes();
    outcome.updates.extend(changes.iter().copied());
    outcome.events.push(Event::BlocksChanged { by: 0, changes });
    true
}

/// Wears the held tool by `amount` uses (not in creative); when that
/// reaches its maximum damage it breaks and clients play the break.
fn wear_held(ctx: &ServerContext, outcome: &mut Outcome, actor: Actor, hand: i32, amount: i32) {
    if actor.game_mode == CREATIVE {
        return;
    }
    let mut inventories = ctx.inventories.lock().expect("inventories lock");
    let Some(hands) = inventories.get_mut(&actor.uuid) else {
        return;
    };
    let slot = hands.hand_slot(hand);
    let Some(stack) = &mut hands.slots[slot] else {
        return;
    };
    let max = ctx.items.max_damage(stack.item);
    if max == 0 {
        return;
    }
    stack.damage += amount;
    if stack.damage >= max {
        hands.slots[slot] = None;
        hands.components.remove(&slot);
        outcome.events.push(Event::HandBroke {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            status: if hand == 1 { 48 } else { 47 },
        });
    }
    outcome.events.push(Event::InventorySlot {
        uuid: actor.uuid,
        slot: slot as i16,
        item: hands.slots[slot].clone(),
    });
}

/// Puts `item` (or nothing) in place of one of the held stack; a stack of
/// several keeps the rest and `item` goes elsewhere in the inventory.
fn replace_held(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    item: Option<i32>,
) {
    let mut inventories = ctx.inventories.lock().expect("inventories lock");
    let Some(hands) = inventories.get_mut(&actor.uuid) else {
        return;
    };
    let slot = hands.hand_slot(hand);
    let mut changed = vec![slot];
    let many = hands.slots[slot].as_ref().is_some_and(|s| s.count > 1);
    if many {
        if let Some(stack) = &mut hands.slots[slot] {
            stack.count -= 1;
        }
        if let Some(item) = item {
            let max = ctx.items.max_stack(item);
            changed.extend(hands.insert(item, 1, max).1);
        }
    } else {
        hands.slots[slot] = item.map(|item| ItemStack {
            count: 1,
            item,
            damage: 0,
            has_component_changes: false,
        });
        hands.components.remove(&slot);
    }
    for slot in changed {
        outcome.events.push(Event::InventorySlot {
            uuid: actor.uuid,
            slot: slot as i16,
            item: hands.slots[slot].clone(),
        });
    }
}

/// The first block the player looks at within reach, and the face the look
/// enters it through. Blocks count as full cubes; fluids are looked through
/// unless `sources` (then only source blocks stop the look).
pub(crate) fn look_at(
    ctx: &ServerContext,
    actor: Actor,
    yaw: f32,
    pitch: f32,
    sources: bool,
) -> Option<(Position, usize)> {
    let pose = actor.pose;
    let eye = [
        pose.x,
        pose.y + if actor.sneaking { 1.27 } else { 1.62 },
        pose.z,
    ];
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    let dir = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let reach = if actor.game_mode == CREATIVE {
        5.0
    } else {
        4.5
    };
    let mut cell = [
        eye[0].floor() as i32,
        eye[1].floor() as i32,
        eye[2].floor() as i32,
    ];
    let step = dir.map(|d| if d > 0.0 { 1 } else { -1 });
    let mut next = [0.0f64; 3];
    let mut delta = [f64::INFINITY; 3];
    for a in 0..3 {
        if dir[a] != 0.0 {
            delta[a] = (1.0 / dir[a]).abs();
            let edge = f64::from(cell[a]) + if dir[a] > 0.0 { 1.0 } else { 0.0 };
            next[a] = (edge - eye[a]) / dir[a];
        } else {
            next[a] = f64::INFINITY;
        }
    }
    // Faces entered when stepping along x, y, z (down/up, north/south,
    // west/east by the sign of the step).
    let mut face = 0;
    let mut travelled = 0.0;
    loop {
        if travelled > 0.0 || face != 0 {
            let p = Position::new(cell[0], cell[1], cell[2]);
            if let Some(state) = ctx.world.block(p, &ctx.data.blocks) {
                let name = ctx.data.blocks.block_name(state).unwrap_or("");
                let fluid = matches!(name, "minecraft:water" | "minecraft:lava");
                let source = ctx.data.blocks.property(state, "level") == Some("0");
                let air = ctx.shapes.is_air(state);
                if !air && (!fluid || (sources && source)) {
                    return Some((p, face));
                }
            }
        }
        let a = (0..3)
            .min_by(|&i, &j| next[i].total_cmp(&next[j]))
            .unwrap_or(0);
        travelled = next[a];
        if travelled > reach {
            return None;
        }
        cell[a] += step[a];
        next[a] += delta[a];
        face = match (a, step[a] > 0) {
            (0, true) => 4,
            (0, false) => 5,
            (1, true) => 0,
            (1, false) => 1,
            (2, true) => 2,
            _ => 3,
        };
    }
}

/// Using the held item in the air (or at a block out of `use_item_on`'s
/// reach): buckets scoop up and pour fluids.
pub fn use_item(ctx: &ServerContext, actor: Actor, hand: i32, yaw: f32, pitch: f32) -> Outcome {
    let mut outcome = Outcome::default();
    if actor.game_mode == SPECTATOR {
        return outcome;
    }
    if let Some(done) = crate::projectiles::use_item(ctx, actor, hand, yaw, pitch) {
        return done;
    }
    if let Some(h) = crate::held::held(ctx, actor.uuid, hand)
        && crate::vehicles::place_boat(ctx, &mut outcome, actor, hand, &h.name, yaw, pitch)
    {
        return outcome;
    }
    let held = ctx
        .inventories
        .lock()
        .expect("inventories lock")
        .get(&actor.uuid)
        .and_then(|h| h.held(hand).cloned());
    let Some(name) = held.and_then(|h| ctx.items.name(h.item)).map(str::to_owned) else {
        return outcome;
    };
    let fluid = match name.as_str() {
        "minecraft:bucket" => None,
        "minecraft:water_bucket" => Some("minecraft:water"),
        "minecraft:lava_bucket" => Some("minecraft:lava"),
        _ => return outcome,
    };
    let Some((hit, face)) = look_at(ctx, actor, yaw, pitch, fluid.is_none()) else {
        return outcome;
    };
    let blocks = &ctx.data.blocks;
    let state_at = |p: Position| ctx.world.block(p, blocks).unwrap_or(0);
    let mut level = WorldLevel::new(ctx);
    let (changed, result) = match fluid {
        // Scooping up a source, water out of a waterlogged block, or powder snow.
        None => {
            let state = state_at(hit);
            let name = blocks.block_name(state).unwrap_or("");
            let (new, bucket) = if blocks.property(state, "waterlogged") == Some("true") {
                (
                    ctx.shapes.with(state, "waterlogged", "false"),
                    "minecraft:water_bucket",
                )
            } else {
                match name {
                    "minecraft:water" => (ctx.shapes.air(), "minecraft:water_bucket"),
                    "minecraft:lava" => (ctx.shapes.air(), "minecraft:lava_bucket"),
                    "minecraft:powder_snow" => (ctx.shapes.air(), "minecraft:powder_snow_bucket"),
                    _ => return outcome,
                }
            };
            (level.set_block(hit, new), ctx.items.id(bucket))
        }
        // Pouring: into a waterloggable block, over something replaceable,
        // or against the face looked at.
        Some(fluid) => {
            let source = ctx.shapes.default_state(fluid).unwrap_or(0);
            let state = state_at(hit);
            let placed = if fluid == "minecraft:water"
                && blocks.property(state, "waterlogged") == Some("false")
            {
                level.set_block(hit, ctx.shapes.with(state, "waterlogged", "true"))
            } else {
                let fits = |s: u32| ctx.shapes.is_air(s) || ctx.rules.is_replaceable(s);
                let at = if fits(state) { hit } else { offset(hit, face) };
                let there = state_at(at);
                let same = blocks.block_name(there) == Some(fluid)
                    && blocks.property(there, "level") == Some("0");
                if !fits(there) || same {
                    false
                } else {
                    level.set_block(at, source)
                }
            };
            (placed, ctx.items.id("minecraft:bucket"))
        }
    };
    if !changed {
        return outcome;
    }
    let changes = level.into_changes();
    outcome.updates.extend(changes.iter().copied());
    outcome.events.push(Event::BlocksChanged { by: 0, changes });
    if actor.game_mode != CREATIVE {
        replace_held(ctx, &mut outcome, actor, hand, result);
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    outcome
}

/// Text written in the sign editor. Returns what viewers must be told.
pub fn sign_update(
    ctx: &ServerContext,
    actor: Actor,
    position: Position,
    front: bool,
    lines: [String; 4],
) -> Option<Event> {
    if actor.game_mode == 3 {
        return None;
    }
    let blocks = &ctx.data.blocks;
    let (dx, dy, dz) = (
        f64::from(position.x) + 0.5 - actor.pose.x,
        f64::from(position.y) + 0.5 - actor.pose.y - 1.62,
        f64::from(position.z) + 0.5 - actor.pose.z,
    );
    if dx * dx + dy * dy + dz * dz > 64.0 {
        return None;
    }
    let id = ctx
        .world
        .block(position, blocks)
        .and_then(|s| blocks.block_name(s))
        .and_then(sign_entity)?;
    let mut be = ctx
        .world
        .block_entity(position, blocks)
        .filter(|be| be.id == id)
        .unwrap_or_else(|| mc_world::BlockEntity {
            x: position.x,
            y: position.y,
            z: position.z,
            id: id.to_owned(),
            data: blank_sign(),
        });
    if matches!(be.data.get("is_waxed"), Some(mc_nbt::Tag::Byte(1))) {
        return None;
    }
    // Formatting codes do not survive, as in the game.
    let lines = lines.map(|l| {
        let mut out = String::new();
        let mut chars = l.chars();
        while let Some(c) = chars.next() {
            if c == '\u{a7}' {
                chars.next();
            } else {
                out.push(c);
            }
        }
        out
    });
    let key = if front { "front_text" } else { "back_text" };
    let mut side = match be.data.get(key) {
        Some(mc_nbt::Tag::Compound(c)) => c.clone(),
        _ => sign_side(Default::default()),
    };
    side.insert(
        "messages",
        mc_nbt::Tag::List(lines.into_iter().map(mc_nbt::Tag::String).collect()),
    );
    be.data.insert(key, mc_nbt::Tag::Compound(side));
    let kind = ctx.world.block_entity_kind(&be.id)?;
    let data = crate::world::client_data(&be.id, &be.data)?;
    ctx.world.set_block_entity(be, blocks);
    Some(Event::BlockEntity {
        position,
        kind,
        data,
    })
}

/// The item pick block gives for a block (`minecraft:` names), if any:
/// usually the block's own item; wall-mounted variants give the standing
/// one, crops their seeds, potted plants the plant.
pub fn clone_item(block: &str, items: &crate::items::Items) -> Option<i32> {
    let name = block.strip_prefix("minecraft:").unwrap_or(block);
    let special = match name {
        "wall_torch" => Some("torch"),
        "soul_wall_torch" => Some("soul_torch"),
        "redstone_wall_torch" => Some("redstone_torch"),
        "copper_wall_torch" => Some("copper_torch"),
        "redstone_wire" => Some("redstone"),
        "tripwire" => Some("string"),
        "wheat" => Some("wheat_seeds"),
        "carrots" => Some("carrot"),
        "potatoes" => Some("potato"),
        "beetroots" => Some("beetroot_seeds"),
        "cocoa" => Some("cocoa_beans"),
        "melon_stem" | "attached_melon_stem" => Some("melon_seeds"),
        "pumpkin_stem" | "attached_pumpkin_stem" => Some("pumpkin_seeds"),
        "torchflower_crop" => Some("torchflower_seeds"),
        "pitcher_crop" => Some("pitcher_pod"),
        "sweet_berry_bush" => Some("sweet_berries"),
        "cave_vines" | "cave_vines_plant" => Some("glow_berries"),
        "kelp_plant" => Some("kelp"),
        "bamboo_sapling" => Some("bamboo"),
        "twisting_vines_plant" => Some("twisting_vines"),
        "weeping_vines_plant" => Some("weeping_vines"),
        "big_dripleaf_stem" => Some("big_dripleaf"),
        "tall_seagrass" => Some("seagrass"),
        "water_cauldron" | "lava_cauldron" | "powder_snow_cauldron" => Some("cauldron"),
        "powder_snow" => Some("powder_snow_bucket"),
        "piston_head" => Some("piston"),
        "candle_cake" => Some("cake"),
        "potted_azalea_bush" => Some("azalea"),
        "potted_flowering_azalea_bush" => Some("flowering_azalea"),
        "skeleton_wall_skull" => Some("skeleton_skull"),
        "wither_skeleton_wall_skull" => Some("wither_skeleton_skull"),
        "frosted_ice" | "fire" | "soul_fire" | "water" | "lava" | "bubble_column"
        | "moving_piston" | "nether_portal" | "end_portal" | "end_gateway" | "air" | "cave_air"
        | "void_air" => return None,
        _ => None,
    };
    let candidate = match special {
        Some(s) => s.to_owned(),
        None if name.ends_with("_candle_cake") => "cake".to_owned(),
        None if name.starts_with("potted_") => name["potted_".len()..].to_owned(),
        None if name.contains("_wall_") => name.replacen("_wall_", "_", 1),
        None => name.to_owned(),
    };
    items.id(&format!("minecraft:{candidate}"))
}

/// The block entity of a sign block, if it is one.
fn sign_entity(block: &str) -> Option<&'static str> {
    if block.ends_with("_hanging_sign") {
        Some("minecraft:hanging_sign")
    } else if block.ends_with("_sign") {
        Some("minecraft:sign")
    } else {
        None
    }
}

fn sign_side(lines: [String; 4]) -> mc_nbt::Compound {
    let mut side = mc_nbt::Compound::new();
    side.insert(
        "messages",
        mc_nbt::Tag::List(lines.into_iter().map(mc_nbt::Tag::String).collect()),
    );
    side.insert("color", mc_nbt::Tag::String("black".into()));
    side.insert("has_glowing_text", mc_nbt::Tag::Byte(0));
    side
}

fn blank_sign() -> mc_nbt::Compound {
    let mut data = mc_nbt::Compound::new();
    data.insert(
        "front_text",
        mc_nbt::Tag::Compound(sign_side(Default::default())),
    );
    data.insert(
        "back_text",
        mc_nbt::Tag::Compound(sign_side(Default::default())),
    );
    data.insert("is_waxed", mc_nbt::Tag::Byte(0));
    data
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

    /// Survival player at the origin holding `damage` worn flint and steel.
    fn with_flint(damage: i32) -> Option<(std::sync::Arc<ServerContext>, Actor)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let ctx = ServerContext::new(crate::Config::new(dir), data).ok()?;
        let flint = ctx.items.id("minecraft:flint_and_steel")?;
        let mut hands = Hands::default();
        hands.slots[36] = Some(ItemStack {
            damage,
            ..ItemStack::new(flint, 1)
        });
        let actor = Actor {
            uuid: 7,
            entity_id: 7,
            game_mode: 0,
            pose: pose(0.5, -60.0, 0.5),
            sneaking: false,
        };
        ctx.inventories.lock().unwrap().insert(7, hands);
        Some((ctx, actor))
    }

    fn slot_after(outcome: &Outcome) -> Option<Option<ItemStack>> {
        outcome.events.iter().rev().find_map(|e| match e {
            Event::InventorySlot { slot: 36, item, .. } => Some(item.clone()),
            _ => None,
        })
    }

    #[test]
    fn flint_and_steel_wears_and_breaks() {
        let Some((ctx, actor)) = with_flint(10) else {
            eprintln!("skipped: no game data");
            return;
        };
        let top = Position::new(0, -61, 0);
        let o = use_item_on(&ctx, actor, 0, top, 1, [0.5; 3], 1);
        assert_eq!(slot_after(&o).flatten().map(|s| s.damage), Some(11));
        // Fire stands on the grass now.
        let above = ctx
            .world
            .block(Position::new(0, -60, 0), &ctx.data.blocks)
            .unwrap();
        assert_eq!(ctx.data.blocks.block_name(above), Some("minecraft:fire"));

        // The last use breaks it.
        let max = ctx
            .items
            .max_damage(ctx.items.id("minecraft:flint_and_steel").unwrap());
        assert_eq!(max, 64);
        ctx.inventories.lock().unwrap().get_mut(&7).unwrap().slots[36]
            .as_mut()
            .unwrap()
            .damage = 63;
        let o = use_item_on(&ctx, actor, 0, Position::new(1, -61, 0), 1, [0.5; 3], 2);
        assert_eq!(slot_after(&o), Some(None));
        assert!(
            o.events
                .iter()
                .any(|e| matches!(e, Event::HandBroke { status: 47, .. }))
        );
        assert!(ctx.inventories.lock().unwrap()[&7].slots[36].is_none());
    }

    #[test]
    fn flint_and_steel_primes_tnt() {
        let Some((ctx, actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let tnt = ctx.data.blocks.default_state("minecraft:tnt").unwrap();
        let at = Position::new(1, -60, 0);
        WorldLevel::new(&ctx).set_block(at, tnt);
        let o = use_item_on(&ctx, actor, 0, at, 1, [0.5; 3], 1);
        assert!(
            o.updates
                .iter()
                .any(|&(p, s)| p == at && ctx.data.blocks.is_air(s))
        );
        let entities = ctx.entities.lock().unwrap();
        assert!(matches!(
            entities[..],
            [mc_entity::Entity {
                kind: mc_entity::Kind::Tnt {
                    fuse: 80,
                    owner: Some(7)
                },
                ..
            }]
        ));
        assert_eq!((entities[0].pos[0], entities[0].pos[1]), (1.5, -60.0));
    }

    #[test]
    fn breaking_unstable_tnt_lights_it() {
        let Some((ctx, actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let tnt = ctx
            .data
            .blocks
            .state_id("minecraft:tnt", &[("unstable", "true")])
            .unwrap();
        let at = Position::new(1, -60, 0);
        WorldLevel::new(&ctx).set_block(at, tnt);
        dig(&ctx, actor, dig::FINISH, at, 1, 1);
        assert!(
            ctx.data
                .blocks
                .is_air(ctx.world.block(at, &ctx.data.blocks).unwrap())
        );
        assert!(matches!(
            ctx.entities.lock().unwrap()[..],
            [mc_entity::Entity {
                kind: mc_entity::Kind::Tnt { fuse: 80, .. },
                ..
            }]
        ));
    }
}
