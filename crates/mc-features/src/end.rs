//! The End's feature types: the obsidian spikes around the main island,
//! the arrival platform, gateways, chorus plants and small floating
//! islands. Behaviour as minecraft.wiki describes each feature.

use mc_tick::Dir;
use mc_worldgen::random::{Legacy, RandomSource};
use serde_json::Value;

use crate::blocks::Facts;
use crate::feature::{Ctx, Loader};
use crate::rand::WorldgenRandom;
use crate::{P, Region};

#[derive(Debug)]
pub enum End {
    /// `end_spike`; with no spikes listed, the ten of the world's seed.
    Spikes(Vec<Spike>),
    /// `end_platform`: the obsidian square players arrive on.
    Platform,
    /// `end_gateway`: a gateway in its bedrock frame.
    Gateway { exit: Option<P>, exact: bool },
    /// `chorus_plant`.
    Chorus,
    /// `end_island`: a small island, wide at the top.
    Island,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spike {
    pub x: i32,
    pub z: i32,
    pub radius: i32,
    pub height: i32,
    pub guarded: bool,
}

/// The ten spikes of a world: placed on a circle of radius 42, their sizes
/// shuffled by the seed.
pub fn spikes_for_seed(seed: i64) -> Vec<Spike> {
    let key = Legacy::new(seed).next_long() & 65535;
    let mut r = Legacy::new(key);
    let mut sizes: Vec<i32> = (0..10).collect();
    for j in (2..=sizes.len()).rev() {
        let k = r.next_int_bound(j as i32) as usize;
        sizes.swap(j - 1, k);
    }
    (0..10)
        .map(|i| {
            let angle = 2.0 * (-std::f64::consts::PI + std::f64::consts::PI / 10.0 * f64::from(i));
            let k = sizes[i as usize];
            Spike {
                x: (42.0 * angle.cos()).floor() as i32,
                z: (42.0 * angle.sin()).floor() as i32,
                radius: 2 + k / 3,
                height: 76 + k * 3,
                guarded: k == 1 || k == 2,
            }
        })
        .collect()
}

impl End {
    pub fn parse(ty: &str, v: &Value, _l: &Loader) -> Option<Self> {
        Some(match ty {
            "end_spike" => End::Spikes(
                v["spikes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        let int = |k: &str| s[k].as_i64().unwrap_or(0) as i32;
                        Spike {
                            x: int("centerX"),
                            z: int("centerZ"),
                            radius: int("radius"),
                            height: int("height"),
                            guarded: s["guarded"].as_bool().unwrap_or(false),
                        }
                    })
                    .collect(),
            ),
            "end_platform" => End::Platform,
            "end_gateway" => End::Gateway {
                exit: v["exit"].as_array().and_then(|a| {
                    let c = |i: usize| a.get(i)?.as_i64().map(|n| n as i32);
                    Some(P::new(c(0)?, c(1)?, c(2)?))
                }),
                exact: v["exact"].as_bool().unwrap_or(false),
            },
            "chorus_plant" => End::Chorus,
            "end_island" => End::Island,
            _ => return None,
        })
    }

    pub fn place(&self, cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
        let facts = cx.facts;
        let state = |n: &str| facts.default_state(n).unwrap_or(0);
        match self {
            End::Spikes(list) => {
                let seeded;
                let list = if list.is_empty() {
                    seeded = spikes_for_seed(region.seed());
                    &seeded
                } else {
                    list
                };
                for s in list {
                    if s.x >> 4 == p.x >> 4 && s.z >> 4 == p.z >> 4 {
                        spike(facts, region, s);
                    }
                }
                true
            }
            End::Platform => {
                let (obsidian, air) = (state("minecraft:obsidian"), state("minecraft:air"));
                for dz in -2..=2 {
                    for dx in -2..=2 {
                        for dy in -1..3 {
                            let q = p.offset(dx, dy, dz);
                            let want = if dy == -1 { obsidian } else { air };
                            if region.block(q) != want {
                                region.set(q, want);
                            }
                        }
                    }
                }
                true
            }
            End::Gateway { exit, exact } => {
                let (bedrock, air) = (state("minecraft:bedrock"), state("minecraft:air"));
                for dz in -1..=1 {
                    for dy in -2..=2 {
                        for dx in -1..=1 {
                            let q = p.offset(dx, dy, dz);
                            let (on_x, on_y, on_z) = (dx == 0, dy == 0, dz == 0);
                            let ends = dy.abs() == 2;
                            if on_x && on_y && on_z {
                                region.set(q, state("minecraft:end_gateway"));
                                let mut be = mc_nbt::Compound::new();
                                be.insert(
                                    "id",
                                    mc_nbt::Tag::String("minecraft:end_gateway".into()),
                                );
                                be.insert("Age", mc_nbt::Tag::Long(0));
                                if let Some(e) = exit {
                                    be.insert(
                                        "exit_portal",
                                        mc_nbt::Tag::IntArray(vec![e.x, e.y, e.z]),
                                    );
                                }
                                if *exact {
                                    be.insert("ExactTeleport", mc_nbt::Tag::Byte(1));
                                }
                                region.set_block_entity(q, be);
                            } else if on_y {
                                region.set(q, air);
                            } else if (ends && on_x && on_z) || ((on_x || on_z) && !ends) {
                                region.set(q, bedrock);
                            } else {
                                region.set(q, air);
                            }
                        }
                    }
                }
                true
            }
            End::Chorus => {
                if !facts.is_air(region.block(p))
                    || !facts.is(region.block(p.below(1)), "minecraft:end_stone")
                {
                    return false;
                }
                chorus_set(facts, region, p);
                chorus_grow(facts, region, r, p, p, 8, 0);
                true
            }
            End::Island => {
                let end_stone = state("minecraft:end_stone");
                let mut f = r.next_int_bound(3) as f32 + 4.0;
                let mut dy = 0;
                while f > 0.5 {
                    for dx in (-f).floor() as i32..=f.ceil() as i32 {
                        for dz in (-f).floor() as i32..=f.ceil() as i32 {
                            if (dx * dx + dz * dz) as f32 <= (f + 1.0) * (f + 1.0) {
                                region.set(p.offset(dx, dy, dz), end_stone);
                            }
                        }
                    }
                    f -= r.next_int_bound(2) as f32 + 0.5;
                    dy -= 1;
                }
                true
            }
        }
    }
}

