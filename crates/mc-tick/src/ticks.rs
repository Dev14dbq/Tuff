//! Scheduled ticks (minecraft.wiki, "Tick" → "Scheduled tick"): a block or
//! fluid asks to be ticked a number of game ticks later. Due ticks run by
//! time, then priority (lower first), then the order they were scheduled.
//! Block ticks run before fluid ticks; at most 65536 of each per game tick.
//! A tick already waiting for the same position and block is not added again.

use std::collections::{BTreeMap, HashSet};

use crate::dir::BlockPos;

pub const MAX_PER_TICK: usize = 65536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    time: i64,
    priority: i32,
    order: u64,
}

/// One queue (block ticks or fluid ticks). `kind` tells what asked: a block
/// id or a fluid id, so a tick for a block that has since been replaced is
/// dropped when it comes due.
#[derive(Debug, Default)]
pub struct TickQueue {
    queue: BTreeMap<Key, (BlockPos, u32)>,
    waiting: HashSet<(BlockPos, u32)>,
    next_order: u64,
}

impl TickQueue {
    /// Schedules `kind` at `pos` to tick at game time `time`.
    pub fn schedule(&mut self, pos: BlockPos, kind: u32, time: i64, priority: i32) {
        if !self.waiting.insert((pos, kind)) {
            return;
        }
        let key = Key {
            time,
            priority,
            order: self.next_order,
        };
        self.next_order += 1;
        self.queue.insert(key, (pos, kind));
    }

    pub fn is_scheduled(&self, pos: BlockPos, kind: u32) -> bool {
        self.waiting.contains(&(pos, kind))
    }

    /// Takes the ticks due at or before `now`, in execution order.
    pub fn take_due(&mut self, now: i64) -> Vec<(BlockPos, u32)> {
        let mut due = Vec::new();
        while due.len() < MAX_PER_TICK {
            let Some((&key, _)) = self.queue.first_key_value() else {
                break;
            };
            if key.time > now {
                break;
            }
            let (_, entry) = self.queue.pop_first().expect("checked");
            self.waiting.remove(&entry);
            due.push(entry);
        }
        due
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// Block and fluid tick queues of one dimension.
#[derive(Debug, Default)]
pub struct ScheduledTicks {
    pub blocks: TickQueue,
    pub fluids: TickQueue,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_is_time_then_priority_then_scheduling() {
        let mut q = TickQueue::default();
        let p = |x| BlockPos::new(x, 0, 0);
        q.schedule(p(1), 7, 10, 0);
        q.schedule(p(2), 7, 9, 0);
        q.schedule(p(3), 7, 10, -1);
        q.schedule(p(4), 7, 10, 0);
        q.schedule(p(1), 7, 5, 0); // already waiting: ignored
        assert_eq!(q.take_due(8), vec![]);
        let order: Vec<i32> = q.take_due(10).into_iter().map(|(p, _)| p.x).collect();
        assert_eq!(order, vec![2, 3, 1, 4]);
        assert!(q.is_empty());
        q.schedule(p(1), 7, 20, 0);
        assert!(q.is_scheduled(p(1), 7));
    }
}
