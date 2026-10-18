//! Block predicates (where a feature may go) and rule tests (what an ore
//! replaces), as the data pack writes them (minecraft.wiki, "Block
//! predicate", "Rule test").

use mc_tick::Dir;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::{Facts, Tag};
use crate::rand::WorldgenRandom;
use crate::value::Anchor;
use crate::{P, Region};

/// A set of blocks: one, a list, or a tag.
#[derive(Clone, Debug, PartialEq)]
pub enum BlockSet {
    Blocks(Vec<u32>),
    Tag(Tag),
}

impl BlockSet {
    pub fn parse(v: &Value, facts: &Facts) -> Option<Self> {
        match v {
            Value::String(s) if s.starts_with('#') => facts.tag(s).map(BlockSet::Tag),
            Value::String(s) => Some(BlockSet::Blocks(facts.block_id(s).into_iter().collect())),
            Value::Array(list) => Some(BlockSet::Blocks(
                list.iter()
                    .filter_map(|e| e.as_str().and_then(|s| facts.block_id(s)))
                    .collect(),
            )),
            _ => None,
        }
    }

    pub fn contains(&self, facts: &Facts, state: u32) -> bool {
        match self {
            BlockSet::Blocks(list) => list.contains(&facts.block(state)),
            BlockSet::Tag(t) => facts.in_tag(state, *t),
        }
    }

