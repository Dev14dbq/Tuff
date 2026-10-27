//! The sculk patch of world generation: vanilla's `SculkPatchFeature` with
//! its world-generation `SculkSpreader` (charge cursors that crawl over
//! sculk and veins, turn stone to sculk and sometimes grow a sensor or a
//! shrieker that can summon), the vein spreading of `SculkVeinBlock` and
//! `MultifaceSpreader`.

use mc_tick::Dir;
use mc_worldgen::random::RandomSource;

use crate::blocks::Facts;
use crate::multiface::{ALL, DEFAULT_ORDER, SAME_SPACE, Spreader};
use crate::rand::WorldgenRandom;
use crate::{P, Region};

/// The world-generation spreader's numbers.
const GROWTH_SPAWN_COST: i32 = 50;
const NO_GROWTH_RADIUS: i32 = 1;
const CHARGE_DECAY_RATE: i32 = 5;
const ADDITIONAL_DECAY_RATE: i32 = 10;
const MAX_CHARGE: i32 = 1000;
const MAX_CURSORS: usize = 32;
const SHRIEKER_PLACEMENT_RATE: i32 = 11;
/// How far (sideways, squared) a cursor may crawl from the patch's origin.
const MAX_WORLDGEN_SPREAD_SQ: i32 = 144;

#[derive(Debug)]
pub struct Patch {
    pub charges: i32,
    pub amount: i32,
    pub attempts: i32,
    pub spread_rounds: i32,
    pub growth_rounds: i32,
}

/// What a block does with a charge on it: sculk, a vein, or anything else.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Sculk,
    Vein,
    Default,
}

struct Cursor {
    pos: P,
    charge: i32,
    update_delay: i32,
    decay_delay: i32,
    /// The vein faces it last stood on (`None` before it moved).
    facings: Option<u8>,
}

fn bit(d: Dir) -> u8 {
    1 << ALL.iter().position(|e| *e == d).unwrap_or(0)
}

fn dist_sq(a: P, b: P) -> i64 {
    let (dx, dy, dz) = (
        i64::from(a.x - b.x),
        i64::from(a.y - b.y),
        i64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz
}

struct Sculk<'a> {
    facts: &'a Facts,
    sculk: u32,
    veins: Spreader<'a>,
    sensor: u32,
    shrieker: u32,
    replaceable_world_gen: Option<crate::blocks::Tag>,
    replaceable: Option<crate::blocks::Tag>,
    inhibitors: Option<crate::blocks::Tag>,
}

