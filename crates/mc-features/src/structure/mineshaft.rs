//! Mineshafts (minecraft.wiki, "Mineshaft"): a dirt-floored room with
//! corridors, crossings and stairs branching from it up to eight steps and
//! 80 blocks away, never overlapping; then moved below sea level (or, in
//! badlands, up to the surface). Corridors have supports, rails, cobwebs,
//! the odd chest and cave spider spawner.

use std::sync::Arc;

use mc_nbt::{Compound, Tag};
use mc_tick::Dir;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use super::Ground;
use super::built::Frame;
use super::legacy::{Kind, Piece};
use super::template::BBox;
use crate::blocks::Facts;
use crate::rand::WorldgenRandom;
use crate::value::Heights;
use crate::{Heightmap, P, Region};

#[derive(Debug, Clone)]
pub enum PartKind {
    Room {
        entrances: Vec<BBox>,
    },
    Corridor {
        rails: bool,
        spiders: bool,
        sections: i32,
    },
    Crossing {
        two_floors: bool,
    },
    Stairs,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub kind: PartKind,
    pub bbox: BBox,
    pub facing: Option<Dir>,
    pub mesa: bool,
}

struct Builder<'r> {
    parts: Vec<Part>,
    r: &'r mut WorldgenRandom,
    origin: P,
    mesa: bool,
}

fn bx(x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) -> BBox {
    BBox::from_corners(P::new(x0, y0, z0), P::new(x1, y1, z1))
}