    pub fn blocks(&self, facts: &Facts) -> Vec<u32> {
        match self {
            BlockSet::Blocks(list) => list.clone(),
            BlockSet::Tag(t) => facts.tag_blocks(*t),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    True,
    Blocks(BlockSet, P),
    Fluids(Vec<String>, P),
    Solid(P),
    Replaceable(P),
    InsideWorld(P),
    Sturdy(Dir, P),
    Survives(u32, P),
    Height(Anchor, Anchor),
    Volume(Box<Predicate>, P, P),
    All(Vec<Predicate>),
    Any(Vec<Predicate>),
    Not(Box<Predicate>),
}

fn offset(v: &Value) -> P {
    match v["offset"].as_array() {
        Some(a) if a.len() == 3 => P::new(
            a[0].as_i64().unwrap_or(0) as i32,
            a[1].as_i64().unwrap_or(0) as i32,
            a[2].as_i64().unwrap_or(0) as i32,
        ),
        _ => P::new(0, 0, 0),
    }
}

fn vec3(v: &Value) -> P {
    match v.as_array() {
        Some(a) if a.len() == 3 => P::new(
            a[0].as_i64().unwrap_or(0) as i32,
            a[1].as_i64().unwrap_or(0) as i32,
            a[2].as_i64().unwrap_or(0) as i32,
        ),
        _ => P::new(0, 0, 0),
    }
}

impl Predicate {
    pub fn parse(v: &Value, facts: &Facts) -> Option<Self> {
        let ty = v["type"].as_str()?.trim_start_matches("minecraft:");
        let list = |k: &str| -> Option<Vec<Predicate>> {
            v[k].as_array()?
                .iter()
                .map(|p| Predicate::parse(p, facts))
                .collect()
        };
        Some(match ty {
            "true" => Predicate::True,
            "matching_blocks" => {
                Predicate::Blocks(BlockSet::parse(&v["blocks"], facts)?, offset(v))
            }
            "matching_block_tag" => {
                Predicate::Blocks(BlockSet::Tag(facts.tag(v["tag"].as_str()?)?), offset(v))
            }
            "matching_fluids" => {
                let names = match &v["fluids"] {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(a) => a
                        .iter()
                        .filter_map(|e| e.as_str().map(str::to_owned))
                        .collect(),
                    _ => Vec::new(),
                };
                Predicate::Fluids(names, offset(v))
            }
            "solid" => Predicate::Solid(offset(v)),
            "replaceable" => Predicate::Replaceable(offset(v)),
            "inside_world_bounds" => Predicate::InsideWorld(offset(v)),
            "has_sturdy_face" => {
                Predicate::Sturdy(Dir::from_name(v["direction"].as_str()?)?, offset(v))
            }
            "would_survive" => {
                Predicate::Survives(crate::state::parse_state(&v["state"], facts)?, offset(v))
            }
            "height_range" => Predicate::Height(
                Anchor::parse(&v["min_inclusive"])?,
                Anchor::parse(&v["max_inclusive"])?,
            ),
            "volume_match" => Predicate::Volume(
                Box::new(Predicate::parse(&v["match"], facts)?),
                vec3(&v["min"]),
                vec3(&v["max"]),
            ),
            "all_of" => Predicate::All(list("predicates")?),
            "any_of" => Predicate::Any(list("predicates")?),
            "not" => Predicate::Not(Box::new(Predicate::parse(&v["predicate"], facts)?)),
            _ => return None,
        })
    }

    pub fn test(&self, region: &dyn Region, facts: &Facts, p: P) -> bool {
        match self {
            Predicate::True => true,
            Predicate::Blocks(set, o) => set.contains(facts, region.block(p.add(*o))),
            Predicate::Fluids(names, o) => {
                let f = facts.fluid(region.block(p.add(*o))).name();
                names.iter().any(|n| n == f)
            }
            Predicate::Solid(o) => facts.solid(region.block(p.add(*o))),
            Predicate::Replaceable(o) => facts.replaceable(region.block(p.add(*o))),
            Predicate::InsideWorld(o) => {
                let h = region.heights();
                let y = p.y + o.y;
                y >= h.min_y && y < h.min_y + h.height
            }
            Predicate::Sturdy(d, o) => facts.sturdy(region.block(p.add(*o)), *d),
            Predicate::Survives(state, o) => facts.survives(region, p.add(*o), *state),
            Predicate::Height(lo, hi) => {
                let h = region.heights();
                p.y >= lo.resolve(h) && p.y <= hi.resolve(h)
            }
            Predicate::Volume(m, lo, hi) => {
                for x in lo.x..=hi.x {
                    for y in lo.y..=hi.y {
                        for z in lo.z..=hi.z {
                            if !m.test(region, facts, p.add(P::new(x, y, z))) {
                                return false;
                            }
                        }
                    }
                }
                true
            }
            Predicate::All(list) => list.iter().all(|q| q.test(region, facts, p)),
            Predicate::Any(list) => list.iter().any(|q| q.test(region, facts, p)),
            Predicate::Not(q) => !q.test(region, facts, p),
        }
    }
}

/// What an ore (or a structure processor) may replace.
#[derive(Clone, Debug, PartialEq)]
pub enum RuleTest {
    Always,
    Tag(Tag),
    Block(u32),
    State(u32),
    RandomBlock(u32, f32),
    RandomState(u32, f32),
    Height(i32, i32),
    All(Vec<RuleTest>),
    Any(Vec<RuleTest>),
    Not(Box<RuleTest>),
}

impl RuleTest {
    pub fn parse(v: &Value, facts: &Facts) -> Option<Self> {
        let ty = v["predicate_type"]
            .as_str()?
            .trim_start_matches("minecraft:");
        let float = |k: &str| v[k].as_f64().unwrap_or(1.0) as f32;
        let rules = || -> Option<Vec<RuleTest>> {
            v["rules"]
                .as_array()?
                .iter()
                .map(|r| RuleTest::parse(r, facts))
                .collect()
        };
        Some(match ty {
            "always_true" => RuleTest::Always,
            "tag_match" => RuleTest::Tag(facts.tag(v["tag"].as_str()?)?),
            "block_match" => RuleTest::Block(facts.block_id(v["block"].as_str()?)?),
            "blockstate_match" => {
                RuleTest::State(crate::state::parse_state(&v["block_state"], facts)?)
            }
            "random_block_match" => {
                RuleTest::RandomBlock(facts.block_id(v["block"].as_str()?)?, float("probability"))
            }
            "random_blockstate_match" => RuleTest::RandomState(
                crate::state::parse_state(&v["block_state"], facts)?,
                float("probability"),
            ),
            "height_match" => RuleTest::Height(
                v["min_inclusive"].as_i64().unwrap_or(i64::from(i32::MIN)) as i32,
                v["max_inclusive"].as_i64().unwrap_or(i64::from(i32::MAX)) as i32,
            ),
            "all_of" => RuleTest::All(rules()?),
            "any_of" => RuleTest::Any(rules()?),
            "not" => RuleTest::Not(Box::new(RuleTest::parse(&v["rule"], facts)?)),
            _ => return None,
        })
    }

    pub fn test(&self, facts: &Facts, state: u32, y: i32, r: &mut WorldgenRandom) -> bool {
        match self {
            RuleTest::Always => true,
            RuleTest::Tag(t) => facts.in_tag(state, *t),
            RuleTest::Block(b) => facts.block(state) == *b,
            RuleTest::State(s) => state == *s,
            RuleTest::RandomBlock(b, p) => facts.block(state) == *b && r.next_float() < *p,
            RuleTest::RandomState(s, p) => state == *s && r.next_float() < *p,
            RuleTest::Height(lo, hi) => (*lo..=*hi).contains(&y),
            RuleTest::All(list) => list.iter().all(|t| t.test(facts, state, y, r)),
            RuleTest::Any(list) => list.iter().any(|t| t.test(facts, state, y, r)),
            RuleTest::Not(t) => !t.test(facts, state, y, r),
        }
    }
}
