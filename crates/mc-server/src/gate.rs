//! Who may open a connection. Vanilla lets every connection in and drops
//! the ones that go quiet for 30 seconds; that stays the default. Two limits
//! for public servers are off unless `server.properties` sets them:
//!
//! - `connection-throttle` (milliseconds): a login from an address that
//!   started one less than this long ago is refused, so one address cannot
//!   hammer the login path (session server lookups, key exchange);
//! - `max-pending-connections-per-ip`: how many connections one address may
//!   hold open before they reach play, so a client cannot pile up
//!   half-open connections faster than the read timeout drops them.
//!
//! Behind a proxy every connection comes from the proxy's address, so both
//! stay off in proxy mode.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Remembered login times beyond which old ones are forgotten.
const REMEMBERED: usize = 4096;

#[derive(Debug, Default)]
pub struct Gate {
    throttle: Duration,
    max_pending: u32,
    /// Only the accept loop and connections before play take this, never
    /// the tick, and never while holding another lock.
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    last_login: HashMap<IpAddr, Instant>,
    pending: HashMap<IpAddr, u32>,
}

impl Gate {
    /// `throttle` zero and `max_pending` zero turn each limit off.
    pub fn new(throttle: Duration, max_pending: u32) -> Arc<Self> {
        Arc::new(Self {
            throttle,
            max_pending,
            state: Mutex::default(),
        })
    }

    /// A new connection from `ip`, or `None` when that address already
    /// holds as many connections short of play as it may.
    pub fn admit(self: &Arc<Self>, ip: IpAddr) -> Option<Pending> {
        if self.max_pending > 0 {
            let mut state = self.state.lock().expect("gate lock");
            let open = state.pending.entry(ip).or_default();
            if *open >= self.max_pending {
                return None;
            }
            *open += 1;
        }
        Some(Pending {
            gate: self.clone(),
            ip,
        })
    }

    fn release(&self, ip: IpAddr) {
        if self.max_pending == 0 {
            return;
        }
        let mut state = self.state.lock().expect("gate lock");
        if let Some(open) = state.pending.get_mut(&ip) {
            *open -= 1;
            if *open == 0 {
                state.pending.remove(&ip);
            }
        }
    }

    /// A login from `ip` at `now` comes too soon after the last one.
    fn throttled(&self, ip: IpAddr, now: Instant) -> bool {
        if self.throttle.is_zero() {
            return false;
        }
        let mut state = self.state.lock().expect("gate lock");
        let too_soon = state
            .last_login
            .get(&ip)
            .is_some_and(|&at| now.duration_since(at) < self.throttle);
        state.last_login.insert(ip, now);
        if state.last_login.len() > REMEMBERED {
            let throttle = self.throttle;
            state
                .last_login
                .retain(|_, at| now.duration_since(*at) < throttle);
        }
        too_soon
    }
}

/// A connection that has not reached play yet; it stops counting against
/// its address when dropped.
#[derive(Debug)]
pub struct Pending {
    gate: Arc<Gate>,
    ip: IpAddr,
}

impl Pending {
    /// This connection asks to log in too soon after the last login from
    /// its address.
    pub fn login_throttled(&self) -> bool {
        self.gate.throttled(self.ip, Instant::now())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.gate.release(self.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));
    const B: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));

    #[test]
    fn off_by_default() {
        let gate = Gate::new(Duration::ZERO, 0);
        let held: Vec<_> = (0..1000).map(|_| gate.admit(A).unwrap()).collect();
        assert!(held.iter().all(|p| !p.login_throttled()));
    }

    #[test]
    fn pending_connections_are_capped_per_address() {
        let gate = Gate::new(Duration::ZERO, 2);
        let first = gate.admit(A).unwrap();
        let _second = gate.admit(A).unwrap();
        assert!(gate.admit(A).is_none());
        // Other addresses are not affected.
        assert!(gate.admit(B).is_some());
        // Reaching play (or closing) frees the place.
        drop(first);
        assert!(gate.admit(A).is_some());
    }

    #[test]
    fn logins_are_throttled_per_address() {
        let gate = Gate::new(Duration::from_secs(4), 0);
        let now = Instant::now();
        assert!(!gate.throttled(A, now));
        assert!(gate.throttled(A, now + Duration::from_secs(1)));
        assert!(!gate.throttled(B, now + Duration::from_secs(1)));
        // Counted from the last attempt.
        assert!(gate.throttled(A, now + Duration::from_secs(4)));
        assert!(!gate.throttled(A, now + Duration::from_secs(9)));
    }

    #[test]
    fn remembered_logins_stay_bounded() {
        let gate = Gate::new(Duration::from_millis(10), 0);
        let now = Instant::now();
        for i in 0..20_000u32 {
            let ip = IpAddr::V6(std::net::Ipv6Addr::from(u128::from(i)));
            gate.throttled(ip, now + Duration::from_millis(u64::from(i)));
        }
        let remembered = gate.state.lock().unwrap().last_login.len();
        assert!(remembered <= REMEMBERED + 1, "{remembered}");
    }
}
