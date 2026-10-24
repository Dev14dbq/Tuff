//! The tick thread: 20 game ticks a second, in the order vanilla documents
//! (minecraft.wiki, "Tick", game process):
//!
//! 1. increment the tick count;
//! 2. every 20 ticks, send the time to players;
//! 3. tick each dimension (for now: advance its clocks; scheduled ticks,
//!    block events and entities come with ROADMAP Ф3 and Ф5);
//! 4. handle what players asked for (digging, placing, commands);
//! 5. autosave every 6000 ticks.
//!
//! If the server falls more than 2 s behind, it stops trying to catch up.
//! `/tick freeze` stops step 3 (time stands still) until `/tick step` or
//! `/tick unfreeze`; requests are still handled, as in vanilla.
//! Movement and chat still go straight between connections; they move here
//! with entities (Ф5).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mc_command::text::translate;
use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::{ClockState, SetTime, TickingState};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::context::{Event, ServerContext};
use crate::interact::{self, Actor, Outcome};

pub const TICK: Duration = Duration::from_millis(50);
const MAX_LAG: Duration = Duration::from_secs(2);
const SEND_TIME_EVERY: u64 = 20;
const UNLOAD_EVERY: u64 = 600;

/// The world's age and clocks.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldTime {
    /// Ticks since the world was created (`Time` in level.dat).
    pub age: i64,
    pub clocks: Vec<Clock>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clock {
    /// Registry name, e.g. `minecraft:overworld`.
    pub name: String,
    /// Id in the `minecraft:world_clock` registry as we sent it.
    pub id: i32,
    pub ticks: i64,
    pub running: bool,
    /// Ticks per tick (`/time rate`), and the part of a tick gathered.
    pub rate: f32,
    pub fraction: f32,
}

impl WorldTime {
    pub fn packet(&self, with_clocks: bool) -> SetTime {
        SetTime {
            world_age: self.age,
            clocks: if with_clocks {
                self.clocks
                    .iter()
                    .map(|c| ClockState {
                        clock: c.id,
                        ticks: c.ticks,
                        fraction: c.fraction,
                        rate: if c.running { c.rate } else { 0.0 },
                    })
                    .collect()
            } else {
                Vec::new()
            },
        }
    }

    pub(crate) fn advance(&mut self) {
        self.age += 1;
        for clock in self.clocks.iter_mut().filter(|c| c.running) {
            clock.fraction += clock.rate;
            let whole = clock.fraction.floor();
            clock.ticks += whole as i64;
            clock.fraction -= whole;
        }
    }
}

/// `/tick` state.
#[derive(Clone, Debug, PartialEq)]
pub struct TickControl {
    /// Target ticks per second.
    pub rate: f32,
    pub frozen: bool,
    /// Ticks still to run while frozen.
    pub steps: u32,
    /// How long the last 100 ticks took (ns), as `/tick query` shows them.
    pub times: [u64; 100],
    /// The tick time smoothed as vanilla does (ms).
    pub smoothed_ms: f32,
    /// `/tick sprint`: running ticks without waiting.
    pub sprint: Option<Sprint>,
}

/// A `/tick sprint` in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Sprint {
    /// Ticks still to run.
    pub remaining: u32,
    /// Ticks run so far, and how long they took.
    pub done: u32,
    pub spent: Duration,
    /// Whether the game was frozen when the sprint began (it is again after).
    pub was_frozen: bool,
}

impl TickControl {
    /// Starts a sprint of `ticks` (unfreezing the game); a sprint already
    /// running is replaced. Says whether one was.
    pub fn start_sprint(&mut self, ticks: u32) -> bool {
        let before = self.sprint.take();
        let was_frozen = before.as_ref().map_or(self.frozen, |s| s.was_frozen);
        self.frozen = false;
        self.steps = 0;
        self.sprint = Some(Sprint {
            remaining: ticks,
            done: 0,
            spent: Duration::ZERO,
            was_frozen,
        });
        before.is_some()
    }

    /// Ends the sprint, as the game was before it, and gives its report.
    pub fn finish_sprint(&mut self) -> Option<Tag> {
        let sprint = self.sprint.take()?;
        self.frozen = sprint.was_frozen;
        let ms = sprint.spent.as_secs_f64() * 1000.0;
        let done = f64::from(sprint.done.max(1));
        let per_tick = ms / done;
        let per_second = if per_tick > 0.0 {
            1000.0 / per_tick
        } else {
            0.0
        };
        Some(translate(
            "commands.tick.sprint.report",
            vec![
                Tag::String(format!("{per_second:.2}")),
                Tag::String(format!("{per_tick:.2}")),
            ],
        ))
    }

    /// Counts a tick of the sprint that took `took`; its report when that
    /// was the last one.
    fn sprinted(&mut self, took: Duration) -> Option<Tag> {
        let sprint = self.sprint.as_mut()?;
        sprint.remaining = sprint.remaining.saturating_sub(1);
        sprint.done += 1;
        sprint.spent += took;
        if sprint.remaining == 0 {
            self.finish_sprint()
        } else {
            None
        }
    }
}

impl Default for TickControl {
    fn default() -> Self {
        Self {
            rate: 20.0,
            frozen: false,
            steps: 0,
            times: [0; 100],
            smoothed_ms: 0.0,
            sprint: None,
        }
    }
}

/// Something a player (or the console) asked for, done during the tick.
#[derive(Debug)]
pub enum Action {
    Command {
        line: String,
        source: crate::commands::Source,
    },
    Dig {
        status: i32,
        position: Position,
        face: i8,
        sequence: i32,
    },
    UseItemOn {
        /// 0 main hand, 1 off hand.
        hand: i32,
        position: Position,
        face: i32,
        cursor: [f32; 3],
        sequence: i32,
    },
    /// Using the held item without a block (buckets).
    UseItem { hand: i32, yaw: f32, pitch: f32 },
    /// A click on an entity: `attack` for the left one.
    /// What a rider says about their vehicle.
    Vehicle(crate::vehicles::Act),
    /// A rider opens the inventory of their mount.
    MountInventory,
    UseEntity {
        entity_id: i32,
        hand: i32,
        attack: bool,
        /// Where on the entity the click landed.
        at: Option<[f64; 3]>,
    },
    /// A command block's window was done (`mode`: 0 chain, 1 repeating, 2
    /// impulse; `flags`: 1 track output, 2 conditional, 4 always active).
    CommandBlock {
        position: Position,
        command: String,
        mode: i32,
        flags: u8,
    },
    /// A jigsaw block's window was done.
    SetJigsaw {
        position: Position,
        name: String,
        target: String,
        pool: String,
        final_state: String,
        joint: String,
        selection_priority: i32,
        placement_priority: i32,
    },
    /// Generate in a jigsaw block's window.
    GenerateJigsaw {
        position: Position,
        levels: i32,
        keep_jigsaws: bool,
    },
    /// A test block's window was done.
    SetTestBlock {
        position: Position,
        mode: i32,
        message: String,
    },
    /// A test instance block's window asked for something.
    TestInstance {
        position: Position,
        action: i32,
        test: Option<String>,
        size: [i32; 3],
        rotation: i32,
        ignore_entities: bool,
    },
    /// A structure block's window was done.
    SetStructure {
        position: Position,
        action: i32,
        mode: i32,
        name: String,
        offset: [i8; 3],
        size: [u8; 3],
        mirror: i32,
        rotation: i32,
        metadata: String,
        integrity: f32,
        seed: i64,
        flags: u8,
    },
    /// A command block minecart's window was done.
    CommandMinecart {
        entity_id: i32,
        command: String,
        track_output: bool,
    },
}

