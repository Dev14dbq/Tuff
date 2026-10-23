//! Villagers and wandering traders: their trades, experience and levels.
//!
//! Rules follow minecraft.wiki ("Trading"): a trade's price grows with the
//! demand for it and shrinks with discounts, a villager levels up from the
//! experience it gets from trades (10, 70, 150 and 250 points) and restocks
//! its trades twice a day.

/// Experience a villager needs to leave level 1..=4, the first number of
/// each pair being the lowest it can have there.
pub const LEVEL_XP: [i32; 5] = [0, 10, 70, 150, 250];

pub const MAX_LEVEL: i32 = 5;

/// Experience at which a villager of `level` moves to the next level.
pub fn xp_for_next(level: i32) -> Option<i32> {
    usize::try_from(level)
        .ok()
        .filter(|l| (1..=4).contains(l))
        .map(|l| LEVEL_XP[l])
}

/// One thing a trade asks for: `count` of the item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    /// Id in the `minecraft:item` registry.
    pub item: i32,
    pub count: i32,
}

/// One trade.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub cost_a: Cost,
    pub cost_b: Option<Cost>,
    pub result: Cost,
    pub uses: i32,
    pub max_uses: i32,
    /// Experience the villager gets.
    pub xp: i32,
    /// How much one point of demand (and one of reputation) moves the price.
    pub price_multiplier: f32,
    pub demand: i32,
    /// Discounts and surcharges of the player trading now.
    pub special_price: i32,
    /// Whether the player gets experience points from it.
    pub reward_xp: bool,
}

impl Offer {
    pub fn out_of_stock(&self) -> bool {
        self.uses >= self.max_uses
    }

    /// What the first payment costs now. `max_stack` is the item's stack
    /// size, a price is never more than one stack and never less than one.
    pub fn price_a(&self, max_stack: i32) -> i32 {
        let base = self.cost_a.count;
        let demand = (base as f32 * self.demand as f32 * self.price_multiplier).floor() as i32;
        (base + demand.max(0) + self.special_price).clamp(1, max_stack.max(1))
    }

    /// Takes the payment out of what the player put in, if it is enough.
    pub fn accepts(&self, a: Option<Cost>, b: Option<Cost>, max_stack: i32) -> bool {
        if self.out_of_stock() {
            return false;
        }
        let enough = |have: Option<Cost>, want: Cost, price: i32| {
            have.is_some_and(|h| h.item == want.item && h.count >= price)
        };
        let price_a = self.price_a(max_stack);
        let pay = |slot_a, slot_b| -> bool {
            enough(slot_a, self.cost_a, price_a)
                && match self.cost_b {
                    Some(cost_b) => enough(slot_b, cost_b, cost_b.count),
                    None => true,
                }
        };
        pay(a, b) || (self.cost_b.is_some() && pay(b, a))
    }

    /// Demand after the villager restocks: it grows by the uses and shrinks
    /// by what was left over.
    pub fn restock(&mut self) {
        self.demand = (self.demand + self.uses - (self.max_uses - self.uses)).max(0);
        self.uses = 0;
        self.special_price = 0;
    }
}

/// A villager or a wandering trader.
#[derive(Clone, Debug, PartialEq)]
pub struct Merchant {
    pub wandering: bool,
    /// The villager's type (`minecraft:plains`), which some trades depend on.
    pub variant: String,
    /// The profession's name without namespace (`farmer`); `None` for an
    /// unemployed villager.
    pub profession: Option<String>,
    pub level: i32,
    pub xp: i32,
    pub offers: Vec<Offer>,
    /// The workstation the villager works at.
    pub job_site: Option<(i32, i32, i32)>,
    /// Game time of the last restock and how many there were today.
    pub last_restock: i64,
    pub restocks_today: i32,
    pub restock_day: i64,
    /// Ticks until a villager that earned a level gets it (40, as vanilla).
    pub level_up_in: Option<i32>,
    /// Players trading now.
    pub trading: Vec<u128>,
    /// Its walking about.
    pub walk: crate::villager::Walk,
}

