//! Strongholds (minecraft.wiki, "Stronghold"): stairs down from the start,
//! then corridors, turns, crossings, prison cells, libraries, store rooms
//! and the end portal room, picked by weight with per-kind limits, each
//! from a door of the one before; generated again until it has a portal
//! room, then moved below sea level.

use std::sync::Arc;

use mc_tick::Dir;
use mc_worldgen::random::RandomSource;

use super::built::Frame;
use super::legacy::{Kind, Piece};
use super::template::BBox;
use crate::blocks::Facts;
use crate::rand::WorldgenRandom;
use crate::value::Heights;
use crate::{P, Region};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    Opening,
    Wood,
    Grates,
    Iron,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum K {
    Straight,
    Prison,
    LeftTurn,
    RightTurn,
    RoomCrossing,
    StraightStairs,
    Stairs,
    FiveCrossing,
    ChestCorridor,
    Library,
    Portal,
    Filler,
}

#[derive(Debug, Clone)]
pub struct Part {
    kind: K,
    bbox: BBox,
    facing: Dir,
    door: Door,
    depth: i32,
    /// Kind-specific choices: turns' sides, crossings' exits, room type,
    /// library height.
    a: bool,
    b: bool,
    c: bool,
    d: bool,
    room: i32,
    source: bool,
}

const WEIGHTS: [(K, i32, i32); 11] = [
    (K::Straight, 40, 0),
    (K::Prison, 5, 5),
    (K::LeftTurn, 20, 0),
    (K::RightTurn, 20, 0),
    (K::RoomCrossing, 10, 6),
    (K::StraightStairs, 5, 5),
    (K::Stairs, 5, 5),
    (K::FiveCrossing, 5, 4),
    (K::ChestCorridor, 5, 4),
    (K::Library, 10, 2),
    (K::Portal, 20, 1),
];

#[allow(clippy::too_many_arguments)]
pub(super) fn orient(
    x: i32,
    y: i32,
    z: i32,
    ox: i32,
    oy: i32,
    oz: i32,
    w: i32,
    h: i32,
    d: i32,
    dir: Dir,
) -> BBox {
    let b = |x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32| {
        BBox::from_corners(P::new(x0, y0, z0), P::new(x1, y1, z1))
    };
    match dir {
        Dir::South => b(
            x + ox,
            y + oy,
            z + oz,
            x + w - 1 + ox,
            y + h - 1 + oy,
            z + d - 1 + oz,
        ),
        Dir::West => b(
            x - d + 1 + oz,
            y + oy,
            z + ox,
            x + oz,
            y + h - 1 + oy,
            z + w - 1 + ox,
        ),
        Dir::East => b(
            x + oz,
            y + oy,
            z + ox,
            x + d - 1 + oz,
            y + h - 1 + oy,
            z + w - 1 + ox,
        ),
        _ => b(
            x + ox,
            y + oy,
            z - d + 1 + oz,
            x + w - 1 + ox,
            y + h - 1 + oy,
            z + oz,
        ),
    }
}

fn random_door(r: &mut WorldgenRandom) -> Door {
    match r.next_int_bound(5) {
        2 => Door::Wood,
        3 => Door::Grates,
        4 => Door::Iron,
        _ => Door::Opening,
    }
}

struct Gen<'r> {
    r: &'r mut WorldgenRandom,
    parts: Vec<Part>,
    pending: Vec<usize>,
    counts: [i32; 11],
    available: Vec<usize>,
    previous: Option<usize>,
    imposed: Option<K>,
    portal: bool,
    origin: P,
}