#[derive(Debug)]
pub struct Request {
    /// The dimension the player is in (index in `context::DIMENSIONS`).
    pub dim: usize,
    pub actor: Actor,
    pub action: Action,
    /// Gets the player's own answer; events for others are broadcast.
    pub reply: UnboundedSender<Outcome>,
}

/// Runs ticks until `stop` is set.
pub fn run(ctx: Arc<ServerContext>, requests: Receiver<Request>, stop: Arc<AtomicBool>) {
    let autosave = (ctx.config.autosave.as_millis() / TICK.as_millis()).max(1) as u64;
    let mut next = Instant::now();
    let mut count: u64 = 0;
    let mut slow_ticks = 0u64;
    let mut control = TickControl::default();
    while !stop.load(Ordering::Relaxed) {
        let interval = Duration::from_secs_f64(1.0 / f64::from(control.rate));
        let now = Instant::now();
        if control.sprint.is_some() {
            next = now;
        } else if now < next {
            std::thread::sleep(next - now);
        } else if now - next > MAX_LAG {
            let behind = now - next;
            warn!(
                "Can't keep up! Is the server overloaded? Running {}ms or {} ticks behind",
                behind.as_millis(),
                (behind.as_secs_f64() * f64::from(control.rate)) as u64
            );
            next = now;
        }
        next += interval;
        let started = Instant::now();
        count += 1;

        if count.is_multiple_of(SEND_TIME_EVERY) {
            let packet = lock(&ctx.time).packet(false);
            let _ = ctx.events.send(Event::Time(packet));
        }
        let dimensions = ctx.open_dimensions();
        let runs = !control.frozen || control.steps > 0;
        if control.frozen && control.steps > 0 {
            control.steps -= 1;
        }
        if runs {
            lock(&ctx.time).advance();
            crate::commands::run_functions(&ctx, &mut control);
            for d in &dimensions {
                run_scheduled_ticks_with(d, Some(&mut control));
                crate::precipitation::tick(d);
                crate::random_ticks::tick(d);
                crate::daylight::tick(d, lock(&d.time).age);
                crate::furnaces::tick(d);
                crate::brewing::tick(d);
                crate::campfires::tick(d);
                crate::jukeboxes::tick(d);
                crate::test_blocks::tick(d, lock(&d.time).age);
                crate::crafter::tick(d);
                crate::beacons::tick(d, count as i64);
                crate::conduits::tick(d, lock(&d.time).age);
                lock(&d.border).tick();
            }
            crate::effects::tick(&ctx, count as i64);
            crate::vitals::tick(&ctx);
            crate::stats::tick(&ctx);
            crate::commands::watch(&ctx);
            crate::worn::tick(&ctx);
            crate::projectiles::trident::tick_spins(&ctx);
            crate::projectiles::crossbow::tick_charging(&ctx);
            crate::vitals::tick_using(&ctx);
            crate::portals::tick(&ctx);
            crate::useblocks::tick(&ctx);
            let advance = ctx.game_rule_bool("advance_weather");
            let changes = lock(&ctx.weather).tick(advance);
            if changes != crate::weather::Changes::default() {
                let _ = ctx.events.send(Event::Weather(changes));
            }
        }

        while let Ok(request) = requests.try_recv() {
            let actor = request.actor.uuid;
            let here = ctx
                .dimension(request.dim)
                .unwrap_or_else(|| Arc::clone(&ctx));
            let ctx = &here;
            let mut outcome = handle(ctx, &mut control, request.actor, request.action);
            // Packets sent to the player come before the reply, as in
            // vanilla, so the reply then goes the same way.
            let after = outcome.events.iter().any(|e| match e {
                Event::Direct { uuid, .. }
                | Event::Message { uuid, .. }
                | Event::Animate { uuid, .. } => *uuid == actor,
                Event::Scoreboard(_) | Event::BiomesChanged { .. } => true,
                _ => false,
            });
            for event in std::mem::take(&mut outcome.events) {
                let _ = ctx.events.send(event);
            }
            if after {
                for line in std::mem::take(&mut outcome.messages) {
                    let _ = ctx.events.send(Event::Message {
                        uuid: actor,
                        line,
                        overlay: false,
                    });
                }
            }
            for (at, drop) in std::mem::take(&mut outcome.drops) {
                drop_item(ctx, at, drop);
            }
            let _ = request.reply.send(outcome);
        }
        for d in &dimensions {
            containers_changed(d);
        }

        if count.is_multiple_of(UNLOAD_EVERY) {
            for d in &dimensions {
                let dropped = d.world.unload_unused();
                crate::persist::unloaded(d, &dropped);
                if !dropped.is_empty() {
                    debug!(
                        "unloaded {} chunks of {}, {} in memory",
                        dropped.len(),
                        d.dimension_name(),
                        d.world.loaded_chunks()
                    );
                }
            }
        }
        if count.is_multiple_of(autosave) && ctx.world.autosaving() {
            crate::save(&ctx);
        }
        *lock(&ctx.ticking) = mc_protocol::packets::play::TickingState {
            tick_rate: control.rate,
            frozen: control.frozen,
        };
        let took = started.elapsed();
        if let Some(report) = control.sprinted(took) {
            let _ = ctx.events.send(Event::Chat(report));
            let _ = ctx.events.send(Event::TickingState(TickingState {
                tick_rate: control.rate,
                frozen: control.frozen,
            }));
            *lock(&ctx.ticking) = TickingState {
                tick_rate: control.rate,
                frozen: control.frozen,
            };
        }
        let nanos = took.as_nanos() as u64;
        control.times[(count % 100) as usize] = nanos;
        control.smoothed_ms = control.smoothed_ms * 0.8 + nanos as f32 / 1.0e6 * 0.2;
        if took > interval {
            slow_ticks += 1;
            if slow_ticks.is_power_of_two() {
                info!(
                    "tick {count} took {} ms ({slow_ticks} slow so far)",
                    took.as_millis()
                );
            }
        }
    }
}