impl Builder<'_> {
    fn collides(&self, b: &BBox) -> bool {
        self.parts.iter().any(|p| p.bbox.intersects(b))
    }

    fn add(&mut self, x: i32, y: i32, z: i32, dir: Dir, depth: i32) -> Option<usize> {
        if depth > 8 || (x - self.origin.x).abs() > 80 || (z - self.origin.z).abs() > 80 {
            return None;
        }
        let depth = depth + 1;
        let i = self.r.next_int_bound(100);
        let part = if i >= 80 {
            let h = if self.r.next_int_bound(4) == 0 { 6 } else { 2 };
            let b = match dir {
                Dir::South => bx(-1, 0, 0, 3, h, 4),
                Dir::West => bx(-4, 0, -1, 0, h, 3),
                Dir::East => bx(0, 0, -1, 4, h, 3),
                _ => bx(-1, 0, -4, 3, h, 0),
            }
            .moved(x, y, z);
            if self.collides(&b) {
                return None;
            }
            Part {
                kind: PartKind::Crossing {
                    two_floors: b.y_span() > 3,
                },
                bbox: b,
                facing: Some(dir),
                mesa: self.mesa,
            }
        } else if i >= 70 {
            let b = match dir {
                Dir::South => bx(0, -5, 0, 2, 2, 8),
                Dir::West => bx(-8, -5, 0, 0, 2, 2),
                Dir::East => bx(0, -5, 0, 8, 2, 2),
                _ => bx(0, -5, -8, 2, 2, 0),
            }
            .moved(x, y, z);
            if self.collides(&b) {
                return None;
            }
            Part {
                kind: PartKind::Stairs,
                bbox: b,
                facing: Some(dir),
                mesa: self.mesa,
            }
        } else {
            let mut found = None;
            let mut n = self.r.next_int_bound(3) + 2;
            while n > 0 {
                let j = n * 5;
                let b = match dir {
                    Dir::South => bx(0, 0, 0, 2, 2, j - 1),
                    Dir::West => bx(-(j - 1), 0, 0, 0, 2, 2),
                    Dir::East => bx(0, 0, 0, j - 1, 2, 2),
                    _ => bx(0, 0, -(j - 1), 2, 2, 0),
                }
                .moved(x, y, z);
                if !self.collides(&b) {
                    found = Some(b);
                    break;
                }
                n -= 1;
            }
            let b = found?;
            let rails = self.r.next_int_bound(3) == 0;
            let spiders = !rails && self.r.next_int_bound(23) == 0;
            let sections = if matches!(dir, Dir::North | Dir::South) {
                b.z_span() / 5
            } else {
                b.x_span() / 5
            };
            Part {
                kind: PartKind::Corridor {
                    rails,
                    spiders,
                    sections,
                },
                bbox: b,
                facing: Some(dir),
                mesa: self.mesa,
            }
        };
        self.parts.push(part);
        let index = self.parts.len() - 1;
        self.children(index, depth);
        Some(index)
    }

    fn children(&mut self, index: usize, depth: i32) {
        let p = self.parts[index].clone();
        let b = p.bbox;
        match p.kind {
            PartKind::Room { .. } => {
                let j = (b.y_span() - 3 - 1).max(1);
                for side in 0..4 {
                    let span = if side == 0 || side == 1 {
                        b.x_span()
                    } else {
                        b.z_span()
                    };
                    let mut k = 0;
                    while k < span {
                        k += self.r.next_int_bound(span);
                        if k + 3 > span {
                            break;
                        }
                        let y = b.min.y + self.r.next_int_bound(j) + 1;
                        let child = match side {
                            0 => self.add(b.min.x + k, y, b.min.z - 1, Dir::North, depth),
                            1 => self.add(b.min.x + k, y, b.max.z + 1, Dir::South, depth),
                            2 => self.add(b.min.x - 1, y, b.min.z + k, Dir::West, depth),
                            _ => self.add(b.max.x + 1, y, b.min.z + k, Dir::East, depth),
                        };
                        if let Some(c) = child {
                            let cb = self.parts[c].bbox;
                            let entrance = match side {
                                0 => {
                                    bx(cb.min.x, cb.min.y, b.min.z, cb.max.x, cb.max.y, b.min.z + 1)
                                }
                                1 => {
                                    bx(cb.min.x, cb.min.y, b.max.z - 1, cb.max.x, cb.max.y, b.max.z)
                                }
                                2 => {
                                    bx(b.min.x, cb.min.y, cb.min.z, b.min.x + 1, cb.max.y, cb.max.z)
                                }
                                _ => {
                                    bx(b.max.x - 1, cb.min.y, cb.min.z, b.max.x, cb.max.y, cb.max.z)
                                }
                            };
                            if let PartKind::Room { entrances } = &mut self.parts[index].kind {
                                entrances.push(entrance);
                            }
                        }
                        k += 4;
                    }
                }
            }
            PartKind::Corridor { .. } => {
                let dir = p.facing.unwrap_or(Dir::North);
                let j = self.r.next_int_bound(4);
                let y = b.min.y - 1 + self.r.next_int_bound(3);
                match dir {
                    Dir::South => match j {
                        0 | 1 => self.add(b.min.x, y, b.max.z + 1, dir, depth),
                        2 => self.add(b.min.x - 1, y, b.max.z - 3, Dir::West, depth),
                        _ => self.add(b.max.x + 1, y, b.max.z - 3, Dir::East, depth),
                    },
                    Dir::West => match j {
                        0 | 1 => self.add(b.min.x - 1, y, b.min.z, dir, depth),
                        2 => self.add(b.min.x, y, b.min.z - 1, Dir::North, depth),
                        _ => self.add(b.min.x, y, b.max.z + 1, Dir::South, depth),
                    },
                    Dir::East => match j {
                        0 | 1 => self.add(b.max.x + 1, y, b.min.z, dir, depth),
                        2 => self.add(b.max.x - 3, y, b.min.z - 1, Dir::North, depth),
                        _ => self.add(b.max.x - 3, y, b.max.z + 1, Dir::South, depth),
                    },
                    _ => match j {
                        0 | 1 => self.add(b.min.x, y, b.min.z - 1, dir, depth),
                        2 => self.add(b.min.x - 1, y, b.min.z, Dir::West, depth),
                        _ => self.add(b.max.x + 1, y, b.min.z, Dir::East, depth),
                    },
                };
                if depth < 8 {
                    if matches!(dir, Dir::North | Dir::South) {
                        let mut k = b.min.z + 3;
                        while k + 3 <= b.max.z {
                            match self.r.next_int_bound(5) {
                                0 => {
                                    self.add(b.min.x - 1, b.min.y, k, Dir::West, depth + 1);
                                }
                                1 => {
                                    self.add(b.max.x + 1, b.min.y, k, Dir::East, depth + 1);
                                }
                                _ => {}
                            }
                            k += 5;
                        }
                    } else {
                        let mut k = b.min.x + 3;
                        while k + 3 <= b.max.x {
                            match self.r.next_int_bound(5) {
                                0 => {
                                    self.add(k, b.min.y, b.min.z - 1, Dir::North, depth + 1);
                                }
                                1 => {
                                    self.add(k, b.min.y, b.max.z + 1, Dir::South, depth + 1);
                                }
                                _ => {}
                            }
                            k += 5;
                        }
                    }
                }
            }
            PartKind::Crossing { two_floors } => {
                let dir = p.facing.unwrap_or(Dir::North);
                let (n, s, w, e) = (
                    (b.min.x + 1, b.min.z - 1, Dir::North),
                    (b.min.x + 1, b.max.z + 1, Dir::South),
                    (b.min.x - 1, b.min.z + 1, Dir::West),
                    (b.max.x + 1, b.min.z + 1, Dir::East),
                );
                let order = match dir {
                    Dir::South => [s, w, e],
                    Dir::West => [n, s, w],
                    Dir::East => [n, s, e],
                    _ => [n, w, e],
                };
                for (x, z, d) in order {
                    self.add(x, b.min.y, z, d, depth);
                }
                if two_floors {
                    let up = b.min.y + 3 + 1;
                    for (x, z, d) in [n, w, e, s] {
                        if self.r.next_bool() {
                            self.add(x, up, z, d, depth);
                        }
                    }
                }
            }
            PartKind::Stairs => {
                let dir = p.facing.unwrap_or(Dir::North);
                match dir {
                    Dir::South => self.add(b.min.x, b.min.y, b.max.z + 1, dir, depth),
                    Dir::West => self.add(b.min.x - 1, b.min.y, b.min.z, dir, depth),
                    Dir::East => self.add(b.max.x + 1, b.min.y, b.min.z, dir, depth),
                    _ => self.add(b.min.x, b.min.y, b.min.z - 1, dir, depth),
                };
            }
        }
    }
}