impl Gen<'_> {
    fn ok(&self, b: &BBox) -> bool {
        b.min.y > 10 && !self.parts.iter().any(|p| p.bbox.intersects(b))
    }

    fn collision(&self, b: &BBox) -> Option<BBox> {
        self.parts
            .iter()
            .find(|p| p.bbox.intersects(b))
            .map(|p| p.bbox)
    }

    fn part(&self, kind: K, bbox: BBox, facing: Dir, depth: i32) -> Part {
        Part {
            kind,
            bbox,
            facing,
            door: Door::Opening,
            depth,
            a: false,
            b: false,
            c: false,
            d: false,
            room: 0,
            source: false,
        }
    }

    fn create(&mut self, kind: K, x: i32, y: i32, z: i32, dir: Dir, depth: i32) -> Option<Part> {
        let (ox, oy, w, h, d) = match kind {
            K::Straight | K::ChestCorridor => (-1, -1, 5, 5, 7),
            K::Prison => (-1, -1, 9, 5, 11),
            K::LeftTurn | K::RightTurn => (-1, -1, 5, 5, 5),
            K::RoomCrossing => (-4, -1, 11, 7, 11),
            K::StraightStairs => (-1, -7, 5, 11, 8),
            K::Stairs => (-1, -7, 5, 11, 5),
            K::FiveCrossing => (-4, -3, 10, 9, 11),
            K::Library => (-4, -1, 14, 11, 15),
            K::Portal => (-4, -1, 11, 8, 16),
            K::Filler => return None,
        };
        let mut b = orient(x, y, z, ox, oy, 0, w, h, d, dir);
        if !self.ok(&b) {
            if kind != K::Library {
                return None;
            }
            b = orient(x, y, z, -4, -1, 0, 14, 6, 15, dir);
            if !self.ok(&b) {
                return None;
            }
        }
        let mut p = self.part(kind, b, dir, depth);
        match kind {
            K::Portal => {}
            _ => p.door = random_door(self.r),
        }
        match kind {
            K::Straight => {
                p.a = self.r.next_int_bound(2) == 0;
                p.b = self.r.next_int_bound(2) == 0;
            }
            K::RoomCrossing => p.room = self.r.next_int_bound(5),
            K::FiveCrossing => {
                p.a = self.r.next_bool();
                p.b = self.r.next_bool();
                p.c = self.r.next_bool();
                p.d = self.r.next_int_bound(3) > 0;
            }
            K::Library => p.a = b.y_span() > 6,
            _ => {}
        }
        Some(p)
    }

    fn next_piece(&mut self, x: i32, y: i32, z: i32, dir: Dir, depth: i32) -> Option<Part> {
        // Any kind still allowed?
        let mut any = false;
        let mut total = 0;
        for &i in &self.available {
            let (_, w, max) = WEIGHTS[i];
            if max > 0 && self.counts[i] < max {
                any = true;
            }
            total += w;
        }
        if !any {
            return None;
        }
        if let Some(k) = self.imposed.take()
            && let Some(p) = self.create(k, x, y, z, dir, depth)
        {
            return Some(p);
        }
        let mut tries = 0;
        'outer: while tries < 5 {
            tries += 1;
            let mut j = self.r.next_int_bound(total);
            for idx in 0..self.available.len() {
                let i = self.available[idx];
                let (k, w, max) = WEIGHTS[i];
                j -= w;
                if j >= 0 {
                    continue;
                }
                let allowed = (max == 0 || self.counts[i] < max)
                    && match k {
                        K::Library => depth > 4,
                        K::Portal => depth > 5,
                        _ => true,
                    };
                if !allowed || self.previous == Some(i) {
                    continue 'outer;
                }
                let Some(p) = self.create(k, x, y, z, dir, depth) else {
                    continue;
                };
                self.counts[i] += 1;
                self.previous = Some(i);
                if max > 0 && self.counts[i] >= max {
                    self.available.remove(idx);
                }
                return Some(p);
            }
        }
        // A short corridor up to whatever is in the way.
        let b = orient(x, y, z, -1, -1, 0, 5, 5, 4, dir);
        let hit = self.collision(&b)?;
        if hit.min.y == b.min.y {
            for i in (1..=2).rev() {
                let b = orient(x, y, z, -1, -1, 0, 5, 5, i, dir);
                if hit.intersects(&b) {
                    continue;
                }
                let b = orient(x, y, z, -1, -1, 0, 5, 5, i + 1, dir);
                if b.min.y > 1 {
                    return Some(self.part(K::Filler, b, dir, depth));
                }
                return None;
            }
        }
        None
    }

    fn add(&mut self, x: i32, y: i32, z: i32, dir: Dir, depth: i32) {
        if depth > 50 || (x - self.origin.x).abs() > 112 || (z - self.origin.z).abs() > 112 {
            return;
        }
        if let Some(p) = self.next_piece(x, y, z, dir, depth + 1) {
            self.parts.push(p);
            self.pending.push(self.parts.len() - 1);
        }
    }

    fn forward(&mut self, i: usize, xo: i32, yo: i32) {
        let p = self.parts[i].clone();
        let b = p.bbox;
        match p.facing {
            Dir::South => self.add(b.min.x + xo, b.min.y + yo, b.max.z + 1, p.facing, p.depth),
            Dir::West => self.add(b.min.x - 1, b.min.y + yo, b.min.z + xo, p.facing, p.depth),
            Dir::East => self.add(b.max.x + 1, b.min.y + yo, b.min.z + xo, p.facing, p.depth),
            _ => self.add(b.min.x + xo, b.min.y + yo, b.min.z - 1, p.facing, p.depth),
        }
    }

    fn left(&mut self, i: usize, yo: i32, zo: i32) {
        let p = self.parts[i].clone();
        let b = p.bbox;
        match p.facing {
            Dir::West | Dir::East => {
                self.add(b.min.x + zo, b.min.y + yo, b.min.z - 1, Dir::North, p.depth)
            }
            _ => self.add(b.min.x - 1, b.min.y + yo, b.min.z + zo, Dir::West, p.depth),
        }
    }

    fn right(&mut self, i: usize, yo: i32, zo: i32) {
        let p = self.parts[i].clone();
        let b = p.bbox;
        match p.facing {
            Dir::West | Dir::East => {
                self.add(b.min.x + zo, b.min.y + yo, b.max.z + 1, Dir::South, p.depth)
            }
            _ => self.add(b.max.x + 1, b.min.y + yo, b.min.z + zo, Dir::East, p.depth),
        }
    }

    fn children(&mut self, i: usize) {
        let p = self.parts[i].clone();
        let ne = matches!(p.facing, Dir::North | Dir::East);
        match p.kind {
            K::Straight => {
                self.forward(i, 1, 1);
                if p.a {
                    self.left(i, 1, 2);
                }
                if p.b {
                    self.right(i, 1, 2);
                }
            }
            K::Prison | K::ChestCorridor | K::StraightStairs => self.forward(i, 1, 1),
            K::LeftTurn => {
                if ne {
                    self.left(i, 1, 1)
                } else {
                    self.right(i, 1, 1)
                }
            }
            K::RightTurn => {
                if ne {
                    self.right(i, 1, 1)
                } else {
                    self.left(i, 1, 1)
                }
            }
            K::RoomCrossing => {
                self.forward(i, 4, 1);
                self.left(i, 1, 4);
                self.right(i, 1, 4);
            }
            K::Stairs => {
                if p.source {
                    self.imposed = Some(K::FiveCrossing);
                }
                self.forward(i, 1, 1);
            }
            K::FiveCrossing => {
                let (mut a, mut b) = (3, 5);
                if matches!(p.facing, Dir::West | Dir::North) {
                    a = 8 - a;
                    b = 8 - b;
                }
                self.forward(i, 5, 1);
                if p.a {
                    self.left(i, a, 1);
                }
                if p.b {
                    self.left(i, b, 7);
                }
                if p.c {
                    self.right(i, a, 1);
                }
                if p.d {
                    self.right(i, b, 7);
                }
            }
            K::Portal => self.portal = true,
            K::Library | K::Filler => {}
        }
    }
}

