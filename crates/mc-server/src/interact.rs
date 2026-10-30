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
use mc_protocol::filter::FilteredText;
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
    /// The stack on the mouse cursor while the inventory is open.
    pub carried: Option<ItemStack>,
}

impl Default for Hands {
    fn default() -> Self {
        Self {
            slots: vec![None; 46],
            selected: 0,
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
            self.slots[index] = stack.as_ref().map(|h| h.resolve(&before));
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
            changed.push(free);
        }
        self.slots[target] = Some(ItemStack {
            count: 1,
            item,
            damage: 0,
            components: Default::default(),
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
        let enchanted = |slot: usize| {
            self.slots[slot]
                .as_ref()
                .is_some_and(|s| s.components.get("enchantments").is_some())
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

    /// Puts items in: first onto matching stacks (the one in the main hand,
    /// then the off hand, then the hotbar and the main inventory), then into
    /// empty slots (hotbar first). Returns how many went in and
    /// the container slots that changed.
    pub fn insert(&mut self, item: i32, count: i32, max: i32) -> (i32, Vec<usize>) {
        self.insert_stack(&ItemStack::new(item, count), max)
    }

    /// [`Hands::insert`] for a stack that may be damaged: it only joins
    /// stacks with the same damage.
    pub fn insert_stack(&mut self, stack: &ItemStack, max: i32) -> (i32, Vec<usize>) {
        let order: Vec<usize> = (36..45).chain(9..36).collect();
        let hands = [self.hand_slot(0), self.hand_slot(1)];
        let mut changed = Vec::new();
        let mut count = stack.count;
        {
            for &slot in hands
                .iter()
                .chain(order.iter().filter(|s| !hands.contains(s)))
            {
                if count == 0 {
                    break;
                }
                if let Some(s) = &mut self.slots[slot]
                    && s.same_kind(stack)
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
    /// The inventory of the mount with this entity id, opened for the
    /// player (with the columns of its chest).
    pub open_mount: Option<(i32, i32)>,
    /// The player opened a crafting table.
    pub open_crafting: bool,
    /// The player opened a block's menu (beacon, anvil...).
    pub open_menu: Option<(Position, crate::menus::Menu)>,
    /// The player placed or clicked a sign: its editor opens on that side
    /// (`true` the front).
    pub open_sign: Option<(Position, bool)>,
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

/// The game event of a block a player used: a door or gate opens or
/// closes, a lever or button turns on or off, anything else changes.
fn used_event(blocks: &mc_data::Blocks, before: u32, after: u32) -> &'static str {
    let flag = |s: u32, k: &str| blocks.property(s, k) == Some("true");
    if flag(before, "open") != flag(after, "open") {
        if flag(after, "open") {
            "block_open"
        } else {
            "block_close"
        }
    } else if flag(before, "powered") != flag(after, "powered") {
        if flag(after, "powered") {
            "block_activate"
        } else {
            "block_deactivate"
        }
    } else {
        "block_change"
    }
}

/// A player's box: 1.5 high while sneaking.
fn player_box(pose: &Pose, sneaking: bool) -> mc_entity::Aabb {
    let half = PLAYER_WIDTH / 2.0;
    let height = if sneaking { 1.5 } else { PLAYER_HEIGHT };
    mc_entity::Aabb {
        min: [pose.x - half, pose.y, pose.z - half],
        max: [pose.x + half, pose.y + height, pose.z + half],
    }
}

/// Entities a block may not be placed into (vanilla's `blocksBuilding`):
/// living ones, boats and minecarts, lit TNT, end crystals; not items, orbs,
/// projectiles, frames or markers.
fn blocks_building(e: &mc_entity::Entity) -> bool {
    use mc_entity::Kind;
    if e.removed {
        return false;
    }
    match &e.kind {
        Kind::Mob(m) => m.dying.is_none(),
        Kind::Merchant(_) | Kind::Tnt { .. } | Kind::Vehicle(_) | Kind::Crystal(_) => true,
        Kind::Hung(h) => matches!(&h.decor, mc_entity::extra::Decor::Stand(s) if !s.marker),
        _ => false,
    }
}

/// The boxes a state collides with in the world, at `p`: its shape (a fence
/// or wall stands 1.5 high), or a full cube.
fn placed_boxes(ctx: &ServerContext, p: Position, state: u32) -> Vec<mc_entity::Aabb> {
    if !ctx.rules.blocks_motion(state) {
        return Vec::new();
    }
    let name = ctx.data.blocks.block_name(state).unwrap_or_default();
    let tall = name.ends_with("_fence")
        || name.ends_with("_wall")
        || name.ends_with("_fence_gate") && ctx.data.blocks.property(state, "open") != Some("true");
    let shapes = crate::collision::table(ctx)
        .get(state as usize)
        .cloned()
        .flatten()
        .unwrap_or_else(|| vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]);
    let (x, y, z) = (f64::from(p.x), f64::from(p.y), f64::from(p.z));
    shapes
        .into_iter()
        .map(|b| {
            let top = if tall && b[4] >= 1.0 { 1.5 } else { b[4] };
            mc_entity::Aabb {
                min: [x + b[0], y + b[1], z + b[2]],
                max: [x + b[3], y + top, z + b[5]],
            }
        })
        .collect()
}

fn overlap(a: &mc_entity::Aabb, b: &mc_entity::Aabb) -> bool {
    (0..3).all(|i| a.min[i] < b.max[i] && b.min[i] < a.max[i])
}

/// Whether placing these states would put a block into a player or an
/// entity that blocks building. Only the block's real shape counts: a slab
/// goes under the feet of a player standing in the top half of its cell.
fn obstructed(ctx: &ServerContext, actor: &Actor, placed: &[(Position, u32)]) -> bool {
    let shapes: Vec<mc_entity::Aabb> = placed
        .iter()
        .flat_map(|&(p, state)| placed_boxes(ctx, p, state))
        .collect();
    if shapes.is_empty() {
        return false;
    }
    let mut boxes = vec![player_box(&actor.pose, actor.sneaking)];
    boxes.extend(
        ctx.players
            .lock()
            .expect("players lock")
            .values()
            .filter(|p| p.dimension == ctx.world.dimension)
            .filter(|p| p.profile.uuid != actor.uuid && p.game_mode != 3)
            .filter_map(|p| p.pose)
            .map(|pose| player_box(&pose, false)),
    );
    boxes.extend(
        crate::tick::lock(&ctx.entities)
            .iter()
            .filter(|e| blocks_building(e))
            .map(|e| e.aabb()),
    );
    shapes.iter().any(|s| boxes.iter().any(|b| overlap(s, b)))
}

/// Whether the held item lets a creative player break blocks: swords, the
/// trident and the mace do not (`can_destroy_blocks_in_creative` of their
/// `tool`, on the stack or by default).
fn breaks_in_creative(ctx: &ServerContext, uuid: u128) -> bool {
    let Some(held) = crate::held::held(ctx, uuid, 0) else {
        return true;
    };
    let own = held.comps.as_ref().and_then(|c| match c {
        mc_nbt::Tag::Compound(c) => match c.get("minecraft:tool") {
            Some(mc_nbt::Tag::Compound(t)) => match t.get("can_destroy_blocks_in_creative") {
                Some(mc_nbt::Tag::Byte(b)) => Some(*b != 0),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    });
    own.unwrap_or_else(|| {
        ctx.items
            .get(held.stack.item)
            .and_then(|i| {
                i.defaults
                    .get("minecraft:tool")?
                    .get("can_destroy_blocks_in_creative")?
                    .as_bool()
            })
            .unwrap_or(true)
    })
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
    if status == dig::START && actor.game_mode != SPECTATOR {
        // Spawn protection says so and goes no further; the border and
        // the adventure rules leave the client's guess to be corrected.
        if crate::adventure::under_spawn_protection(ctx, actor.uuid, position) {
            crate::adventure::protected_message(&mut outcome, actor.uuid, position);
            return outcome;
        }
        let refused = !crate::adventure::may_interact(ctx, actor.uuid, position)
            || actor.game_mode != CREATIVE
                && crate::adventure::break_restricted(ctx, &actor, position);
        if refused {
            if let Some(state) = world.block(position, blocks) {
                outcome.updates.push((position, state));
            }
            return outcome;
        }
    }
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
    // Hitting redstone ore lights it.
    if status == dig::START && actor.game_mode != SPECTATOR {
        let mut level = WorldLevel::new(ctx);
        let logic = level.logic();
        mc_tick::redstone::touch_ore(&mut level, logic, to_pos(position));
        let changes = level.into_changes();
        if !changes.is_empty() {
            outcome.events.push(Event::BlocksChanged { by: 0, changes });
        }
    }
    let Some(state) = world.block(position, blocks) else {
        return outcome;
    };
    // Spectators do not dig; creative breaks at once; survival breaks at once
    // for blocks that break instantly, else when the client says it finished
    // and the time it takes has passed.
    if status == dig::CANCEL {
        crate::digging::stop(ctx, actor.uuid);
        return outcome;
    }
    let breaks = match (status, actor.game_mode) {
        (_, SPECTATOR) => false,
        (dig::START, CREATIVE) => breaks_in_creative(ctx, actor.uuid),
        (dig::START, _) => {
            crate::digging::start(ctx, &actor, position);
            blocks::breaks_instantly(&ctx.data, &ctx.rules, state)
                || crate::digging::factors(ctx, actor.uuid, &actor.pose, state)
                    .is_some_and(|f| crate::digging::ticks_to_break(&f) == Some(0))
        }
        (dig::FINISH, CREATIVE) => false,
        (dig::FINISH, _) => {
            let ok = in_reach(&actor.pose, position)
                && !crate::adventure::break_restricted(ctx, &actor, position)
                && crate::digging::may_finish(ctx, &actor, position, state);
            crate::digging::stop(ctx, actor.uuid);
            if !ok {
                // The client broke it already: show what is there.
                outcome.updates.push((position, state));
            }
            ok
        }
        _ => return Outcome::default(),
    };
    if !breaks || !in_reach(&actor.pose, position) {
        return outcome;
    }
    if blocks.is_air(state) {
        return outcome;
    }
    let block_at = |p: Position| world.block(p, blocks);
    // Read the pot before the block (and its entity) is removed.
    let pot = (blocks.block_name(state) == Some("minecraft:decorated_pot")).then(|| {
        crate::pots::harvest(
            ctx,
            position,
            state,
            crate::pots::Breaker::player(ctx, actor.uuid, actor.game_mode == CREATIVE),
        )
    });
    // The other half of a door, bed or tall plant goes with the shape
    // updates and drops what it would. A player who gets nothing for this
    // block (in creative, or without the tool it needs: an iron door by
    // hand) breaking the half that drops nothing (a top, a bed's foot)
    // takes the other half first, so it drops nothing either; this half
    // then goes with that.
    let changes = blocks::break_block(&ctx.data, &ctx.rules, position, state, &block_at);
    let left = changes[0].1;
    let mut level = WorldLevel::new(ctx);
    let drops = actor.game_mode != CREATIVE
        && ctx.game_rule_bool("block_drops")
        && crate::digging::factors(ctx, actor.uuid, &actor.pose, state)
            .is_none_or(|f| f.right_tool);
    let dropless_half = blocks.property(state, "half") == Some("upper")
        || blocks.property(state, "part") == Some("foot");
    if !drops
        && dropless_half
        && let Some(&(partner, gone)) = changes.get(1)
    {
        level.set_block(partner, gone);
    }
    // String cut with shears does not trip its line.
    if blocks.block_name(state) == Some("minecraft:tripwire")
        && crate::held::held(ctx, actor.uuid, 0).is_some_and(|h| h.name == "minecraft:shears")
    {
        let logic = level.logic();
        mc_tick::tripwire::disarm(&mut level, logic, to_pos(position));
    }
    // A block entity goes with its block: what it holds is taken first.
    let name = blocks.block_name(state).unwrap_or("");
    let container = crate::containers::kind_of(name).map(|_| {
        let furnace = matches!(
            name,
            "minecraft:furnace" | "minecraft:blast_furnace" | "minecraft:smoker"
        );
        let points = if furnace && actor.game_mode != CREATIVE {
            crate::furnaces::collect_xp(ctx, position)
        } else {
            0
        };
        (crate::containers::broken(ctx, position).0, points)
    });
    let food = crate::campfires::is_campfire(name).then(|| crate::campfires::broken(ctx, position));
    let record = (name == "minecraft:jukebox")
        .then(|| ctx.world.block_entity(position, blocks))
        .flatten()
        .and_then(|be| match be.data.get("RecordItem") {
            Some(mc_nbt::Tag::Compound(record)) => match record.get("id") {
                Some(mc_nbt::Tag::String(id)) => ctx.items.id(id),
                _ => None,
            },
            _ => None,
        });
    // Flying mobs (flying-mobs): the bees of a broken hive come out angry.
    if crate::mobs::bees::is_hive(name) && actor.game_mode != CREATIVE {
        let tool = crate::held::held(ctx, actor.uuid, 0).map(|h| h.stack);
        crate::criteria::bee_nest(
            ctx,
            actor.uuid,
            name,
            tool.as_ref(),
            crate::mobs::bees::inside(ctx, position).len(),
        );
        crate::mobs::bees::disturbed(ctx, position, Some(actor.uuid), false);
    }
    if !level.set_block(position, left) && world.block(position, blocks) != Some(left) {
        outcome.updates.push((position, state));
        return outcome;
    }
    outcome.updates.push((position, left));
    // Creative players break without it being mined.
    if actor.game_mode != CREATIVE {
        crate::stats::block(ctx, actor.uuid, crate::stats::MINED, state, 1);
    }
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
    if let Some((items, points)) = container {
        crate::xp::spawn(ctx, centre(position), points);
        for stack in items {
            outcome
                .drops
                .push((position, crate::loot::Drop::of(&stack)));
        }
        outcome.events.push(Event::ContainerGone { position });
    }
    for stack in food.into_iter().flatten() {
        outcome
            .drops
            .push((position, crate::loot::Drop::of(&stack)));
    }
    if blocks.block_name(state) == Some("minecraft:creaking_heart") {
        crate::mobs::creaking::broken(ctx, position, state, Some(actor.uuid));
    }
    if let Some(item) = record {
        outcome.drops.push((
            position,
            crate::loot::Drop {
                item,
                count: 1,
                components: mc_nbt::Compound::new(),
            },
        ));
        crate::jukeboxes::stop(ctx, position);
    }
    let shown = pot.as_ref().map(|h| h.shown).unwrap_or(state);
    if let Some(harvest) = pot {
        if actor.game_mode != CREATIVE && ctx.game_rule_bool("block_drops") {
            for drop in harvest.drops {
                outcome.drops.push((position, drop));
            }
        }
    } else if actor.game_mode != CREATIVE
        && ctx.game_rule_bool("block_drops")
        && let Some(name) = blocks.block_name(state)
    {
        let props = |k: &str| blocks.property(state, k).map(str::to_owned);
        let tool = crate::held::held(ctx, actor.uuid, 0).map(|h| crate::loot::Tool {
            item: h.name.clone(),
            enchants: crate::mobs::hurt::enchant_list(h.comps.as_ref()),
        });
        let env = crate::commands::loot::Env(ctx);
        for drop in ctx.loot.get().drops_mined(&env, name, &props, tool) {
            outcome.drops.push((position, drop));
        }
        if !silk_touch(ctx, actor.uuid) {
            crate::xp::spawn(ctx, centre(position), crate::xp::block_xp(name));
            // Whatever was hiding in an infested block comes out.
            crate::mobs::infest::emerge(ctx, position, state);
        }
    }
    outcome.events.push(Event::BlockBroken {
        by: actor.uuid,
        position,
        state: shown,
    });
    let source = crate::vibrations::Source::player(ctx, actor.uuid);
    crate::vibrations::emit_at(ctx, "block_destroy", position, source, Some(state));
    // Everyone gets every change, the breaker included (vanilla sends the
    // broken block to the breaker twice: as the answer and with the rest).
    if actor.game_mode != CREATIVE {
        wear_tool(ctx, actor.uuid, state);
    }
    outcome.events.push(Event::BlocksChanged {
        by: 0,
        changes: level.into_changes(),
    });
    outcome
}

/// The tool in the main hand wears with each block it breaks
/// (minecraft.wiki, "Item durability"): by its `damage_per_block` (2 for a
/// sword), but not on a block that breaks at once; shears wear on anything
/// but fire.
fn wear_tool(ctx: &ServerContext, uuid: u128, state: u32) {
    let Some(held) = crate::held::held(ctx, uuid, 0) else {
        return;
    };
    let Some(tool) = ctx
        .items
        .get(held.stack.item)
        .and_then(|i| i.defaults.get("minecraft:tool"))
    else {
        return;
    };
    let per_block = tool["damage_per_block"].as_i64().unwrap_or(1) as i32;
    let name = ctx.data.blocks.block_name(state).unwrap_or("");
    let wears = if held.name == "minecraft:shears" {
        !matches!(name, "minecraft:fire" | "minecraft:soul_fire")
    } else {
        crate::hardness::hardness(name).is_some_and(|h| h != 0.0)
    };
    if per_block > 0 && wears {
        crate::held::wear(ctx, uuid, held.slot, per_block);
    }
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
        h.main_hand().is_some_and(|s| {
            crate::held::enchant(crate::held::comps_of(s).as_ref(), "silk_touch") > 0
        })
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
    let held: Option<ItemStack> = ctx
        .inventories
        .lock()
        .expect("inventories lock")
        .get(&actor.uuid)
        .and_then(|h| h.held(hand).cloned());
    let (world, blocks) = (&ctx.world, &ctx.data.blocks);
    let placed_at = usize::try_from(face)
        .ok()
        .filter(|f| *f < 6)
        .map(|f| offset(clicked, f));
    let before = [Some(clicked), placed_at].map(|p| p.and_then(|p| world.block(p, blocks)));
    let outcome = use_item_on_block(ctx, actor, hand, clicked, face, cursor, sequence);
    let done = outcome
        .events
        .iter()
        .any(|e| matches!(e, Event::Swing { .. }));
    if done {
        // Something placed (seeds place crops, so any held stack counts):
        // where a block now stands that did not before.
        if held.is_some() {
            for (p, was) in [placed_at, Some(clicked)]
                .into_iter()
                .zip([before[1], before[0]])
            {
                if let Some(p) = p
                    && let Some(now) = world.block(p, blocks)
                    && Some(now) != was
                    && blocks.block_name(now) != Some("minecraft:air")
                {
                    crate::criteria::on_block(
                        ctx,
                        actor.uuid,
                        "minecraft:placed_block",
                        p,
                        held.as_ref(),
                    );
                    break;
                }
            }
        }
        crate::criteria::on_block(
            ctx,
            actor.uuid,
            "minecraft:item_used_on_block",
            clicked,
            held.as_ref(),
        );
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn use_item_on_block(
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
    // Spawn protection and the border keep the client's guess from standing.
    if !crate::adventure::may_interact(ctx, actor.uuid, clicked) {
        if crate::adventure::under_spawn_protection(ctx, actor.uuid, clicked) {
            crate::adventure::protected_message(&mut outcome, actor.uuid, clicked);
        }
        for p in [clicked, offset(clicked, face)] {
            if let Some(state) = block_at(p) {
                outcome.updates.push((p, state));
            }
        }
        return outcome;
    }
    let use_blocks = !(actor.sneaking && held.is_some());
    // Crafting tables open their grid.
    if use_blocks
        && block_at(clicked).and_then(|s| blocks.block_name(s)) == Some("minecraft:crafting_table")
    {
        outcome.open_crafting = true;
        crate::stats::custom(ctx, actor.uuid, "interact_with_crafting_table", 1);
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
    // Beacons, anvils and the like open their menus.
    if use_blocks
        && let Some(menu) = block_at(clicked)
            .and_then(|s| blocks.block_name(s))
            .and_then(crate::menus::Menu::of_block)
    {
        outcome.open_menu = Some((clicked, menu));
        crate::stats::opened(
            ctx,
            actor.uuid,
            block_at(clicked).and_then(|s| blocks.block_name(s)),
        );
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
        crate::stats::opened(
            ctx,
            actor.uuid,
            block_at(clicked).and_then(|s| blocks.block_name(s)),
        );
        outcome.events.push(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return outcome;
    }
    // Cauldrons, cakes, flower pots, signs, berries...
    if use_blocks
        && crate::blockuse::use_block(ctx, &mut outcome, actor, hand, clicked, face, cursor)
    {
        return outcome;
    }
    // Composters, jukeboxes, bells, beds, respawn anchors.
    if use_blocks && crate::useblocks::try_use(ctx, &mut outcome, actor, hand, clicked, face) {
        return outcome;
    }
    // Levers, doors, repeaters... react to being used instead of having
    // something placed against them.
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    let before = block_at(clicked);
    if use_blocks && mc_tick::use_block(&mut level, logic, to_pos(clicked)) {
        if let Some(state) = block_at(clicked) {
            outcome.updates.push((clicked, state));
            if let Some(before) = before {
                let name = used_event(&ctx.data.blocks, before, state);
                let source = crate::vibrations::Source::player(ctx, actor.uuid);
                crate::vibrations::emit_at(ctx, name, clicked, source, Some(state));
            }
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
    // An adventure player's item acts on the blocks it lists in
    // `can_place_on`, and on no others.
    if !crate::adventure::may_use_on(ctx, &actor, held, clicked) {
        return refuse(outcome);
    }
    // Axes, hoes, shovels, bone meal, honeycomb and stacking candles act
    // on the block.
    if crate::blockuse::use_item_on(ctx, &mut outcome, actor, hand, clicked, face, cursor) {
        if let Some(item) = held {
            crate::stats::item(ctx, actor.uuid, crate::stats::USED, item.item, 1);
        }
        return outcome;
    }
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
        crate::fx::level_event(
            ctx,
            1503,
            [
                f64::from(clicked.x),
                f64::from(clicked.y),
                f64::from(clicked.z),
            ],
            0,
        );
        crate::portals::light_end_portal(&mut level, to_pos(clicked));
        let changes = level.into_changes();
        outcome.updates.extend(changes.iter().copied());
        outcome.events.push(Event::BlocksChanged { by: 0, changes });
        crate::stats::item(
            ctx,
            actor.uuid,
            crate::stats::USED,
            ctx.items.id("minecraft:ender_eye").unwrap_or(0),
            1,
        );
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
    if let Some(name) = held_name
        && name.ends_with("_spawn_egg")
        && crate::mobs::eggs::refused_by_spawner(ctx, &mut outcome, actor, clicked)
    {
        return refuse(outcome);
    }
    // Spawn eggs of mobs (and a spawner's kind).
    if let Some(name) = held_name
        && crate::mobs::eggs::use_on_block(ctx, &mut outcome, actor, hand, name, clicked, face)
    {
        return refuse(outcome);
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
        && (crate::bosses::crystal::place(ctx, &mut outcome, actor, hand, name, clicked)
            || crate::decor::place(ctx, &mut outcome, actor, hand, name, clicked, face)
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
        if let Some(item) = held {
            crate::stats::item(ctx, actor.uuid, crate::stats::USED, item.item, 1);
        }
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
        if let Some(item) = held {
            crate::stats::item(ctx, actor.uuid, crate::stats::USED, item.item, 1);
        }
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
    let map_item = held.is_some_and(|stack| {
        matches!(
            ctx.items.name(stack.item),
            Some("minecraft:map" | "minecraft:filled_map")
        )
    });
    if map_item && crate::maps::use_held(ctx, &mut outcome, actor, hand, Some(clicked)) {
        return outcome;
    }
    let Some(block) = held.and_then(|item| ctx.rules.block_for_item(item.item)) else {
        return refuse(outcome);
    };
    // Game master blocks: only operators in creative place them.
    let game_master = matches!(
        held_name,
        Some(
            "minecraft:command_block"
                | "minecraft:chain_command_block"
                | "minecraft:repeating_command_block"
                | "minecraft:structure_block"
                | "minecraft:jigsaw"
                | "minecraft:test_block"
                | "minecraft:test_instance_block"
        )
    );
    if game_master && !crate::command_blocks::may_use(ctx, actor.uuid, actor.game_mode) {
        return refuse(outcome);
    }
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

    let mut level = WorldLevel::new(ctx);
    // Nothing to hold it up there: not placed.
    let unsupported = placed
        .blocks
        .iter()
        .any(|&(p, state)| !mc_tick::support::can_survive(&level, level.logic(), to_pos(p), state));
    if unsupported {
        return refuse(outcome);
    }
    // Connections and shape from the neighbours, as the block is placed.
    let finals: Vec<(Position, u32)> = placed
        .blocks
        .iter()
        .map(|&(p, state)| {
            let state = mc_tick::placement_state(&level, level.logic(), to_pos(p), state);
            let state =
                mc_tick::redstone::player_placement(&level, level.logic(), to_pos(p), state);
            (p, state)
        })
        .collect();
    // No block goes into a player or a mob.
    if obstructed(ctx, &actor, &finals) {
        return refuse(outcome);
    }
    let mut placed_any = false;
    for &(p, state) in &finals {
        placed_any |= level.set_block(p, state);
    }
    if placed_any && let Some(&(p, state)) = finals.first() {
        let source = crate::vibrations::Source::player(ctx, actor.uuid);
        crate::vibrations::emit_at(ctx, "block_place", p, source, Some(state));
    }
    if !placed_any {
        return refuse(outcome);
    }
    let changes = level.into_changes();
    // Containers get their (empty) block entity.
    for &(p, state) in &placed.blocks {
        crate::containers::placed(ctx, p, state);
        crate::command_blocks::placed(ctx, p, state);
        crate::structure_blocks::placed(ctx, p, state);
        crate::test_blocks::placed(ctx, p, state);
        crate::pots::placed(ctx, p, state, actor.uuid, hand);
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
            outcome.open_sign = Some((p, true));
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
    // The last skull of a wither's T.
    for &(p, state) in &placed.blocks {
        crate::bosses::wither::placed(ctx, &mut outcome.events, p, state);
        // The head of a golem.
        outcome
            .events
            .extend(crate::mobs::golems::placed(ctx, p, state));
    }
    // Survival and adventure use up the block.
    if actor.game_mode != CREATIVE {
        let mut inventories = ctx.inventories.lock().expect("inventories lock");
        if let Some(hands) = inventories.get_mut(&actor.uuid) {
            let slot = hands.hand_slot(hand);
            if let Some(stack) = &mut hands.slots[slot] {
                stack.count -= 1;
                if stack.count <= 0 {
                    hands.slots[slot] = None;
                }
            }
            outcome.events.push(Event::InventorySlot {
                uuid: actor.uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
    }
    if let Some(item) = held {
        crate::stats::item(ctx, actor.uuid, crate::stats::USED, item.item, 1);
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
    let mut broke = None;
    stack.damage += crate::enchant_effects::wear(ctx, stack, amount);
    if stack.damage >= max {
        broke = Some(stack.item);
        hands.slots[slot] = None;
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
    drop(inventories);
    if let Some(item) = broke {
        crate::stats::item(ctx, actor.uuid, crate::stats::BROKEN, item, 1);
    }
}

/// Puts `item` (or nothing) in place of one of the held stack; a stack of
/// several keeps the rest and `item` goes elsewhere in the inventory.
pub(crate) fn replace_held(
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
            components: Default::default(),
        });
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
    if crate::mobs::ride::boost(ctx, &actor, hand).is_some() {
        return outcome;
    }
    let thrown = crate::held::held(ctx, actor.uuid, hand);
    if thrown
        .as_ref()
        .is_some_and(|h| h.name == "minecraft:spyglass")
    {
        crate::criteria::spyglass(actor.uuid, Some(hand));
    }
    if let Some(done) = crate::projectiles::use_item(ctx, actor, hand, yaw, pitch) {
        // Thrown things are used (bows, rods and tridents count when they let go).
        if let Some(h) = thrown
            && !matches!(
                h.name.as_str(),
                "minecraft:bow"
                    | "minecraft:crossbow"
                    | "minecraft:trident"
                    | "minecraft:fishing_rod"
            )
            && !done.events.is_empty()
        {
            crate::stats::item(ctx, actor.uuid, crate::stats::USED, h.stack.item, 1);
        }
        return done;
    }
    if let Some(done) = crate::mobs::eggs::use_in_air(ctx, actor, hand, yaw, pitch) {
        return done;
    }
    if let Some(h) = crate::held::held(ctx, actor.uuid, hand)
        && crate::vehicles::place_boat(ctx, &mut outcome, actor, hand, &h.name, yaw, pitch)
    {
        return outcome;
    }
    if crate::maps::use_held(ctx, &mut outcome, actor, hand, None) {
        return outcome;
    }
    let held = ctx
        .inventories
        .lock()
        .expect("inventories lock")
        .get(&actor.uuid)
        .and_then(|h| h.held(hand).cloned());
    let Some(name) = held
        .as_ref()
        .and_then(|h| ctx.items.name(h.item))
        .map(str::to_owned)
    else {
        return outcome;
    };
    let fluid = match name.as_str() {
        "minecraft:bucket" => None,
        "minecraft:water_bucket" => Some("minecraft:water"),
        // Aquatic (Claude): a bucket with a creature in it pours water.
        n if crate::mobs::aqua::bucket_species(n).is_some() => Some("minecraft:water"),
        "minecraft:lava_bucket" => Some("minecraft:lava"),
        _ => return outcome,
    };
    let Some((hit, face)) = look_at(ctx, actor, yaw, pitch, fluid.is_none()) else {
        return outcome;
    };
    if !crate::adventure::may_interact(ctx, actor.uuid, hit)
        || !crate::adventure::may_use_on(ctx, &actor, held.as_ref(), hit)
    {
        return outcome;
    }
    let blocks = &ctx.data.blocks;
    let state_at = |p: Position| ctx.world.block(p, blocks).unwrap_or(0);
    let mut level = WorldLevel::new(ctx);
    let mut poured: Option<Position> = None;
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
            let fits = |s: u32| ctx.shapes.is_air(s) || ctx.rules.is_replaceable(s);
            let logs = fluid == "minecraft:water"
                && blocks.property(state, "waterlogged") == Some("false");
            // In the Nether the water is gone at once; the bucket is empty.
            let gone = (fluid == "minecraft:water" && mc_tick::Level::ultrawarm(&level))
                .then(|| {
                    if logs || fits(state) {
                        hit
                    } else {
                        offset(hit, face)
                    }
                })
                .filter(|&at| at == hit || fits(state_at(at)));
            let placed = if let Some(at) = gone {
                mc_tick::Level::sponge_dried(&mut level, crate::level::to_pos(at));
                poured = Some(at);
                true
            } else if logs {
                poured = Some(hit);
                level.set_block(hit, crate::campfires::waterlogged(ctx, hit, state))
            } else {
                let at = if fits(state) { hit } else { offset(hit, face) };
                let there = state_at(at);
                let same = blocks.block_name(there) == Some(fluid)
                    && blocks.property(there, "level") == Some("0");
                if !fits(there) || same {
                    false
                } else {
                    poured = Some(at);
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
    // Aquatic (Claude): the creature swims out.
    if let (Some(at), Some(stack)) = (poured, held.as_ref()) {
        crate::mobs::aqua::release(ctx, &name, stack, at);
    }
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

/// A sign line as the editor sent it: formatting codes do not survive, as in
/// the game.
pub fn strip_sign_formatting(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{a7}' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Text written in the sign editor, as the text filter judged it (an editor
/// with text filtering on only ever stores the filtered text). Returns what
/// viewers must be told.
pub fn sign_update(
    ctx: &ServerContext,
    actor: Actor,
    position: Position,
    front: bool,
    lines: &[FilteredText],
    editor_filtering: bool,
) -> Option<Event> {
    if lines.len() != 4 {
        return None;
    }
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
    let key = if front { "front_text" } else { "back_text" };
    let mut side = match be.data.get(key) {
        Some(mc_nbt::Tag::Compound(c)) => c.clone(),
        _ => sign_side(Default::default()),
    };
    let lines_of = |text: &dyn Fn(&FilteredText) -> String| {
        mc_nbt::Tag::List(lines.iter().map(|l| mc_nbt::Tag::String(text(l))).collect())
    };
    let shown = lines_of(&|l| l.filtered_or_empty());
    if editor_filtering {
        side.insert("messages", shown);
        side.remove("filtered_messages");
    } else {
        let raw = lines_of(&|l| l.raw.clone());
        // Stored only when the filter changed something, as in the game.
        if raw == shown {
            side.remove("filtered_messages");
        } else {
            side.insert("filtered_messages", shown);
        }
        side.insert("messages", raw);
    }
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
pub(crate) fn sign_entity(block: &str) -> Option<&'static str> {
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
    fn player_box_is_its_cell_column() {
        let me = pose(-9.5, -60.0, 10.5);
        let cube = |x: i32, y: i32, z: i32| {
            let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
            mc_entity::Aabb {
                min: [x, y, z],
                max: [x + 1.0, y + 1.0, z + 1.0],
            }
        };
        let b = super::player_box(&me, false);
        assert!(overlap(&cube(-10, -60, 10), &b));
        assert!(overlap(&cube(-10, -59, 10), &b));
        assert!(!overlap(&cube(-10, -61, 10), &b));
        assert!(!overlap(&cube(-8, -60, 10), &b));
        assert!(!overlap(&cube(-10, -58, 10), &b));
        // Sneaking, the head is lower.
        let low = pose(-9.5, -59.6, 10.5);
        assert!(overlap(
            &cube(-10, -58, 10),
            &super::player_box(&low, false)
        ));
        assert!(!overlap(
            &cube(-10, -58, 10),
            &super::player_box(&low, true)
        ));
    }

    #[test]
    fn blocks_go_by_their_shape_and_not_into_mobs() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // Standing in the top half of the cell: a slab fits under the feet,
        // a full block does not.
        crate::testkit::join(&ctx, 1, 10, [0.5, -59.5, 0.5], 0);
        let me = Actor {
            uuid: 1,
            entity_id: 10,
            game_mode: 0,
            pose: pose(0.5, -59.5, 0.5),
            sneaking: false,
        };
        let hold = |name: &str| {
            crate::testkit::wear(
                &ctx,
                1,
                36,
                crate::testkit::stack(&ctx, name.trim_start_matches("minecraft:"), &[]),
            )
        };
        let floor = Position::new(0, -61, 0);
        hold("minecraft:stone");
        use_item_on(&ctx, me, 0, floor, 1, [0.5, 1.0, 0.5], 1);
        assert_eq!(crate::testkit::block(&ctx, [0, -60, 0]), "minecraft:air");
        hold("minecraft:oak_slab");
        use_item_on(&ctx, me, 0, floor, 1, [0.5, 1.0, 0.5], 2);
        assert_eq!(
            crate::testkit::block(&ctx, [0, -60, 0]),
            "minecraft:oak_slab"
        );
        // A fence reaches 1.5 up: not under someone standing half a block
        // above its cell.
        hold("minecraft:oak_fence");
        let side = Position::new(1, -61, 0);
        let me_up = Actor {
            pose: pose(1.5, -59.5, 0.5),
            ..me
        };
        use_item_on(&ctx, me_up, 0, side, 1, [0.5, 1.0, 0.5], 3);
        assert_eq!(crate::testkit::block(&ctx, [1, -60, 0]), "minecraft:air");
        // Not into a cow; next to it, yes.
        let cow = crate::mobs::new_mob(&ctx, mc_entity::Species::Cow, [3.5, -60.0, 0.5]);
        crate::tick::lock(&ctx.entities).push(cow);
        hold("minecraft:stone");
        use_item_on(&ctx, me, 0, Position::new(3, -61, 0), 1, [0.5, 1.0, 0.5], 4);
        assert_eq!(crate::testkit::block(&ctx, [3, -60, 0]), "minecraft:air");
        use_item_on(&ctx, me, 0, Position::new(5, -61, 0), 1, [0.5, 1.0, 0.5], 5);
        assert_eq!(crate::testkit::block(&ctx, [5, -60, 0]), "minecraft:stone");
        // An item on the ground does not stop a block.
        let stick = ctx.items.id("minecraft:stick").unwrap();
        crate::tick::spawn_item_at(&ctx, [-1.5, -60.0, 0.5], stick, 1);
        hold("minecraft:stone");
        use_item_on(
            &ctx,
            me,
            0,
            Position::new(-2, -61, 0),
            1,
            [0.5, 1.0, 0.5],
            6,
        );
        assert_eq!(crate::testkit::block(&ctx, [-2, -60, 0]), "minecraft:stone");
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
        dig(&ctx, actor, dig::START, at, 1, 1);
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

    /// Puts the two halves (`lower`, `upper` property values of `key`) of
    /// `block` at x=1, breaks `half` as `actor` long enough after starting,
    /// and gives the items it dropped, wherever they went.
    fn break_two_halves(
        ctx: &ServerContext,
        actor: Actor,
        block: &str,
        key: &str,
        [first, second]: [(&str, Position); 2],
        broken: usize,
    ) -> Vec<(i32, i32)> {
        let blocks = &ctx.data.blocks;
        let mut level = WorldLevel::new(ctx);
        for (value, at) in [first, second] {
            let state = blocks.state_id(block, &[(key, value)]).unwrap();
            level.set_block(at, state);
            assert_eq!(ctx.world.block(at, blocks), Some(state));
        }
        let at = [first, second][broken].1;
        dig(ctx, actor, dig::START, at, 1, 1);
        ctx.time.lock().unwrap().age += 1000;
        let outcome = dig(ctx, actor, dig::FINISH, at, 1, 1);
        for (_, at) in [first, second] {
            assert!(blocks.is_air(ctx.world.block(at, blocks).unwrap()));
        }
        let mut got: Vec<(i32, i32)> = outcome
            .drops
            .iter()
            .map(|(_, d)| (d.item, d.count))
            .collect();
        got.extend(
            ctx.entities
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| e.kind.as_stack().map(|s| (s.item, s.count))),
        );
        got
    }

    const LOWER: Position = Position { x: 1, y: -60, z: 0 };
    const UPPER: Position = Position { x: 1, y: -59, z: 0 };

    #[test]
    fn a_door_drops_once_whichever_half_breaks() {
        for broken in [0, 1] {
            let Some((ctx, actor)) = with_flint(0) else {
                eprintln!("skipped: no game data");
                return;
            };
            let door = ctx.items.id("minecraft:oak_door").unwrap();
            let halves = [("lower", LOWER), ("upper", UPPER)];
            let got = break_two_halves(&ctx, actor, "minecraft:oak_door", "half", halves, broken);
            assert_eq!(got, [(door, 1)], "breaking half {broken}");
        }
    }

    #[test]
    fn the_tool_in_hand_decides_the_drop() {
        let Some((ctx, actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let item = |n: &str| ctx.items.id(&format!("minecraft:{n}")).unwrap();
        let mut sequence = 0;
        let mut mine = |block: &str, tool: &str, enchants: &[(&str, i32)]| {
            crate::testkit::wear(&ctx, 7, 36, crate::testkit::stack(&ctx, tool, enchants));
            let state = ctx.data.blocks.default_state(block).unwrap();
            WorldLevel::new(&ctx).set_block(LOWER, state);
            sequence += 1;
            // Shears take leaves at once, on the first swing.
            let first = dig(&ctx, actor, dig::START, LOWER, 1, sequence);
            ctx.time.lock().unwrap().age += 1000;
            let last = dig(&ctx, actor, dig::FINISH, LOWER, 1, sequence);
            first
                .drops
                .iter()
                .chain(&last.drops)
                .map(|(_, d)| (d.item, d.count))
                .collect::<Vec<_>>()
        };
        let silk = [("silk_touch", 1)];
        assert_eq!(
            mine("minecraft:stone", "diamond_pickaxe", &[]),
            [(item("cobblestone"), 1)]
        );
        assert_eq!(
            mine("minecraft:stone", "diamond_pickaxe", &silk),
            [(item("stone"), 1)]
        );
        assert!(mine("minecraft:glass", "diamond_pickaxe", &[]).is_empty());
        assert_eq!(
            mine("minecraft:glass", "diamond_pickaxe", &silk),
            [(item("glass"), 1)]
        );
        assert_eq!(
            mine("minecraft:oak_leaves", "shears", &[]),
            [(item("oak_leaves"), 1)]
        );
    }

    #[test]
    fn water_poured_in_the_nether_evaporates() {
        let Some((overworld, mut actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let nether = overworld.in_dimension("minecraft:the_nether").unwrap();
        actor.pose = Pose {
            pitch: 90.0,
            ..pose(0.5, 70.0, 0.5)
        };
        for ctx in [&overworld, &nether] {
            let blocks = &ctx.data.blocks;
            let mut level = WorldLevel::new(ctx);
            level.set_block(
                Position::new(0, 69, 0),
                blocks.default_state("minecraft:stone").unwrap(),
            );
            for y in 70..73 {
                level.set_block(Position::new(0, y, 0), ctx.shapes.air());
            }
            let bucket = ctx.items.id("minecraft:water_bucket").unwrap();
            let mut hands = Hands::default();
            hands.slots[36] = Some(ItemStack::new(bucket, 1));
            ctx.inventories.lock().unwrap().insert(7, hands);
            use_item(ctx, actor, 0, 0.0, 90.0);
            let held = ctx.inventories.lock().unwrap()[&7].slots[36].clone();
            assert_eq!(
                held.map(|s| s.item),
                ctx.items.id("minecraft:bucket"),
                "{}",
                ctx.world.dimension
            );
        }
        assert_eq!(
            crate::testkit::block(&overworld, [0, 70, 0]),
            "minecraft:water"
        );
        assert_eq!(crate::testkit::block(&nether, [0, 70, 0]), "minecraft:air");
    }

    #[test]
    fn a_door_top_drops_nothing_in_creative_or_without_the_tool() {
        let Some((ctx, mut actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        actor.game_mode = CREATIVE;
        let halves = [("lower", LOWER), ("upper", UPPER)];
        assert!(break_two_halves(&ctx, actor, "minecraft:oak_door", "half", halves, 1).is_empty());
        let (ctx, actor) = with_flint(0).unwrap();
        assert!(break_two_halves(&ctx, actor, "minecraft:iron_door", "half", halves, 1).is_empty());
    }

    #[test]
    fn a_bed_and_a_tall_flower_drop_from_their_other_half() {
        let Some((ctx, actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        // Facing south, the head is south (+z) of the foot.
        let bed = ctx.items.id("minecraft:red_bed").unwrap();
        let blocks = &ctx.data.blocks;
        let mut level = WorldLevel::new(&ctx);
        let (foot, head) = (LOWER, Position { z: 1, ..LOWER });
        for (part, at) in [("foot", foot), ("head", head)] {
            let state = blocks
                .state_id("minecraft:red_bed", &[("part", part), ("facing", "south")])
                .unwrap();
            level.set_block(at, state);
        }
        dig(&ctx, actor, dig::START, foot, 1, 1);
        ctx.time.lock().unwrap().age += 1000;
        let outcome = dig(&ctx, actor, dig::FINISH, foot, 1, 1);
        assert!(blocks.is_air(ctx.world.block(head, blocks).unwrap()));
        assert!(outcome.drops.is_empty());
        let dropped: Vec<_> = ctx
            .entities
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| e.kind.as_stack().map(|s| (s.item, s.count)))
            .collect();
        assert_eq!(dropped, [(bed, 1)]);

        let (ctx, actor) = with_flint(0).unwrap();
        let rose = ctx.items.id("minecraft:rose_bush").unwrap();
        let halves = [("lower", LOWER), ("upper", UPPER)];
        let got = break_two_halves(&ctx, actor, "minecraft:rose_bush", "half", halves, 1);
        assert_eq!(got, [(rose, 1)]);
    }

    #[test]
    fn tools_wear_with_the_blocks_they_break() {
        let Some((ctx, actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let held = |item: &str| {
            let id = ctx.items.id(item).unwrap();
            ctx.inventories.lock().unwrap().get_mut(&7).unwrap().slots[36] =
                Some(ItemStack::new(id, 1));
        };
        let damage = || {
            ctx.inventories.lock().unwrap()[&7].slots[36]
                .as_ref()
                .map(|s| s.damage)
        };
        let mine = |block: &str| {
            let at = Position::new(1, -60, 0);
            let state = ctx.data.blocks.default_state(block).unwrap();
            WorldLevel::new(&ctx).set_block(at, state);
            dig(&ctx, actor, dig::START, at, 1, 1);
            ctx.time.lock().unwrap().age += 1000;
            dig(&ctx, actor, dig::FINISH, at, 1, 1);
            assert!(
                ctx.data
                    .blocks
                    .is_air(ctx.world.block(at, &ctx.data.blocks).unwrap())
            );
        };
        held("minecraft:iron_pickaxe");
        mine("minecraft:stone");
        assert_eq!(damage(), Some(1));
        // A block that breaks at once costs nothing.
        mine("minecraft:short_grass");
        assert_eq!(damage(), Some(1));
        held("minecraft:diamond_sword");
        mine("minecraft:oak_planks");
        assert_eq!(damage(), Some(2));
        held("minecraft:shears");
        mine("minecraft:short_grass");
        assert_eq!(damage(), Some(1));
    }

    #[test]
    fn a_broken_furnace_gives_back_what_was_in_it() {
        let Some((ctx, actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        let id = |n: &str| ctx.items.id(n).unwrap();
        ctx.inventories.lock().unwrap().get_mut(&7).unwrap().slots[36] =
            Some(ItemStack::new(id("minecraft:iron_pickaxe"), 1));
        let at = Position::new(1, -60, 0);
        let furnace = ctx.data.blocks.default_state("minecraft:furnace").unwrap();
        WorldLevel::new(&ctx).set_block(at, furnace);
        crate::containers::placed(&ctx, at, furnace);
        let kind = crate::containers::kind_of("minecraft:furnace").unwrap();
        crate::containers::put(
            &ctx,
            at,
            kind,
            &[
                (0, Some(ItemStack::new(id("minecraft:raw_iron"), 5))),
                (1, Some(ItemStack::new(id("minecraft:coal"), 3))),
            ],
        );
        dig(&ctx, actor, dig::START, at, 1, 1);
        ctx.time.lock().unwrap().age += 1000;
        let outcome = dig(&ctx, actor, dig::FINISH, at, 1, 1);
        let mut got: Vec<(i32, i32)> = outcome
            .drops
            .iter()
            .map(|(_, d)| (d.item, d.count))
            .collect();
        got.sort();
        let mut want = vec![
            (id("minecraft:furnace"), 1),
            (id("minecraft:raw_iron"), 5),
            (id("minecraft:coal"), 3),
        ];
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn a_sword_breaks_nothing_in_creative() {
        let Some((ctx, mut actor)) = with_flint(0) else {
            eprintln!("skipped: no game data");
            return;
        };
        actor.game_mode = CREATIVE;
        let at = Position::new(1, -61, 0);
        let stone = ctx.data.blocks.default_state("minecraft:stone").unwrap();
        WorldLevel::new(&ctx).set_block(at, stone);
        let sword = ctx.items.id("minecraft:diamond_sword").unwrap();
        ctx.inventories.lock().unwrap().get_mut(&7).unwrap().slots[36] =
            Some(ItemStack::new(sword, 1));
        dig(&ctx, actor, dig::START, at, 1, 1);
        assert_eq!(ctx.world.block(at, &ctx.data.blocks), Some(stone));
        // A pickaxe, or an empty hand, breaks it at once.
        ctx.inventories.lock().unwrap().get_mut(&7).unwrap().slots[36] = None;
        dig(&ctx, actor, dig::START, at, 1, 1);
        assert!(
            ctx.data
                .blocks
                .is_air(ctx.world.block(at, &ctx.data.blocks).unwrap())
        );
    }
}
