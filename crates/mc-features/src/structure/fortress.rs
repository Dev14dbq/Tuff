//! Nether fortresses (minecraft.wiki, "Nether Fortress"): a bridge
//! crossing at the start, then bridges, crossings, stair rooms, the blaze
//! throne and castle entrances, and inside the castle corridors, turns with
//! chests, balconies, stairs and the nether wart rooms, picked by weight
//! with per-kind limits from each exit of the piece before; then moved to a
//! random height between 48 and 70.

use std::sync::Arc;

use mc_tick::Dir;
use mc_worldgen::random::{Legacy, RandomSource};

use super::built::Frame;
use super::legacy::{Kind, Piece};
use super::stronghold::orient;
use super::template::BBox;
use crate::blocks::Facts;
use crate::rand::WorldgenRandom;
use crate::{P, Region};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum K {
    // Bridge pieces.
    BridgeStraight,
    BridgeCrossing,
    RoomCrossing,
    StairsRoom,
    MonsterThrone,
    CastleEntrance,
    // Castle pieces.
    SmallCorridor,
    SmallCorridorCrossing,
    RightTurn,
    LeftTurn,
    CorridorStairs,
    TBalcony,
    StalkRoom,
    /// Where nothing else fits: a ragged bridge end.
    EndFiller,
}

#[derive(Debug, Clone)]
pub struct Part {
    kind: K,
    bbox: BBox,
    facing: Dir,
    depth: i32,
    /// Turns: a chest inside; end fillers: their own seed.
    chest: bool,
    seed: i32,
}

/// Kind, weight, most of it (0: any number), may follow itself.
const BRIDGE: [(K, i32, i32, bool); 6] = [
    (K::BridgeStraight, 30, 0, true),
    (K::BridgeCrossing, 10, 4, false),
    (K::RoomCrossing, 10, 4, false),
    (K::StairsRoom, 10, 3, false),
    (K::MonsterThrone, 5, 2, false),
    (K::CastleEntrance, 5, 1, false),
];

const CASTLE: [(K, i32, i32, bool); 7] = [
    (K::SmallCorridor, 25, 0, true),
    (K::SmallCorridorCrossing, 15, 5, false),
    (K::RightTurn, 5, 10, false),
    (K::LeftTurn, 5, 10, false),
    (K::CorridorStairs, 10, 3, true),
    (K::TBalcony, 7, 2, false),
    (K::StalkRoom, 5, 2, false),
];

struct Weights {
    list: Vec<(K, i32, i32, bool)>,
    counts: Vec<i32>,
}

impl Weights {
    fn new(list: &[(K, i32, i32, bool)]) -> Self {
        Self {
            list: list.to_vec(),
            counts: vec![0; list.len()],
        }
    }
}

struct Gen<'r> {
    r: &'r mut WorldgenRandom,
    parts: Vec<Part>,
    pending: Vec<usize>,
    bridge: Weights,
    castle: Weights,
    previous: Option<K>,
    origin: P,
}