pub fn generate(
    v: &Value,
    r: &mut WorldgenRandom,
    x: i32,
    z: i32,
    h: Heights,
    ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    let mesa = v["mineshaft_type"].as_str() == Some("mesa");
    r.next_double();
    let (rx, rz) = (x * 16 + 2, z * 16 + 2);
    let room = bx(
        rx,
        50,
        rz,
        rx + 7 + r.next_int_bound(6),
        54 + r.next_int_bound(6),
        rz + 7 + r.next_int_bound(6),
    );
    let mut b = Builder {
        parts: vec![Part {
            kind: PartKind::Room {
                entrances: Vec::new(),
            },
            bbox: room,
            facing: None,
            mesa,
        }],
        r,
        origin: room.min,
        mesa,
    };
    b.children(0, 0);
    let parts = b.parts;
    let all = parts.iter().map(|p| p.bbox).reduce(BBox::union)?;
    let sea = h.sea_level;
    let dy = if mesa {
        let c = all.center();
        let j = ground.height(c.x, c.z, false);
        let k = if j <= sea {
            sea
        } else {
            b.r.int_between(sea, j)
        };
        k - c.y
    } else {
        let i = sea - 10;
        let mut j = all.y_span() + h.min_y + 1;
        if j < i {
            j += b.r.next_int_bound(i - j);
        }
        j - all.max.y
    };
    let at = P::new(x * 16 + 8, 50 + dy, z * 16);
    let pieces = parts
        .into_iter()
        .map(|mut p| {
            p.bbox = p.bbox.moved(0, dy, 0);
            if let PartKind::Room { entrances } = &mut p.kind {
                for e in entrances.iter_mut() {
                    *e = e.moved(0, dy, 0);
                }
            }
            Piece {
                bbox: p.bbox,
                kind: Arc::new(Kind::Mineshaft(p)),
            }
        })
        .collect();
    Some((pieces, at))
}

fn st(facts: &Facts, name: &str, props: &[(&str, &str)]) -> u32 {
    facts
        .state(&format!("minecraft:{name}"), props)
        .or_else(|| facts.default_state(&format!("minecraft:{name}")))
        .unwrap_or(facts.air)
}

/// A frame for parts without a facing: plain world coordinates.
fn plain(facts: &Facts, chunk: BBox) -> Frame<'_> {
    Frame {
        facts,
        bbox: BBox {
            min: P::default(),
            max: P::default(),
        },
        facing: Dir::Up,
        chunk,
    }
}

