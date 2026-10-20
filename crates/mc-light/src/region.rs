//! [`light_region`](crate::light_region) for one new chunk and the 3×3
//! chunks around it, on flat arrays: block facts and light are unpacked once,
//! light spreads over plain indices, and changed sections are packed back.
//! The light (and which sections get stored arrays) is the same as lighting
//! the chunks through a [`LightWorld`](crate::LightWorld) view of them.

use std::collections::VecDeque;

use mc_world::{Chunk, SECTION_VOLUME};

use crate::{Kind, Tables};

/// Cells across: 3 chunks and a border on each side.
const SIDE: usize = 50;
const LAYER: usize = SIDE * SIDE;
/// Marks a cell outside the loaded chunks or the world.
const ABSENT: u16 = 1 << 15;

/// Neighbour offsets in [`crate::NEIGHBOURS`] order.
const STEPS: [isize; 6] = [
    -(LAYER as isize),
    LAYER as isize,
    -(SIDE as isize),
    SIDE as isize,
    -1,
    1,
];

/// Reusable buffers (a few MB at 384 blocks high).
#[derive(Debug, Default)]
pub struct Scratch {
    /// Per cell: opacity (bits 0-3), stopping faces (4-9), emission (10-13),
    /// or [`ABSENT`].
    props: Vec<u16>,
    light: [Vec<u8>; 2],
    /// Per chunk, kind and section: whether a light value there changed.
    dirty: Vec<bool>,
    queue: VecDeque<u32>,
    /// Section-sized buffer.
    cells: Vec<u16>,
}

fn opacity(p: u16) -> u8 {
    (p & 15) as u8
}

fn faces(p: u16) -> u16 {
    (p >> 4) & 63
}

/// Light that reaches a neighbour with props `to` from a cell with `level`
/// and props `from`, moving in direction `d`.
fn passed(kind: Kind, level: u8, from: u16, to: u16, d: usize) -> u8 {
    if faces(from) & (1 << d) != 0 || faces(to) & (1 << (d ^ 1)) != 0 {
        return 0;
    }
    let opacity = opacity(to);
    if kind == Kind::Sky && level == 15 && d == 0 && opacity == 0 {
        return 15;
    }
    level.saturating_sub(opacity.max(1))
}

/// Whether a chunk's stored sky light reads the same with every missing
/// array as open sky: no missing array below a stored one.
fn sky_from_top(chunk: &Chunk) -> bool {
    let stored = |s: usize| chunk.light.get(s).is_some_and(|l| l.sky.is_some());
    let mut seen = false;
    for s in (0..chunk.sections.len()).rev() {
        if stored(s) {
            seen = true;
        } else if seen {
            return false;
        }
    }
    true
}

impl Scratch {
    fn index(x: usize, y: usize, z: usize) -> usize {
        (y * SIDE + z) * SIDE + x
    }

    fn sections(&self) -> usize {
        self.props.len() / LAYER / 16
    }

    fn set(&mut self, kind: Kind, i: usize, level: u8) {
        self.light[kind as usize][i] = level;
        let y = i / LAYER - 1;
        let z = (i % LAYER) / SIDE - 1;
        let x = i % SIDE - 1;
        let chunk = (z / 16) * 3 + x / 16;
        let sections = self.sections();
        self.dirty[(chunk * 2 + kind as usize) * sections + y / 16] = true;
    }

    fn spread(&mut self, kind: Kind) {
        let mut queue = std::mem::take(&mut self.queue);
        while let Some(p) = queue.pop_front() {
            let p = p as usize;
            let level = self.light[kind as usize][p];
            if level <= 1 {
                continue;
            }
            let from = self.props[p];
            for (d, step) in STEPS.into_iter().enumerate() {
                let n = p.wrapping_add_signed(step);
                let to = self.props[n];
                if to & ABSENT != 0 {
                    continue;
                }
                let value = passed(kind, level, from, to, d);
                if value > self.light[kind as usize][n] {
                    self.set(kind, n, value);
                    queue.push_back(n as u32);
                }
            }
        }
        self.queue = queue;
    }

    /// Unpacks a chunk's blocks and light into its third of the grid.
    fn load(&mut self, tables: &Tables, chunk: &Chunk, at: usize) {
        let (x0, z0) = (1 + (at % 3) * 16, 1 + (at / 3) * 16);
        self.cells.resize(SECTION_VOLUME, 0);
        for (s, section) in chunk.sections.iter().enumerate() {
            section.blocks.map_into(&mut self.cells, |state| {
                u16::from(tables.opacity(state).min(15))
                    | u16::from(tables.faces(state) & 63) << 4
                    | u16::from(tables.emission(state).min(15)) << 10
            });
            for ly in 0..16 {
                for lz in 0..16 {
                    let from = (ly << 8) | (lz << 4);
                    let to = Self::index(x0, 1 + s * 16 + ly, z0 + lz);
                    self.props[to..to + 16].copy_from_slice(&self.cells[from..from + 16]);
                }
            }
            let stored = chunk.light.get(s);
            for (kind, array) in [
                (Kind::Sky, stored.and_then(|l| l.sky.as_deref())),
                (Kind::Block, stored.and_then(|l| l.block.as_deref())),
            ] {
                let light = &mut self.light[kind as usize];
                for ly in 0..16 {
                    for lz in 0..16 {
                        let to = Self::index(x0, 1 + s * 16 + ly, z0 + lz);
                        let row = &mut light[to..to + 16];
                        match array {
                            Some(a) => {
                                let from = ((ly << 8) | (lz << 4)) / 2;
                                for (pair, &byte) in row.chunks_exact_mut(2).zip(&a[from..from + 8])
                                {
                                    pair[0] = byte & 15;
                                    pair[1] = byte >> 4;
                                }
                            }
                            None => row.fill(if kind == Kind::Sky { 15 } else { 0 }),
                        }
                    }
                }
            }
        }
    }

