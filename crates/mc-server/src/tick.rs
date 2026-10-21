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

use mc_protocol::Position;
use mc_protocol::packets::play::{ClockState, SetTime};
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

    fn advance(&mut self) {
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
}

impl Default for TickControl {
    fn default() -> Self {
        Self {
            rate: 20.0,
            frozen: false,
            steps: 0,
            times: [0; 100],
            smoothed_ms: 0.0,
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
    UseEntity {
        entity_id: i32,
        hand: i32,
        attack: bool,
        /// Where on the entity the click landed.
        at: Option<[f64; 3]>,
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
        if now < next {
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
                run_scheduled_ticks(d);
                crate::random_ticks::tick(d);
                crate::furnaces::tick(d);
                lock(&d.border).tick();
            }
            crate::effects::tick(&ctx, count as i64);
            crate::vitals::tick(&ctx);
            crate::vitals::tick_using(&ctx);
            crate::portals::tick(&ctx);
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
                if dropped > 0 {
                    debug!(
                        "unloaded {dropped} chunks of {}, {} in memory",
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

/// Due block ticks, then due fluid ticks, then entities; what they change
/// goes to everyone.
pub(crate) fn run_scheduled_ticks(ctx: &ServerContext) {
    let now = lock(&ctx.time).age;
    let relit = ctx.world.take_relit();
    if !relit.is_empty() {
        let _ = ctx.events.send(Event::LightChanged(relit));
    }
    let due = {
        let mut ticks = lock(&ctx.ticks);
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
    let logic = level.logic();
    for (pos, block) in due.0 {
        mc_tick::block_tick(&mut level, logic, pos, block);
    }
    for (pos, fluid) in due.1 {
        mc_tick::fluids::tick(&mut level, logic, pos, fluid);
    }
    run_block_events(ctx, &mut level);
    tick_entities(ctx, &mut level);
    crate::trading::tick_spawner(ctx, &mut level, lock(&ctx.time).age);
    crate::mobs::tick(ctx, &mut level);
    press_plates(ctx, &mut level);
    mc_tick::pistons::tick(&mut level, logic);
    crate::hoppers::tick(ctx, &mut level);
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
fn containers_changed(ctx: &ServerContext) {
    let changed = std::mem::take(&mut *lock(&ctx.container_updates));
    if changed.is_empty() {
        return;
    }
    for &p in &changed {
        crate::furnaces::wake(ctx, p);
    }
    let mut level = crate::level::WorldLevel::new(ctx);
    let logic = level.logic();
    for p in changed {
        mc_tick::redstone::container_changed(&mut level, logic, crate::level::to_pos(p));
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

/// Players standing in pressure plates press them (other entities do as
/// they move).
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
        let name = ctx.shapes.block_name(level.block(p)).unwrap_or("");
        if name.ends_with("_pressure_plate") {
            mc_tick::redstone::entity_inside(level, logic, p);
        }
    }
}

/// Blocks as entities bump into them.
pub(crate) struct Solid<'a, 'b>(&'a crate::level::WorldLevel<'b>);

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
        kind: mc_entity::Kind::item(
            drop.item,
            drop.count,
            match drop.components.get("minecraft:damage") {
                Some(mc_nbt::Tag::Int(d)) => *d,
                _ => 0,
            },
            10,
        ),
        pos: [
            f64::from(at.x) + 0.5 + r(0.5),
            f64::from(at.y) + 0.25 + r(0.5),
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

/// An item entity at an exact position (`/loot spawn`, leftovers a
/// player has no room for).
pub fn spawn_item_at(ctx: &ServerContext, pos: [f64; 3], item: i32, count: i32) {
    let r = |spread: f64| {
        (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64 * spread - spread / 2.0
    };
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: mc_entity::Kind::item(item, count, 0, 10),
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
    let (yaw, pitch) = (
        f64::from(pose.yaw).to_radians(),
        f64::from(pose.pitch).to_radians(),
    );
    let speed = 0.3;
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: mc_entity::Kind::item(stack.item, stack.count, stack.damage, 40),
        pos: [pose.x, pose.y + 1.62 - 0.3, pose.z],
        vel: [
            -yaw.sin() * pitch.cos() * speed,
            -pitch.sin() * speed + 0.1,
            yaw.cos() * pitch.cos() * speed,
        ],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
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
        kind: mc_entity::Kind::item(stack.item, stack.count, stack.damage, 40),
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
    let targets: Vec<crate::hits::Target> = entities
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                mc_entity::Kind::Hung(_) | mc_entity::Kind::Vehicle(_)
            )
        })
        .map(|e| crate::hits::Target {
            id: e.id,
            aabb: e.aabb(),
        })
        .collect();
    crate::decor::leash::tick(ctx);
    level.minecarts = entities
        .iter()
        .filter(|e| {
            matches!(&e.kind, mc_entity::Kind::Vehicle(v) if matches!(v.craft, mc_entity::extra::Craft::Minecart(_)))
        })
        .map(|e| (e.id, e.aabb()))
        .collect();
    let mut out = crate::hits::Out::default();
    let (mut moved, mut removed) = (Vec::new(), Vec::new());
    let mut gone = Vec::new();
    let (mut blasts, mut fuses) = (Vec::new(), Vec::new());
    let now = lock(&ctx.time).age;
    let mut claimed = crate::trading::claimed_sites(&entities);
    // Plates see entities where they are at that moment of the tick.
    level.entity_boxes = Some(entities.iter().map(|e| e.aabb()).collect());
    for (i, entity) in entities.iter_mut().enumerate() {
        let before = (entity.pos, entity.on_ground);
        let extra = matches!(
            entity.kind,
            mc_entity::Kind::Projectile(_) | mc_entity::Kind::Hung(_) | mc_entity::Kind::Vehicle(_)
        );
        if extra {
            let mut t = crate::hits::Tick {
                ctx,
                players: &whos,
                targets: &targets,
                out: &mut out,
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
                if entity.on_ground || entity.age > 600 {
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
            mc_entity::Kind::Merchant(_) => {
                mc_entity::fall(entity, &Solid(level));
                entity.vel = [0.0; 3];
                fuses.extend(crate::trading::tick(ctx, level, entity, &mut claimed, now));
            }
            // Mobs have their own tick.
            mc_entity::Kind::Mob(_) => {}
            mc_entity::Kind::ExperienceOrb { .. } => {
                mc_entity::fall_with(entity, &Solid(level), mc_entity::ORB_GRAVITY);
                if entity.on_ground {
                    entity.vel[0] *= 0.6;
                    entity.vel[2] *= 0.6;
                }
                crate::xp::tick(ctx, entity, &players);
            }
            mc_entity::Kind::Item { pickup_delay, .. } => {
                mc_entity::fall(entity, &Solid(level));
                if entity.on_ground {
                    // Ground friction (slipperiness 0.6) and a small bounce.
                    entity.vel[0] *= 0.6;
                    entity.vel[2] *= 0.6;
                    if entity.vel[1] < 0.0 {
                        entity.vel[1] *= -0.5;
                    }
                    // Come to rest instead of creeping forever.
                    for v in &mut entity.vel {
                        if v.abs() < 0.003 {
                            *v = 0.0;
                        }
                    }
                }
                // 32767 is "never": it does not count down.
                let delay = if pickup_delay == 32767 {
                    pickup_delay
                } else {
                    (pickup_delay - 1).max(0)
                };
                if let mc_entity::Kind::Item { pickup_delay, .. } = &mut entity.kind {
                    *pickup_delay = delay;
                }
                if entity.age >= 6000 {
                    entity.removed = true;
                } else if delay == 0 {
                    pick_up(ctx, entity, &players);
                }
            }
        }
        if let Some(boxes) = level.entity_boxes.as_mut() {
            boxes[i] = entity.aabb();
        }
        if let Some(m) = level.minecarts.iter_mut().find(|(id, _)| *id == entity.id) {
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
            if ctx
                .shapes
                .block_name(level.block(at))
                .is_some_and(|n| n.ends_with("_pressure_plate"))
            {
                mc_tick::redstone::entity_inside(level, logic, at);
            }
        }
        if entity.removed {
            removed.push(entity.id);
            gone.push(entity.uuid);
        } else if !extra && (entity.pos, entity.on_ground) != before {
            moved.push((entity.id, entity.pos, entity.on_ground));
        }
    }
    level.entity_boxes = None;
    blasts.append(&mut out.blasts);
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
    for hit in std::mem::take(&mut out.hits) {
        crate::hits::apply(ctx, &mut entities, &mut removed, &mut gone, hit);
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
    if !moved.is_empty() {
        let _ = ctx.events.send(Event::EntitiesMoved(moved));
    }
    if !removed.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(removed));
    }
}

/// The first nearby player with room takes the item (all of it, or what fits).
fn pick_up(
    ctx: &ServerContext,
    entity: &mut mc_entity::Entity,
    players: &[(u128, i32, i32, crate::context::Pose)],
) {
    let mc_entity::Kind::Item {
        item,
        count,
        damage,
        ..
    } = entity.kind
    else {
        return;
    };
    for (uuid, player_entity, mode, pose) in players {
        if *mode == 3 || !picks_up(pose, entity) {
            continue;
        }
        let max = ctx.items.max_stack(item);
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(uuid) else {
            continue;
        };
        let stack = mc_protocol::packets::play::ItemStack {
            damage,
            ..mc_protocol::packets::play::ItemStack::new(item, count)
        };
        let (taken, changed) = hands.insert_stack(&stack, max);
        if taken == 0 {
            continue;
        }
        for slot in changed {
            let _ = ctx.events.send(Event::InventorySlot {
                uuid: *uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
        drop(inventories);
        let _ = ctx.events.send(Event::ItemTaken {
            item_entity: entity.id,
            collector: *player_entity,
            count: taken,
        });
        if taken >= count {
            entity.removed = true;
        } else if let mc_entity::Kind::Item { count, .. } = &mut entity.kind {
            *count -= taken;
        }
        return;
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
        Action::UseEntity {
            entity_id,
            hand,
            attack,
            at,
        } => crate::hits::use_entity(ctx, actor, entity_id, hand, attack, at),
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().expect("lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