fn liquid_near(facts: &Facts, region: &dyn Region, b: &BBox) -> bool {
    let x0 = b.min.x - 1;
    let x1 = b.max.x + 1;
    let z0 = b.min.z - 1;
    let z1 = b.max.z + 1;
    for x in x0..=x1 {
        for z in z0..=z1 {
            for y in [b.min.y - 1, b.max.y + 1] {
                if !facts.fluid(region.block(P::new(x, y, z))).is_none() {
                    return true;
                }
            }
        }
    }
    for x in x0..=x1 {
        for y in b.min.y - 1..=b.max.y + 1 {
            for z in [z0, z1] {
                if !facts.fluid(region.block(P::new(x, y, z))).is_none() {
                    return true;
                }
            }
        }
    }
    for z in z0..=z1 {
        for y in b.min.y - 1..=b.max.y + 1 {
            for x in [x0, x1] {
                if !facts.fluid(region.block(P::new(x, y, z))).is_none() {
                    return true;
                }
            }
        }
    }
    false
}

pub fn place(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: &Part,
    chunk: BBox,
) {
    if liquid_near(facts, region, &p.bbox) {
        return;
    }
    let cave_air = facts.cave_air;
    let wood = if p.mesa { "dark_oak" } else { "oak" };
    let planks = st(facts, &format!("{wood}_planks"), &[]);
    let fence = st(facts, &format!("{wood}_fence"), &[]);
    let b = p.bbox;
    match &p.kind {
        PartKind::Room { entrances } => {
            let f = plain(facts, chunk);
            let dirt = st(facts, "dirt", &[]);
            f.fill(
                region, b.min.x, b.min.y, b.min.z, b.max.x, b.min.y, b.max.z, dirt, dirt, true,
            );
            f.fill(
                region,
                b.min.x,
                b.min.y + 1,
                b.min.z,
                b.max.x,
                (b.min.y + 3).min(b.max.y),
                b.max.z,
                cave_air,
                cave_air,
                false,
            );
            for e in entrances {
                f.fill(
                    region,
                    e.min.x,
                    e.max.y - 2,
                    e.min.z,
                    e.max.x,
                    e.max.y,
                    e.max.z,
                    cave_air,
                    cave_air,
                    false,
                );
            }
            // The domed top.
            let (cx, cz) = (
                f64::from(b.min.x + b.max.x) / 2.0,
                f64::from(b.min.z + b.max.z) / 2.0,
            );
            let (rx, rz) = (f64::from(b.x_span()) / 2.0, f64::from(b.z_span()) / 2.0);
            let ry = f64::from(b.max.y - (b.min.y + 4) + 1);
            for y in b.min.y + 4..=b.max.y {
                for x in b.min.x..=b.max.x {
                    for z in b.min.z..=b.max.z {
                        let dx = (f64::from(x) + 0.5 - cx - 0.5) / rx;
                        let dz = (f64::from(z) + 0.5 - cz - 0.5) / rz;
                        let dyv = f64::from(y - (b.min.y + 4)) / ry.max(1.0);
                        if dx * dx + dyv * dyv + dz * dz <= 1.05 {
                            let q = P::new(x, y, z);
                            if chunk.inside(q) {
                                region.set(q, cave_air);
                            }
                        }
                    }
                }
            }
        }
        PartKind::Corridor {
            rails,
            spiders,
            sections,
        } => {
            let f = Frame {
                facts,
                bbox: b,
                facing: p.facing.unwrap_or(Dir::North),
                chunk,
            };
            let m = sections * 5 - 1;
            f.fill(region, 0, 0, 0, 2, 1, m, cave_air, cave_air, false);
            maybe_box(&f, region, r, 0.8, (0, 2, 0), (2, 2, m), cave_air, false);
            if *spiders {
                let web = st(facts, "cobweb", &[]);
                maybe_box(&f, region, r, 0.6, (0, 0, 0), (2, 1, m), web, true);
            }
            let mut spider_placed = false;
            for n in 0..*sections {
                let o = 2 + n * 5;
                support(&f, region, r, o, planks, fence);
                for (chance, x, z) in [
                    (0.1, 0, o - 1),
                    (0.1, 2, o - 1),
                    (0.1, 0, o + 1),
                    (0.1, 2, o + 1),
                    (0.05, 0, o - 2),
                    (0.05, 2, o - 2),
                    (0.05, 0, o + 2),
                    (0.05, 2, o + 2),
                ] {
                    cobweb(&f, region, r, chance, x, 2, z);
                }
                if r.next_int_bound(100) == 0 {
                    minecart_chest(&f, region, r, 2, 0, o - 1);
                }
                if r.next_int_bound(100) == 0 {
                    minecart_chest(&f, region, r, 0, 0, o + 1);
                }
                if !*spiders || spider_placed {
                    continue;
                }
                let q = o - 1 + r.next_int_bound(3);
                let wp = f.world(1, 0, q);
                if !chunk.inside(wp) || !interior(&f, region, 1, 0, q) {
                    continue;
                }
                spider_placed = true;
                if let Some(spawner) = facts.default_state("minecraft:spawner") {
                    region.set(wp, spawner);
                    region
                        .set_block_entity(wp, crate::misc::spawner_entity("minecraft:cave_spider"));
                }
            }
            for x in 0..=2 {
                for z in 0..=m {
                    if !interior(&f, region, x, -1, z) {
                        continue;
                    }
                    let wp = f.world(x, -1, z);
                    if !facts.sturdy(region.block(wp), Dir::Up) {
                        f.set(region, planks, x, -1, z);
                    }
                }
            }
            if *rails {
                let rail = st(facts, "rail", &[("shape", "north_south")]);
                for z in 0..=m {
                    let below = f.get(region, 1, -1, z);
                    if facts.is_air(below) || !facts.solid_render(below) {
                        continue;
                    }
                    let chance = if interior(&f, region, 1, 0, z) {
                        0.7
                    } else {
                        0.9
                    };
                    if r.next_float() < chance {
                        f.set(region, rail, 1, 0, z);
                    }
                }
            }
        }
        PartKind::Crossing { two_floors } => {
            let f = plain(facts, chunk);
            if *two_floors {
                f.fill(
                    region,
                    b.min.x + 1,
                    b.min.y,
                    b.min.z,
                    b.max.x - 1,
                    b.min.y + 2,
                    b.max.z,
                    cave_air,
                    cave_air,
                    false,
                );
                f.fill(
                    region,
                    b.min.x,
                    b.min.y,
                    b.min.z + 1,
                    b.max.x,
                    b.min.y + 2,
                    b.max.z - 1,
                    cave_air,
                    cave_air,
                    false,
                );
                f.fill(
                    region,
                    b.min.x + 1,
                    b.max.y - 2,
                    b.min.z,
                    b.max.x - 1,
                    b.max.y,
                    b.max.z,
                    cave_air,
                    cave_air,
                    false,
                );
                f.fill(
                    region,
                    b.min.x,
                    b.max.y - 2,
                    b.min.z + 1,
                    b.max.x,
                    b.max.y,
                    b.max.z - 1,
                    cave_air,
                    cave_air,
                    false,
                );
                f.fill(
                    region,
                    b.min.x + 1,
                    b.min.y + 3,
                    b.min.z + 1,
                    b.max.x - 1,
                    b.min.y + 3,
                    b.max.z - 1,
                    cave_air,
                    cave_air,
                    false,
                );
            } else {
                f.fill(
                    region,
                    b.min.x + 1,
                    b.min.y,
                    b.min.z,
                    b.max.x - 1,
                    b.max.y,
                    b.max.z,
                    cave_air,
                    cave_air,
                    false,
                );
                f.fill(
                    region,
                    b.min.x,
                    b.min.y,
                    b.min.z + 1,
                    b.max.x,
                    b.max.y,
                    b.max.z - 1,
                    cave_air,
                    cave_air,
                    false,
                );
            }
            for (x, z) in [
                (b.min.x + 1, b.min.z + 1),
                (b.min.x + 1, b.max.z - 1),
                (b.max.x - 1, b.min.z + 1),
                (b.max.x - 1, b.max.z - 1),
            ] {
                if !facts.is_air(region.block(P::new(x, b.max.y + 1, z))) {
                    f.fill(
                        region, x, b.min.y, z, x, b.max.y, z, planks, cave_air, false,
                    );
                }
            }
            let y = b.min.y - 1;
            let sea_floor =
                |region: &dyn Region, x: i32, z: i32| region.height(Heightmap::OceanFloorWg, x, z);
            for x in b.min.x..=b.max.x {
                for z in b.min.z..=b.max.z {
                    let q = P::new(x, y, z);
                    if chunk.inside(q)
                        && q.y < sea_floor(region, x, z)
                        && !facts.sturdy(region.block(q), Dir::Up)
                    {
                        region.set(q, planks);
                    }
                }
            }
        }
        PartKind::Stairs => {
            let f = Frame {
                facts,
                bbox: b,
                facing: p.facing.unwrap_or(Dir::North),
                chunk,
            };
            f.fill(region, 0, 5, 0, 2, 7, 1, cave_air, cave_air, false);
            f.fill(region, 0, 0, 7, 2, 2, 8, cave_air, cave_air, false);
            for i in 0..5 {
                f.fill(
                    region,
                    0,
                    5 - i - i32::from(i < 4),
                    2 + i,
                    2,
                    7 - i,
                    2 + i,
                    cave_air,
                    cave_air,
                    false,
                );
            }
        }
    }
}