const DIRS: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

pub fn generate(seed: i64, x: i32, z: i32, h: Heights) -> Option<(Vec<Piece>, P)> {
    let mut attempt = 0i64;
    loop {
        let mut r = WorldgenRandom::legacy(0);
        r.set_large_feature_seed(seed.wrapping_add(attempt), x, z);
        attempt += 1;
        let dir = DIRS[r.next_int_bound(4) as usize];
        let (sx, sz) = (x * 16 + 2, z * 16 + 2);
        let b = BBox::from_corners(P::new(sx, 64, sz), P::new(sx + 4, 74, sz + 4));
        let mut g = Gen {
            r: &mut r,
            parts: Vec::new(),
            pending: Vec::new(),
            counts: [0; 11],
            available: (0..WEIGHTS.len()).collect(),
            previous: None,
            imposed: None,
            portal: false,
            origin: b.min,
        };
        let mut start = g.part(K::Stairs, b, dir, 0);
        start.door = Door::Opening;
        start.source = true;
        g.parts.push(start);
        g.children(0);
        while !g.pending.is_empty() {
            let j = g.r.next_int_bound(g.pending.len() as i32) as usize;
            let i = g.pending.remove(j);
            g.children(i);
        }
        let parts = std::mem::take(&mut g.parts);
        let portal = g.portal;
        // Below sea level.
        let all = parts.iter().map(|p| p.bbox).reduce(BBox::union)?;
        let top = h.sea_level - 10;
        let mut y = all.y_span() + h.min_y + 1;
        if y < top {
            y += r.next_int_bound(top - y);
        }
        let dy = y - all.max.y;
        if !portal || parts.is_empty() {
            if attempt > 50 {
                return None;
            }
            continue;
        }
        let at = P::new(x * 16, 0, z * 16);
        let pieces = parts
            .into_iter()
            .map(|mut p| {
                p.bbox = p.bbox.moved(0, dy, 0);
                Piece {
                    bbox: p.bbox,
                    kind: Arc::new(Kind::Stronghold(p)),
                }
            })
            .collect();
        return Some((pieces, at));
    }
}

