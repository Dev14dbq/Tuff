//! Block state providers: which block a feature puts down (minecraft.wiki,
//! "Block state provider": simple, weighted, randomized property, random
//! axis, noise-chosen flowers, rule based).

use std::collections::HashMap;

use mc_worldgen::noise::{FbmNoise, FbmParameters};
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::Facts;
use crate::predicate::{BlockSet, Predicate};
use crate::rand::WorldgenRandom;
use crate::value::{IntProvider, weighted};
use crate::{P, Region};

/// `"minecraft:stone"` or `{"id"/"Name": ..., "properties"/"Properties": {...}}`.
pub fn parse_state(v: &Value, facts: &Facts) -> Option<u32> {
    match v {
        Value::String(s) => facts.default_state(s),
        Value::Object(o) => {
            let name = o.get("id").or_else(|| o.get("Name"))?.as_str()?;
            let props = o.get("properties").or_else(|| o.get("Properties"));
            // Properties the block does not have (a fluid's `falling`) are
            // left out.
            let mut state = facts.default_state(name)?;
            for (k, v) in props.and_then(Value::as_object).into_iter().flatten() {
                if let Some(v) = v.as_str() {
                    state = facts.with(state, k, v);
                }
            }
            Some(state)
        }
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub enum StateProvider {
    Simple(u32),
    Weighted(Vec<(StateProvider, i32)>),
    RandomizedInt {
        property: String,
        source: Box<StateProvider>,
        values: IntProvider,
    },
    /// A random axis, or a set facing.
    Rotated(Box<StateProvider>, Option<String>),
    NoiseThreshold {
        noise: Box<FbmNoise>,
        scale: f64,
        threshold: f64,
        high_chance: f32,
        default: u32,
        low: Vec<u32>,
        high: Vec<u32>,
    },
    Noise {
        noise: Box<FbmNoise>,
        scale: f64,
        states: Vec<u32>,
    },
    DualNoise {
        noise: Box<FbmNoise>,
        scale: f64,
        slow: Box<FbmNoise>,
        slow_scale: f64,
        states: Vec<u32>,
        variety: (i32, i32),
    },
    RuleBased {
        rules: Vec<(Predicate, StateProvider)>,
        fallback: Option<Box<StateProvider>>,
    },
    RandomBlock(Vec<u32>),
}

/// Named providers (`worldgen/block_state_provider`).
pub type Named = HashMap<String, Value>;

fn noise(v: &Value, seed: i64) -> Option<Box<FbmNoise>> {
    let params = FbmParameters::from_json(v)?;
    let mut random = WorldgenRandom::legacy(seed);
    Some(Box::new(FbmNoise::new(&mut random, &params, false)))
}

fn states(v: &Value, facts: &Facts) -> Vec<u32> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| parse_state(s, facts))
        .collect()
}

/// A state from a list by a noise value in -1..1.
fn by_noise(states: &[u32], value: f64) -> Option<u32> {
    if states.is_empty() {
        return None;
    }
    let d = ((1.0 + value) / 2.0).clamp(0.0, 0.9999);
    Some(states[(d * states.len() as f64) as usize])
}