/// Underground: below the ocean floor heightmap.
fn interior(f: &Frame, region: &dyn Region, x: i32, y: i32, z: i32) -> bool {
    let p = f.world(x, y, z);
    f.chunk.inside(p) && p.y < region.height(Heightmap::OceanFloorWg, p.x, p.z)
}

#[allow(clippy::too_many_arguments)]
fn maybe_box(
    f: &Frame,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    chance: f32,
    a: (i32, i32, i32),
    b: (i32, i32, i32),
    state: u32,
    need_interior: bool,
) {
    for y in a.1..=b.1 {
        for x in a.0..=b.0 {
            for z in a.2..=b.2 {
                if r.next_float() > chance || (need_interior && !interior(f, region, x, y, z)) {
                    continue;
                }
                f.set(region, state, x, y, z);
            }
        }
    }
}

fn cobweb(
    f: &Frame,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    chance: f32,
    x: i32,
    y: i32,
    z: i32,
) {
    if !interior(f, region, x, y, z) || r.next_float() >= chance {
        return;
    }
    let p = f.world(x, y, z);
    let sturdy = [
        Dir::North,
        Dir::East,
        Dir::South,
        Dir::West,
        Dir::Up,
        Dir::Down,
    ]
    .iter()
    .filter(|d| f.facts.sturdy(region.block(p.rel(**d)), d.opposite()))
    .count();
    if sturdy >= 2 {
        let web = st(f.facts, "cobweb", &[]);
        f.set(region, web, x, y, z);
    }
}