impl Gen<'_> {
    fn free(&self, b: &BBox) -> bool {
        b.min.y > 10 && !self.parts.iter().any(|p| p.bbox.intersects(b))
    }

    fn part(kind: K, bbox: BBox, facing: Dir, depth: i32) -> Part {
        Part {
            kind,
            bbox,
            facing,
            depth,
            chest: false,
            seed: 0,
        }
    }

    fn filler(&mut self, x: i32, y: i32, z: i32, dir: Dir, depth: i32) -> Option<Part> {
        let b = orient(x, y, z, -1, -3, 0, 5, 10, 8, dir);
        if !self.free(&b) {
            return None;
        }
        let mut p = Self::part(K::EndFiller, b, dir, depth);
        p.seed = self.r.next_int();
        Some(p)
    }

    fn create(&mut self, kind: K, x: i32, y: i32, z: i32, dir: Dir, depth: i32) -> Option<Part> {
        let (ox, oy, w, h, d) = match kind {
            K::BridgeStraight => (-1, -3, 5, 10, 19),
            K::BridgeCrossing => (-8, -3, 19, 10, 19),
            K::RoomCrossing => (-2, 0, 7, 9, 7),
            K::StairsRoom => (-2, 0, 7, 11, 7),
            K::MonsterThrone => (-2, 0, 7, 8, 9),
            K::CastleEntrance | K::StalkRoom => (-5, -3, 13, 14, 13),
            K::SmallCorridor | K::SmallCorridorCrossing | K::RightTurn | K::LeftTurn => {
                (-1, 0, 5, 7, 5)
            }
            K::CorridorStairs => (-1, -7, 5, 14, 10),
            K::TBalcony => (-3, 0, 9, 7, 9),
            K::EndFiller => return None,
        };
        let b = orient(x, y, z, ox, oy, 0, w, h, d, dir);
        if !self.free(&b) {
            return None;
        }
        let mut p = Self::part(kind, b, dir, depth);
        if matches!(kind, K::RightTurn | K::LeftTurn) {
            p.chest = self.r.next_int_bound(3) == 0;
        }
        Some(p)
    }

    fn next_piece(
        &mut self,
        castle: bool,
        x: i32,
        y: i32,
        z: i32,
        dir: Dir,
        depth: i32,
    ) -> Option<Part> {
        let w = if castle { &self.castle } else { &self.bridge };
        let mut open = false;
        let mut total = 0;
        for (i, &(_, weight, max, _)) in w.list.iter().enumerate() {
            if max > 0 && w.counts[i] < max {
                open = true;
            }
            total += weight;
        }
        let go = open && total > 0 && depth <= 30;
        let mut tries = 0;
        while tries < 5 && go {
            tries += 1;
            let mut j = self.r.next_int_bound(total);
            let n = if castle {
                self.castle.list.len()
            } else {
                self.bridge.list.len()
            };
            let mut i = 0;
            while i < n {
                let w = if castle { &self.castle } else { &self.bridge };
                let (k, weight, max, in_row) = w.list[i];
                j -= weight;
                if j < 0 {
                    let allowed = max == 0 || w.counts[i] < max;
                    if !allowed || (self.previous == Some(k) && !in_row) {
                        break;
                    }
                    if let Some(p) = self.create(k, x, y, z, dir, depth) {
                        let w = if castle {
                            &mut self.castle
                        } else {
                            &mut self.bridge
                        };
                        w.counts[i] += 1;
                        self.previous = Some(k);
                        if max > 0 && w.counts[i] >= max {
                            w.list.remove(i);
                            w.counts.remove(i);
                        }
                        return Some(p);
                    }
                }
                i += 1;
            }
        }
        self.filler(x, y, z, dir, depth)
    }

    #[allow(clippy::too_many_arguments)]
    fn add(&mut self, x: i32, y: i32, z: i32, dir: Dir, depth: i32, castle: bool) {
        if (x - self.origin.x).abs() > 112 || (z - self.origin.z).abs() > 112 {
            // Made (it draws its seed) but not kept.
            let _ = self.filler(x, y, z, dir, depth);
            return;
        }
        if let Some(p) = self.next_piece(castle, x, y, z, dir, depth + 1) {
            self.parts.push(p);
            self.pending.push(self.parts.len() - 1);
        }
    }

    fn forward(&mut self, i: usize, xo: i32, yo: i32, castle: bool) {
        let p = self.parts[i].clone();
        let b = p.bbox;
        let (x, z) = match p.facing {
            Dir::South => (b.min.x + xo, b.max.z + 1),
            Dir::West => (b.min.x - 1, b.min.z + xo),
            Dir::East => (b.max.x + 1, b.min.z + xo),
            _ => (b.min.x + xo, b.min.z - 1),
        };
        self.add(x, b.min.y + yo, z, p.facing, p.depth, castle);
    }

    fn left(&mut self, i: usize, yo: i32, zo: i32, castle: bool) {
        let p = self.parts[i].clone();
        let b = p.bbox;
        match p.facing {
            Dir::West | Dir::East => self.add(
                b.min.x + zo,
                b.min.y + yo,
                b.min.z - 1,
                Dir::North,
                p.depth,
                castle,
            ),
            _ => self.add(
                b.min.x - 1,
                b.min.y + yo,
                b.min.z + zo,
                Dir::West,
                p.depth,
                castle,
            ),
        }
    }

    fn right(&mut self, i: usize, yo: i32, zo: i32, castle: bool) {
        let p = self.parts[i].clone();
        let b = p.bbox;
        match p.facing {
            Dir::West | Dir::East => self.add(
                b.min.x + zo,
                b.min.y + yo,
                b.max.z + 1,
                Dir::South,
                p.depth,
                castle,
            ),
            _ => self.add(
                b.max.x + 1,
                b.min.y + yo,
                b.min.z + zo,
                Dir::East,
                p.depth,
                castle,
            ),
        }
    }

    fn children(&mut self, i: usize) {
        let p = self.parts[i].clone();
        match p.kind {
            K::BridgeCrossing => {
                self.forward(i, 8, 3, false);
                self.left(i, 3, 8, false);
                self.right(i, 3, 8, false);
            }
            K::BridgeStraight => self.forward(i, 1, 3, false),
            K::RoomCrossing => {
                self.forward(i, 2, 0, false);
                self.left(i, 0, 2, false);
                self.right(i, 0, 2, false);
            }
            K::StairsRoom => self.right(i, 6, 2, false),
            K::CastleEntrance => self.forward(i, 5, 3, true),
            K::StalkRoom => {
                self.forward(i, 5, 3, true);
                self.forward(i, 5, 11, true);
            }
            K::SmallCorridor | K::CorridorStairs => self.forward(i, 1, 0, true),
            K::SmallCorridorCrossing => {
                self.forward(i, 1, 0, true);
                self.left(i, 0, 1, true);
                self.right(i, 0, 1, true);
            }
            K::RightTurn => self.right(i, 0, 1, true),
            K::LeftTurn => self.left(i, 0, 1, true),
            K::TBalcony => {
                let side = if matches!(p.facing, Dir::West | Dir::North) {
                    5
                } else {
                    1
                };
                let castle = self.r.next_int_bound(8) > 0;
                self.left(i, 0, side, castle);
                let castle = self.r.next_int_bound(8) > 0;
                self.right(i, 0, side, castle);
            }
            K::MonsterThrone | K::EndFiller => {}
        }
    }
}

