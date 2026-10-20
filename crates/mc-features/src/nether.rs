//! The Nether's feature types: glowstone and wart spreads, huge fungi,
//! lava deltas, basalt and blackstone blobs, basalt pillars and column
//! clusters. Behaviour as minecraft.wiki describes each feature; 26.3 moved
//! their numbers into the data pack.

use std::sync::Arc;

use mc_tick::Dir;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::Facts;
use crate::feature::{Ctx, Loader};
use crate::placement::Placed;
use crate::predicate::{BlockSet, Predicate};
use crate::rand::WorldgenRandom;
use crate::state::{StateProvider, parse_state};
use crate::value::IntProvider;
use crate::{P, Region};

/// `Direction.values()` order.
const ALL: [Dir; 6] = [
    Dir::Down,
    Dir::Up,
    Dir::North,
    Dir::South,
    Dir::West,
    Dir::East,
];

/// What a lava delta never replaces.
const DELTA_KEEPS: [&str; 7] = [
    "minecraft:bedrock",
    "minecraft:nether_bricks",
    "minecraft:nether_brick_fence",
    "minecraft:nether_brick_stairs",
    "minecraft:nether_wart",
    "minecraft:chest",
    "minecraft:spawner",
];

#[derive(Debug)]
pub enum Nether {
    /// `random_neighbor_spread`: the block at the origin, then tries around
    /// it that keep it only where exactly one neighbour is accepted.
    Spread {
        block: StateProvider,
        accepted: BlockSet,
        can_replace: Predicate,
        attempts: i32,
        xz: IntProvider,
        y: IntProvider,
    },
    /// `huge_fungus`.
    Fungus {
        base: u32,
        stem: u32,
        hat: u32,
        decor: u32,
        replaceable: Predicate,
        planted: bool,
    },
    /// `delta_feature`: a lava pool with a magma rim.
    Delta {
        contents: u32,
        rim: u32,
        size: IntProvider,
        rim_size: IntProvider,
    },
    /// `netherrack_replace_blobs`.
    Blobs {
        target: u32,
        state: u32,
        radius: IntProvider,
    },
    /// `single_block_pillar`: a line of blocks while they can replace.
    Pillar {
        block: StateProvider,
        can_replace: Predicate,
        dir: Dir,
        chance: Option<f32>,
        cap: Option<Arc<Placed>>,
    },
    /// `projected_random_patchy_square`: blocks around the origin, thinner
    /// to the corners, each dropped through what it may.
    PatchySquare {
        block: StateProvider,
        through: Predicate,
        size: i32,
        max_projection: i32,
    },
    /// `stepped_column_cluster`: basalt columns, taller to the middle.
    Columns(Box<Columns>),
}

#[derive(Debug)]
pub struct Columns {
    block: StateProvider,
    can_replace: Predicate,
    cannot_place_on: BlockSet,
    continue_through: Predicate,
    cluster_reach: i32,
    count: i32,
    reach: IntProvider,
    height: IntProvider,
}

fn ip(v: &Value, k: &str, d: i32) -> IntProvider {
    IntProvider::parse(&v[k]).unwrap_or(IntProvider::Constant(d))
}

fn pred(v: &Value, facts: &Facts) -> Predicate {
    if v.is_null() {
        Predicate::True
    } else {
        Predicate::parse(v, facts).unwrap_or(Predicate::True)
    }
}