fn support(
    f: &Frame,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    z: i32,
    planks: u32,
    fence: u32,
) {
    // Only under rock.
    if (0..=2).any(|x| f.facts.is_air(f.get(region, x, 3, z))) {
        return;
    }
    let west = f.facts.with(fence, "west", "true");
    let east = f.facts.with(fence, "east", "true");
    f.fill(region, 0, 0, z, 0, 1, z, west, f.facts.cave_air, false);
    f.fill(region, 2, 0, z, 2, 1, z, east, f.facts.cave_air, false);
    if r.next_int_bound(4) == 0 {
        f.set(region, planks, 0, 2, z);
        f.set(region, planks, 2, 2, z);
    } else {
        f.fill(region, 0, 2, z, 2, 2, z, planks, planks, false);
        let torch = |d: &str| st(f.facts, "wall_torch", &[("facing", d)]);
        if r.next_float() < 0.05 {
            f.set(region, torch("south"), 1, 2, z - 1);
        }
        if r.next_float() < 0.05 {
            f.set(region, torch("north"), 1, 2, z + 1);
        }
    }
}

/// Mineshaft loot sits in a chest minecart; until there are entities it is
/// a chest on the rails' spot.
fn minecart_chest(
    f: &Frame,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    x: i32,
    y: i32,
    z: i32,
) {
    let p = f.world(x, y, z);
    if !f.chunk.inside(p)
        || !f.facts.is_air(region.block(p))
        || f.facts.is_air(region.block(p.below(1)))
    {
        return;
    }
    let Some(chest) = f.facts.default_state("minecraft:chest") else {
        return;
    };
    region.set(p, chest);
    let mut be = Compound::new();
    be.insert("id", Tag::String("minecraft:chest".into()));
    be.insert(
        "LootTable",
        Tag::String("minecraft:chests/abandoned_mineshaft".into()),
    );
    be.insert("LootTableSeed", Tag::Long(r.next_long()));
    region.set_block_entity(p, be);
}
