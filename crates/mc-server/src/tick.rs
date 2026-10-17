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
use mc_protocol::packets::play::{ClockState, ItemStack, SetTime};
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
        /// What is in the hand used, as the connection knows it.
        held: Option<ItemStack>,
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
        }

        while let Ok(request) = requests.try_recv() {
            let mut outcome = handle(&ctx, &mut control, request.actor, request.action);
            for event in std::mem::take(&mut outcome.events) {
                let _ = ctx.events.send(event);
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
            held,
            position,
            face,
            cursor,
            sequence,
        } => interact::use_item_on(ctx, actor, held.as_ref(), position, face, cursor, sequence),
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
