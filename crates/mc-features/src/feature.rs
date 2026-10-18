//! Configured features: what is placed at a position (minecraft.wiki,
//! "Configured feature"). Features name other features and placed
//! features by registry id or define them inline; the loader resolves both
//! once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::Facts;
use crate::misc;
use crate::placement::{Modifier, Placed};
use crate::rand::WorldgenRandom;
use crate::registry::Features;
use crate::state::{Named, StateProvider};
use crate::tree::{FallenTree, Tree};
use crate::{P, Region};

/// What placing needs besides the region.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub facts: &'a Facts,
    pub features: &'a Features,
}

#[derive(Debug)]
pub enum Feature {
    NoOp,
    SimpleBlock {
        provider: StateProvider,
        schedule_tick: bool,
    },
    RandomSelector {
        entries: Vec<(Arc<Placed>, f32)>,
        default: Arc<Placed>,
    },
    SimpleRandom(Vec<Arc<Placed>>),
    WeightedRandom(Vec<(Arc<Placed>, i32)>),
    RandomBoolean(Arc<Placed>, Arc<Placed>),
    Sequence(Vec<Arc<Placed>>),
    Overlay(Vec<Arc<Placed>>),
    Tree(Box<Tree>),
    FallenTree(Box<FallenTree>),
    Other(Box<misc::Misc>),
    /// A type we do not generate yet.
    Unsupported(String),
}

/// Reads features from the data pack, each once.
#[derive(Debug)]
pub struct Loader<'a> {
    pub facts: &'a Facts,
    root: PathBuf,
    named: Named,
    configured: RefCell<HashMap<String, Arc<Feature>>>,
    placed: RefCell<HashMap<String, Arc<Placed>>>,
    pub unsupported: RefCell<HashMap<String, usize>>,
}

fn short(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

impl<'a> Loader<'a> {
    /// `root` is the data pack's `data/minecraft/worldgen` folder.
    pub fn new(facts: &'a Facts, root: PathBuf) -> Self {
        let mut named = Named::new();
        if let Ok(dir) = std::fs::read_dir(root.join("block_state_provider")) {
            for e in dir.flatten() {
                let path = e.path();
                if let (Some(stem), Ok(text)) = (
                    path.file_stem().and_then(|s| s.to_str()),
                    std::fs::read_to_string(&path),
                ) && let Ok(v) = serde_json::from_str(&text)
                {
                    named.insert(stem.to_owned(), v);
                }
            }
        }
        Self {
            facts,
            root,
            named,
            configured: RefCell::new(HashMap::new()),
            placed: RefCell::new(HashMap::new()),
            unsupported: RefCell::new(HashMap::new()),
        }
    }

    fn read(&self, kind: &str, id: &str) -> Option<Value> {
        let path = self.root.join(kind).join(format!("{}.json", short(id)));
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    /// Processors: a `processor_list` id or `{"processors": [...]}`.
    pub fn processors(
        &self,
        v: &Value,
    ) -> Vec<std::sync::Arc<crate::structure::template::Processor>> {
        let def = match v {
            Value::String(id) => self.read("processor_list", id).unwrap_or(Value::Null),
            o => o.clone(),
        };
        let list = if def.is_array() {
            def.clone()
        } else {
            def["processors"].clone()
        };
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                crate::structure::template::Processor::parse(p, self.facts).map(std::sync::Arc::new)
            })
            .collect()
    }

    pub fn provider(&self, v: &Value) -> Option<StateProvider> {
        StateProvider::parse(v, self.facts, &self.named)
    }

    /// A placed feature: an id in `placed_feature`, or `{feature, placement}`.
    pub fn placed(&self, v: &Value) -> Arc<Placed> {
        if let Value::String(id) = v {
            let key = format!("minecraft:{}", short(id));
            if let Some(p) = self.placed.borrow().get(&key) {
                return p.clone();
            }
            let parsed = match self.read("placed_feature", id) {
                Some(def) => self.placed_inline(&def, Some(key.clone())),
                None => Arc::new(Placed {
                    id: Some(key.clone()),
                    feature: Arc::new(Feature::Unsupported(format!("missing {key}"))),
                    modifiers: Vec::new(),
                }),
            };
            self.placed.borrow_mut().insert(key, parsed.clone());
            return parsed;
        }
        if v.get("type").is_some() {
            // A configured feature where a placed one goes: no modifiers.
            return Arc::new(Placed {
                id: None,
                feature: self.configured(v),
                modifiers: Vec::new(),
            });
        }
        self.placed_inline(v, None)
    }

    fn placed_inline(&self, v: &Value, id: Option<String>) -> Arc<Placed> {
        let modifiers = v["placement"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                let parsed = Modifier::parse(m, self.facts);
                if parsed.is_none() {
                    self.note(&format!("modifier {}", m["type"]));
                }
                parsed
            })
            .collect();
        Arc::new(Placed {
            id,
            feature: self.configured(&v["feature"]),
            modifiers,
        })
    }

    /// A configured feature: an id in `feature`, or an inline definition.
    pub fn configured(&self, v: &Value) -> Arc<Feature> {
        if let Value::String(id) = v {
            let key = format!("minecraft:{}", short(id));
            if let Some(f) = self.configured.borrow().get(&key) {
                return f.clone();
            }
            let parsed = match self.read("feature", id) {
                Some(def) => Arc::new(self.parse(&def)),
                None => Arc::new(Feature::Unsupported(format!("missing {key}"))),
            };
            self.configured.borrow_mut().insert(key, parsed.clone());
            return parsed;
        }
        Arc::new(self.parse(v))
    }

    fn note(&self, what: &str) {
        *self
            .unsupported
            .borrow_mut()
            .entry(what.to_owned())
            .or_default() += 1;
    }

    fn parse(&self, v: &Value) -> Feature {
        let ty = v["type"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("minecraft:");
        let list = |k: &str| -> Vec<Arc<Placed>> {
            v[k].as_array()
                .into_iter()
                .flatten()
                .map(|e| self.placed(e))
                .collect()
        };
        let parsed = match ty {
            "no_op" => Some(Feature::NoOp),
            "simple_block" => self
                .provider(&v["to_place"])
                .map(|provider| Feature::SimpleBlock {
                    provider,
                    schedule_tick: v["schedule_tick"].as_bool().unwrap_or(false),
                }),
            "random_selector" => Some(Feature::RandomSelector {
                entries: v["features"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        (
                            self.placed(&e["feature"]),
                            e["chance"].as_f64().unwrap_or(0.0) as f32,
                        )
                    })
                    .collect(),
                default: self.placed(&v["default"]),
            }),
            "simple_random_selector" => Some(Feature::SimpleRandom(list("features"))),
            "weighted_random_selector" => Some(Feature::WeightedRandom(
                v["features"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| {
                        (
                            self.placed(&e["data"]),
                            e["weight"].as_i64().unwrap_or(1) as i32,
                        )
                    })
                    .collect(),
            )),
            "random_boolean_selector" => Some(Feature::RandomBoolean(
                self.placed(&v["feature_true"]),
                self.placed(&v["feature_false"]),
            )),
            "sequence" => Some(Feature::Sequence(list("features"))),
            "overlay" => Some(Feature::Overlay(list("features"))),
            "tree" => Tree::parse(v, self).map(|t| Feature::Tree(Box::new(t))),
            "fallen_tree" => FallenTree::parse(v, self).map(|t| Feature::FallenTree(Box::new(t))),
            _ => misc::Misc::parse(ty, v, self).map(|m| Feature::Other(Box::new(m))),
        };
        parsed.unwrap_or_else(|| {
            self.note(ty);
            Feature::Unsupported(ty.to_owned())
        })
    }

    /// Every configured feature read so far, by id.
    pub fn configured_by_id(&self) -> HashMap<String, Arc<Feature>> {
        self.configured.borrow().clone()
    }
}