const DIRS: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

/// The pieces of a fortress starting in chunk `x, z`, with `r` seeded for
/// the chunk.
pub fn generate(r: &mut WorldgenRandom, x: i32, z: i32) -> Option<(Vec<Piece>, P)> {
    let dir = DIRS[r.next_int_bound(4) as usize];
    let (sx, sz) = (x * 16 + 2, z * 16 + 2);
    let b = BBox::from_corners(P::new(sx, 64, sz), P::new(sx + 18, 73, sz + 18));
    let mut g = Gen {
        r,
        parts: Vec::new(),
        pending: Vec::new(),
        bridge: Weights::new(&BRIDGE),
        castle: Weights::new(&CASTLE),
        previous: None,
        origin: b.min,
    };
    g.parts.push(Gen::part(K::BridgeCrossing, b, dir, 0));
    g.children(0);
    while !g.pending.is_empty() {
        let j = g.r.next_int_bound(g.pending.len() as i32) as usize;
        let i = g.pending.remove(j);
        g.children(i);
    }
    let parts = std::mem::take(&mut g.parts);
    // Between y 48 and 70.
    let all = parts.iter().map(|p| p.bbox).reduce(BBox::union)?;
    let room = 70 - 48 + 1 - all.y_span();
    let y = if room > 1 {
        48 + g.r.next_int_bound(room)
    } else {
        48
    };
    let dy = y - all.min.y;
    let at = P::new(x * 16, 64, z * 16);
    let pieces = parts
        .into_iter()
        .map(|mut p| {
            p.bbox = p.bbox.moved(0, dy, 0);
            Piece {
                bbox: p.bbox,
                kind: Arc::new(Kind::Fortress(p)),
            }
        })
        .collect();
    Some((pieces, at))
}

fn st(facts: &Facts, name: &str) -> u32 {
    facts
        .default_state(&format!("minecraft:{name}"))
        .unwrap_or(facts.air)
}

/// A nether brick fence joined to the given sides.
fn fence(facts: &Facts, sides: &[&str]) -> u32 {
    sides
        .iter()
        .fold(st(facts, "nether_brick_fence"), |s, side| {
            facts.with(s, side, "true")
        })
}

