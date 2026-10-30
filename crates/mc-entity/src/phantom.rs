//! The phantom spawner's clock: it counts ticks whether or not anyone is
//! near to be visited, and is held still while mob spawning (or monster or
//! phantom spawning) is off.

/// When the spawner next acts, in game ticks, and the last tick it was
/// brought up to date for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Clock {
    next_at: i64,
    seen: Option<i64>,
}

impl Clock {
    /// Brings the clock to tick `now`: whether the spawner acts on exactly
    /// this tick. Ticks it was not asked about since last time still
    /// counted, and any time it should have acted in them is lost with
    /// nobody there. While `runs` is false the time passed does not count
    /// (the next action moves out by as much). `interval` rolls the ticks
    /// to the next action, each time one is scheduled.
    pub fn advance(&mut self, now: i64, runs: bool, mut interval: impl FnMut() -> i64) -> bool {
        let first = self.seen.is_none();
        let seen = self.seen.replace(now).unwrap_or(now - 1);
        if first {
            // Counting starts from zero: the first tick it runs acts.
            self.next_at = now - 1;
        }
        if !runs {
            self.next_at += (now - seen).max(0);
            return false;
        }
        let mut acts = false;
        while self.next_at <= now {
            acts = self.next_at.max(seen + 1) == now;
            self.next_at += interval().max(1);
        }
        acts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_acts_on_the_first_tick_and_then_after_each_interval() {
        let mut c = Clock::default();
        assert!(c.advance(100, true, || 1200));
        let acted: Vec<i64> = (101..=2500)
            .filter(|&t| c.advance(t, true, || 1200))
            .collect();
        assert_eq!(acted, vec![1299, 2499]);
    }

    #[test]
    fn time_nobody_was_asked_about_still_counts() {
        let mut c = Clock::default();
        assert!(c.advance(0, true, || 1200));
        // Not asked at all from 1 to 5000: it would have acted at 1199,
        // 2399, 3599 and 4799, with nobody there; the next is 5999.
        assert!(!c.advance(5000, true, || 1200));
        assert!(!c.advance(5998, true, || 1200));
        assert!(c.advance(5999, true, || 1200));
    }

    #[test]
    fn it_stands_still_while_the_rules_are_off() {
        let mut c = Clock::default();
        assert!(c.advance(0, true, || 1200));
        for t in 1..=1000 {
            assert!(!c.advance(t, false, || 1200));
        }
        // 1000 ticks held: due at 2199 instead of 1199.
        let acted: Vec<i64> = (1001..=2300)
            .filter(|&t| c.advance(t, true, || 1200))
            .collect();
        assert_eq!(acted, vec![2199]);
    }
}