impl Nether {
    pub fn parse(ty: &str, v: &Value, l: &Loader) -> Option<Self> {
        let facts = l.facts;
        let int = |k: &str, d: i32| v[k].as_i64().map_or(d, |n| n as i32);
        Some(match ty {
            "random_neighbor_spread" => Nether::Spread {
                block: l.provider(&v["block"])?,
                accepted: BlockSet::parse(&v["accepted_neighbors"], facts)?,
                can_replace: pred(&v["can_replace"], facts),
                attempts: int("attempts", 0),
                xz: ip(v, "xz_offset", 0),
                y: ip(v, "y_offset", 0),
            },
            "huge_fungus" => Nether::Fungus {
                base: facts.block_id(v["valid_base_block"].as_str()?)?,
                stem: parse_state(&v["stem_state"], facts)?,
                hat: parse_state(&v["hat_state"], facts)?,
                decor: parse_state(&v["decor_state"], facts)?,
                replaceable: pred(&v["replaceable_blocks"], facts),
                planted: v["planted"].as_bool().unwrap_or(false),
            },
            "delta_feature" => Nether::Delta {
                contents: parse_state(&v["contents"], facts)?,
                rim: parse_state(&v["rim"], facts)?,
                size: ip(v, "size", 0),
                rim_size: ip(v, "rim_size", 0),
            },
            "netherrack_replace_blobs" => Nether::Blobs {
                target: parse_state(&v["target"], facts)?,
                state: parse_state(&v["state"], facts)?,
                radius: ip(v, "radius", 0),
            },
            "single_block_pillar" => Nether::Pillar {
                block: l.provider(&v["block"])?,
                can_replace: pred(&v["can_replace"], facts),
                dir: Dir::from_name(v["direction"].as_str().unwrap_or("down"))?,
                chance: v["chance_to_continue"].as_f64().map(|c| c as f32),
                cap: (!v["cap_feature"].is_null()).then(|| l.placed(&v["cap_feature"])),
            },
            "projected_random_patchy_square" => Nether::PatchySquare {
                block: l.provider(&v["block"])?,
                through: pred(&v["project_through"], facts),
                size: int("size", 0),
                max_projection: int("max_projection_height", 0),
            },
            "stepped_column_cluster" => Nether::Columns(Box::new(Columns {
                block: l.provider(&v["block"])?,
                can_replace: pred(&v["can_replace"], facts),
                cannot_place_on: BlockSet::parse(&v["cannot_place_on"], facts)?,
                continue_through: pred(&v["continue_through"], facts),
                cluster_reach: int("cluster_reach", 0),
                count: int("column_count", 0),
                reach: ip(v, "column_reach", 0),
                height: ip(v, "height", 0),
            })),
            _ => return None,
        })
    }

    pub fn place(&self, cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
        let facts = cx.facts;
        match self {
            Nether::Spread {
                block,
                accepted,
                can_replace,
                attempts,
                xz,
                y,
            } => {
                let Some(state) = block.get(region, facts, r, p) else {
                    return false;
                };
                region.set(p, state);
                for _ in 0..*attempts {
                    let dx = xz.sample(r);
                    let dy = y.sample(r);
                    let dz = xz.sample(r);
                    let q = p.offset(dx, dy, dz);
                    if !can_replace.test(region, facts, q) {
                        continue;
                    }
                    let mut n = 0;
                    for d in ALL {
                        if accepted.contains(facts, region.block(q.rel(d))) {
                            n += 1;
                            if n > 1 {
                                break;
                            }
                        }
                    }
                    if n == 1
                        && let Some(s) = block.get(region, facts, r, q)
                    {
                        region.set(q, s);
                    }
                }
                true
            }
            Nether::Fungus { .. } => fungus(self, facts, region, r, p),
            Nether::Delta {
                contents,
                rim,
                size,
                rim_size,
            } => {
                let clear = |region: &dyn Region, q: P| {
                    let s = region.block(q);
                    if facts.block(s) == facts.block(*contents)
                        || DELTA_KEEPS.iter().any(|n| facts.is(s, n))
                    {
                        return false;
                    }
                    ALL.iter().all(|&d| {
                        let air = facts.is_air(region.block(q.rel(d)));
                        air == (d == Dir::Up)
                    })
                };
                let with_rim = r.next_double() < 0.9;
                let rx = if with_rim { rim_size.sample(r) } else { 0 };
                let rz = if with_rim { rim_size.sample(r) } else { 0 };
                let rimmed = with_rim && rx != 0 && rz != 0;
                let sx = size.sample(r);
                let sz = size.sample(r);
                let reach = sx.max(sz);
                let mut any = false;
                for q in within_manhattan(p, sx, 0, sz) {
                    if manhattan(q, p) > reach {
                        break;
                    }
                    if clear(region, q) {
                        if rimmed {
                            any = true;
                            region.set(q, *rim);
                        }
                        let q2 = q.offset(rx, 0, rz);
                        if clear(region, q2) {
                            any = true;
                            region.set(q2, *contents);
                        }
                    }
                }
                any
            }
            Nether::Blobs {
                target,
                state,
                radius,
            } => {
                let h = region.heights();
                let target_block = facts.block(*target);
                let mut q = p.with_y(p.y.clamp(h.min_y + 1, h.min_y + h.height - 1));
                loop {
                    if q.y <= h.min_y + 1 {
                        return false;
                    }
                    if facts.block(region.block(q)) == target_block {
                        break;
                    }
                    q = q.below(1);
                }
                let (rx, ry, rz) = (radius.sample(r), radius.sample(r), radius.sample(r));
                let reach = rx.max(ry).max(rz);
                let mut any = false;
                for b in within_manhattan(q, rx, ry, rz) {
                    if manhattan(b, q) > reach {
                        break;
                    }
                    if facts.block(region.block(b)) == target_block {
                        region.set(b, *state);
                        any = true;
                    }
                }
                any
            }
            Nether::Pillar {
                block,
                can_replace,
                dir,
                chance,
                cap,
            } => {
                let h = region.heights();
                let mut q = p;
                let mut last = None;
                while q.y >= h.min_y
                    && q.y < h.min_y + h.height
                    && can_replace.test(region, facts, q)
                {
                    if let Some(c) = chance
                        && last.is_some()
                        && r.next_float() >= *c
                    {
                        break;
                    }
                    let Some(s) = block.get(region, facts, r, q) else {
                        break;
                    };
                    region.set(q, s);
                    last = Some(q);
                    q = q.rel(*dir);
                }
                let Some(end) = last else {
                    return false;
                };
                if let Some(cap) = cap {
                    cap.place(cx, region, r, end, None);
                }
                true
            }
            Nether::PatchySquare {
                block,
                through,
                size,
                max_projection,
            } => {
                let mut any = false;
                for i in -size..=*size {
                    for j in -size..=*size {
                        let k = i.abs() * j.abs();
                        if r.next_int_bound(10) >= 10 - k {
                            continue;
                        }
                        let mut q = p.offset(i, 0, j);
                        let mut left = *max_projection;
                        while left > 0 && through.test(region, facts, q.below(1)) {
                            q = q.below(1);
                            left -= 1;
                        }
                        if let Some(s) = block.get(region, facts, r, q) {
                            region.set(q, s);
                            any = true;
                        }
                    }
                }
                any
            }
            Nether::Columns(c) => columns(c, facts, region, r, p),
        }
    }
}

