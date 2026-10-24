//! What a villager has heard about each player (minecraft.wiki, "Villager",
//! Gossiping): five kinds of gossip, each with what one event adds, how much
//! it fades in 20 minutes, what sharing it costs, a ceiling, and a weight in
//! the player's reputation. Villagers pass it on when they meet.

/// The kinds of gossip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A villager was killed nearby.
    MajorNegative,
    /// The villager was attacked.
    MinorNegative,
    /// The villager was cured (permanent, never shared).
    MajorPositive,
    MinorPositive,
    /// A trade.
    Trading,
}

pub const KINDS: [Kind; 5] = [
    Kind::MajorNegative,
    Kind::MinorNegative,
    Kind::MajorPositive,
    Kind::MinorPositive,
    Kind::Trading,
];

impl Kind {
    /// The name the save files use.
    pub fn name(self) -> &'static str {
        match self {
            Kind::MajorNegative => "major_negative",
            Kind::MinorNegative => "minor_negative",
            Kind::MajorPositive => "major_positive",
            Kind::MinorPositive => "minor_positive",
            Kind::Trading => "trading",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        KINDS.into_iter().find(|k| k.name() == name)
    }

    /// What one event adds.
    pub fn gain(self) -> i32 {
        match self {
            Kind::MajorNegative | Kind::MinorNegative | Kind::MinorPositive => 25,
            Kind::MajorPositive => 20,
            Kind::Trading => 4,
        }
    }

    /// What it fades by every 20 minutes.
    pub fn decay(self) -> i32 {
        match self {
            Kind::MajorNegative => 10,
            Kind::MinorNegative => 20,
            Kind::MajorPositive => 0,
            Kind::MinorPositive => 1,
            Kind::Trading => 2,
        }
    }

    /// What is taken off when it is passed on.
    pub fn sharing_cost(self) -> i32 {
        match self {
            Kind::MajorNegative => 10,
            Kind::MinorNegative => 20,
            Kind::MajorPositive => 100,
            Kind::MinorPositive => 5,
            Kind::Trading => 20,
        }
    }

    pub fn max(self) -> i32 {
        match self {
            Kind::MajorNegative => 100,
            Kind::MinorNegative => 200,
            Kind::MajorPositive => 20,
            Kind::MinorPositive | Kind::Trading => 25,
        }
    }

    /// How much one point counts for in the reputation.
    pub fn weight(self) -> i32 {
        match self {
            Kind::MajorNegative => -5,
            Kind::MinorNegative => -1,
            Kind::MajorPositive => 5,
            Kind::MinorPositive | Kind::Trading => 1,
        }
    }
}

/// Reputation at or below which iron golems that players did not make go
/// for a player.
pub const HOSTILE: i32 = -100;

/// One villager's gossip: about whom, of what kind, how much.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gossips(Vec<(u128, Kind, i32)>);

impl Gossips {
    /// Adds `amount` (up to the kind's ceiling).
    pub fn add(&mut self, about: u128, kind: Kind, amount: i32) {
        let max = kind.max();
        match self.0.iter_mut().find(|e| e.0 == about && e.1 == kind) {
            Some(e) => e.2 = (e.2 + amount).clamp(0, max),
            None => {
                if amount > 0 {
                    self.0.push((about, kind, amount.min(max)));
                }
            }
        }
    }

    /// What one event of the kind adds.
    pub fn happen(&mut self, about: u128, kind: Kind) {
        self.add(about, kind, kind.gain());
    }

    pub fn value(&self, about: u128, kind: Kind) -> i32 {
        self.0
            .iter()
            .find(|e| e.0 == about && e.1 == kind)
            .map_or(0, |e| e.2)
    }

    /// What the villager thinks of the player: each gossip times its weight.
    pub fn reputation(&self, about: u128) -> i32 {
        self.0
            .iter()
            .filter(|e| e.0 == about)
            .map(|e| e.2 * e.1.weight())
            .sum()
    }

    /// Players the villager has heard of.
    pub fn people(&self) -> Vec<u128> {
        let mut all: Vec<u128> = self.0.iter().map(|e| e.0).collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    /// Twenty minutes pass: everything fades.
    pub fn fade(&mut self) {
        for e in &mut self.0 {
            e.2 = (e.2 - e.1.decay()).max(0);
        }
        self.0.retain(|e| e.2 > 0);
    }

    /// Tells another villager what is worth passing on: each gossip less its
    /// sharing cost, which the other keeps if it is more than it has.
    pub fn tell(&self, other: &mut Gossips) {
        for &(about, kind, value) in &self.0 {
            let shared = value - kind.sharing_cost();
            if shared > 0 && shared > other.value(about, kind) {
                let had = other.value(about, kind);
                other.add(about, kind, shared - had);
            }
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = (u128, Kind, i32)> + '_ {
        self.0.iter().copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reputation_is_the_weighted_sum_and_gains_stop_at_the_ceiling() {
        let mut g = Gossips::default();
        for _ in 0..30 {
            g.happen(1, Kind::Trading);
        }
        assert_eq!(g.value(1, Kind::Trading), 25);
        g.happen(1, Kind::MinorNegative);
        g.happen(1, Kind::MajorNegative);
        g.happen(1, Kind::MajorPositive);
        // 25 - 25 - 5 * 25 + 5 * 20
        assert_eq!(g.reputation(1), 25 - 25 - 125 + 100);
        assert_eq!(g.reputation(2), 0);
        for _ in 0..20 {
            g.happen(1, Kind::MinorNegative);
        }
        assert_eq!(g.value(1, Kind::MinorNegative), 200);
    }

    #[test]
    fn gossip_fades_by_kind_and_curing_never_fades() {
        let mut g = Gossips::default();
        g.happen(1, Kind::MinorNegative);
        g.happen(1, Kind::MajorNegative);
        g.happen(1, Kind::MajorPositive);
        g.happen(1, Kind::Trading);
        g.fade();
        assert_eq!(g.value(1, Kind::MinorNegative), 5);
        assert_eq!(g.value(1, Kind::MajorNegative), 15);
        assert_eq!(g.value(1, Kind::MajorPositive), 20);
        assert_eq!(g.value(1, Kind::Trading), 2);
        g.fade();
        g.fade();
        assert_eq!(g.value(1, Kind::MinorNegative), 0);
        assert_eq!(g.value(1, Kind::MajorNegative), 0);
        assert_eq!(g.value(1, Kind::MajorPositive), 20);
        assert_eq!(g.value(1, Kind::Trading), 0);
    }

    #[test]
    fn sharing_costs_and_never_lowers_what_the_other_knows() {
        let mut a = Gossips::default();
        a.happen(1, Kind::MajorNegative);
        a.happen(1, Kind::MajorPositive);
        a.happen(1, Kind::Trading);
        let mut b = Gossips::default();
        b.add(1, Kind::MajorNegative, 20);
        a.tell(&mut b);
        // 25 - 10 is less than the 20 it knew already; curing is not shared;
        // a trade (4) is not worth 20.
        assert_eq!(b.value(1, Kind::MajorNegative), 20);
        assert_eq!(b.value(1, Kind::MajorPositive), 0);
        assert_eq!(b.value(1, Kind::Trading), 0);
        let mut c = Gossips::default();
        a.tell(&mut c);
        assert_eq!(c.value(1, Kind::MajorNegative), 15);
    }
}
