//! End cities (minecraft.wiki, "End City"): a house of templates, then
//! towers grown recursively with bridges to more houses, fat towers and
//! at most one ship; a branch that runs into another branch is dropped.

use mc_worldgen::random::RandomSource;

use super::legacy::{Piece, Post, template_piece};
use super::template::{BBox, Mirror, Rotation, Settings, transform};
use super::{Ground, Structures};
use crate::P;
use crate::rand::WorldgenRandom;
use crate::value::Heights;

/// One template piece while the city grows.
#[derive(Debug, Clone)]
struct Part {
    name: &'static str,
    pos: P,
    rot: Rotation,
    overwrite: bool,
    depth: i32,
    bbox: BBox,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Tower,
    TowerBridge,
    HouseTower,
    FatTower,
}

const TOWER_BRIDGES: [(u8, P); 4] = [
    (0, P::new(1, -1, 0)),
    (1, P::new(6, -1, 1)),
    (3, P::new(0, -1, 5)),
    (2, P::new(5, -1, 6)),
];

const FAT_TOWER_BRIDGES: [(u8, P); 4] = [
    (0, P::new(4, -1, 0)),
    (1, P::new(12, -1, 4)),
    (3, P::new(0, -1, 8)),
    (2, P::new(8, -1, 12)),
];

struct Gen<'a> {
    s: &'a Structures,
    r: &'a mut WorldgenRandom,
    /// Every piece made; lists below hold indices into it.
    parts: Vec<Part>,
    ship: bool,
}

fn turn(a: Rotation, b: u8) -> Rotation {
    Rotation((a.0 + b) % 4)
}

impl Gen<'_> {
    fn make(
        &mut self,
        name: &'static str,
        pos: P,
        rot: Rotation,
        overwrite: bool,
    ) -> Option<usize> {
        let t = self.s.template(&format!("minecraft:end_city/{name}"))?;
        let bbox = t.bbox(pos, rot, P::default(), Mirror::None);
        self.parts.push(Part {
            name,
            pos,
            rot,
            overwrite,
            depth: 0,
            bbox,
        });
        Some(self.parts.len() - 1)
    }

    /// A piece joined to `prev` at `offset` (in `prev`'s frame), added to
    /// `list`.
    fn add(
        &mut self,
        list: &mut Vec<usize>,
        prev: usize,
        offset: P,
        name: &'static str,
        rot: Rotation,
        overwrite: bool,
    ) -> Option<usize> {
        let p = &self.parts[prev];
        let pos = p
            .pos
            .add(transform(offset, Mirror::None, p.rot, P::default()));
        let i = self.make(name, pos, rot, overwrite)?;
        list.push(i);
        Some(i)
    }

    fn recurse(
        &mut self,
        section: Section,
        depth: i32,
        parent: usize,
        offset: Option<P>,
        pieces: &mut Vec<usize>,
    ) -> bool {
        if depth > 8 {
            return false;
        }
        let mut list = Vec::new();
        if self.section(section, depth, parent, offset, &mut list) != Some(true) {
            return false;
        }
        let gen_depth = self.r.next_int();
        let parent_depth = self.parts[parent].depth;
        let mut blocked = false;
        for &i in &list {
            self.parts[i].depth = gen_depth;
            let b = self.parts[i].bbox;
            if let Some(&c) = pieces.iter().find(|&&c| self.parts[c].bbox.intersects(&b))
                && self.parts[c].depth != parent_depth
            {
                blocked = true;
                break;
            }
        }
        if blocked {
            return false;
        }
        pieces.extend(list);
        true
    }

    fn section(
        &mut self,
        section: Section,
        depth: i32,
        parent: usize,
        offset: Option<P>,
        list: &mut Vec<usize>,
    ) -> Option<bool> {
        let rot = self.parts[parent].rot;
        match section {
            Section::HouseTower => {
                if depth > 8 {
                    return Some(false);
                }
                let base = offset.unwrap_or_default();
                let mut p = self.add(list, parent, base, "base_floor", rot, true)?;
                match self.r.next_int_bound(3) {
                    0 => {
                        self.add(list, p, P::new(-1, 4, -1), "base_roof", rot, true)?;
                    }
                    1 => {
                        p = self.add(list, p, P::new(-1, 0, -1), "second_floor_2", rot, false)?;
                        p = self.add(list, p, P::new(-1, 8, -1), "second_roof", rot, false)?;
                        self.recurse(Section::Tower, depth + 1, p, None, list);
                    }
                    _ => {
                        p = self.add(list, p, P::new(-1, 0, -1), "second_floor_2", rot, false)?;
                        p = self.add(list, p, P::new(-1, 4, -1), "third_floor_2", rot, false)?;
                        p = self.add(list, p, P::new(-1, 8, -1), "third_roof", rot, true)?;
                        self.recurse(Section::Tower, depth + 1, p, None, list);
                    }
                }
                Some(true)
            }
            Section::Tower => {
                let (bx, bz) = (3 + self.r.next_int_bound(2), 3 + self.r.next_int_bound(2));
                let mut p = self.add(list, parent, P::new(bx, -3, bz), "tower_base", rot, true)?;
                p = self.add(list, p, P::new(0, 7, 0), "tower_piece", rot, true)?;
                let mut bridge = (self.r.next_int_bound(3) == 0).then_some(p);
                let n = 1 + self.r.next_int_bound(3);
                for j in 0..n {
                    p = self.add(list, p, P::new(0, 4, 0), "tower_piece", rot, true)?;
                    if j < n - 1 && self.r.next_bool() {
                        bridge = Some(p);
                    }
                }
                if let Some(b) = bridge {
                    for (turn_by, at) in TOWER_BRIDGES {
                        if self.r.next_bool() {
                            let e =
                                self.add(list, b, at, "bridge_end", turn(rot, turn_by), true)?;
                            self.recurse(Section::TowerBridge, depth + 1, e, None, list);
                        }
                    }
                } else if depth != 7 {
                    return Some(self.recurse(Section::FatTower, depth + 1, p, None, list));
                }
                self.add(list, p, P::new(-1, 4, -1), "tower_top", rot, true)?;
                Some(true)
            }
            Section::TowerBridge => {
                let n = self.r.next_int_bound(4) + 1;
                let mut p = self.add(list, parent, P::new(0, 0, -4), "bridge_piece", rot, true)?;
                self.parts[p].depth = -1;
                let mut y = 0;
                for _ in 0..n {
                    if self.r.next_bool() {
                        p = self.add(list, p, P::new(0, y, -4), "bridge_piece", rot, true)?;
                        y = 0;
                    } else {
                        p = if self.r.next_bool() {
                            self.add(list, p, P::new(0, y, -4), "bridge_steep_stairs", rot, true)?
                        } else {
                            self.add(list, p, P::new(0, y, -8), "bridge_gentle_stairs", rot, true)?
                        };
                        y = 4;
                    }
                }
                if !self.ship && self.r.next_int_bound(10 - depth) == 0 {
                    let (sx, sz) = (
                        -8 + self.r.next_int_bound(8),
                        -70 + self.r.next_int_bound(10),
                    );
                    self.add(list, p, P::new(sx, y, sz), "ship", rot, true)?;
                    self.ship = true;
                } else if !self.recurse(
                    Section::HouseTower,
                    depth + 1,
                    p,
                    Some(P::new(-3, y + 1, -11)),
                    list,
                ) {
                    return Some(false);
                }
                let e = self.add(list, p, P::new(4, y, 0), "bridge_end", turn(rot, 2), true)?;
                self.parts[e].depth = -1;
                Some(true)
            }
            Section::FatTower => {
                let mut p =
                    self.add(list, parent, P::new(-3, 4, -3), "fat_tower_base", rot, true)?;
                p = self.add(list, p, P::new(0, 4, 0), "fat_tower_middle", rot, true)?;
                let mut i = 0;
                while i < 2 && self.r.next_int_bound(3) != 0 {
                    p = self.add(list, p, P::new(0, 8, 0), "fat_tower_middle", rot, true)?;
                    for (turn_by, at) in FAT_TOWER_BRIDGES {
                        if self.r.next_bool() {
                            let e =
                                self.add(list, p, at, "bridge_end", turn(rot, turn_by), true)?;
                            self.recurse(Section::TowerBridge, depth + 1, e, None, list);
                        }
                    }
                    i += 1;
                }
                self.add(list, p, P::new(-2, 8, -2), "fat_tower_top", rot, true)?;
                Some(true)
            }
        }
    }
}