/// Places the part of a fortress piece inside `chunk`.
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
    let nb = st(facts, "nether_bricks");
    let air = facts.air;
    let fwe = fence(facts, &["west", "east"]);
    let fns = fence(facts, &["north", "south"]);
    let boxed = |region: &mut dyn Region, b: [i32; 6], s: u32| {
        f.fill(region, b[0], b[1], b[2], b[3], b[4], b[5], s, s, false);
    };
    let columns = |region: &mut dyn Region, x0: i32, x1: i32, z0: i32, z1: i32| {
        for x in x0..=x1 {
            for z in z0..=z1 {
                f.column_down(region, nb, x, -1, z);
            }
        }
    };
    match p.kind {
        K::BridgeCrossing => {
            for b in [[7, 3, 0, 11, 4, 18], [0, 3, 7, 18, 4, 11]] {
                boxed(region, b, nb);
            }
            boxed(region, [8, 5, 0, 10, 7, 18], air);
            boxed(region, [0, 5, 8, 18, 7, 10], air);
            for b in [
                [7, 5, 0, 7, 5, 7],
                [7, 5, 11, 7, 5, 18],
                [11, 5, 0, 11, 5, 7],
                [11, 5, 11, 11, 5, 18],
                [0, 5, 7, 7, 5, 7],
                [11, 5, 7, 18, 5, 7],
                [0, 5, 11, 7, 5, 11],
                [11, 5, 11, 18, 5, 11],
                [7, 2, 0, 11, 2, 5],
                [7, 2, 13, 11, 2, 18],
                [7, 0, 0, 11, 1, 3],
                [7, 0, 15, 11, 1, 18],
            ] {
                boxed(region, b, nb);
            }
            for x in 7..=11 {
                for z in 0..=2 {
                    f.column_down(region, nb, x, -1, z);
                    f.column_down(region, nb, x, -1, 18 - z);
                }
            }
            for b in [
                [0, 2, 7, 5, 2, 11],
                [13, 2, 7, 18, 2, 11],
                [0, 0, 7, 3, 1, 11],
                [15, 0, 7, 18, 1, 11],
            ] {
                boxed(region, b, nb);
            }
            for x in 0..=2 {
                for z in 7..=11 {
                    f.column_down(region, nb, x, -1, z);
                    f.column_down(region, nb, 18 - x, -1, z);
                }
            }
        }
        K::BridgeStraight => {
            boxed(region, [0, 3, 0, 4, 4, 18], nb);
            boxed(region, [1, 5, 0, 3, 7, 18], air);
            for b in [
                [0, 5, 0, 0, 5, 18],
                [4, 5, 0, 4, 5, 18],
                [0, 2, 0, 4, 2, 5],
                [0, 2, 13, 4, 2, 18],
                [0, 0, 0, 4, 1, 3],
                [0, 0, 15, 4, 1, 18],
            ] {
                boxed(region, b, nb);
            }
            for x in 0..=4 {
                for z in 0..=2 {
                    f.column_down(region, nb, x, -1, z);
                    f.column_down(region, nb, x, -1, 18 - z);
                }
            }
            let fnse = fence(facts, &["north", "south", "east"]);
            let fnsw = fence(facts, &["north", "south", "west"]);
            for b in [
                [0, 1, 1, 0, 4, 1],
                [0, 3, 4, 0, 4, 4],
                [0, 3, 14, 0, 4, 14],
                [0, 1, 17, 0, 4, 17],
            ] {
                boxed(region, b, fnse);
            }
            for b in [
                [4, 1, 1, 4, 4, 1],
                [4, 3, 4, 4, 4, 4],
                [4, 3, 14, 4, 4, 14],
                [4, 1, 17, 4, 4, 17],
            ] {
                boxed(region, b, fnsw);
            }
        }
        K::EndFiller => {
            let mut sr = Legacy::new(i64::from(p.seed));
            for x in 0..=4 {
                for y in 3..=4 {
                    let k = sr.next_int_bound(8);
                    boxed(region, [x, y, 0, x, y, k], nb);
                }
            }
            let k = sr.next_int_bound(8);
            boxed(region, [0, 5, 0, 0, 5, k], nb);
            let k = sr.next_int_bound(8);
            boxed(region, [4, 5, 0, 4, 5, k], nb);
            for x in 0..=4 {
                let k = sr.next_int_bound(5);
                boxed(region, [x, 2, 0, x, 2, k], nb);
            }
            for x in 0..=4 {
                for y in 0..=1 {
                    let k = sr.next_int_bound(3);
                    boxed(region, [x, y, 0, x, y, k], nb);
                }
            }
        }
        K::RoomCrossing => {
            boxed(region, [0, 0, 0, 6, 1, 6], nb);
            boxed(region, [0, 2, 0, 6, 7, 6], air);
            for b in [
                [0, 2, 0, 1, 6, 0],
                [0, 2, 6, 1, 6, 6],
                [5, 2, 0, 6, 6, 0],
                [5, 2, 6, 6, 6, 6],
                [0, 2, 0, 0, 6, 1],
                [0, 2, 5, 0, 6, 6],
                [6, 2, 0, 6, 6, 1],
                [6, 2, 5, 6, 6, 6],
            ] {
                boxed(region, b, nb);
            }
            boxed(region, [2, 6, 0, 4, 6, 0], nb);
            boxed(region, [2, 5, 0, 4, 5, 0], fwe);
            boxed(region, [2, 6, 6, 4, 6, 6], nb);
            boxed(region, [2, 5, 6, 4, 5, 6], fwe);
            boxed(region, [0, 6, 2, 0, 6, 4], nb);
            boxed(region, [0, 5, 2, 0, 5, 4], fns);
            boxed(region, [6, 6, 2, 6, 6, 4], nb);
            boxed(region, [6, 5, 2, 6, 5, 4], fns);
            columns(region, 0, 6, 0, 6);
        }
        K::StairsRoom => {
            boxed(region, [0, 0, 0, 6, 1, 6], nb);
            boxed(region, [0, 2, 0, 6, 10, 6], air);
            for b in [
                [0, 2, 0, 1, 8, 0],
                [5, 2, 0, 6, 8, 0],
                [0, 2, 1, 0, 8, 6],
                [6, 2, 1, 6, 8, 6],
                [1, 2, 6, 5, 8, 6],
            ] {
                boxed(region, b, nb);
            }
            boxed(region, [0, 3, 2, 0, 5, 4], fns);
            boxed(region, [6, 3, 2, 6, 5, 2], fns);
            boxed(region, [6, 3, 4, 6, 5, 4], fns);
            f.set(region, nb, 5, 2, 5);
            for b in [
                [4, 2, 5, 4, 3, 5],
                [3, 2, 5, 3, 4, 5],
                [2, 2, 5, 2, 5, 5],
                [1, 2, 5, 1, 6, 5],
                [1, 7, 1, 5, 7, 4],
            ] {
                boxed(region, b, nb);
            }
            boxed(region, [6, 8, 2, 6, 8, 4], air);
            boxed(region, [2, 6, 0, 4, 8, 0], nb);
            boxed(region, [2, 5, 0, 4, 5, 0], fwe);
            columns(region, 0, 6, 0, 6);
        }
        K::MonsterThrone => {
            boxed(region, [0, 2, 0, 6, 7, 7], air);
            for b in [
                [1, 0, 0, 5, 1, 7],
                [1, 2, 1, 5, 2, 7],
                [1, 3, 2, 5, 3, 7],
                [1, 4, 3, 5, 4, 7],
                [1, 2, 0, 1, 4, 2],
                [5, 2, 0, 5, 4, 2],
                [1, 5, 2, 1, 5, 3],
                [5, 5, 2, 5, 5, 3],
                [0, 5, 3, 0, 5, 8],
                [6, 5, 3, 6, 5, 8],
                [1, 5, 8, 5, 5, 8],
            ] {
                boxed(region, b, nb);
            }
            f.set(region, fence(facts, &["west"]), 1, 6, 3);
            f.set(region, fence(facts, &["east"]), 5, 6, 3);
            f.set(region, fence(facts, &["east", "north"]), 0, 6, 3);
            f.set(region, fence(facts, &["west", "north"]), 6, 6, 3);
            boxed(region, [0, 6, 4, 0, 6, 7], fns);
            boxed(region, [6, 6, 4, 6, 6, 7], fns);
            f.set(region, fence(facts, &["east", "south"]), 0, 6, 8);
            f.set(region, fence(facts, &["west", "south"]), 6, 6, 8);
            boxed(region, [1, 6, 8, 5, 6, 8], fwe);
            f.set(region, fence(facts, &["east"]), 1, 7, 8);
            boxed(region, [2, 7, 8, 4, 7, 8], fwe);
            f.set(region, fence(facts, &["west"]), 5, 7, 8);
            f.set(region, fence(facts, &["east"]), 2, 8, 8);
            f.set(region, fwe, 3, 8, 8);
            f.set(region, fence(facts, &["west"]), 4, 8, 8);
            let sp = f.world(3, 5, 5);
            if chunk.inside(sp) {
                region.set(sp, st(facts, "spawner"));
                region.set_block_entity(sp, crate::misc::spawner_entity("minecraft:blaze"));
            }
            columns(region, 0, 6, 0, 6);
        }
        K::CastleEntrance | K::StalkRoom => {
            castle_hall(&f, region, nb, air, fwe, fns);
            if p.kind == K::CastleEntrance {
                boxed(region, [5, 8, 0, 7, 8, 0], st(facts, "nether_brick_fence"));
                hall_floor(&f, region, nb);
                boxed(region, [5, 5, 5, 7, 5, 7], nb);
                boxed(region, [6, 1, 6, 6, 4, 6], air);
                f.set(region, nb, 6, 0, 6);
                let lava = st(facts, "lava");
                f.set(region, lava, 6, 5, 6);
                let lp = f.world(6, 5, 6);
                if chunk.inside(lp) {
                    region.schedule_tick(lp);
                }
            } else {
                stalk_room(&f, region, nb, air);
                hall_floor(&f, region, nb);
            }
        }
        K::SmallCorridor => {
            boxed(region, [0, 0, 0, 4, 1, 4], nb);
            boxed(region, [0, 2, 0, 4, 5, 4], air);
            boxed(region, [0, 2, 0, 0, 5, 4], nb);
            boxed(region, [4, 2, 0, 4, 5, 4], nb);
            for b in [
                [0, 3, 1, 0, 4, 1],
                [0, 3, 3, 0, 4, 3],
                [4, 3, 1, 4, 4, 1],
                [4, 3, 3, 4, 4, 3],
            ] {
                boxed(region, b, fns);
            }
            boxed(region, [0, 6, 0, 4, 6, 4], nb);
            columns(region, 0, 4, 0, 4);
        }
        K::SmallCorridorCrossing => {
            boxed(region, [0, 0, 0, 4, 1, 4], nb);
            boxed(region, [0, 2, 0, 4, 5, 4], air);
            for b in [
                [0, 2, 0, 0, 5, 0],
                [4, 2, 0, 4, 5, 0],
                [0, 2, 4, 0, 5, 4],
                [4, 2, 4, 4, 5, 4],
                [0, 6, 0, 4, 6, 4],
            ] {
                boxed(region, b, nb);
            }
            columns(region, 0, 4, 0, 4);
        }
        K::RightTurn | K::LeftTurn => {
            let right = p.kind == K::RightTurn;
            boxed(region, [0, 0, 0, 4, 1, 4], nb);
            boxed(region, [0, 2, 0, 4, 5, 4], air);
            let (wall, open) = if right { (0, 4) } else { (4, 0) };
            boxed(region, [wall, 2, 0, wall, 5, 4], nb);
            boxed(region, [wall, 3, 1, wall, 4, 1], fns);
            boxed(region, [wall, 3, 3, wall, 4, 3], fns);
            boxed(region, [open, 2, 0, open, 5, 0], nb);
            if right {
                boxed(region, [1, 2, 4, 4, 5, 4], nb);
            } else {
                boxed(region, [0, 2, 4, 3, 5, 4], nb);
            }
            boxed(region, [1, 3, 4, 1, 4, 4], fwe);
            boxed(region, [3, 3, 4, 3, 4, 4], fwe);
            let cx = if right { 1 } else { 3 };
            if p.chest && chunk.inside(f.world(cx, 2, 3)) {
                f.chest(region, r, cx, 2, 3, "minecraft:chests/nether_bridge");
            }
            boxed(region, [0, 6, 0, 4, 6, 4], nb);
            columns(region, 0, 4, 0, 4);
        }
        K::CorridorStairs => {
            let stairs = facts.with(st(facts, "nether_brick_stairs"), "facing", "south");
            for i in 0..=9 {
                let j = (7 - i).max(1);
                let k = (j + 5).max(14 - i).min(13);
                boxed(region, [0, 0, i, 4, j, i], nb);
                boxed(region, [1, j + 1, i, 3, k - 1, i], air);
                if i <= 6 {
                    for x in 1..=3 {
                        f.set(region, stairs, x, j + 1, i);
                    }
                }
                boxed(region, [0, k, i, 4, k, i], nb);
                boxed(region, [0, j + 1, i, 0, k - 1, i], nb);
                boxed(region, [4, j + 1, i, 4, k - 1, i], nb);
                if i & 1 == 0 {
                    boxed(region, [0, j + 2, i, 0, j + 3, i], fns);
                    boxed(region, [4, j + 2, i, 4, j + 3, i], fns);
                }
                for x in 0..=4 {
                    f.column_down(region, nb, x, -1, i);
                }
            }
        }
        K::TBalcony => {
            boxed(region, [0, 0, 0, 8, 1, 8], nb);
            boxed(region, [0, 2, 0, 8, 5, 8], air);
            boxed(region, [0, 6, 0, 8, 6, 5], nb);
            boxed(region, [0, 2, 0, 2, 5, 0], nb);
            boxed(region, [6, 2, 0, 8, 5, 0], nb);
            boxed(region, [1, 3, 0, 1, 4, 0], fwe);
            boxed(region, [7, 3, 0, 7, 4, 0], fwe);
            boxed(region, [0, 2, 4, 8, 2, 8], nb);
            boxed(region, [1, 1, 4, 2, 2, 4], air);
            boxed(region, [6, 1, 4, 7, 2, 4], air);
            boxed(region, [1, 3, 8, 7, 3, 8], fwe);
            f.set(region, fence(facts, &["east", "south"]), 0, 3, 8);
            f.set(region, fence(facts, &["west", "south"]), 8, 3, 8);
            boxed(region, [0, 3, 6, 0, 3, 7], fns);
            boxed(region, [8, 3, 6, 8, 3, 7], fns);
            for b in [
                [0, 3, 4, 0, 5, 5],
                [8, 3, 4, 8, 5, 5],
                [1, 3, 5, 2, 5, 5],
                [6, 3, 5, 7, 5, 5],
            ] {
                boxed(region, b, nb);
            }
            boxed(region, [1, 4, 5, 1, 5, 5], fwe);
            boxed(region, [7, 4, 5, 7, 5, 5], fwe);
            for z in 0..=5 {
                for x in 0..=8 {
                    f.column_down(region, nb, x, -1, z);
                }
            }
        }
    }
}