impl StateProvider {
    pub fn parse(v: &Value, facts: &Facts, named: &Named) -> Option<Self> {
        if let Value::String(s) = v {
            if let Some(def) = named.get(s.trim_start_matches("minecraft:")) {
                return StateProvider::parse(def, facts, named);
            }
            return parse_state(v, facts).map(StateProvider::Simple);
        }
        let Some(ty) = v["type"].as_str() else {
            return parse_state(v, facts).map(StateProvider::Simple);
        };
        let seed = v["seed"].as_i64().unwrap_or(0);
        let scale = v["scale"].as_f64().unwrap_or(1.0);
        Some(match ty.trim_start_matches("minecraft:") {
            "simple_state_provider" => StateProvider::Simple(parse_state(&v["state"], facts)?),
            "weighted" => StateProvider::Weighted(
                v["entries"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| {
                        Some((
                            StateProvider::parse(&e["data"], facts, named)?,
                            e["weight"].as_i64()? as i32,
                        ))
                    })
                    .collect(),
            ),
            "randomized_int" => StateProvider::RandomizedInt {
                property: v["property"].as_str()?.to_owned(),
                source: Box::new(StateProvider::parse(&v["source"], facts, named)?),
                values: IntProvider::parse(&v["values"])?,
            },
            "rotated" => StateProvider::Rotated(
                Box::new(StateProvider::parse(&v["state"], facts, named)?),
                v["direction"].as_str().map(str::to_owned),
            ),
            "noise_threshold" => StateProvider::NoiseThreshold {
                noise: noise(&v["noise"], seed)?,
                scale,
                threshold: v["threshold"].as_f64().unwrap_or(0.0),
                high_chance: v["high_chance"].as_f64().unwrap_or(0.0) as f32,
                default: parse_state(&v["default_state"], facts)?,
                low: states(&v["low_states"], facts),
                high: states(&v["high_states"], facts),
            },
            "noise" => StateProvider::Noise {
                noise: noise(&v["noise"], seed)?,
                scale,
                states: states(&v["states"], facts),
            },
            "dual_noise" => {
                let variety = match &v["variety"] {
                    Value::Array(a) if a.len() == 2 => {
                        (a[0].as_i64()? as i32, a[1].as_i64()? as i32)
                    }
                    Value::Number(n) => (n.as_i64()? as i32, n.as_i64()? as i32),
                    o => (
                        o["min_inclusive"].as_i64()? as i32,
                        o["max_inclusive"].as_i64()? as i32,
                    ),
                };
                StateProvider::DualNoise {
                    noise: noise(&v["noise"], seed)?,
                    scale,
                    slow: noise(&v["slow_noise"], seed)?,
                    slow_scale: v["slow_scale"].as_f64().unwrap_or(1.0),
                    states: states(&v["states"], facts),
                    variety,
                }
            }
            "rule_based" => StateProvider::RuleBased {
                rules: v["rules"]
                    .as_array()?
                    .iter()
                    .filter_map(|r| {
                        Some((
                            Predicate::parse(&r["if_true"], facts)?,
                            StateProvider::parse(&r["then"], facts, named)?,
                        ))
                    })
                    .collect(),
                fallback: v
                    .get("fallback")
                    .and_then(|f| StateProvider::parse(f, facts, named))
                    .map(Box::new),
            },
            "random_block" => StateProvider::RandomBlock(
                BlockSet::parse(&v["blocks"], facts)?
                    .blocks(facts)
                    .into_iter()
                    .map(|b| facts.default_of(b))
                    .collect(),
            ),
            _ => return None,
        })
    }

    /// The state for `p`; `None` when a rule-based provider has no rule
    /// that applies.
    pub fn get(
        &self,
        region: &dyn Region,
        facts: &Facts,
        r: &mut WorldgenRandom,
        p: P,
    ) -> Option<u32> {
        let at = |n: &FbmNoise, s: f64| {
            n.value(f64::from(p.x) * s, f64::from(p.y) * s, f64::from(p.z) * s)
        };
        match self {
            StateProvider::Simple(s) => Some(*s),
            StateProvider::Weighted(list) => weighted(list, r)?.get(region, facts, r, p),
            StateProvider::RandomizedInt {
                property,
                source,
                values,
            } => {
                let state = source.get(region, facts, r, p)?;
                let v = values.sample(r);
                Some(facts.with(state, property, &v.to_string()))
            }
            StateProvider::Rotated(source, facing) => {
                let s = source.get(region, facts, r, p)?;
                match facing {
                    Some(f) => Some(facts.with(s, "facing", f)),
                    None => {
                        let axis = ["x", "y", "z"][r.next_int_bound(3) as usize];
                        Some(facts.with(s, "axis", axis))
                    }
                }
            }
            StateProvider::NoiseThreshold {
                noise,
                scale,
                threshold,
                high_chance,
                default,
                low,
                high,
            } => {
                if at(noise, *scale) < *threshold {
                    if low.is_empty() {
                        return Some(*default);
                    }
                    return Some(*r.pick(low));
                }
                if !high.is_empty() && r.next_float() < *high_chance {
                    return Some(*r.pick(high));
                }
                Some(*default)
            }
            StateProvider::Noise {
                noise,
                scale,
                states,
            } => by_noise(states, at(noise, *scale)),
            StateProvider::DualNoise {
                noise,
                scale,
                slow,
                slow_scale,
                states,
                variety,
            } => {
                let slow_at = |q: P| {
                    slow.value(
                        f64::from(q.x) * slow_scale,
                        f64::from(q.y) * slow_scale,
                        f64::from(q.z) * slow_scale,
                    )
                };
                let d = slow_at(p);
                let t = ((d + 1.0) / 2.0).clamp(0.0, 1.0);
                let count =
                    (f64::from(variety.0) + t * f64::from(variety.1 + 1 - variety.0)) as i32;
                let picked: Vec<u32> = (0..count.max(0))
                    .filter_map(|j| {
                        by_noise(states, slow_at(p.add(P::new(j * 54545, 0, j * 34234))))
                    })
                    .collect();
                by_noise(&picked, at(noise, *scale))
            }
            StateProvider::RuleBased { rules, fallback } => {
                for (pred, then) in rules {
                    if pred.test(region, facts, p) {
                        return then.get(region, facts, r, p);
                    }
                }
                fallback.as_ref()?.get(region, facts, r, p)
            }
            StateProvider::RandomBlock(list) => (!list.is_empty()).then(|| *r.pick(list)),
        }
    }
}