/// The waiting scheduled ticks as chunk files keep them, by chunk.
pub(crate) fn ticks_by_chunk(
    ctx: &ServerContext,
) -> std::collections::HashMap<(i32, i32), crate::world::PendingTicks> {
    let now = lock(&ctx.time).age;
    let ticks = lock(&ctx.ticks);
    let mut out: std::collections::HashMap<(i32, i32), crate::world::PendingTicks> =
        Default::default();
    for (pos, kind, time, priority) in ticks.blocks.pending() {
        let Some(id) = ctx.shapes.index_name(kind) else {
            continue;
        };
        let tick = crate::world::SavedTick {
            fluid: false,
            id: id.to_owned(),
            pos,
            delay: time - now,
            priority,
        };
        out.entry((pos.x >> 4, pos.z >> 4))
            .or_default()
            .0
            .push(tick.tag());
    }
    for (pos, kind, time, priority) in ticks.fluids.pending() {
        let lava = kind == mc_tick::fluids::Fluid::Lava as u32;
        let still = ctx
            .world
            .block(crate::level::to_position(pos), &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.property(s, "level"))
            == Some("0");
        let id = match (lava, still) {
            (false, true) => "minecraft:water",
            (false, false) => "minecraft:flowing_water",
            (true, true) => "minecraft:lava",
            (true, false) => "minecraft:flowing_lava",
        };
        let tick = crate::world::SavedTick {
            fluid: true,
            id: id.to_owned(),
            pos,
            delay: time - now,
            priority,
        };
        out.entry((pos.x >> 4, pos.z >> 4))
            .or_default()
            .1
            .push(tick.tag());
    }
    out
}

/// Ticks stored in chunks just read go back in the queues.
fn queue_saved_ticks(ctx: &ServerContext, ticks: &mut mc_tick::ScheduledTicks, now: i64) {
    for t in ctx.world.take_saved_ticks() {
        if t.fluid {
            let fluid = if t.id.ends_with("lava") {
                mc_tick::fluids::Fluid::Lava
            } else {
                mc_tick::fluids::Fluid::Water
            };
            ticks
                .fluids
                .schedule(t.pos, fluid as u32, now + t.delay, t.priority);
        } else if let Some(state) = ctx.data.blocks.default_state(&t.id) {
            let kind = ctx.shapes.block_index(state);
            ticks
                .blocks
                .schedule(t.pos, kind, now + t.delay, t.priority);
        }
    }
}

/// Due block ticks, then due fluid ticks, then entities; what they change
/// goes to everyone.
#[cfg(test)]
pub(crate) fn run_scheduled_ticks(ctx: &ServerContext) {
    run_scheduled_ticks_with(ctx, None);
}

/// [`run_scheduled_ticks`], with the tick's state for command blocks.
pub(crate) fn run_scheduled_ticks_with<'a>(
    ctx: &'a ServerContext,
    control: Option<&'a mut TickControl>,
) {
    crate::persist::load_stored(ctx);
    let now = lock(&ctx.time).age;
    for (p, id) in ctx.world.take_woken() {
        match id.as_str() {
            "minecraft:hopper" => crate::hoppers::register(ctx, p),
            "minecraft:daylight_detector" => crate::daylight::register(ctx, p),
            "minecraft:brewing_stand" => crate::brewing::wake(ctx, p),
            "minecraft:campfire" => crate::campfires::wake(ctx, p),
            "minecraft:crafter" => crate::crafter::wake(ctx, p),
            "minecraft:brushable_block" => crate::archaeology::loaded(ctx, p),
            "minecraft:jukebox" => crate::jukeboxes::wake(ctx, p),
            _ => crate::furnaces::wake(ctx, p),
        }
    }
    let relit = ctx.world.take_relit();
    if !relit.is_empty() {
        let _ = ctx.events.send(Event::LightChanged(relit));
    }
    let due = {
        let mut ticks = lock(&ctx.ticks);
        queue_saved_ticks(ctx, &mut ticks, now);
        // Flow new chunks pass on to chunks players already have.
        for (pos, fluid) in ctx.world.take_live_ticks() {
            let delay = if fluid == mc_tick::fluids::Fluid::Lava as u32 {
                30
            } else {
                5
            };
            ticks.fluids.schedule(pos, fluid, now + delay, 0);
        }
        let blocks = ticks.blocks.take_due(now);
        let fluids = ticks.fluids.take_due(now);
        (blocks, fluids)
    };
    let mut level = crate::level::WorldLevel::new(ctx);
    level.control = control;
    let logic = level.logic();
    // Detector rails look for minecarts in their ticks.
    level.minecarts = minecart_boxes(&lock(&ctx.entities));
    for (pos, block) in due.0 {
        mc_tick::block_tick(&mut level, logic, pos, block);
    }
    for (pos, fluid) in due.1 {
        mc_tick::fluids::tick(&mut level, logic, pos, fluid);
    }
    run_block_events(ctx, &mut level);
    tick_entities(ctx, &mut level);
    crate::trading::tick_spawner(ctx, lock(&ctx.time).age);
    crate::mobs::tick(ctx, &mut level);
    crate::bosses::tick(ctx, &mut level);
    press_plates(ctx, &mut level);
    mc_tick::pistons::tick(&mut level, logic);
    crate::hoppers::tick(ctx, &mut level);
    crate::hoppers::tick_carts(ctx);
    let (changes, spawned) = level.finish();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
    if !spawned.is_empty() {
        let mut entities = lock(&ctx.entities);
        for entity in spawned {
            let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
            entities.push(entity);
        }
    }
    crate::archaeology::tick(ctx);
    crate::maps::tick(ctx);
}

/// Ids, boxes and container kinds of the minecarts.
type CartBox = (i32, mc_entity::Aabb, Option<crate::containers::Kind>);

fn minecart_boxes(entities: &[mc_entity::Entity]) -> Vec<CartBox> {
    entities
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            mc_entity::Kind::Vehicle(v)
                if matches!(v.craft, mc_entity::extra::Craft::Minecart(_)) =>
            {
                let kind = crate::containers::entity_kind(&crate::vehicles::item_of(&v.craft));
                Some((e.id, e.aabb(), kind))
            }
            _ => None,
        })
        .collect()
}