fn manhattan(a: P, b: P) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()
}

/// `BlockPos.withinManhattan`: positions within the reaches, nearest
/// (by Manhattan distance) first; x, then y rising, each z and its mirror.
pub fn within_manhattan(origin: P, rx: i32, ry: i32, rz: i32) -> Vec<P> {
    let mut out = Vec::new();
    for depth in 0..=rx + ry + rz {
        let max_x = rx.min(depth);
        for x in -max_x..=max_x {
            let max_y = ry.min(depth - x.abs());
            for y in -max_y..=max_y {
                let z = depth - x.abs() - y.abs();
                if z <= rz {
                    out.push(origin.offset(x, y, z));
                    if z != 0 {
                        out.push(origin.offset(x, y, -z));
                    }
                }
            }
        }
    }
    out
}

fn fungus(
    f: &Nether,
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
) -> bool {
    let &Nether::Fungus {
        base,
        stem,
        hat,
        decor,
        ref replaceable,
        planted,
    } = f
    else {
        return false;
    };
    if facts.block(region.block(p.below(1))) != base {
        return false;
    }
    let mut height = r.int_between(4, 13);
    if r.next_int_bound(12) == 0 {
        height *= 2;
    }
    if !planted {
        let h = region.heights();
        if p.y + height + 1 >= h.height {
            return false;
        }
    }
    let huge = !planted && r.next_float() < 0.06;
    let air = facts.default_state("minecraft:air").unwrap_or(0);
    region.set(p, air);
    let can_replace = |region: &dyn Region, q: P, check: bool| {
        facts.replaceable(region.block(q)) || (check && replaceable.test(region, facts, q))
    };
    // The stem.
    let w = i32::from(huge);
    for dx in -w..=w {
        for dz in -w..=w {
            let corner = huge && dx.abs() == w && dz.abs() == w;
            for dy in 0..height {
                let q = p.offset(dx, dy, dz);
                if !can_replace(region, q, true) {
                    continue;
                }
                if corner {
                    if r.next_float() < 0.1 {
                        region.set(q, stem);
                    }
                } else {
                    region.set(q, stem);
                }
            }
        }
    }
    // The hat.
    let vines = facts.is(hat, "minecraft:nether_wart_block");
    let hat_height = (r.next_int_bound(1 + height / 3) + 5).min(height);
    let start = height - hat_height;
    for dy in start..=height {
        let mut radius = if dy < height - r.next_int_bound(3) {
            2
        } else {
            1
        };
        if hat_height > 8 && dy < start + 4 {
            radius = 3;
        }
        if huge {
            radius += 1;
        }
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let edge_x = dx == -radius || dx == radius;
                let edge_z = dz == -radius || dz == radius;
                let inside = !edge_x && !edge_z && dy != height;
                let corner = edge_x && edge_z;
                let low = dy < start + 3;
                let q = p.offset(dx, dy, dz);
                if !can_replace(region, q, false) {
                    continue;
                }
                if low {
                    if !inside {
                        hat_drop(facts, region, r, q, hat, vines);
                    }
                } else if inside {
                    hat_block(facts, region, r, q, (hat, decor), 0.1, 0.2, vines, 0.1);
                } else if corner {
                    hat_block(facts, region, r, q, (hat, decor), 0.01, 0.7, vines, 0.083);
                } else {
                    hat_block(facts, region, r, q, (hat, decor), 0.0005, 0.98, vines, 0.07);
                }
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn hat_block(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    q: P,
    (hat, decor): (u32, u32),
    decor_chance: f32,
    hat_chance: f32,
    vines: bool,
    vine_chance: f32,
) {
    if r.next_float() < decor_chance {
        region.set(q, decor);
    } else if r.next_float() < hat_chance {
        region.set(q, hat);
        if r.next_float() < if vines { vine_chance } else { 0.0 } {
            fungus_vines(facts, region, r, q);
        }
    }
}

fn hat_drop(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    q: P,
    hat: u32,
    vines: bool,
) {
    if region.block(q.below(1)) == hat {
        region.set(q, hat);
    } else if r.next_float() < 0.15 {
        region.set(q, hat);
        if vines && r.next_int_bound(11) == 0 {
            fungus_vines(facts, region, r, q);
        }
    }
}

fn fungus_vines(facts: &Facts, region: &mut dyn Region, r: &mut WorldgenRandom, q: P) {
    let below = q.below(1);
    if !facts.is_air(region.block(below)) {
        return;
    }
    let mut length = r.int_between(1, 5);
    if r.next_int_bound(7) == 0 {
        length *= 2;
    }
    weeping_vines(facts, region, r, below, length, 23, 25);
}

/// A column of weeping vines down from `p`, its tip aged between the two.
pub fn weeping_vines(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    mut p: P,
    length: i32,
    min_age: i32,
    max_age: i32,
) {
    let (Some(head), Some(plant)) = (
        facts.default_state("minecraft:weeping_vines"),
        facts.default_state("minecraft:weeping_vines_plant"),
    ) else {
        return;
    };
    for i in 0..=length {
        if facts.is_air(region.block(p)) {
            if i == length || !facts.is_air(region.block(p.below(1))) {
                let age = r.int_between(min_age, max_age).to_string();
                region.set(p, facts.with(head, "age", &age));
                break;
            }
            region.set(p, plant);
        }
        p = p.below(1);
    }
}

fn columns(
    c: &Columns,
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
) -> bool {
    if !can_stand(c, facts, region, p) {
        return false;
    }
    let height = c.height.sample(r);
    let reach = height.min(c.cluster_reach);
    let mut any = false;
    for _ in 0..c.count {
        let x = p.x - reach + r.next_int_bound(2 * reach + 1);
        let y = p.y + r.next_int_bound(1);
        let z = p.z - reach + r.next_int_bound(2 * reach + 1);
        let q = P::new(x, y, z);
        let left = height - manhattan(q, p);
        if left >= 0 {
            let column_reach = c.reach.sample(r);
            any |= column(c, facts, region, r, q, left, column_reach);
        }
    }
    any
}

/// Room for a column at `p`: replaceable there, on a block columns stand on.
fn can_stand(c: &Columns, facts: &Facts, region: &dyn Region, p: P) -> bool {
    if !c.can_replace.test(region, facts, p) {
        return false;
    }
    let below = region.block(p.below(1));
    !facts.is_air(below) && !c.cannot_place_on.contains(facts, below)
}

fn column(
    c: &Columns,
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    distance: i32,
    reach: i32,
) -> bool {
    let h = region.heights();
    let mut any = false;
    for dz in -reach..=reach {
        for dx in -reach..=reach {
            let b = p.offset(dx, 0, dz);
            let d = manhattan(b, p);
            let start = if c.can_replace.test(region, facts, b) {
                // Down to the ground.
                let (mut q, mut left) = (b, d);
                let mut found = None;
                while q.y > h.min_y + 1 && left > 0 {
                    left -= 1;
                    if can_stand(c, facts, region, q) {
                        found = Some(q);
                        break;
                    }
                    q = q.below(1);
                }
                found
            } else {
                // Up to the first air.
                let (mut q, mut left) = (b, d);
                let mut found = None;
                while q.y < h.min_y + h.height - 1 && left > 0 {
                    left -= 1;
                    let s = region.block(q);
                    if c.cannot_place_on.contains(facts, s) {
                        break;
                    }
                    if facts.is_air(s) {
                        found = Some(q);
                        break;
                    }
                    q = q.above(1);
                }
                found
            };
            let Some(mut q) = start else {
                continue;
            };
            let mut n = distance - d / 2;
            while n >= 0 {
                if c.can_replace.test(region, facts, q) {
                    if let Some(s) = c.block.get(region, facts, r, q) {
                        region.set(q, s);
                    }
                    any = true;
                } else if !c.continue_through.test(region, facts, q) {
                    break;
                }
                q = q.above(1);
                n -= 1;
            }
        }
    }
    any
}