impl Feature {
    pub fn place(&self, cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
        let facts = cx.facts;
        match self {
            Feature::NoOp => true,
            Feature::Unsupported(_) => false,
            Feature::SimpleBlock {
                provider,
                schedule_tick,
            } => {
                let Some(state) = provider.get(region, facts, r, p) else {
                    return false;
                };
                if !facts.survives(region, p, state) {
                    return false;
                }
                if facts.property(state, "half") == Some("lower")
                    && facts.has_property(facts.with(state, "half", "upper"), "half")
                    && facts.with(state, "half", "upper") != state
                {
                    // A two-block plant: room above for the top half.
                    let above = p.above(1);
                    if !facts.is_air(region.block(above))
                        && !facts.is_water_source(region.block(above))
                    {
                        return false;
                    }
                    let lower = waterlog(facts, region, p, state);
                    let upper = waterlog(facts, region, above, facts.with(state, "half", "upper"));
                    region.set(p, lower);
                    region.set(above, upper);
                } else {
                    let s = waterlog(facts, region, p, state);
                    region.set(p, s);
                }
                if *schedule_tick {
                    region.schedule_tick(p);
                }
                true
            }
            Feature::RandomSelector { entries, default } => {
                for (f, chance) in entries {
                    if r.next_float() < *chance {
                        return f.place(cx, region, r, p, None);
                    }
                }
                default.place(cx, region, r, p, None)
            }
            Feature::SimpleRandom(list) => {
                if list.is_empty() {
                    return false;
                }
                let i = r.next_int_bound(list.len() as i32) as usize;
                list[i].place(cx, region, r, p, None)
            }
            Feature::WeightedRandom(list) => match crate::value::weighted(list, r) {
                Some(f) => f.place(cx, region, r, p, None),
                None => false,
            },
            Feature::RandomBoolean(t, f) => {
                if r.next_bool() {
                    t.place(cx, region, r, p, None)
                } else {
                    f.place(cx, region, r, p, None)
                }
            }
            Feature::Sequence(list) => {
                for f in list {
                    if !f.place(cx, region, r, p, None) {
                        return false;
                    }
                }
                true
            }
            Feature::Overlay(list) => {
                let mut any = false;
                for f in list {
                    any |= f.place(cx, region, r, p, None);
                }
                any
            }
            Feature::Tree(t) => t.place(cx, region, r, p),
            Feature::FallenTree(t) => t.place(cx, region, r, p),
            Feature::Other(m) => m.place(cx, region, r, p),
        }
    }
}

/// Plants that hold water take it from where they are put.
pub fn waterlog(facts: &Facts, region: &dyn Region, p: P, state: u32) -> u32 {
    if facts.has_property(state, "waterlogged") {
        let wet = facts.is_water_source(region.block(p));
        facts.with(state, "waterlogged", if wet { "true" } else { "false" })
    } else {
        state
    }
}