fn st(facts: &Facts, name: &str, props: &[(&str, &str)]) -> u32 {
    facts
        .state(&format!("minecraft:{name}"), props)
        .or_else(|| facts.default_state(&format!("minecraft:{name}")))
        .unwrap_or(facts.air)
}

/// Stone bricks for walls (some cracked, mossy or infested), air inside;
/// where `skip_air`, open cave stays open.
#[allow(clippy::too_many_arguments)]
fn bricks(
    f: &Frame,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    x0: i32,
    y0: i32,
    z0: i32,
    x1: i32,
    y1: i32,
    z1: i32,
    skip_air: bool,
) {
    let facts = f.facts;
    let (sb, cracked, mossy, infested) = (
        st(facts, "stone_bricks", &[]),
        st(facts, "cracked_stone_bricks", &[]),
        st(facts, "mossy_stone_bricks", &[]),
        st(facts, "infested_stone_bricks", &[]),
    );
    for y in y0..=y1 {
        for x in x0..=x1 {
            for z in z0..=z1 {
                if skip_air && facts.is_air(f.get(region, x, y, z)) {
                    continue;
                }
                let wall = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                let s = if wall {
                    let v = r.next_float();
                    if v < 0.2 {
                        cracked
                    } else if v < 0.5 {
                        mossy
                    } else if v < 0.55 {
                        infested
                    } else {
                        sb
                    }
                } else {
                    facts.cave_air
                };
                f.set(region, s, x, y, z);
            }
        }
    }
}

