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
                        fraction: 0.0,
                        rate: if c.running { 1.0 } else { 0.0 },
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
            clock.ticks += 1;
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
}

impl Default for TickControl {
    fn default() -> Self {
        Self {
            rate: 20.0,
            frozen: false,
            steps: 0,
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
}

#[derive(Debug)]
pub struct Request {
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
            warn!(
                "can't keep up: {} ms behind, skipping ahead",
                (now - next).as_millis()
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
        let runs = !control.frozen || control.steps > 0;
        if control.frozen && control.steps > 0 {
            control.steps -= 1;
        }
        if runs {
            lock(&ctx.time).advance();
            run_scheduled_ticks(&ctx);
            crate::vitals::tick(&ctx);
            crate::vitals::tick_using(&ctx);
        }

        while let Ok(request) = requests.try_recv() {
            let mut outcome = handle(&ctx, &mut control, request.actor, request.action);
            for event in std::mem::take(&mut outcome.events) {
                let _ = ctx.events.send(event);
            }
            for (at, drop) in std::mem::take(&mut outcome.drops) {
                drop_item(&ctx, at, drop);
            }
            let _ = request.reply.send(outcome);
        }

        if count.is_multiple_of(UNLOAD_EVERY) {
            let dropped = ctx.world.unload_unused();
            if dropped > 0 {
                debug!(
                    "unloaded {dropped} chunks, {} in memory",
                    ctx.world.loaded_chunks()
                );
            }
        }
        if count.is_multiple_of(autosave) {
            crate::save(&ctx);
        }
        *lock(&ctx.ticking) = mc_protocol::packets::play::TickingState {
            tick_rate: control.rate,
            frozen: control.frozen,
        };
        let took = started.elapsed();
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
fn run_scheduled_ticks(ctx: &ServerContext) {
    let now = lock(&ctx.time).age;
    let due = {
        let mut ticks = lock(&ctx.ticks);
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
    tick_entities(ctx, &mut level);
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

/// Blocks as entities bump into them.
struct Solid<'a, 'b>(&'a crate::level::WorldLevel<'b>);

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
        kind: mc_entity::Kind::Item {
            item: drop.item,
            count: drop.count,
            pickup_delay: 10,
        },
        pos: [
            f64::from(at.x) + 0.5 + r(0.5),
            f64::from(at.y) + 0.25 + r(0.5),
            f64::from(at.z) + 0.5 + r(0.5),
        ],
        vel: [r(0.2), 0.2, r(0.2)],
        on_ground: false,
        age: 0,
        removed: false,
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
        kind: mc_entity::Kind::Item {
            item: stack.item,
            count: stack.count,
            pickup_delay: 40,
        },
        pos: [pose.x, pose.y + 1.62 - 0.3, pose.z],
        vel: [
            -yaw.sin() * pitch.cos() * speed,
            -pitch.sin() * speed + 0.1,
            yaw.cos() * pitch.cos() * speed,
        ],
        on_ground: false,
        age: 0,
        removed: false,
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
        kind: mc_entity::Kind::Item {
            item: stack.item,
            count: stack.count,
            pickup_delay: 40,
        },
        pos: [pose.x, pose.y + 1.32, pose.z],
        vel: [angle.sin() * speed, 0.2, angle.cos() * speed],
        on_ground: false,
        age: 0,
        removed: false,
    };
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
}

/// How far around a player items are picked up: the player's box grown by
/// one block sideways and half a block up and down.
fn picks_up(pose: &crate::context::Pose, item: &mc_entity::Entity) -> bool {
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
        .filter_map(|p| {
            p.pose
                .map(|pose| (p.profile.uuid, p.entity_id, p.game_mode, pose))
        })
        .collect();
    let (mut moved, mut removed) = (Vec::new(), Vec::new());
    for entity in &mut entities {
        let before = (entity.pos, entity.on_ground);
        match entity.kind.clone() {
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
            mc_entity::Kind::Item {
                item,
                count,
                pickup_delay,
            } => {
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
                let delay = (pickup_delay - 1).max(0);
                entity.kind = mc_entity::Kind::Item {
                    item,
                    count,
                    pickup_delay: delay,
                };
                if entity.age >= 6000 {
                    entity.removed = true;
                } else if delay == 0 {
                    pick_up(ctx, entity, &players);
                }
            }
        }
        if entity.removed {
            removed.push(entity.id);
        } else if (entity.pos, entity.on_ground) != before {
            moved.push((entity.id, entity.pos, entity.on_ground));
        }
    }
    entities.retain(|e| !e.removed);
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
    let mc_entity::Kind::Item { item, count, .. } = entity.kind else {
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
        let (taken, changed) = hands.insert(item, count, max);
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
        } else {
            entity.kind = mc_entity::Kind::Item {
                item,
                count: count - taken,
                pickup_delay: 0,
            };
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
            sequence,
        } => interact::dig(ctx, actor, status, position, sequence),
        Action::UseItemOn {
            hand,
            position,
            face,
            cursor,
            sequence,
        } => interact::use_item_on(ctx, actor, hand, position, face, cursor, sequence),
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
                },
                Clock {
                    name: "minecraft:the_end".into(),
                    id: 1,
                    ticks: 7,
                    running: false,
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