/// The pieces of an End city starting in chunk `x, z`: on the lowest of
/// four corners of a 5 × 5 square next to the chunk's middle, none below 60.
pub fn generate(
    s: &Structures,
    r: &mut WorldgenRandom,
    x: i32,
    z: i32,
    h: Heights,
    ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    let rot = Rotation(r.next_int_bound(4) as u8);
    let (i, j) = match rot.0 {
        1 => (-5, 5),
        2 => (-5, -5),
        3 => (5, -5),
        _ => (5, 5),
    };
    let (sx, sz) = (x * 16 + 7, z * 16 + 7);
    let y = [(sx, sz), (sx, sz + j), (sx + i, sz), (sx + i, sz + j)]
        .into_iter()
        .map(|(x, z)| ground.height(x, z, false) - 1)
        .min()?;
    if y < 60 {
        return None;
    }
    let at = P::new(sx, y, sz);
    let mut g = Gen {
        s,
        r,
        parts: Vec::new(),
        ship: false,
    };
    let mut pieces = Vec::new();
    let mut p = g.make("base_floor", at, rot, true)?;
    pieces.push(p);
    p = g.add(
        &mut pieces,
        p,
        P::new(-1, 0, -1),
        "second_floor_1",
        rot,
        false,
    )?;
    p = g.add(
        &mut pieces,
        p,
        P::new(-1, 4, -1),
        "third_floor_1",
        rot,
        false,
    )?;
    p = g.add(&mut pieces, p, P::new(-1, 8, -1), "third_roof", rot, true)?;
    g.recurse(Section::Tower, 1, p, None, &mut pieces);
    let out = pieces
        .iter()
        .filter_map(|&i| {
            let part = &g.parts[i];
            let settings = Settings {
                rotation: part.rot,
                processors: vec![std::sync::Arc::new(super::template::Processor::Ignore {
                    air: !part.overwrite,
                })],
                ..Settings::default()
            };
            template_piece(
                s,
                &format!("minecraft:end_city/{}", part.name),
                part.pos,
                settings,
                Post::EndCity,
                h,
            )
        })
        .collect();
    Some((out, at))
}