/// Block events queued so far (and those they queue), each once.
fn run_block_events(ctx: &ServerContext, level: &mut crate::level::WorldLevel) {
    use mc_tick::Level as _;
    let logic = level.logic();
    for _ in 0..64 {
        let events = std::mem::take(&mut *lock(&ctx.block_events));
        if events.is_empty() {
            return;
        }
        for (p, block, action, param) in events {
            if ctx.shapes.block_index(level.block(p)) != block {
                continue;
            }
            let name = ctx
                .shapes
                .block_name(level.block(p))
                .unwrap_or("")
                .to_owned();
            if mc_tick::pistons::block_event(level, logic, p, action, param) {
                let _ = ctx.events.send(Event::BlockAction {
                    position: crate::level::to_position(p),
                    action,
                    param,
                    block: ctx.data.builtin.id("minecraft:block", &name).unwrap_or(0),
                });
            }
        }
    }
}

/// Comparators look at containers whose contents changed (even while the
/// game is frozen: they only schedule their ticks).
pub(crate) fn containers_changed(ctx: &ServerContext) {
    let mut changed = std::mem::take(&mut *lock(&ctx.container_updates));
    // Entities holding items have no comparators next to them.
    changed.retain(|&p| crate::containers::entity_of(p).is_none());
    if changed.is_empty() {
        return;
    }
    for &p in &changed {
        crate::furnaces::wake(ctx, p);
        crate::brewing::wake(ctx, p);
    }
    let mut level = crate::level::WorldLevel::new(ctx);
    let logic = level.logic();
    for p in changed {
        let at = crate::level::to_pos(p);
        mc_tick::redstone::container_changed(&mut level, logic, at);
        // A trapped chest's power follows its viewers.
        if ctx
            .world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            == Some("minecraft:trapped_chest")
        {
            mc_tick::redstone::update_neighbors_at(&mut level, logic, at);
            mc_tick::redstone::update_neighbors_at(
                &mut level,
                logic,
                at.offset(mc_tick::Dir::Down),
            );
        }
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

/// Players standing in pressure plates and string press them (other
/// entities do as they move).
fn press_plates(ctx: &ServerContext, level: &mut crate::level::WorldLevel) {
    use mc_tick::Level as _;
    let at: Vec<mc_tick::BlockPos> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter(|p| p.game_mode != 3)
        .filter_map(|p| p.pose)
        .map(|pose| {
            mc_tick::BlockPos::new(
                pose.x.floor() as i32,
                pose.y.floor() as i32,
                pose.z.floor() as i32,
            )
        })
        .collect();
    let logic = level.logic();
    for p in at {
        mc_tick::redstone::touch_ore(level, logic, p.offset(mc_tick::Dir::Down));
        let name = ctx.shapes.block_name(level.block(p)).unwrap_or("");
        if name.ends_with("_pressure_plate") {
            mc_tick::redstone::entity_inside(level, logic, p);
        } else if name == "minecraft:tripwire" {
            mc_tick::tripwire::entity_inside(level, logic, p);
        }
    }
}

/// Blocks as entities bump into them.
pub(crate) struct Solid<'a, 'b>(pub(crate) &'a crate::level::WorldLevel<'b>);

impl mc_entity::Collide for Solid<'_, '_> {
    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        use mc_tick::Level as _;
        let state = self.0.block(mc_tick::BlockPos::new(x, y, z));
        !mc_tick::gravity::free(self.0.logic(), state)
    }
}

/// Drops an item stack from a block: near its centre, with a little random
/// velocity, not to be picked up for 10 ticks (minecraft.wiki, "Item
/// (entity)").
pub fn drop_item(ctx: &ServerContext, at: mc_protocol::Position, drop: crate::loot::Drop) {
    let r = |spread: f64| {
        (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64 * spread - spread / 2.0
    };
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: mc_entity::Kind::stack(&drop.stack(), 10),
        pos: [
            f64::from(at.x) + 0.5 + r(0.5),
            f64::from(at.y) + 0.375 + r(0.5),
            f64::from(at.z) + 0.5 + r(0.5),
        ],
        vel: [r(0.2), 0.2, r(0.2)],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
}

/// Adds a new entity to the world and shows it to everyone.
pub fn spawn_entity(ctx: &ServerContext, entity: mc_entity::Entity) {
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
}

/// An item entity at an exact position (`/loot spawn`, leftovers a
/// player has no room for).
pub fn spawn_item_at(ctx: &ServerContext, pos: [f64; 3], item: i32, count: i32) {
    spawn_stack_at(
        ctx,
        pos,
        &mc_protocol::packets::play::ItemStack::new(item, count),
    );
}

/// [`spawn_item_at`] for a stack with components.
pub fn spawn_stack_at(
    ctx: &ServerContext,
    pos: [f64; 3],
    stack: &mc_protocol::packets::play::ItemStack,
) {
    let r = |spread: f64| {
        (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64 * spread - spread / 2.0
    };
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: mc_entity::Kind::stack(stack, 10),
        pos,
        vel: [r(0.2), 0.2, r(0.2)],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
}

/// A player throws a stack: from the eyes, along the view, not to be picked
/// up again for 40 ticks (minecraft.wiki, "Item (entity)").
pub fn throw_item(
    ctx: &ServerContext,
    pose: crate::context::Pose,
    stack: mc_protocol::packets::play::ItemStack,
) {
    throw_item_aged(ctx, pose, stack, 0);
}

/// [`throw_item`] for an item that starts `age` ticks old (items dropped
/// from the creative inventory last one minute instead of five).
pub fn throw_item_aged(
    ctx: &ServerContext,
    pose: crate::context::Pose,
    stack: mc_protocol::packets::play::ItemStack,
    age: u32,
) {
    let (yaw, pitch) = (
        f64::from(pose.yaw).to_radians(),
        f64::from(pose.pitch).to_radians(),
    );
    let speed = 0.3;
    // A little spread, so stacks thrown one after another do not overlap.
    let r = crate::util::random_f64;
    let (angle, spread) = (r() * std::f64::consts::TAU, r() * 0.02);
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: thrown_by(ctx, pose, &stack),
        pos: [pose.x, pose.y + 1.62 - 0.3, pose.z],
        vel: [
            -yaw.sin() * pitch.cos() * speed + angle.cos() * spread,
            -pitch.sin() * speed + 0.1 + (r() - r()) * 0.1,
            yaw.cos() * pitch.cos() * speed + angle.sin() * spread,
        ],
        on_ground: false,
        age,
        removed: false,
        tags: Vec::new(),
    };
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
}

/// A stack a player drops, with them as its `Thrower` (the player standing
/// where `pose` says).
fn thrown_by(
    ctx: &ServerContext,
    pose: crate::context::Pose,
    stack: &mc_protocol::packets::play::ItemStack,
) -> mc_entity::Kind {
    let mut kind = mc_entity::Kind::stack(stack, 40);
    if let mc_entity::Kind::Item { thrower, .. } = &mut kind {
        *thrower = lock(&ctx.players)
            .values()
            .find(|p| p.pose == Some(pose))
            .map(|p| p.profile.uuid);
    }
    kind
}

/// Drops a stack around a player in a random direction (a death scatters the
/// inventory like this).
pub fn scatter_item(
    ctx: &ServerContext,
    pose: crate::context::Pose,
    stack: mc_protocol::packets::play::ItemStack,
) {
    let r = || (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64;
    let (angle, speed) = (r() * std::f64::consts::TAU, r() * 0.5);
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: mc_entity::Kind::stack(&stack, 40),
        pos: [pose.x, pose.y + 1.32, pose.z],
        vel: [angle.sin() * speed, 0.2, angle.cos() * speed],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
}

/// How far around a player items are picked up: the player's box grown by
/// one block sideways and half a block up and down.
pub(crate) fn picks_up(pose: &crate::context::Pose, item: &mc_entity::Entity) -> bool {
    let b = item.aabb();
    pose.x - 1.3 < b.max[0]
        && pose.x + 1.3 > b.min[0]
        && pose.y - 0.5 < b.max[1]
        && pose.y + 2.3 > b.min[1]
        && pose.z - 1.3 < b.max[2]
        && pose.z + 1.3 > b.min[2]
}

/// Whether `pos` is in a chunk within the simulation distance of a player
/// (a square of chunks around them).
pub(crate) fn simulated_at(
    ctx: &ServerContext,
    players: &[(u128, i32, i32, crate::context::Pose)],
    pos: [f64; 3],
) -> bool {
    let d = i32::from(ctx.config.simulation_distance);
    let (cx, cz) = ((pos[0].floor() as i32) >> 4, (pos[2].floor() as i32) >> 4);
    players.iter().any(|(.., pose)| {
        let (px, pz) = ((pose.x.floor() as i32) >> 4, (pose.z.floor() as i32) >> 4);
        (cx - px).abs() <= d && (cz - pz).abs() <= d
    })
}

/// Falling blocks fall and land; items fall, slide, despawn and are picked up.
fn tick_entities(ctx: &ServerContext, level: &mut crate::level::WorldLevel) {
    let mut entities = std::mem::take(&mut *lock(&ctx.entities));
    if entities.is_empty() {
        return;
    }
    let logic = level.logic();
    let players: Vec<(u128, i32, i32, crate::context::Pose)> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            p.pose
                .map(|pose| (p.profile.uuid, p.entity_id, p.game_mode, pose))
        })
        .collect();
    let whos: Vec<crate::hits::Who> = players
        .iter()
        .map(|&(uuid, entity_id, mode, pose)| crate::hits::Who {
            uuid,
            entity_id,
            mode,
            pose,
        })
        .collect();
    let mut targets: Vec<crate::hits::Target> = entities
        .iter()
        .filter(|e| match &e.kind {
            mc_entity::Kind::Hung(_)
            | mc_entity::Kind::Vehicle(_)
            | mc_entity::Kind::Crystal(_) => true,
            // The dragon is hit in its parts.
            mc_entity::Kind::Mob(m) => m.dying.is_none() && m.dragon().is_none(),
            _ => false,
        })
        .map(|e| crate::hits::Target {
            id: e.id,
            aabb: e.aabb(),
            kind: match &e.kind {
                mc_entity::Kind::Mob(m) => Some(m.species.id()),
                _ => None,
            },
        })
        .collect();
    targets.extend(crate::bosses::part_targets(&entities));
    crate::decor::leash::tick(ctx);
    level.minecarts = minecart_boxes(&entities);
    let mut out = crate::hits::Out::default();
    let (mut moved, mut removed) = (Vec::new(), Vec::new());
    let mut gone = Vec::new();
    // Items that moved into another block look for stacks to join sooner.
    let mut crossed = Vec::new();
    let (mut blasts, mut fuses) = (std::mem::take(&mut *lock(&ctx.pending_blasts)), Vec::new());
    let now = lock(&ctx.time).age;
    let mut claimed = crate::trading::claimed_sites(&entities);
    // What villagers see: night for the bed, zombies to run from.
    let night = {
        let t = crate::mobs::day_time(ctx).rem_euclid(24000);
        ctx.world.dimension == "minecraft:overworld" && (12000..23000).contains(&t)
    };
    let threats: Vec<[f64; 3]> = entities
        .iter()
        .filter(|e| {
            !e.removed
                && matches!(&e.kind, mc_entity::Kind::Mob(m) if m.dying.is_none() && matches!(
                    m.species,
                    mc_entity::mob::Species::Zombie
                        | mc_entity::mob::Species::Husk
                        | mc_entity::mob::Species::Drowned
                        | mc_entity::mob::Species::ZombieVillager
                ))
        })
        .map(|e| e.pos)
        .collect();
    // Plates see entities where they are at that moment of the tick.
    level.entity_boxes = Some(entities.iter().map(|e| e.aabb()).collect());
    for (i, entity) in entities.iter_mut().enumerate() {
        let before = (entity.pos, entity.on_ground);
        let block_before = entity.block_pos();
        let before_vel = entity.vel;
        let extra = matches!(
            entity.kind,
            mc_entity::Kind::Projectile(_) | mc_entity::Kind::Hung(_) | mc_entity::Kind::Vehicle(_)
        );
        if extra {
            let shooter = match &entity.kind {
                mc_entity::Kind::Projectile(p) => p.mob,
                _ => None,
            };
            let mut t = crate::hits::Tick {
                ctx,
                players: &whos,
                targets: &targets,
                out: &mut out,
                shooter,
            };
            match entity.kind {
                mc_entity::Kind::Projectile(_) => crate::projectiles::tick(&mut t, entity),
                mc_entity::Kind::Hung(_) => crate::decor::tick(&mut t, entity),
                mc_entity::Kind::Vehicle(_) => crate::vehicles::tick(&mut t, entity),
                _ => {}
            }
        }
        match entity.kind.clone() {
            mc_entity::Kind::Projectile(_)
            | mc_entity::Kind::Hung(_)
            | mc_entity::Kind::Vehicle(_) => {}
            mc_entity::Kind::FallingBlock { state } => {
                mc_entity::fall(entity, &Solid(level));
                let suspicious = matches!(
                    ctx.data.blocks.block_name(state),
                    Some("minecraft:suspicious_sand" | "minecraft:suspicious_gravel")
                );
                // A fall breaks suspicious sand or gravel (CancelDrop). Past
                // 30 seconds in the air, the block item drops without its loot.
                if suspicious && (entity.on_ground || entity.age > 600) {
                    if !entity.on_ground
                        && entity.age > 600
                        && ctx.game_rule_bool("block_drops")
                        && let Some(item) = ctx
                            .data
                            .blocks
                            .block_name(state)
                            .and_then(|n| ctx.items.id(n))
                    {
                        spawn_item_at(ctx, entity.pos, item, 1);
                    }
                    entity.removed = true;
                } else if entity.on_ground || entity.age > 600 {
                    let (x, y, z) = entity.block_pos();
                    let at = mc_tick::BlockPos::new(x, y, z);
                    use mc_tick::Level as _;
                    if entity.on_ground && mc_tick::gravity::free(logic, level.block(at)) {
                        mc_tick::set_block(level, logic, at, state, mc_tick::Flags::ALL);
                    }
                    entity.removed = true;
                }
            }
            mc_entity::Kind::Tnt { fuse, owner } => {
                // Far from every player it does not tick, so it never goes
                // off (entities tick only within the simulation distance).
                if !simulated_at(ctx, &players, entity.pos) {
                    continue;
                }
                mc_entity::fall(entity, &Solid(level));
                if entity.on_ground {
                    entity.vel[0] *= 0.7;
                    entity.vel[1] *= -0.5;
                    entity.vel[2] *= 0.7;
                }
                let fuse = fuse - 1;
                if fuse <= 0 {
                    entity.removed = true;
                    blasts.push(crate::explosion::Blast::tnt(entity, owner));
                } else {
                    entity.kind = mc_entity::Kind::Tnt { fuse, owner };
                    fuses.push(crate::explosion::fuse_data(entity.id, fuse));
                }
            }
            mc_entity::Kind::Merchant(m) => {
                // They walk about where a player is near enough to see.
                let near = players.iter().any(|&(_, _, _, p)| {
                    (p.x - entity.pos[0]).powi(2) + (p.z - entity.pos[2]).powi(2) < 128.0 * 128.0
                });
                let mut ops = Vec::new();
                if near {
                    let ground = crate::mobs::Ground::new(ctx);
                    mc_entity::villager::step(
                        entity,
                        &ground,
                        now as u32,
                        !m.trading.is_empty(),
                        &mc_entity::villager::Env {
                            night,
                            threats: &threats,
                        },
                        &mut ops,
                    );
                } else {
                    mc_entity::fall(entity, &Solid(level));
                    entity.vel = [0.0; 3];
                }
                for op in ops {
                    match op {
                        mc_entity::villager::Op::Open(c) => {
                            crate::mobs::blocks::open_door(ctx, level, c, true)
                        }
                        mc_entity::villager::Op::Close(c) => {
                            crate::mobs::blocks::open_door(ctx, level, c, false)
                        }
                        mc_entity::villager::Op::Sleep(c) => {
                            crate::mobs::blocks::lie_down(ctx, level, entity, c)
                        }
                        mc_entity::villager::Op::Wake(c) => {
                            crate::mobs::blocks::get_up(ctx, level, entity.id, c)
                        }
                    }
                }
                fuses.extend(crate::trading::tick(ctx, level, entity, &mut claimed, now));
            }
            // Mobs have their own tick, and so do crystals.
            mc_entity::Kind::Mob(_) | mc_entity::Kind::Crystal(_) => {}
            mc_entity::Kind::ExperienceOrb { .. } => {
                mc_entity::fall_with(entity, &Solid(level), mc_entity::ORB_GRAVITY);
                if entity.on_ground {
                    entity.vel[0] *= 0.6;
                    entity.vel[2] *= 0.6;
                }
                crate::xp::tick(ctx, entity, &players);
            }
            mc_entity::Kind::Item { .. } => {
                crate::dropped::tick(ctx, level, entity, &players, &mut out.sync);
            }
        }
        if let Some(boxes) = level.entity_boxes.as_mut() {
            boxes[i] = entity.aabb();
        }
        if let Some(m) = level
            .minecarts
            .iter_mut()
            .find(|(id, _, _)| *id == entity.id)
        {
            m.1 = entity.aabb();
        }
        if !entity.removed {
            let (x, y, z) = entity.block_pos();
            let at = mc_tick::BlockPos::new(x, y, z);
            use mc_tick::Level as _;
            if matches!(&entity.kind, mc_entity::Kind::Vehicle(v) if matches!(v.craft, mc_entity::extra::Craft::Minecart(_)))
            {
                mc_tick::rails::entity_inside(level, logic, at);
            }
            // Walking on redstone ore lights it.
            if entity.on_ground {
                mc_tick::redstone::touch_ore(level, logic, at.offset(mc_tick::Dir::Down));
            }
            match ctx.shapes.block_name(level.block(at)) {
                Some(n) if n.ends_with("_pressure_plate") => {
                    mc_tick::redstone::entity_inside(level, logic, at);
                }
                Some("minecraft:tripwire") => mc_tick::tripwire::entity_inside(level, logic, at),
                _ => {}
            }
        }
        if entity.removed {
            removed.push(entity.id);
            gone.push(entity.uuid);
        } else if matches!(entity.kind, mc_entity::Kind::Item { .. }) {
            if entity.block_pos() != block_before {
                crossed.push(entity.id);
            }
        } else if !extra && (entity.pos, entity.on_ground) != before {
            // Clients move items and orbs themselves: as vanilla's tracker,
            // they hear where these are only once a second (and when they
            // come to rest), or the two simulations fight and they shake.
            let simulated = matches!(
                entity.kind,
                mc_entity::Kind::Item { .. } | mc_entity::Kind::ExperienceOrb { .. }
            );
            let rests = entity.vel == [0.0; 3] && before_vel != [0.0; 3];
            if !simulated || entity.age % 20 == 0 || rests {
                moved.push((entity.id, entity.pos, entity.on_ground));
                if simulated {
                    crate::fx::emit(
                        ctx,
                        crate::fx::Fx::Motion(mc_protocol::packets::play::SetEntityMotion {
                            entity_id: entity.id,
                            velocity: entity.vel,
                        }),
                    );
                }
            }
        }
    }
    level.entity_boxes = None;
    blasts.append(&mut out.blasts);
    if let Some(fire) = ctx.data.blocks.default_state("minecraft:fire") {
        use mc_tick::Level as _;
        for c in std::mem::take(&mut out.fires) {
            let at = mc_tick::BlockPos::new(c[0], c[1], c[2]);
            if level.block(at) == ctx.shapes.air() {
                mc_tick::set_block(level, logic, at, fire, mc_tick::Flags::ALL);
            }
        }
    }
    if entities
        .iter()
        .any(|e| e.age % 20 == 0 && matches!(e.kind, mc_entity::Kind::ExperienceOrb { .. }))
    {
        crate::xp::merge(&mut entities);
        for e in entities
            .iter()
            .filter(|e| e.removed && matches!(e.kind, mc_entity::Kind::ExperienceOrb { .. }))
        {
            if !removed.contains(&e.id) {
                removed.push(e.id);
                gone.push(e.uuid);
            }
        }
    }
    let (joined, grown) = crate::dropped::merge(ctx, &mut entities, &crossed);
    for i in joined {
        removed.push(entities[i].id);
        gone.push(entities[i].uuid);
    }
    for i in grown {
        if let Some(p) = crate::dropped::metadata(&entities[i]) {
            let _ = ctx.events.send(Event::Fx(crate::fx::Fx::Data(p)));
        }
    }
    for (id, uuid) in crate::explosion::explode_all(ctx, level, &mut entities, &blasts) {
        removed.push(id);
        gone.push(uuid);
    }
    if !fuses.is_empty() {
        let _ = ctx.events.send(Event::EntitiesData(fuses));
    }
    if !out.data.is_empty() {
        let _ = ctx
            .events
            .send(Event::EntitiesData(std::mem::take(&mut out.data)));
    }
    if !out.sync.is_empty() {
        let _ = ctx
            .events
            .send(Event::EntitySync(std::mem::take(&mut out.sync)));
    }
    for g in std::mem::take(&mut out.gusts) {
        crate::projectiles::wind::burst(ctx, &mut entities, &g);
    }
    for hit in std::mem::take(&mut out.hits) {
        crate::hits::apply(ctx, &mut entities, &mut removed, &mut gone, hit);
    }
    for d in std::mem::take(&mut out.doses) {
        if let Some(e) = entities.iter_mut().find(|e| e.id == d.target && !e.removed) {
            crate::mob_effects::dose(ctx, e, &d.effects, d.strength, d.splash, d.by);
        }
    }
    for entity in out.spawn.drain(..) {
        let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
        lock(&ctx.entities).push(entity);
    }
    entities.retain(|e| !e.removed);
    if !gone.is_empty() {
        let mut board = lock(&ctx.scoreboard);
        for uuid in gone {
            for p in board.forget(&crate::util::uuid_string(uuid)) {
                let _ = ctx.events.send(Event::Scoreboard(p));
            }
        }
    }
    let mut stored = lock(&ctx.entities);
    entities.append(&mut stored);
    *stored = entities;
    drop(stored);
    crate::command_blocks::run_carts(ctx, level, std::mem::take(&mut out.carts));
    if !moved.is_empty() {
        let _ = ctx.events.send(Event::EntitiesMoved(moved));
    }
    if !removed.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(removed));
    }
}