/// The walls, roof and battlements of the castle entrance and the wart room.
fn castle_hall(f: &Frame, region: &mut dyn Region, nb: u32, air: u32, fwe: u32, fns: u32) {
    let facts = f.facts;
    let boxed = |region: &mut dyn Region, b: [i32; 6], s: u32| {
        f.fill(region, b[0], b[1], b[2], b[3], b[4], b[5], s, s, false);
    };
    boxed(region, [0, 3, 0, 12, 4, 12], nb);
    boxed(region, [0, 5, 0, 12, 13, 12], air);
    for b in [
        [0, 5, 0, 1, 12, 12],
        [11, 5, 0, 12, 12, 12],
        [2, 5, 11, 4, 12, 12],
        [8, 5, 11, 10, 12, 12],
        [5, 9, 11, 7, 12, 12],
        [2, 5, 0, 4, 12, 1],
        [8, 5, 0, 10, 12, 1],
        [5, 9, 0, 7, 12, 1],
        [2, 11, 2, 10, 12, 10],
    ] {
        boxed(region, b, nb);
    }
    for i in (1..=11).step_by(2) {
        boxed(region, [i, 10, 0, i, 11, 0], fwe);
        boxed(region, [i, 10, 12, i, 11, 12], fwe);
        boxed(region, [0, 10, i, 0, 11, i], fns);
        boxed(region, [12, 10, i, 12, 11, i], fns);
        f.set(region, nb, i, 13, 0);
        f.set(region, nb, i, 13, 12);
        f.set(region, nb, 0, 13, i);
        f.set(region, nb, 12, 13, i);
        if i != 11 {
            f.set(region, fwe, i + 1, 13, 0);
            f.set(region, fwe, i + 1, 13, 12);
            f.set(region, fns, 0, 13, i + 1);
            f.set(region, fns, 12, 13, i + 1);
        }
    }
    f.set(region, fence(facts, &["north", "east"]), 0, 13, 0);
    f.set(region, fence(facts, &["south", "east"]), 0, 13, 12);
    f.set(region, fence(facts, &["south", "west"]), 12, 13, 12);
    f.set(region, fence(facts, &["north", "west"]), 12, 13, 0);
    let fnsw = facts.with(fns, "west", "true");
    let fnse = facts.with(fns, "east", "true");
    for i in (3..=9).step_by(2) {
        boxed(region, [1, 7, i, 1, 8, i], fnsw);
        boxed(region, [11, 7, i, 11, 8, i], fnse);
    }
}