fn spike(facts: &Facts, region: &mut dyn Region, s: &Spike) {
    let state = |n: &str| facts.default_state(n).unwrap_or(0);
    let (obsidian, air) = (state("minecraft:obsidian"), state("minecraft:air"));
    let h = region.heights();
    let r2 = f64::from(s.radius * s.radius + 1);
    for z in s.z - s.radius..=s.z + s.radius {
        for y in h.min_y..=s.height + 10 {
            for x in s.x - s.radius..=s.x + s.radius {
                let (dx, dz) = (f64::from(x - s.x), f64::from(z - s.z));
                let q = P::new(x, y, z);
                if dx * dx + dz * dz <= r2 && y < s.height {
                    region.set(q, obsidian);
                } else if y > 65 {
                    region.set(q, air);
                }
            }
        }
    }
    if s.guarded {
        let bars = state("minecraft:iron_bars");
        let b = |v: bool| if v { "true" } else { "false" };
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                for dy in 0..=3 {
                    let (edge_x, edge_z, top) = (dx.abs() == 2, dz.abs() == 2, dy == 3);
                    if !(edge_x || edge_z || top) {
                        continue;
                    }
                    let ns = edge_x || top;
                    let we = edge_z || top;
                    let mut st = facts.with(bars, "north", b(ns && dz != -2));
                    st = facts.with(st, "south", b(ns && dz != 2));
                    st = facts.with(st, "west", b(we && dx != -2));
                    st = facts.with(st, "east", b(we && dx != 2));
                    region.set(P::new(s.x + dx, s.height + dy, s.z + dz), st);
                }
            }
        }
    }
    // The crystal (an entity, Ф5) stands on bedrock in fire.
    region.set(P::new(s.x, s.height, s.z), state("minecraft:bedrock"));
    region.set(P::new(s.x, s.height + 1, s.z), state("minecraft:fire"));
}

const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

/// A chorus plant at `p`, joined to what is around it now.
fn chorus_set(facts: &Facts, region: &mut dyn Region, p: P) {
    let Some(plant) = facts.default_state("minecraft:chorus_plant") else {
        return;
    };
    let joins = |q: P, end_stone: bool| {
        let s = region.block(q);
        facts.is(s, "minecraft:chorus_plant")
            || facts.is(s, "minecraft:chorus_flower")
            || (end_stone && facts.is(s, "minecraft:end_stone"))
    };
    let b = |v: bool| if v { "true" } else { "false" };
    let mut st = facts.with(plant, "down", b(joins(p.below(1), true)));
    st = facts.with(st, "up", b(joins(p.above(1), false)));
    for d in HORIZONTAL {
        st = facts.with(st, d.name(), b(joins(p.rel(d), false)));
    }
    region.set(p, st);
}

fn chorus_free(facts: &Facts, region: &dyn Region, p: P, except: Option<Dir>) -> bool {
    HORIZONTAL
        .iter()
        .all(|&d| Some(d) == except || facts.is_air(region.block(p.rel(d))))
}

fn chorus_grow(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    branch: P,
    origin: P,
    max_distance: i32,
    depth: i32,
) {
    let mut height = r.next_int_bound(4) + 1;
    if depth == 0 {
        height += 1;
    }
    for j in 0..height {
        let q = branch.above(j + 1);
        if !chorus_free(facts, region, q, None) {
            return;
        }
        chorus_set(facts, region, q);
        chorus_set(facts, region, q.below(1));
    }
    let mut grew = false;
    if depth < 4 {
        let mut branches = r.next_int_bound(4);
        if depth == 0 {
            branches += 1;
        }
        for _ in 0..branches {
            let d = HORIZONTAL[r.next_int_bound(4) as usize];
            let q = branch.above(height).rel(d);
            if (q.x - origin.x).abs() < max_distance
                && (q.z - origin.z).abs() < max_distance
                && facts.is_air(region.block(q))
                && facts.is_air(region.block(q.below(1)))
                && chorus_free(facts, region, q, Some(d.opposite()))
            {
                grew = true;
                chorus_set(facts, region, q);
                chorus_set(facts, region, q.rel(d.opposite()));
                chorus_grow(facts, region, r, q, origin, max_distance, depth + 1);
            }
        }
    }
    if !grew && let Some(flower) = facts.default_state("minecraft:chorus_flower") {
        region.set(branch.above(height), facts.with(flower, "age", "5"));
    }
}