impl Merchant {
    pub fn villager() -> Self {
        Self {
            wandering: false,
            variant: "minecraft:plains".to_owned(),
            profession: None,
            level: 1,
            xp: 0,
            offers: Vec::new(),
            job_site: None,
            last_restock: 0,
            restocks_today: 0,
            restock_day: 0,
            level_up_in: None,
            trading: Vec::new(),
            walk: Default::default(),
        }
    }

    pub fn wandering_trader() -> Self {
        Self {
            wandering: true,
            ..Self::villager()
        }
    }

    /// Whether the experience is enough for the next level.
    pub fn should_level_up(&self) -> bool {
        xp_for_next(self.level).is_some_and(|need| self.xp >= need)
    }

    pub fn needs_restock(&self) -> bool {
        self.offers.iter().any(|o| o.uses > 0)
    }

    /// Whether it may restock at `time` (two times a day, 2400 ticks apart).
    pub fn may_restock(&mut self, time: i64) -> bool {
        let day = time.div_euclid(24000);
        if day != self.restock_day {
            self.restock_day = day;
            self.restocks_today = 0;
        }
        self.restocks_today == 0 || (self.restocks_today < 2 && time > self.last_restock + 2400)
    }

    pub fn restock(&mut self, time: i64) {
        for offer in &mut self.offers {
            offer.restock();
        }
        self.restocks_today += 1;
        self.last_restock = time;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> Offer {
        Offer {
            cost_a: Cost { item: 1, count: 10 },
            cost_b: None,
            result: Cost { item: 2, count: 1 },
            uses: 0,
            max_uses: 4,
            xp: 1,
            price_multiplier: 0.05,
            demand: 0,
            special_price: 0,
            reward_xp: true,
        }
    }

    #[test]
    fn demand_raises_and_discount_lowers_the_price() {
        let mut o = offer();
        assert_eq!(o.price_a(64), 10);
        o.demand = 4;
        assert_eq!(o.price_a(64), 12);
        o.special_price = -3;
        assert_eq!(o.price_a(64), 9);
        o.special_price = -50;
        assert_eq!(o.price_a(64), 1);
        o.special_price = 500;
        assert_eq!(o.price_a(64), 64);
    }

    #[test]
    fn restock_updates_demand() {
        let mut o = offer();
        o.uses = 4;
        o.restock();
        assert_eq!((o.uses, o.demand), (0, 4));
        o.uses = 1;
        o.restock();
        // 4 + 1 - 3
        assert_eq!(o.demand, 2);
    }

    #[test]
    fn payment_in_either_slot() {
        let mut o = offer();
        o.cost_b = Some(Cost { item: 3, count: 1 });
        let a = Some(Cost { item: 1, count: 10 });
        let b = Some(Cost { item: 3, count: 1 });
        assert!(o.accepts(a, b, 64));
        assert!(o.accepts(b, a, 64));
        assert!(!o.accepts(a, None, 64));
        assert!(!o.accepts(Some(Cost { item: 1, count: 9 }), b, 64));
        o.uses = 4;
        assert!(!o.accepts(a, b, 64));
    }

    #[test]
    fn levels() {
        let mut m = Merchant::villager();
        m.xp = 9;
        assert!(!m.should_level_up());
        m.xp = 10;
        assert!(m.should_level_up());
        m.level = 5;
        m.xp = 1000;
        assert!(!m.should_level_up());
    }

    #[test]
    fn restocks_twice_a_day() {
        let mut m = Merchant::villager();
        assert!(m.may_restock(100));
        m.restock(100);
        assert!(!m.may_restock(200));
        assert!(m.may_restock(2600));
        m.restock(2600);
        assert!(!m.may_restock(6000));
        assert!(m.may_restock(24100));
    }
}