fn door(f: &Frame, region: &mut dyn Region, kind: Door, x: i32, y: i32, z: i32) {
    let facts = f.facts;
    let sb = st(facts, "stone_bricks", &[]);
    let air = facts.cave_air;
    match kind {
        Door::Opening => f.fill(region, x, y, z, x + 2, y + 2, z, air, air, false),
        Door::Wood | Door::Iron => {
            for (dx, dy) in [(0, 0), (0, 1), (0, 2), (1, 2), (2, 2), (2, 1), (2, 0)] {
                f.set(region, sb, x + dx, y + dy, z);
            }
            let name = if kind == Door::Wood {
                "oak_door"
            } else {
                "iron_door"
            };
            f.set(region, st(facts, name, &[("half", "lower")]), x + 1, y, z);
            f.set(
                region,
                st(facts, name, &[("half", "upper")]),
                x + 1,
                y + 1,
                z,
            );
            if kind == Door::Iron {
                f.set(
                    region,
                    st(facts, "stone_button", &[("facing", "north")]),
                    x + 2,
                    y + 1,
                    z + 1,
                );
                f.set(
                    region,
                    st(facts, "stone_button", &[("facing", "south")]),
                    x + 2,
                    y + 1,
                    z - 1,
                );
            }
        }
        Door::Grates => {
            f.set(region, air, x + 1, y, z);
            f.set(region, air, x + 1, y + 1, z);
            let bars = |e: &[(&str, &str)]| st(facts, "iron_bars", e);
            f.set(region, bars(&[("west", "true")]), x, y, z);
            f.set(region, bars(&[("west", "true")]), x, y + 1, z);
            f.set(
                region,
                bars(&[("east", "true"), ("west", "true")]),
                x,
                y + 2,
                z,
            );
            f.set(
                region,
                bars(&[("east", "true"), ("west", "true")]),
                x + 1,
                y + 2,
                z,
            );
            f.set(
                region,
                bars(&[("east", "true"), ("west", "true")]),
                x + 2,
                y + 2,
                z,
            );
            f.set(region, bars(&[("east", "true")]), x + 2, y + 1, z);
            f.set(region, bars(&[("east", "true")]), x + 2, y, z);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe(
    f: &Frame,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    chance: f32,
    x: i32,
    y: i32,
    z: i32,
    state: u32,
) {
    if r.next_float() < chance {
        f.set(region, state, x, y, z);
    }
}

pub fn place(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: &Part,
    chunk: BBox,
) {
    let f = Frame {
        facts,
        bbox: p.bbox,
        facing: p.facing,
        chunk,
    };
    let air = facts.cave_air;
    let sb = st(facts, "stone_bricks", &[]);
    let slab = st(facts, "smooth_stone_slab", &[]);
    let torch = |d: &str| st(facts, "wall_torch", &[("facing", d)]);
    match p.kind {
        K::Straight => {
            bricks(&f, region, r, 0, 0, 0, 4, 4, 6, true);
            door(&f, region, p.door, 1, 1, 0);
            door(&f, region, Door::Opening, 1, 1, 6);
            maybe(&f, region, r, 0.1, 1, 2, 1, torch("east"));
            maybe(&f, region, r, 0.1, 3, 2, 1, torch("west"));
            maybe(&f, region, r, 0.1, 1, 2, 5, torch("east"));
            maybe(&f, region, r, 0.1, 3, 2, 5, torch("west"));
            if p.a {
                f.fill(region, 0, 1, 2, 0, 3, 4, air, air, false);
            }
            if p.b {
                f.fill(region, 4, 1, 2, 4, 3, 4, air, air, false);
            }
        }
        K::Prison => {
            bricks(&f, region, r, 0, 0, 0, 8, 4, 10, true);
            door(&f, region, p.door, 1, 1, 0);
            f.fill(region, 1, 1, 10, 3, 3, 10, air, air, false);
            for z in [1, 3, 7, 9] {
                bricks(&f, region, r, 4, 1, z, 4, 3, z, false);
            }
            let bars = |e: &[(&str, &str)]| st(facts, "iron_bars", e);
            for y in 1..=3 {
                f.set(
                    region,
                    bars(&[("north", "true"), ("south", "true")]),
                    4,
                    y,
                    4,
                );
                f.set(
                    region,
                    bars(&[("north", "true"), ("south", "true"), ("east", "true")]),
                    4,
                    y,
                    5,
                );
                f.set(
                    region,
                    bars(&[("north", "true"), ("south", "true")]),
                    4,
                    y,
                    6,
                );
                for x in 5..=7 {
                    f.set(region, bars(&[("east", "true"), ("west", "true")]), x, y, 5);
                }
            }
            f.set(
                region,
                bars(&[("north", "true"), ("south", "true")]),
                4,
                3,
                2,
            );
            f.set(
                region,
                bars(&[("north", "true"), ("south", "true")]),
                4,
                3,
                8,
            );
            for z in [2, 8] {
                f.set(
                    region,
                    st(facts, "iron_door", &[("facing", "west"), ("half", "lower")]),
                    4,
                    1,
                    z,
                );
                f.set(
                    region,
                    st(facts, "iron_door", &[("facing", "west"), ("half", "upper")]),
                    4,
                    2,
                    z,
                );
            }
        }
        K::LeftTurn | K::RightTurn => {
            bricks(&f, region, r, 0, 0, 0, 4, 4, 4, true);
            door(&f, region, p.door, 1, 1, 0);
            let ne = matches!(p.facing, Dir::North | Dir::East);
            let west_side = (p.kind == K::LeftTurn) == ne;
            if west_side {
                f.fill(region, 0, 1, 1, 0, 3, 3, air, air, false);
            } else {
                f.fill(region, 4, 1, 1, 4, 3, 3, air, air, false);
            }
        }
        K::RoomCrossing => {
            bricks(&f, region, r, 0, 0, 0, 10, 6, 10, true);
            door(&f, region, p.door, 4, 1, 0);
            f.fill(region, 4, 1, 10, 6, 3, 10, air, air, false);
            f.fill(region, 0, 1, 4, 0, 3, 6, air, air, false);
            f.fill(region, 10, 1, 4, 10, 3, 6, air, air, false);
            match p.room {
                0 => {
                    for y in 1..=3 {
                        f.set(region, sb, 5, y, 5);
                    }
                    f.set(region, torch("west"), 4, 3, 5);
                    f.set(region, torch("east"), 6, 3, 5);
                    f.set(region, torch("south"), 5, 3, 4);
                    f.set(region, torch("north"), 5, 3, 6);
                    for (x, z) in [
                        (4, 4),
                        (4, 5),
                        (4, 6),
                        (6, 4),
                        (6, 5),
                        (6, 6),
                        (5, 4),
                        (5, 6),
                    ] {
                        f.set(region, slab, x, 1, z);
                    }
                }
                1 => {
                    for i in 0..5 {
                        f.set(region, sb, 3, 1, 3 + i);
                        f.set(region, sb, 7, 1, 3 + i);
                        f.set(region, sb, 3 + i, 1, 3);
                        f.set(region, sb, 3 + i, 1, 7);
                    }
                    for y in 1..=3 {
                        f.set(region, sb, 5, y, 5);
                    }
                    f.set(region, facts.water, 5, 4, 5);
                }
                2 => {
                    let cobble = st(facts, "cobblestone", &[]);
                    let planks = st(facts, "oak_planks", &[]);
                    for z in 1..=9 {
                        f.set(region, cobble, 1, 3, z);
                        f.set(region, cobble, 9, 3, z);
                    }
                    for x in 1..=9 {
                        f.set(region, cobble, x, 3, 1);
                        f.set(region, cobble, x, 3, 9);
                    }
                    for (x, y, z) in [
                        (5, 1, 4),
                        (5, 1, 6),
                        (5, 3, 4),
                        (5, 3, 6),
                        (4, 1, 5),
                        (6, 1, 5),
                        (4, 3, 5),
                        (6, 3, 5),
                    ] {
                        f.set(region, cobble, x, y, z);
                    }
                    for y in 1..=3 {
                        for (x, z) in [(4, 4), (6, 4), (4, 6), (6, 6)] {
                            f.set(region, cobble, x, y, z);
                        }
                    }
                    f.set(region, st(facts, "wall_torch", &[]), 5, 3, 5);
                    for z in 2..=8 {
                        f.set(region, planks, 2, 3, z);
                        f.set(region, planks, 3, 3, z);
                        if z <= 3 || z >= 7 {
                            for x in 4..=6 {
                                f.set(region, planks, x, 3, z);
                            }
                        }
                        f.set(region, planks, 7, 3, z);
                        f.set(region, planks, 8, 3, z);
                    }
                    let ladder = st(facts, "ladder", &[("facing", "west")]);
                    for y in 1..=3 {
                        f.set(region, ladder, 9, y, 3);
                    }
                    f.chest(region, r, 3, 4, 8, "minecraft:chests/stronghold_crossing");
                }
                _ => {}
            }
        }
        K::StraightStairs => {
            bricks(&f, region, r, 0, 0, 0, 4, 10, 7, true);
            door(&f, region, p.door, 1, 7, 0);
            door(&f, region, Door::Opening, 1, 1, 7);
            let stairs = st(facts, "cobblestone_stairs", &[("facing", "south")]);
            for i in 0..6 {
                for x in 1..=3 {
                    f.set(region, stairs, x, 6 - i, 1 + i);
                    if i < 5 {
                        f.set(region, sb, x, 5 - i, 1 + i);
                    }
                }
            }
        }
        K::Stairs => {
            bricks(&f, region, r, 0, 0, 0, 4, 10, 4, true);
            door(&f, region, p.door, 1, 7, 0);
            door(&f, region, Door::Opening, 1, 1, 4);
            for (s, x, y, z) in [
                (sb, 2, 6, 1),
                (sb, 1, 5, 1),
                (slab, 1, 6, 1),
                (sb, 1, 5, 2),
                (sb, 1, 4, 3),
                (slab, 1, 5, 3),
                (sb, 2, 4, 3),
                (sb, 3, 3, 3),
                (slab, 3, 4, 3),
                (sb, 3, 3, 2),
                (sb, 3, 2, 1),
                (slab, 3, 3, 1),
                (sb, 2, 2, 1),
                (sb, 1, 1, 1),
                (slab, 1, 2, 1),
                (sb, 1, 1, 2),
                (slab, 1, 1, 3),
            ] {
                f.set(region, s, x, y, z);
            }
        }
        K::FiveCrossing => {
            bricks(&f, region, r, 0, 0, 0, 9, 8, 10, true);
            door(&f, region, p.door, 4, 3, 0);
            if p.a {
                f.fill(region, 0, 3, 1, 0, 5, 3, air, air, false);
            }
            if p.c {
                f.fill(region, 9, 3, 1, 9, 5, 3, air, air, false);
            }
            if p.b {
                f.fill(region, 0, 5, 7, 0, 7, 9, air, air, false);
            }
            if p.d {
                f.fill(region, 9, 5, 7, 9, 7, 9, air, air, false);
            }
            f.fill(region, 5, 1, 10, 7, 3, 10, air, air, false);
            bricks(&f, region, r, 1, 2, 1, 8, 2, 6, false);
            bricks(&f, region, r, 4, 1, 5, 4, 4, 9, false);
            bricks(&f, region, r, 8, 1, 5, 8, 4, 9, false);
            bricks(&f, region, r, 1, 4, 7, 3, 4, 9, false);
            bricks(&f, region, r, 1, 3, 5, 3, 3, 6, false);
            f.fill(region, 1, 3, 4, 3, 3, 4, slab, slab, false);
            f.fill(region, 1, 4, 6, 3, 4, 6, slab, slab, false);
            bricks(&f, region, r, 5, 1, 7, 7, 1, 8, false);
            f.fill(region, 5, 1, 9, 7, 1, 9, slab, slab, false);
            f.fill(region, 5, 2, 7, 7, 2, 7, slab, slab, false);
            f.fill(region, 4, 5, 7, 4, 5, 9, slab, slab, false);
            f.fill(region, 8, 5, 7, 8, 5, 9, slab, slab, false);
            let double = facts.with(slab, "type", "double");
            f.fill(region, 5, 5, 7, 7, 5, 9, double, double, false);
            f.set(region, torch("south"), 6, 5, 6);
        }
        K::ChestCorridor => {
            bricks(&f, region, r, 0, 0, 0, 4, 4, 6, true);
            door(&f, region, p.door, 1, 1, 0);
            door(&f, region, Door::Opening, 1, 1, 6);
            let slab = st(facts, "stone_brick_slab", &[]);
            f.fill(region, 3, 1, 2, 3, 1, 4, sb, sb, false);
            for (x, y, z) in [(3, 1, 1), (3, 1, 5), (3, 2, 2), (3, 2, 4)] {
                f.set(region, slab, x, y, z);
            }
            for z in 2..=4 {
                f.set(region, slab, 2, 1, z);
            }
            f.chest(region, r, 3, 2, 3, "minecraft:chests/stronghold_corridor");
        }
        K::Library => library(&f, region, r, p),
        K::Portal => portal_room(&f, region, r),
        K::Filler => {
            let steps = if matches!(p.facing, Dir::North | Dir::South) {
                p.bbox.z_span()
            } else {
                p.bbox.x_span()
            };
            for i in 0..steps {
                for x in 0..=4 {
                    f.set(region, sb, x, 0, i);
                    f.set(region, sb, x, 4, i);
                }
                for y in 1..=3 {
                    f.set(region, sb, 0, y, i);
                    for x in 1..=3 {
                        f.set(region, air, x, y, i);
                    }
                    f.set(region, sb, 4, y, i);
                }
            }
        }
    }
}

fn library(f: &Frame, region: &mut dyn Region, r: &mut WorldgenRandom, p: &Part) {
    let facts = f.facts;
    let tall = p.a;
    let top = if tall { 10 } else { 5 };
    let planks = st(facts, "oak_planks", &[]);
    let shelf = st(facts, "bookshelf", &[]);
    let web = st(facts, "cobweb", &[]);
    let torch = |d: &str| st(facts, "wall_torch", &[("facing", d)]);
    bricks(f, region, r, 0, 0, 0, 13, top, 14, true);
    door(f, region, p.door, 4, 1, 0);
    for y in 1..=4 {
        for x in 2..=11 {
            for z in 1..=13 {
                if r.next_float() < 0.07 {
                    f.set(region, web, x, y, z);
                }
            }
        }
    }
    for l in 1..=13 {
        let s = if (l - 1) % 4 == 0 { planks } else { shelf };
        f.fill(region, 1, 1, l, 1, 4, l, s, s, false);
        f.fill(region, 12, 1, l, 12, 4, l, s, s, false);
        if (l - 1) % 4 == 0 {
            f.set(region, torch("east"), 2, 3, l);
            f.set(region, torch("west"), 11, 3, l);
        }
        if tall {
            f.fill(region, 1, 6, l, 1, 9, l, s, s, false);
            f.fill(region, 12, 6, l, 12, 9, l, s, s, false);
        }
    }
    let mut l = 3;
    while l < 12 {
        for x0 in [3, 6, 9] {
            f.fill(region, x0, 1, l, x0 + 1, 3, l, shelf, shelf, false);
        }
        l += 2;
    }
    if tall {
        f.fill(region, 1, 5, 1, 3, 5, 13, planks, planks, false);
        f.fill(region, 10, 5, 1, 12, 5, 13, planks, planks, false);
        f.fill(region, 4, 5, 1, 9, 5, 2, planks, planks, false);
        f.fill(region, 4, 5, 12, 9, 5, 13, planks, planks, false);
        f.set(region, planks, 9, 5, 11);
        f.set(region, planks, 8, 5, 11);
        f.set(region, planks, 9, 5, 10);
        let fence = st(facts, "oak_fence", &[]);
        f.fill(region, 3, 6, 3, 3, 6, 11, fence, fence, false);
        f.fill(region, 10, 6, 3, 10, 6, 9, fence, fence, false);
        f.fill(region, 4, 6, 2, 9, 6, 2, fence, fence, false);
        f.fill(region, 4, 6, 12, 7, 6, 12, fence, fence, false);
        let ladder = st(facts, "ladder", &[("facing", "north")]);
        for y in 1..=7 {
            f.set(region, ladder, 10, y, 13);
        }
        // The chandelier.
        for y in 7..=9 {
            f.set(region, fence, 6, y, 7);
        }
        for (x, z) in [(5, 7), (7, 7), (6, 6), (6, 8)] {
            f.set(region, fence, x, 7, z);
        }
        f.set(region, torch("west"), 5, 8, 7);
        f.set(region, torch("east"), 7, 8, 7);
        f.set(region, torch("north"), 6, 8, 6);
        f.set(region, torch("south"), 6, 8, 8);
    }
    f.chest(region, r, 3, 3, 5, "minecraft:chests/stronghold_library");
    if tall {
        f.set(region, facts.cave_air, 12, 9, 1);
        f.chest(region, r, 12, 8, 1, "minecraft:chests/stronghold_library");
    }
}

fn portal_room(f: &Frame, region: &mut dyn Region, r: &mut WorldgenRandom) {
    let facts = f.facts;
    bricks(f, region, r, 0, 0, 0, 10, 7, 15, false);
    door(f, region, Door::Grates, 4, 1, 0);
    bricks(f, region, r, 1, 6, 1, 1, 6, 14, false);
    bricks(f, region, r, 9, 6, 1, 9, 6, 14, false);
    bricks(f, region, r, 2, 6, 1, 8, 6, 2, false);
    bricks(f, region, r, 2, 6, 14, 8, 6, 14, false);
    bricks(f, region, r, 1, 1, 1, 2, 1, 4, false);
    bricks(f, region, r, 8, 1, 1, 9, 1, 4, false);
    f.fill(region, 1, 1, 1, 1, 1, 3, facts.lava, facts.lava, false);
    f.fill(region, 9, 1, 1, 9, 1, 3, facts.lava, facts.lava, false);
    bricks(f, region, r, 3, 1, 8, 7, 1, 12, false);
    f.fill(region, 4, 1, 9, 6, 1, 11, facts.lava, facts.lava, false);
    let ns = st(facts, "iron_bars", &[("north", "true"), ("south", "true")]);
    let ew = st(facts, "iron_bars", &[("east", "true"), ("west", "true")]);
    let mut j = 3;
    while j < 14 {
        f.fill(region, 0, 3, j, 0, 4, j, ns, ns, false);
        f.fill(region, 10, 3, j, 10, 4, j, ns, ns, false);
        j += 2;
    }
    let mut j = 2;
    while j < 9 {
        f.fill(region, j, 3, 15, j, 4, 15, ew, ew, false);
        j += 2;
    }
    let stairs = st(facts, "stone_brick_stairs", &[("facing", "north")]);
    bricks(f, region, r, 4, 1, 5, 6, 1, 7, false);
    bricks(f, region, r, 4, 2, 6, 6, 2, 7, false);
    bricks(f, region, r, 4, 3, 7, 6, 3, 7, false);
    for k in 4..=6 {
        f.set(region, stairs, k, 1, 4);
        f.set(region, stairs, k, 2, 5);
        f.set(region, stairs, k, 3, 6);
    }
    let frame = |d: &str, eye: bool| {
        st(
            facts,
            "end_portal_frame",
            &[("facing", d), ("eye", if eye { "true" } else { "false" })],
        )
    };
    let mut eyes = [false; 12];
    let mut all = true;
    for e in eyes.iter_mut() {
        *e = r.next_float() > 0.9;
        all &= *e;
    }
    for (i, (d, x, z)) in [
        ("north", 4, 8),
        ("north", 5, 8),
        ("north", 6, 8),
        ("south", 4, 12),
        ("south", 5, 12),
        ("south", 6, 12),
        ("east", 3, 9),
        ("east", 3, 10),
        ("east", 3, 11),
        ("west", 7, 9),
        ("west", 7, 10),
        ("west", 7, 11),
    ]
    .into_iter()
    .enumerate()
    {
        f.set(region, frame(d, eyes[i]), x, 3, z);
    }
    if all {
        let portal = st(facts, "end_portal", &[]);
        for x in 4..=6 {
            for z in 9..=11 {
                f.set(region, portal, x, 3, z);
            }
        }
    }
    let sp = f.world(5, 3, 6);
    if f.chunk.inside(sp)
        && let Some(spawner) = facts.default_state("minecraft:spawner")
    {
        region.set(sp, spawner);
        region.set_block_entity(sp, crate::misc::spawner_entity("minecraft:silverfish"));
    }
}