/// The floor and pillars under a castle hall.
fn hall_floor(f: &Frame, region: &mut dyn Region, nb: u32) {
    for b in [
        [4, 2, 0, 8, 2, 12],
        [0, 2, 4, 12, 2, 8],
        [4, 0, 0, 8, 1, 3],
        [4, 0, 9, 8, 1, 12],
        [0, 0, 4, 3, 1, 8],
        [9, 0, 4, 12, 1, 8],
    ] {
        f.fill(region, b[0], b[1], b[2], b[3], b[4], b[5], nb, nb, false);
    }
    for x in 4..=8 {
        for z in 0..=2 {
            f.column_down(region, nb, x, -1, z);
            f.column_down(region, nb, x, -1, 12 - z);
        }
    }
    for x in 0..=2 {
        for z in 4..=8 {
            f.column_down(region, nb, x, -1, z);
            f.column_down(region, nb, 12 - x, -1, z);
        }
    }
}

/// The wart room's stairs, planters and wart.
fn stalk_room(f: &Frame, region: &mut dyn Region, nb: u32, air: u32) {
    let facts = f.facts;
    let boxed = |region: &mut dyn Region, b: [i32; 6], s: u32| {
        f.fill(region, b[0], b[1], b[2], b[3], b[4], b[5], s, s, false);
    };
    let stairs = st(facts, "nether_brick_stairs");
    let facing = |d: &str| facts.with(stairs, "facing", d);
    let north = facing("north");
    for j in 0..=6 {
        let k = j + 4;
        for x in 5..=7 {
            f.set(region, north, x, 5 + j, k);
        }
        if (5..=8).contains(&k) {
            boxed(region, [5, 5, k, 7, j + 4, k], nb);
        } else if (9..=10).contains(&k) {
            boxed(region, [5, 8, k, 7, j + 4, k], nb);
        }
        if j >= 1 {
            boxed(region, [5, 6 + j, k, 7, 9 + j, k], air);
        }
    }
    for x in 5..=7 {
        f.set(region, north, x, 12, 11);
    }
    let fns = fence(facts, &["north", "south"]);
    boxed(region, [5, 6, 7, 5, 7, 7], facts.with(fns, "east", "true"));
    boxed(region, [7, 6, 7, 7, 7, 7], facts.with(fns, "west", "true"));
    boxed(region, [5, 13, 12, 7, 13, 12], air);
    for b in [
        [2, 5, 2, 3, 5, 3],
        [2, 5, 9, 3, 5, 10],
        [9, 5, 2, 10, 5, 3],
        [9, 5, 9, 10, 5, 10],
        [10, 5, 4, 10, 5, 8],
    ] {
        boxed(region, b, nb);
    }
    let (east, west) = (facing("east"), facing("west"));
    for z in [2, 3, 9, 10] {
        f.set(region, west, 4, 5, z);
        f.set(region, east, 8, 5, z);
    }
    let soul = st(facts, "soul_sand");
    let wart = st(facts, "nether_wart");
    boxed(region, [3, 4, 4, 4, 4, 8], soul);
    boxed(region, [8, 4, 4, 9, 4, 8], soul);
    boxed(region, [3, 5, 4, 4, 5, 8], wart);
    boxed(region, [8, 5, 4, 9, 5, 8], wart);
}