fn handle(ctx: &ServerContext, control: &mut TickControl, actor: Actor, action: Action) -> Outcome {
    match action {
        Action::Command { line, source } => {
            let output = crate::commands::execute(ctx, control, &source, &line);
            Outcome {
                events: output.events,
                messages: output.lines,
                ..Outcome::default()
            }
        }
        Action::Dig {
            status,
            position,
            face,
            sequence,
        } => interact::dig(ctx, actor, status, position, face, sequence),
        Action::UseItemOn {
            hand,
            position,
            face,
            cursor,
            sequence,
        } => interact::use_item_on(ctx, actor, hand, position, face, cursor, sequence),
        Action::UseItem { hand, yaw, pitch } => interact::use_item(ctx, actor, hand, yaw, pitch),
        Action::Vehicle(act) => crate::vehicles::act(ctx, actor, act),
        Action::MountInventory => {
            let mut outcome = Outcome::default();
            crate::mobs::chest::open_riding(ctx, &actor, &mut outcome);
            outcome
        }
        Action::UseEntity {
            entity_id,
            hand,
            attack,
            at,
        } => crate::hits::use_entity(ctx, actor, entity_id, hand, attack, at),
        Action::CommandBlock {
            position,
            command,
            mode,
            flags,
        } => Outcome {
            messages: crate::command_blocks::configure(
                ctx,
                actor.uuid,
                actor.game_mode,
                position,
                &command,
                mode,
                flags,
            )
            .into_iter()
            .collect(),
            ..Outcome::default()
        },
        Action::SetJigsaw {
            position,
            name,
            target,
            pool,
            final_state,
            joint,
            selection_priority,
            placement_priority,
        } => {
            crate::structure_blocks::set_jigsaw(
                ctx,
                actor.uuid,
                actor.game_mode,
                position,
                &name,
                &target,
                &pool,
                &final_state,
                &joint,
                selection_priority,
                placement_priority,
            );
            Outcome::default()
        }
        Action::GenerateJigsaw {
            position,
            levels,
            keep_jigsaws,
        } => Outcome {
            messages: crate::structure_blocks::generate_jigsaw(
                ctx,
                actor.uuid,
                actor.game_mode,
                position,
                levels,
                keep_jigsaws,
            ),
            ..Outcome::default()
        },
        Action::SetTestBlock {
            position,
            mode,
            message,
        } => Outcome {
            messages: crate::test_blocks::set_test_block(
                ctx,
                actor.uuid,
                actor.game_mode,
                position,
                mode,
                &message,
            ),
            ..Outcome::default()
        },
        Action::TestInstance {
            position,
            action,
            test,
            size,
            rotation,
            ignore_entities,
        } => crate::test_blocks::instance_action(
            ctx,
            actor,
            position,
            action,
            test,
            size,
            rotation,
            ignore_entities,
        ),
        Action::SetStructure {
            position,
            action,
            mode,
            name,
            offset,
            size,
            mirror,
            rotation,
            metadata,
            integrity,
            seed,
            flags,
        } => Outcome {
            messages: crate::structure_blocks::set_structure(
                ctx,
                actor.uuid,
                actor.game_mode,
                position,
                action,
                mode,
                &name,
                offset,
                size,
                mirror,
                rotation,
                &metadata,
                integrity,
                seed,
                flags,
            ),
            ..Outcome::default()
        },
        Action::CommandMinecart {
            entity_id,
            command,
            track_output,
        } => Outcome {
            messages: crate::command_blocks::configure_cart(
                ctx,
                actor.uuid,
                actor.game_mode,
                entity_id,
                &command,
                track_output,
            )
            .into_iter()
            .collect(),
            ..Outcome::default()
        },
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().expect("lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_ticks_survive_a_save() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        if mc_data::McData::load(&dir).is_err() {
            eprintln!("skipped: no game data");
            return;
        }
        let data = || mc_data::McData::load(&dir).expect("game data");
        let folder = std::env::temp_dir().join(format!("mc-ticks-{}", std::process::id()));
        let open = |data: mc_data::McData| {
            let mut config = crate::Config::new(dir.clone());
            config.world = Some(folder.clone());
            config.seed = Some(12345);
            ServerContext::new(config, data).expect("context")
        };
        let p = Position::new(3, 200, 5);
        {
            let ctx = open(data());
            let repeater = ctx.data.blocks.default_state("minecraft:repeater").unwrap();
            crate::level::WorldLevel::new(&ctx).set_block(p, repeater);
            let kind = ctx.shapes.block_index(repeater);
            let now = lock(&ctx.time).age;
            lock(&ctx.ticks)
                .blocks
                .schedule(crate::level::to_pos(p), kind, now + 7, -1);
            crate::save(&ctx);
        }
        let ctx = open(data());
        let _ = ctx.world.block(p, &ctx.data.blocks);
        let now = lock(&ctx.time).age;
        let mut ticks = lock(&ctx.ticks);
        queue_saved_ticks(&ctx, &mut ticks, now);
        let pending: Vec<_> = ticks.blocks.pending().collect();
        let _ = std::fs::remove_dir_all(&folder);
        let repeater = ctx.data.blocks.default_state("minecraft:repeater").unwrap();
        assert_eq!(
            pending,
            vec![(
                crate::level::to_pos(p),
                ctx.shapes.block_index(repeater),
                now + 7,
                -1
            )]
        );
    }

    #[test]
    fn tick_sprint_runs_stops_and_restores_the_freeze() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Ok(data) = mc_data::McData::load(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let ctx = ServerContext::new(crate::Config::new(dir), data).expect("context");
        let mut control = TickControl::default();
        // The console's feedback goes to the log, so the events and the
        // state are what show.
        let say = |control: &mut TickControl, line: &str| {
            let out =
                crate::commands::execute(&ctx, control, &crate::commands::Source::Console, line);
            format!("{:?}", out.events)
        };
        // Nothing to stop.
        assert!(!say(&mut control, "tick sprint stop").contains("sprint.report"));
        // A frozen game sprints, and is frozen again afterwards.
        say(&mut control, "tick freeze");
        assert!(control.frozen);
        say(&mut control, "tick sprint 3");
        assert!(!control.frozen);
        assert_eq!(control.sprint.as_ref().map(|s| s.remaining), Some(3));
        // Ticks count down; the last one reports.
        assert!(control.sprinted(Duration::from_millis(2)).is_none());
        assert!(control.sprinted(Duration::from_millis(2)).is_none());
        let report = format!("{:?}", control.sprinted(Duration::from_millis(2)));
        assert!(report.contains("sprint.report") && report.contains("500.00"));
        assert!(control.sprint.is_none() && control.frozen);
        // A new sprint replaces the old one without a report; stopping
        // reports and restores.
        say(&mut control, "tick unfreeze");
        say(&mut control, "tick sprint 100");
        control.sprinted(Duration::from_millis(1));
        let replaced = say(&mut control, "tick sprint 7");
        assert!(!replaced.contains("sprint.report"), "{replaced}");
        assert_eq!(
            control.sprint.as_ref().map(|s| (s.remaining, s.done)),
            Some((7, 0))
        );
        let stopped = say(&mut control, "tick sprint stop");
        assert!(stopped.contains("sprint.report"), "{stopped}");
        assert!(control.sprint.is_none() && !control.frozen);
        // Freezing during a sprint ends it.
        say(&mut control, "tick sprint 5");
        say(&mut control, "tick freeze");
        assert!(control.sprint.is_none() && control.frozen);
    }

    #[test]
    fn stopped_clocks_do_not_advance() {
        let mut time = WorldTime {
            age: 10,
            clocks: vec![
                Clock {
                    name: "minecraft:overworld".into(),
                    id: 0,
                    ticks: 100,
                    running: true,
                    rate: 1.0,
                    fraction: 0.0,
                },
                Clock {
                    name: "minecraft:the_end".into(),
                    id: 1,
                    ticks: 7,
                    running: false,
                    rate: 1.0,
                    fraction: 0.0,
                },
            ],
        };
        time.advance();
        assert_eq!(time.age, 11);
        assert_eq!((time.clocks[0].ticks, time.clocks[1].ticks), (101, 7));
        let packet = time.packet(true);
        assert_eq!(packet.clocks[1].rate, 0.0);
        assert!(time.packet(false).clocks.is_empty());
    }

    #[test]
    fn detector_rails_see_minecarts_and_how_full_they_are() {
        let Some(ctx) = crate::projectiles::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let blocks = &ctx.data.blocks;
        let p = Position::new(30, -60, 30);
        let rail = blocks
            .state_id(
                "minecraft:detector_rail",
                &[("shape", "east_west"), ("powered", "false")],
            )
            .unwrap();
        crate::level::WorldLevel::new(&ctx).set_block(p, rail);
        let powered = |ctx: &ServerContext| {
            let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
            ctx.data.blocks.property(s, "powered") == Some("true")
        };
        run_scheduled_ticks(&ctx);
        assert!(!powered(&ctx));
        // A chest minecart with a few items stops on it.
        let cart = crate::vehicles::new(
            &ctx,
            mc_entity::extra::Craft::Minecart(mc_entity::extra::Cart::Chest),
            [30.5, -59.9375, 30.5],
            0.0,
        );
        let id = cart.id;
        let kind = crate::containers::entity_kind("minecraft:chest_minecart").unwrap();
        let stone = ctx.items.id("minecraft:stone").unwrap();
        crate::containers::put(
            &ctx,
            crate::containers::entity_key(id),
            kind,
            &[(
                0,
                Some(mc_protocol::packets::play::ItemStack::new(stone, 64)),
            )],
        );
        lock(&ctx.entities).push(cart);
        run_scheduled_ticks(&ctx);
        assert!(powered(&ctx));
        // A comparator reads it: one full stack of 27 slots is 1 + 14/27.
        let mut level = crate::level::WorldLevel::new(&ctx);
        level.minecarts = minecart_boxes(&lock(&ctx.entities));
        assert_eq!(
            mc_tick::rails::comparator_reading(&level, level.logic(), crate::level::to_pos(p)),
            Some(1)
        );
        crate::containers::put(
            &ctx,
            crate::containers::entity_key(id),
            kind,
            &(1..14)
                .map(|i| {
                    (
                        i,
                        Some(mc_protocol::packets::play::ItemStack::new(stone, 64)),
                    )
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            mc_tick::rails::comparator_reading(&level, level.logic(), crate::level::to_pos(p)),
            Some(8)
        );
        // Gone: 20 ticks later the rail is off again.
        lock(&ctx.entities).clear();
        for _ in 0..21 {
            lock(&ctx.time).age += 1;
            run_scheduled_ticks(&ctx);
        }
        assert!(!powered(&ctx));
    }
}