    fn pack(&self, kind: Kind, at: usize, s: usize, array: &mut Vec<u8>) {
        let (x0, z0) = (1 + (at % 3) * 16, 1 + (at / 3) * 16);
        array.resize(SECTION_VOLUME / 2, 0);
        let light = &self.light[kind as usize];
        for ly in 0..16 {
            for lz in 0..16 {
                let from = Self::index(x0, 1 + s * 16 + ly, z0 + lz);
                let to = ((ly << 8) | (lz << 4)) / 2;
                for (byte, pair) in array[to..to + 8]
                    .iter_mut()
                    .zip(light[from..from + 16].chunks_exact(2))
                {
                    *byte = pair[0] | (pair[1] << 4);
                }
            }
        }
    }

    /// Writes changed light back; whether anything in the chunk changed.
    fn store(&self, chunk: &mut Chunk, at: usize, center: bool) -> bool {
        let sections = self.sections();
        let dirty = |kind: Kind, s: usize| self.dirty[(at * 2 + kind as usize) * sections + s];
        let touched = (0..sections).any(|s| dirty(Kind::Sky, s) || dirty(Kind::Block, s));
        if !touched && !center {
            return false;
        }
        if chunk.light.len() < sections {
            chunk
                .light
                .resize(sections, mc_world::SectionLight::default());
        }
        // A missing sky array reads as open sky; storing one stores those
        // below it too (with what they read).
        if let Some(top) = (0..sections).rev().find(|&s| dirty(Kind::Sky, s)) {
            for s in 0..=top {
                let light = &mut chunk.light[s];
                if light.sky.is_none() || dirty(Kind::Sky, s) {
                    self.pack(Kind::Sky, at, s, light.sky.get_or_insert_default());
                }
            }
        }
        for s in (0..sections).filter(|&s| dirty(Kind::Block, s)) {
            self.pack(
                Kind::Block,
                at,
                s,
                chunk.light[s].block.get_or_insert_default(),
            );
        }
        touched
    }
}

/// Lights the chunk `chunks[4]` from scratch with the chunks around it
/// (`chunks[(dz + 1) * 3 + dx + 1]`, `None` where not loaded): sky light
/// falls down its columns, block light starts at emitting blocks, both
/// spread into and from the neighbours. Returns which chunks' light changed,
/// or `None` (and changes nothing) when a chunk's stored light cannot be
/// read this way (a missing sky array below a stored one, or a different
/// height).
pub fn light_new_chunk(
    scratch: &mut Scratch,
    tables: &Tables,
    chunks: &mut [Option<&mut Chunk>; 9],
) -> Option<[bool; 9]> {
    let sections = chunks[4].as_ref()?.sections.len();
    if chunks
        .iter()
        .flatten()
        .any(|c| c.sections.len() != sections || !sky_from_top(c))
    {
        return None;
    }
    let height = sections * 16;
    let s = scratch;
    s.props.clear();
    s.props.resize(LAYER * (height + 2), ABSENT);
    for light in &mut s.light {
        light.resize(s.props.len(), 0);
    }
    s.dirty.clear();
    s.dirty.resize(9 * 2 * sections, false);
    for (at, chunk) in chunks.iter().enumerate() {
        if let Some(chunk) = chunk {
            s.load(tables, chunk, at);
        }
    }
    // Sky down the new columns, block light from what emits there.
    let mut block = VecDeque::new();
    s.queue.clear();
    for z in 17..33 {
        for x in 17..33 {
            let mut level = 15u8;
            let mut above: Option<u16> = None;
            for y in (1..=height).rev() {
                let i = Scratch::index(x, y, z);
                let p = s.props[i];
                let open = above.is_none_or(|a| faces(a) & 1 == 0 && faces(p) & 2 == 0);
                if !(level == 15 && open && opacity(p) == 0) {
                    level = 0;
                }
                above = Some(p);
                if s.light[Kind::Sky as usize][i] != level {
                    s.set(Kind::Sky, i, level);
                }
                if level == 15 {
                    s.queue.push_back(i as u32);
                }
                let emission = ((p >> 10) & 15) as u8;
                if s.light[Kind::Block as usize][i] != emission {
                    s.set(Kind::Block, i, emission);
                }
                if emission > 0 {
                    block.push_back(i as u32);
                }
            }
        }
    }
    // Light from around comes in too.
    for z in 16..34 {
        for x in 16..34 {
            if (17..33).contains(&x) && (17..33).contains(&z) {
                continue;
            }
            for y in 1..=height {
                let i = Scratch::index(x, y, z);
                if s.props[i] & ABSENT != 0 {
                    continue;
                }
                if s.light[Kind::Sky as usize][i] > 1 {
                    s.queue.push_back(i as u32);
                }
                if s.light[Kind::Block as usize][i] > 1 {
                    block.push_back(i as u32);
                }
            }
        }
    }
    s.spread(Kind::Sky);
    s.queue = block;
    s.spread(Kind::Block);
    let mut touched = [false; 9];
    for (at, chunk) in chunks.iter_mut().enumerate() {
        if let Some(chunk) = chunk {
            touched[at] = s.store(chunk, at, at == 4);
        }
    }
    Some(touched)
}