impl Sculk<'_> {
    fn behaviour(&self, s: u32) -> Behaviour {
        let block = self.facts.block(s);
        if block == self.facts.block(self.sculk) {
            Behaviour::Sculk
        } else if self.veins.is_block(s) {
            Behaviour::Vein
        } else {
            Behaviour::Default
        }
    }

    fn is_vein(&self, s: u32) -> bool {
        self.behaviour(s) == Behaviour::Vein
    }

    fn has_face(&self, s: u32, d: Dir) -> bool {
        self.veins.has_face(s, d)
    }

    fn faces(&self, s: u32) -> u8 {
        if !self.is_vein(s) {
            return 0;
        }
        ALL.iter()
            .filter(|d| self.has_face(s, **d))
            .fold(0, |m, d| m | bit(*d))
    }

    fn in_tag(&self, s: u32, tag: Option<crate::blocks::Tag>) -> bool {
        tag.is_some_and(|t| self.facts.in_tag(s, t))
    }

    fn has_fluid(&self, s: u32) -> bool {
        !self.facts.fluid(s).is_none()
    }

    /// `isCollisionShapeFullBlock`, from the six sturdy faces.
    fn full_cube(&self, s: u32) -> bool {
        self.facts.blocks_motion(s) && ALL.iter().all(|d| self.facts.sturdy(s, *d))
    }

    fn spread_all(
        &self,
        region: &mut dyn Region,
        state: u32,
        p: P,
        types: &[crate::multiface::Spread],
    ) -> i64 {
        self.veins.spread_all(region, state, p, types)
    }

    /// `SculkVeinBlock.regrow`: a vein on those of `faces` that can hold one.
    fn regrow(&self, region: &mut dyn Region, p: P, existing: u32, faces: u8) -> bool {
        let mut s = self.veins.block;
        let mut any = false;
        for d in ALL {
            if faces & bit(d) != 0 && self.veins.can_attach(region, p, d) {
                s = self.veins.with_face(s, d, true);
                any = true;
            }
        }
        if !any {
            return false;
        }
        if self.has_fluid(existing) {
            s = self.facts.with(s, "waterlogged", "true");
        }
        region.set(p, s);
        true
    }

    fn has_substrate_access(&self, region: &dyn Region, s: u32, p: P) -> bool {
        self.is_vein(s)
            && ALL.iter().any(|d| {
                self.has_face(s, *d) && self.in_tag(region.block(p.rel(*d)), self.replaceable)
            })
    }

    // What each block does with a charge (`SculkBehaviour`).

    fn attempt_spread_vein(
        &self,
        region: &mut dyn Region,
        b: Behaviour,
        p: P,
        state: u32,
        facings: Option<u8>,
    ) -> bool {
        if b == Behaviour::Default {
            match facings {
                None => {
                    let here = region.block(p);
                    return self.spread_all(region, here, p, SAME_SPACE) > 0;
                }
                Some(f) if f != 0 => {
                    if !self.facts.is_air(state) && !self.facts.is_water_source(state) {
                        return false;
                    }
                    return self.regrow(region, p, state, f);
                }
                Some(_) => {}
            }
        }
        self.spread_all(region, state, p, DEFAULT_ORDER) > 0
    }

    fn attempt_use_charge(
        &self,
        region: &mut dyn Region,
        b: Behaviour,
        c: &Cursor,
        origin: P,
        r: &mut WorldgenRandom,
        spread_veins: bool,
    ) -> i32 {
        match b {
            Behaviour::Default => {
                if c.decay_delay > 0 {
                    c.charge
                } else {
                    0
                }
            }
            Behaviour::Vein => {
                if spread_veins && self.place_sculk(region, c.pos, r) {
                    c.charge - 1
                } else if r.next_int_bound(CHARGE_DECAY_RATE) == 0 {
                    (c.charge as f32 * 0.5).floor() as i32
                } else {
                    c.charge
                }
            }
            Behaviour::Sculk => self.sculk_charge(region, c, origin, r),
        }
    }

    /// `SculkBlock.attemptUseCharge`.
    fn sculk_charge(
        &self,
        region: &mut dyn Region,
        c: &Cursor,
        origin: P,
        r: &mut WorldgenRandom,
    ) -> i32 {
        let charge = c.charge;
        if charge == 0 || r.next_int_bound(CHARGE_DECAY_RATE) != 0 {
            return charge;
        }
        let at = c.pos;
        let close = dist_sq(at, origin) < i64::from(NO_GROWTH_RADIUS * NO_GROWTH_RADIUS);
        if !close && self.can_place_growth(region, at) {
            if r.next_int_bound(GROWTH_SPAWN_COST) < charge {
                let above = at.above(1);
                let mut growth = if r.next_int_bound(SHRIEKER_PLACEMENT_RATE) == 0 {
                    self.facts.with(self.shrieker, "can_summon", "true")
                } else {
                    self.sensor
                };
                if self.has_fluid(region.block(above)) {
                    growth = self.facts.with(growth, "waterlogged", "true");
                }
                region.set(above, growth);
            }
            return (charge - GROWTH_SPAWN_COST).max(0);
        }
        if r.next_int_bound(ADDITIONAL_DECAY_RATE) != 0 {
            charge
        } else if close {
            charge - 1
        } else {
            charge - decay_penalty(at, origin, charge)
        }
    }

    /// Room above, and at most two growth inhibitors (sensors, shriekers)
    /// within 4 blocks sideways and 2 up.
    fn can_place_growth(&self, region: &dyn Region, p: P) -> bool {
        let above = region.block(p.above(1));
        if !(self.facts.is_air(above)
            || self.facts.is(above, "minecraft:water") && self.facts.is_water_source(above))
        {
            return false;
        }
        let mut found = 0;
        for y in p.y..=p.y + 2 {
            for z in p.z - 4..=p.z + 4 {
                for x in p.x - 4..=p.x + 4 {
                    if self.in_tag(region.block(P::new(x, y, z)), self.inhibitors) {
                        found += 1;
                        if found > 2 {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    /// `SculkVeinBlock.attemptPlaceSculk`: the block a face of the vein
    /// clings to turns to sculk, veins spread over it.
    fn place_sculk(&self, region: &mut dyn Region, p: P, r: &mut WorldgenRandom) -> bool {
        let state = region.block(p);
        let mut dirs = ALL;
        r.shuffle(&mut dirs);
        for support in dirs {
            if !self.has_face(state, support) {
                continue;
            }
            let sp = p.rel(support);
            if !self.in_tag(region.block(sp), self.replaceable_world_gen) {
                continue;
            }
            region.set(sp, self.sculk);
            self.spread_all(region, self.sculk, sp, DEFAULT_ORDER);
            let skip = support.opposite();
            for d in ALL {
                if d == skip {
                    continue;
                }
                let vp = sp.rel(d);
                let vs = region.block(vp);
                if self.is_vein(vs) {
                    self.discharged(region, Behaviour::Vein, vs, vp);
                }
            }
            return true;
        }
        false
    }

    /// `onDischarged`: a vein drops the faces that lie on sculk.
    fn discharged(&self, region: &mut dyn Region, b: Behaviour, mut state: u32, p: P) {
        if b != Behaviour::Vein || !self.is_vein(state) {
            return;
        }
        for d in ALL {
            if self.has_face(state, d) && self.behaviour(region.block(p.rel(d))) == Behaviour::Sculk
            {
                state = self.veins.with_face(state, d, false);
            }
        }
        if !ALL.iter().any(|d| self.has_face(state, *d)) {
            let wet = self.has_fluid(region.block(p));
            state = if wet {
                self.facts.default_state("minecraft:water").unwrap_or(0)
            } else {
                self.facts.default_state("minecraft:air").unwrap_or(0)
            };
        }
        region.set(p, state);
    }

    // The cursors (`SculkSpreader.ChargeCursor`).

    fn update(
        &self,
        region: &mut dyn Region,
        c: &mut Cursor,
        origin: P,
        r: &mut WorldgenRandom,
        spread_veins: bool,
    ) {
        if c.charge <= 0 {
            return;
        }
        if c.update_delay > 0 {
            c.update_delay -= 1;
            return;
        }
        let mut state = region.block(c.pos);
        let mut b = self.behaviour(state);
        if spread_veins
            && self.attempt_spread_vein(region, b, c.pos, state, c.facings)
            && b != Behaviour::Sculk
        {
            state = region.block(c.pos);
            b = self.behaviour(state);
        }
        c.charge = self.attempt_use_charge(region, b, c, origin, r, spread_veins);
        if c.charge <= 0 {
            self.discharged(region, b, state, c.pos);
            return;
        }
        match self.movement(region, c.pos, r, origin) {
            Some(to) => {
                self.discharged(region, b, state, c.pos);
                c.pos = to;
                state = region.block(to);
            }
            None => {
                self.discharged(region, b, state, c.pos);
                c.charge = 0;
                return;
            }
        }
        if self.behaviour(state) != Behaviour::Default {
            c.facings = Some(self.faces(state));
        }
        c.decay_delay = if b == Behaviour::Default {
            (c.decay_delay - 1).max(0)
        } else {
            1
        };
        c.update_delay = 1;
    }

    /// `getValidMovementPos`: a neighbouring sculk or vein (no corners),
    /// one with stone under it first.
    fn movement(&self, region: &dyn Region, p: P, r: &mut WorldgenRandom, origin: P) -> Option<P> {
        let mut offsets = non_corner_neighbours();
        r.shuffle(&mut offsets);
        let mut to = p;
        for o in offsets {
            let n = p.add(o);
            let (dx, dz) = (origin.x - n.x, origin.z - n.z);
            if dx * dx + dz * dz > MAX_WORLDGEN_SPREAD_SQ {
                continue;
            }
            let s = region.block(n);
            if self.behaviour(s) != Behaviour::Default && self.unobstructed(region, p, n) {
                to = n;
                if self.has_substrate_access(region, s, n) {
                    break;
                }
            }
        }
        (to != p).then_some(to)
    }

    fn unobstructed(&self, region: &dyn Region, from: P, to: P) -> bool {
        let d = P::new(to.x - from.x, to.y - from.y, to.z - from.z);
        if d.x.abs() + d.y.abs() + d.z.abs() == 1 {
            return true;
        }
        let dx = if d.x < 0 { Dir::West } else { Dir::East };
        let dy = if d.y < 0 { Dir::Down } else { Dir::Up };
        let dz = if d.z < 0 { Dir::North } else { Dir::South };
        let open = |dir: Dir| {
            !self
                .facts
                .sturdy(region.block(from.rel(dir)), dir.opposite())
        };
        if d.x == 0 {
            open(dy) || open(dz)
        } else if d.y == 0 {
            open(dx) || open(dz)
        } else {
            open(dx) || open(dy)
        }
    }
}

fn decay_penalty(p: P, origin: P, charge: i32) -> i32 {
    let outer = ((dist_sq(p, origin) as f64).sqrt() as f32 - NO_GROWTH_RADIUS as f32).powi(2);
    let max_reach = ((24 - NO_GROWTH_RADIUS) * (24 - NO_GROWTH_RADIUS)) as f32;
    let factor = (outer / max_reach).min(1.0);
    ((charge as f32 * factor * 0.5) as i32).max(1)
}

/// The 18 neighbours that share a face or an edge, in
/// `BlockPos.betweenClosed` order (x fastest, then y, then z).
fn non_corner_neighbours() -> Vec<P> {
    let mut out = Vec::with_capacity(18);
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                let zeros = [x, y, z].iter().filter(|v| **v == 0).count();
                if (1..3).contains(&zeros) {
                    out.push(P::new(x, y, z));
                }
            }
        }
    }
    out
}

/// `SculkPatchFeature.place`.
pub fn patch(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    origin: P,
    c: &Patch,
) -> bool {
    let (Some(sculk), Some(vein), Some(sensor), Some(shrieker)) = (
        facts.default_state("minecraft:sculk"),
        facts.default_state("minecraft:sculk_vein"),
        facts.default_state("minecraft:sculk_sensor"),
        facts.default_state("minecraft:sculk_shrieker"),
    ) else {
        return false;
    };
    let s = Sculk {
        facts,
        sculk,
        veins: Spreader::new(facts, vein),
        sensor,
        shrieker,
        replaceable_world_gen: facts.tag("minecraft:sculk_replaceable_world_gen"),
        replaceable: facts.tag("minecraft:sculk_replaceable"),
        inhibitors: facts.tag("minecraft:sculk_growth_inhibitors"),
    };
    // Only from sculk, or from open space beside a full block.
    let start = region.block(origin);
    if s.behaviour(start) == Behaviour::Default {
        let open = facts.is_air(start)
            || facts.is(start, "minecraft:water") && facts.is_water_source(start);
        if !open
            || !ALL
                .iter()
                .any(|d| s.full_cube(region.block(origin.rel(*d))))
        {
            return false;
        }
    }
    let mut cursors: Vec<Cursor> = Vec::new();
    for round in 0..c.spread_rounds + c.growth_rounds {
        for _ in 0..c.charges {
            let mut left = c.amount;
            while left > 0 {
                let charge = left.min(MAX_CHARGE);
                if cursors.len() < MAX_CURSORS {
                    cursors.push(Cursor {
                        pos: origin,
                        charge,
                        update_delay: 0,
                        decay_delay: 1,
                        facings: None,
                    });
                }
                left -= charge;
            }
        }
        let spread_veins = round < c.spread_rounds;
        for _ in 0..c.attempts {
            if cursors.is_empty() {
                break;
            }
            for cur in &mut cursors {
                let far = (cur.pos.x - origin.x)
                    .abs()
                    .max((cur.pos.y - origin.y).abs())
                    .max((cur.pos.z - origin.z).abs())
                    > 1024;
                if far {
                    cur.charge = 0;
                    continue;
                }
                s.update(region, cur, origin, r, spread_veins);
            }
            cursors.retain(|cur| cur.charge > 0);
        }
        cursors.clear();
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eighteen_neighbours_without_corners() {
        let n = non_corner_neighbours();
        assert_eq!(n.len(), 18);
        assert_eq!(n[0], P::new(0, -1, -1));
        assert!(!n.contains(&P::new(1, 1, 1)));
        assert!(!n.contains(&P::new(0, 0, 0)));
    }

    #[test]
    fn decay_grows_with_distance() {
        assert_eq!(decay_penalty(P::new(2, 0, 0), P::new(0, 0, 0), 100), 1);
        assert_eq!(decay_penalty(P::new(30, 0, 0), P::new(0, 0, 0), 100), 50);
    }
}
