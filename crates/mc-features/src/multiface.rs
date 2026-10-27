//! Blocks on the faces of others (glow lichen, sculk veins) and how they
//! spread: vanilla's `MultifaceBlock` placement and `MultifaceSpreader`, with
//! the sculk vein's own rules (`SculkVeinSpreaderConfig`).

use mc_tick::Dir;

use crate::blocks::{Facts, Tag};
use crate::rand::WorldgenRandom;
use crate::{P, Region};

/// `Direction.values()` order.
pub(crate) const ALL: [Dir; 6] = [
    Dir::Down,
    Dir::Up,
    Dir::North,
    Dir::South,
    Dir::West,
    Dir::East,
];

/// How a spread steps from a face.
#[derive(Clone, Copy)]
pub(crate) enum Spread {
    SamePosition,
    SamePlane,
    WrapAround,
}

pub(crate) const DEFAULT_ORDER: &[Spread] =
    &[Spread::SamePosition, Spread::SamePlane, Spread::WrapAround];
pub(crate) const SAME_SPACE: &[Spread] = &[Spread::SamePosition];

impl Spread {
    fn pos(self, p: P, toward: Dir, from_face: Dir) -> (P, Dir) {
        match self {
            Spread::SamePosition => (p, toward),
            Spread::SamePlane => (p.rel(toward), from_face),
            Spread::WrapAround => (p.rel(toward).rel(from_face), toward.opposite()),
        }
    }
}

/// The spreader of one multiface block.
pub(crate) struct Spreader<'a> {
    facts: &'a Facts,
    /// The block's default state.
    pub block: u32,
    /// Sculk vein rules: not onto sculk or catalysts, not round a sturdy
    /// corner, and any other block may be spread from.
    vein: Option<VeinRules>,
}

struct VeinRules {
    sculk: u32,
    fire: Option<Tag>,
}

impl<'a> Spreader<'a> {
    pub fn new(facts: &'a Facts, block: u32) -> Self {
        let vein = (facts.block_name(block) == "minecraft:sculk_vein").then(|| VeinRules {
            sculk: facts
                .default_state("minecraft:sculk")
                .map_or(u32::MAX, |s| facts.block(s)),
            fire: facts.tag("minecraft:fire"),
        });
        Self { facts, block, vein }
    }

    pub fn is_block(&self, s: u32) -> bool {
        self.facts.block(s) == self.facts.block(self.block)
    }

    pub fn has_face(&self, s: u32, d: Dir) -> bool {
        self.facts.property(s, d.name()) == Some("true")
    }

    pub fn with_face(&self, s: u32, d: Dir, on: bool) -> u32 {
        self.facts
            .with(s, d.name(), if on { "true" } else { "false" })
    }

    /// `MultifaceBlock.canAttachTo`: the neighbour toward `d` shows a full
    /// face.
    pub fn can_attach(&self, region: &dyn Region, p: P, d: Dir) -> bool {
        self.facts.sturdy(region.block(p.rel(d)), d.opposite())
    }

    /// `isOtherBlockValidAsSource`.
    fn other_source(&self, s: u32) -> bool {
        self.vein.is_some() && !self.is_block(s)
    }

    fn state_can_be_replaced(
        &self,
        region: &dyn Region,
        source: P,
        at: P,
        face: Dir,
        existing: u32,
    ) -> bool {
        let facts = self.facts;
        let plain =
            facts.is_air(existing) || self.is_block(existing) || facts.is_water_source(existing);
        let Some(v) = &self.vein else {
            return plain;
        };
        let against = region.block(at.rel(face));
        if facts.block(against) == v.sculk
            || facts.is(against, "minecraft:sculk_catalyst")
            || facts.is(against, "minecraft:moving_piston")
        {
            return false;
        }
        let manhattan = (source.x - at.x).abs() + (source.y - at.y).abs() + (source.z - at.z).abs();
        if manhattan == 2 && facts.sturdy(region.block(source.rel(face.opposite())), face) {
            return false;
        }
        let fluid = facts.fluid(existing);
        if !fluid.is_none() && !matches!(fluid, crate::blocks::Fluid::Water { .. }) {
            return false;
        }
        if v.fire.is_some_and(|t| facts.in_tag(existing, t)) {
            return false;
        }
        facts.replaceable(existing) || plain
    }

    /// `isValidStateForPlacement`.
    fn valid_for_placement(&self, region: &dyn Region, old: u32, at: P, face: Dir) -> bool {
        (!self.is_block(old) || !self.has_face(old, face)) && self.can_attach(region, at, face)
    }

    /// `getStateForPlacement`: `old` with one more face, or a new block
    /// (waterlogged in water).
    pub fn state_for_placement(
        &self,
        region: &dyn Region,
        old: u32,
        at: P,
        face: Dir,
    ) -> Option<u32> {
        if !self.valid_for_placement(region, old, at, face) {
            return None;
        }
        let base = if self.is_block(old) {
            old
        } else if self.facts.is_water_source(old) {
            self.facts.with(self.block, "waterlogged", "true")
        } else {
            self.block
        };
        Some(self.with_face(base, face, true))
    }

    fn can_spread_into(&self, region: &dyn Region, source: P, at: P, face: Dir) -> bool {
        let existing = region.block(at);
        self.state_can_be_replaced(region, source, at, face, existing)
            && self.valid_for_placement(region, existing, at, face)
    }

    fn place(&self, region: &mut dyn Region, at: P, face: Dir) -> bool {
        let old = region.block(at);
        match self.state_for_placement(region, old, at, face) {
            Some(s) => {
                region.set(at, s);
                true
            }
            None => false,
        }
    }

    /// `getSpreadFromFaceTowardDirection`.
    fn spread_pos(
        &self,
        region: &dyn Region,
        state: u32,
        p: P,
        from: Dir,
        toward: Dir,
        types: &[Spread],
    ) -> Option<(P, Dir)> {
        if toward.axis() == from.axis() {
            return None;
        }
        let ok =
            self.other_source(state) || self.has_face(state, from) && !self.has_face(state, toward);
        if !ok {
            return None;
        }
        types
            .iter()
            .map(|t| t.pos(p, toward, from))
            .find(|&(q, face)| self.can_spread_into(region, p, q, face))
    }

    fn spread_toward(
        &self,
        region: &mut dyn Region,
        state: u32,
        p: P,
        from: Dir,
        toward: Dir,
        types: &[Spread],
    ) -> bool {
        self.spread_pos(region, state, p, from, toward, types)
            .is_some_and(|(q, face)| self.place(region, q, face))
    }

    /// `spreadAll`: from every face of `state` toward every direction; how
    /// many faces grew.
    pub fn spread_all(&self, region: &mut dyn Region, state: u32, p: P, types: &[Spread]) -> i64 {
        let mut grown = 0;
        for from in ALL {
            if !(self.other_source(state) || self.has_face(state, from)) {
                continue;
            }
            for toward in ALL {
                if self.spread_toward(region, state, p, from, toward, types) {
                    grown += 1;
                }
            }
        }
        grown
    }

    /// `spreadFromFaceTowardRandomDirection`: the first direction, in a
    /// shuffled order, it grows toward.
    pub fn spread_from_face(
        &self,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        state: u32,
        p: P,
        from: Dir,
    ) -> bool {
        let mut dirs = ALL;
        r.shuffle(&mut dirs);
        dirs.into_iter()
            .any(|toward| self.spread_toward(region, state, p, from, toward, DEFAULT_ORDER))
    }
}
