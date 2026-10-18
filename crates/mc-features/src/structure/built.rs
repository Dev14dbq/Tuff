//! Structures built block by block in their own frame (minecraft.wiki:
//! "Swamp Hut", "Desert Pyramid", "Jungle Pyramid"): x across, z from the
//! front, y up from the ground; the frame turned to the structure's facing.

use mc_nbt::{Compound, Tag};
use mc_tick::Dir;
use mc_worldgen::random::RandomSource;

use super::legacy::{Built, BuiltKind};
use super::template::{BBox, Mirror, Rotation, transform_state};
use crate::blocks::Facts;
use crate::rand::WorldgenRandom;
use crate::{Heightmap, P, Region};

/// A piece's frame: its box and facing.
#[allow(missing_debug_implementations)]
pub struct Frame<'a> {
    pub facts: &'a Facts,
    pub bbox: BBox,
    pub facing: Dir,
    pub chunk: BBox,
}

impl Frame<'_> {
    pub fn world(&self, x: i32, y: i32, z: i32) -> P {
        let b = &self.bbox;
        let wx = match self.facing {
            Dir::North | Dir::South => b.min.x + x,
            Dir::West => b.max.x - z,
            Dir::East => b.min.x + z,
            _ => x,
        };
        let wz = match self.facing {
            Dir::North => b.max.z - z,
            Dir::South => b.min.z + z,
            Dir::West | Dir::East => b.min.z + x,
            _ => z,
        };
        P::new(wx, b.min.y + y, wz)
    }

    fn turn(&self) -> (Mirror, Rotation) {
        match self.facing {
            Dir::South => (Mirror::LeftRight, Rotation(0)),
            Dir::West => (Mirror::LeftRight, Rotation(1)),
            Dir::East => (Mirror::None, Rotation(1)),
            _ => (Mirror::None, Rotation(0)),
        }
    }

    /// The block there; air outside the chunk being placed, as pieces see
    /// it (so a wall that skips air draws no random number there either).
    pub fn get(&self, region: &dyn Region, x: i32, y: i32, z: i32) -> u32 {
        let p = self.world(x, y, z);
        if self.chunk.inside(p) {
            region.block(p)
        } else {
            self.facts.air
        }
    }

    pub fn set(&self, region: &mut dyn Region, state: u32, x: i32, y: i32, z: i32) {
        let p = self.world(x, y, z);
        if !self.chunk.inside(p) {
            return;
        }
        let (m, r) = self.turn();
        region.set(p, transform_state(self.facts, state, m, r));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill(
        &self,
        region: &mut dyn Region,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: u32,
        inner: u32,
        keep_air: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if keep_air && self.facts.is_air(self.get(region, x, y, z)) {
                        continue;
                    }
                    let on_edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    self.set(region, if on_edge { edge } else { inner }, x, y, z);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fill_with(
        &self,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        pick: &dyn Fn(&mut WorldgenRandom, bool) -> u32,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let on_edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    let s = pick(r, on_edge);
                    self.set(region, s, x, y, z);
                }
            }
        }
    }

    /// Fills down from `y` while there is air or fluid.
    pub fn column_down(&self, region: &mut dyn Region, state: u32, x: i32, y: i32, z: i32) {
        let mut p = self.world(x, y, z);
        if !self.chunk.inside(p) {
            return;
        }
        let min = region.heights().min_y;
        while p.y > min + 1 {
            let s = region.block(p);
            if !self.facts.is_air(s) && self.facts.fluid(s).is_none() {
                break;
            }
            region.set(p, state);
            p = p.below(1);
        }
    }

    /// A chest with loot, facing away from the wall it stands at.
    pub fn chest(
        &self,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        x: i32,
        y: i32,
        z: i32,
        table: &str,
    ) {
        let p = self.world(x, y, z);
        if !self.chunk.inside(p) || self.facts.is(region.block(p), "minecraft:chest") {
            return;
        }
        let Some(chest) = self.facts.default_state("minecraft:chest") else {
            return;
        };
        let facing = reorient(self.facts, region, p);
        region.set(p, self.facts.with(chest, "facing", facing.name()));
        let mut be = Compound::new();
        be.insert("id", Tag::String("minecraft:chest".into()));
        be.insert("LootTable", Tag::String(table.into()));
        be.insert("LootTableSeed", Tag::Long(r.next_long()));
        region.set_block_entity(p, be);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn dispenser(
        &self,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        x: i32,
        y: i32,
        z: i32,
        facing: Dir,
        table: &str,
    ) {
        let p = self.world(x, y, z);
        if !self.chunk.inside(p) {
            return;
        }
        let Some(d) = self.facts.default_state("minecraft:dispenser") else {
            return;
        };
        region.set(p, self.facts.with(d, "facing", facing.name()));
        let mut be = Compound::new();
        be.insert("id", Tag::String("minecraft:dispenser".into()));
        be.insert("LootTable", Tag::String(table.into()));
        be.insert("LootTableSeed", Tag::Long(r.next_long()));
        region.set_block_entity(p, be);
    }
}

/// Which way a chest at `p` faces: away from its one solid side.
pub fn reorient(facts: &Facts, region: &dyn Region, p: P) -> Dir {
    let mut dir: Option<Dir> = None;
    for d in [Dir::North, Dir::East, Dir::South, Dir::West] {
        let s = region.block(p.rel(d));
        if facts.is(s, "minecraft:chest") {
            return Dir::North;
        }
        if !facts.solid_render(s) {
            continue;
        }
        if dir.is_some() {
            dir = None;
            break;
        }
        dir = Some(d);
    }
    if let Some(d) = dir {
        return d.opposite();
    }
    let mut e = Dir::North;
    if facts.solid_render(region.block(p.rel(e))) {
        e = e.opposite();
    }
    if facts.solid_render(region.block(p.rel(e))) {
        e = e.cw();
    }
    if facts.solid_render(region.block(p.rel(e))) {
        e = e.opposite();
    }
    e
}

fn st(facts: &Facts, name: &str, props: &[(&str, &str)]) -> u32 {
    facts
        .state(&format!("minecraft:{name}"), props)
        .or_else(|| facts.default_state(&format!("minecraft:{name}")))
        .unwrap_or(facts.air)
}

pub fn place(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    b: &Built,
    chunk: BBox,
) {
    let mut bbox = b.bbox;
    // Ground: the average top of the columns inside this chunk (when the
    // first chunk places it), as the game does.
    if b.kind != BuiltKind::DesertPyramid {
        let y = *b.ground.get_or_init(|| {
            let (mut sum, mut n) = (0, 0);
            for z in bbox.min.z..=bbox.max.z {
                for x in bbox.min.x..=bbox.max.x {
                    if chunk.inside(P::new(x, chunk.min.y.max(64).min(chunk.max.y), z)) {
                        sum += region.height(Heightmap::MotionBlockingNoLeaves, x, z);
                        n += 1;
                    }
                }
            }
            if n == 0 { bbox.min.y } else { sum / n }
        });
        bbox = bbox.moved(0, y - bbox.min.y, 0);
    } else {
        let y = *b.ground.get_or_init(|| {
            // On the lowest of its corners.
            let corners = [
                (bbox.min.x, bbox.min.z),
                (bbox.max.x, bbox.min.z),
                (bbox.min.x, bbox.max.z),
                (bbox.max.x, bbox.max.z),
            ];
            corners
                .iter()
                .map(|&(x, z)| region.height(Heightmap::WorldSurfaceWg, x, z))
                .min()
                .unwrap_or(64)
                - 1
        });
        bbox = bbox.moved(0, y - bbox.min.y, 0);
    }
    let f = Frame {
        facts,
        bbox,
        facing: b.facing,
        chunk,
    };
    match b.kind {
        BuiltKind::SwampHut => swamp_hut(&f, region),
        BuiltKind::DesertPyramid => desert_pyramid(&f, region, r),
        BuiltKind::JunglePyramid => jungle_pyramid(&f, region, r),
    }
}

fn swamp_hut(f: &Frame, region: &mut dyn Region) {
    let facts = f.facts;
    let planks = st(facts, "spruce_planks", &[]);
    let log = st(facts, "oak_log", &[]);
    let fence = st(facts, "oak_fence", &[]);
    let air = facts.air;
    f.fill(region, 1, 1, 1, 5, 1, 7, planks, planks, false);
    f.fill(region, 1, 4, 2, 5, 4, 7, planks, planks, false);
    f.fill(region, 2, 1, 0, 4, 1, 0, planks, planks, false);
    f.fill(region, 2, 2, 2, 3, 3, 2, planks, planks, false);
    f.fill(region, 1, 2, 3, 1, 3, 6, planks, planks, false);
    f.fill(region, 5, 2, 3, 5, 3, 6, planks, planks, false);
    f.fill(region, 2, 2, 7, 4, 3, 7, planks, planks, false);
    f.fill(region, 1, 0, 2, 1, 3, 2, log, log, false);
    f.fill(region, 5, 0, 2, 5, 3, 2, log, log, false);
    f.fill(region, 1, 0, 7, 1, 3, 7, log, log, false);
    f.fill(region, 5, 0, 7, 5, 3, 7, log, log, false);
    f.set(region, fence, 2, 3, 2);
    f.set(region, fence, 3, 3, 7);
    f.set(region, air, 1, 3, 4);
    f.set(region, air, 5, 3, 4);
    f.set(region, air, 5, 3, 5);
    f.set(region, st(facts, "potted_red_mushroom", &[]), 1, 3, 5);
    f.set(region, st(facts, "crafting_table", &[]), 3, 2, 6);
    f.set(region, st(facts, "cauldron", &[]), 4, 2, 6);
    f.set(region, fence, 1, 2, 1);
    f.set(region, fence, 5, 2, 1);
    let stairs =
        |d: &str, shape: &str| st(facts, "spruce_stairs", &[("facing", d), ("shape", shape)]);
    let (n, e, w, s) = (
        stairs("north", "straight"),
        stairs("east", "straight"),
        stairs("west", "straight"),
        stairs("south", "straight"),
    );
    f.fill(region, 0, 4, 1, 6, 4, 1, n, n, false);
    f.fill(region, 0, 4, 2, 0, 4, 7, e, e, false);
    f.fill(region, 6, 4, 2, 6, 4, 7, w, w, false);
    f.fill(region, 0, 4, 8, 6, 4, 8, s, s, false);
    f.set(region, stairs("north", "outer_right"), 0, 4, 1);
    f.set(region, stairs("north", "outer_left"), 6, 4, 1);
    f.set(region, stairs("south", "outer_left"), 0, 4, 8);
    f.set(region, stairs("south", "outer_right"), 6, 4, 8);
    for z in [2, 7] {
        for x in [1, 5] {
            f.column_down(region, log, x, -1, z);
        }
    }
}

fn desert_pyramid(f: &Frame, region: &mut dyn Region, r: &mut WorldgenRandom) {
    let facts = f.facts;
    let (w, d) = (21, 21);
    let sandstone = st(facts, "sandstone", &[]);
    let cut = st(facts, "cut_sandstone", &[]);
    let chiseled = st(facts, "chiseled_sandstone", &[]);
    let orange = st(facts, "orange_terracotta", &[]);
    let blue = st(facts, "blue_terracotta", &[]);
    let air = facts.air;
    let stairs = |dir: &str| st(facts, "sandstone_stairs", &[("facing", dir)]);
    let (sn, ss, se, sw) = (
        stairs("north"),
        stairs("south"),
        stairs("east"),
        stairs("west"),
    );
    f.fill(
        region,
        0,
        -4,
        0,
        w - 1,
        0,
        d - 1,
        sandstone,
        sandstone,
        false,
    );
    for i in 1..=9 {
        f.fill(
            region,
            i,
            i,
            i,
            w - 1 - i,
            i,
            d - 1 - i,
            sandstone,
            sandstone,
            false,
        );
        f.fill(
            region,
            i + 1,
            i,
            i + 1,
            w - 2 - i,
            i,
            d - 2 - i,
            air,
            air,
            false,
        );
    }
    for x in 0..w {
        for z in 0..d {
            f.column_down(region, sandstone, x, -5, z);
        }
    }
    f.fill(region, 0, 0, 0, 4, 9, 4, sandstone, air, false);
    f.fill(region, 1, 10, 1, 3, 10, 3, sandstone, sandstone, false);
    f.set(region, sn, 2, 10, 0);
    f.set(region, ss, 2, 10, 4);
    f.set(region, se, 0, 10, 2);
    f.set(region, sw, 4, 10, 2);
    f.fill(region, w - 5, 0, 0, w - 1, 9, 4, sandstone, air, false);
    f.fill(
        region,
        w - 4,
        10,
        1,
        w - 2,
        10,
        3,
        sandstone,
        sandstone,
        false,
    );
    f.set(region, sn, w - 3, 10, 0);
    f.set(region, ss, w - 3, 10, 4);
    f.set(region, se, w - 5, 10, 2);
    f.set(region, sw, w - 1, 10, 2);
    f.fill(region, 8, 0, 0, 12, 4, 4, sandstone, air, false);
    f.fill(region, 9, 1, 0, 11, 3, 4, air, air, false);
    for (x, y) in [(9, 1), (9, 2), (9, 3), (10, 3), (11, 3), (11, 2), (11, 1)] {
        f.set(region, cut, x, y, 1);
    }
    f.fill(region, 4, 1, 1, 8, 3, 3, sandstone, air, false);
    f.fill(region, 4, 1, 2, 8, 2, 2, air, air, false);
    f.fill(region, 12, 1, 1, 16, 3, 3, sandstone, air, false);
    f.fill(region, 12, 1, 2, 16, 2, 2, air, air, false);
    f.fill(
        region,
        5,
        4,
        5,
        w - 6,
        4,
        d - 6,
        sandstone,
        sandstone,
        false,
    );
    f.fill(region, 9, 4, 9, 11, 4, 11, air, air, false);
    for (x, z) in [(8, 8), (12, 8), (8, 12), (12, 12)] {
        f.fill(region, x, 1, z, x, 3, z, cut, cut, false);
    }
    f.fill(region, 1, 1, 5, 4, 4, 11, sandstone, sandstone, false);
    f.fill(
        region,
        w - 5,
        1,
        5,
        w - 2,
        4,
        11,
        sandstone,
        sandstone,
        false,
    );
    f.fill(region, 6, 7, 9, 6, 7, 11, sandstone, sandstone, false);
    f.fill(
        region,
        w - 7,
        7,
        9,
        w - 7,
        7,
        11,
        sandstone,
        sandstone,
        false,
    );
    f.fill(region, 5, 5, 9, 5, 7, 11, cut, cut, false);
    f.fill(region, w - 6, 5, 9, w - 6, 7, 11, cut, cut, false);
    for (x, y) in [(5, 5), (5, 6), (6, 6), (w - 6, 5), (w - 6, 6), (w - 7, 6)] {
        f.set(region, air, x, y, 10);
    }
    f.fill(region, 2, 4, 4, 2, 6, 4, air, air, false);
    f.fill(region, w - 3, 4, 4, w - 3, 6, 4, air, air, false);
    f.set(region, sn, 2, 4, 5);
    f.set(region, sn, 2, 3, 4);
    f.set(region, sn, w - 3, 4, 5);
    f.set(region, sn, w - 3, 3, 4);
    f.fill(region, 1, 1, 3, 2, 2, 3, sandstone, sandstone, false);
    f.fill(
        region,
        w - 3,
        1,
        3,
        w - 2,
        2,
        3,
        sandstone,
        sandstone,
        false,
    );
    f.set(region, sandstone, 1, 1, 2);
    f.set(region, sandstone, w - 2, 1, 2);
    let slab = st(facts, "sandstone_slab", &[]);
    f.set(region, slab, 1, 2, 2);
    f.set(region, slab, w - 2, 2, 2);
    f.set(region, sw, 2, 1, 2);
    f.set(region, se, w - 3, 1, 2);
    f.fill(region, 4, 3, 5, 4, 3, 17, sandstone, sandstone, false);
    f.fill(
        region,
        w - 5,
        3,
        5,
        w - 5,
        3,
        17,
        sandstone,
        sandstone,
        false,
    );
    f.fill(region, 3, 1, 5, 4, 2, 16, air, air, false);
    f.fill(region, w - 6, 1, 5, w - 5, 2, 16, air, air, false);
    let mut l = 5;
    while l <= 17 {
        f.set(region, cut, 4, 1, l);
        f.set(region, chiseled, 4, 2, l);
        f.set(region, cut, w - 5, 1, l);
        f.set(region, chiseled, w - 5, 2, l);
        l += 2;
    }
    for (x, z) in [
        (10, 7),
        (10, 8),
        (9, 9),
        (11, 9),
        (8, 10),
        (12, 10),
        (7, 10),
        (13, 10),
        (9, 11),
        (11, 11),
        (10, 12),
        (10, 13),
    ] {
        f.set(region, orange, x, 0, z);
    }
    f.set(region, blue, 10, 0, 10);
    for l in [0, w - 1] {
        let col = [
            (2, 1, cut),
            (2, 2, orange),
            (2, 3, cut),
            (3, 1, cut),
            (3, 2, orange),
            (3, 3, cut),
            (4, 1, orange),
            (4, 2, chiseled),
            (4, 3, orange),
            (5, 1, cut),
            (5, 2, orange),
            (5, 3, cut),
            (6, 1, orange),
            (6, 2, chiseled),
            (6, 3, orange),
            (7, 1, orange),
            (7, 2, orange),
            (7, 3, orange),
            (8, 1, cut),
            (8, 2, cut),
            (8, 3, cut),
        ];
        for (y, z, s) in col {
            f.set(region, s, l, y, z);
        }
    }
    for l in [2, w - 3] {
        let col = [
            (-1, 2, cut),
            (0, 2, orange),
            (1, 2, cut),
            (-1, 3, cut),
            (0, 3, orange),
            (1, 3, cut),
            (-1, 4, orange),
            (0, 4, chiseled),
            (1, 4, orange),
            (-1, 5, cut),
            (0, 5, orange),
            (1, 5, cut),
            (-1, 6, orange),
            (0, 6, chiseled),
            (1, 6, orange),
            (-1, 7, orange),
            (0, 7, orange),
            (1, 7, orange),
            (-1, 8, cut),
            (0, 8, cut),
            (1, 8, cut),
        ];
        for (dx, y, s) in col {
            f.set(region, s, l + dx, y, 0);
        }
    }
    f.fill(region, 8, 4, 0, 12, 6, 0, cut, cut, false);
    f.set(region, air, 8, 6, 0);
    f.set(region, air, 12, 6, 0);
    f.set(region, orange, 9, 5, 0);
    f.set(region, chiseled, 10, 5, 0);
    f.set(region, orange, 11, 5, 0);
    // The treasure room with its trap.
    f.fill(region, 8, -14, 8, 12, -11, 12, cut, cut, false);
    f.fill(region, 8, -10, 8, 12, -10, 12, chiseled, chiseled, false);
    f.fill(region, 8, -9, 8, 12, -9, 12, cut, cut, false);
    f.fill(region, 8, -8, 8, 12, -1, 12, sandstone, sandstone, false);
    f.fill(region, 9, -11, 9, 11, -1, 11, air, air, false);
    f.set(region, st(facts, "stone_pressure_plate", &[]), 10, -11, 10);
    let tnt = st(facts, "tnt", &[]);
    f.fill(region, 9, -13, 9, 11, -13, 11, tnt, air, false);
    for (x, z, cx, cz) in [
        (8, 10, 7, 10),
        (12, 10, 13, 10),
        (10, 8, 10, 7),
        (10, 12, 10, 13),
    ] {
        f.set(region, air, x, -11, z);
        f.set(region, air, x, -10, z);
        f.set(region, chiseled, cx, -10, cz);
        f.set(region, cut, cx, -11, cz);
    }
    for (dx, dz) in [(0, -2), (2, 0), (0, 2), (-2, 0)] {
        f.chest(
            region,
            r,
            10 + dx,
            -11,
            10 + dz,
            "minecraft:chests/desert_pyramid",
        );
    }
    // The cellar under the north-east corner, with suspicious sand.
    let sand = st(facts, "sand", &[]);
    f.fill(region, 13, -6, 13, 17, -2, 17, sandstone, air, false);
    f.fill(region, 14, -5, 14, 16, -3, 16, air, air, false);
    for i in 0..3 {
        f.set(
            region,
            st(facts, "sandstone_stairs", &[("facing", "west")]),
            13 + i,
            -1 - i,
            17,
        );
    }
    for (x, z) in [(14, 14), (15, 14), (16, 14), (14, 15), (16, 16), (15, 16)] {
        f.set(region, sand, x, -5, z);
    }
    if let Some(sus) = facts.default_state("minecraft:suspicious_sand") {
        for (x, z) in [(15, 15), (14, 16)] {
            let p = f.world(x, -5, z);
            if f.chunk.inside(p) {
                region.set(p, sus);
                let mut be = Compound::new();
                be.insert("id", Tag::String("minecraft:brushable_block".into()));
                be.insert(
                    "LootTable",
                    Tag::String("minecraft:archaeology/desert_pyramid".into()),
                );
                be.insert("LootTableSeed", Tag::Long(r.next_long()));
                region.set_block_entity(p, be);
            }
        }
    }
}

fn jungle_pyramid(f: &Frame, region: &mut dyn Region, r: &mut WorldgenRandom) {
    let facts = f.facts;
    let cobble = st(facts, "cobblestone", &[]);
    let mossy = st(facts, "mossy_cobblestone", &[]);
    let air = facts.air;
    let pick = move |r: &mut WorldgenRandom, _edge: bool| {
        if r.next_float() < 0.4 { cobble } else { mossy }
    };
    let sel = |f: &Frame,
               region: &mut dyn Region,
               r: &mut WorldgenRandom,
               a: (i32, i32, i32),
               b: (i32, i32, i32)| {
        f.fill_with(region, r, a.0, a.1, a.2, b.0, b.1, b.2, &pick);
    };
    let airbox = |f: &Frame, region: &mut dyn Region, a: (i32, i32, i32), b: (i32, i32, i32)| {
        f.fill(region, a.0, a.1, a.2, b.0, b.1, b.2, air, air, false);
    };
    let (w, d) = (12, 15);
    sel(f, region, r, (0, -4, 0), (w - 1, 0, d - 1));
    sel(f, region, r, (2, 1, 2), (9, 2, 2));
    sel(f, region, r, (2, 1, 12), (9, 2, 12));
    sel(f, region, r, (2, 1, 3), (2, 2, 11));
    sel(f, region, r, (9, 1, 3), (9, 2, 11));
    sel(f, region, r, (1, 3, 1), (10, 6, 1));
    sel(f, region, r, (1, 3, 13), (10, 6, 13));
    sel(f, region, r, (1, 3, 2), (1, 6, 12));
    sel(f, region, r, (10, 3, 2), (10, 6, 12));
    sel(f, region, r, (2, 3, 2), (9, 3, 12));
    sel(f, region, r, (2, 6, 2), (9, 6, 12));
    sel(f, region, r, (3, 7, 3), (8, 7, 11));
    sel(f, region, r, (4, 8, 4), (7, 8, 10));
    airbox(f, region, (3, 1, 3), (8, 2, 11));
    airbox(f, region, (4, 3, 6), (7, 3, 9));
    airbox(f, region, (2, 4, 2), (9, 5, 12));
    airbox(f, region, (4, 6, 5), (7, 6, 9));
    airbox(f, region, (5, 7, 6), (6, 7, 8));
    airbox(f, region, (5, 1, 2), (6, 2, 2));
    airbox(f, region, (5, 2, 12), (6, 2, 12));
    airbox(f, region, (5, 5, 1), (6, 5, 1));
    airbox(f, region, (5, 5, 13), (6, 5, 13));
    for (x, z) in [(1, 5), (10, 5), (1, 9), (10, 9)] {
        f.set(region, air, x, 5, z);
    }
    for z in [0, 14] {
        for x in [2, 4, 7, 9] {
            sel(f, region, r, (x, 4, z), (x, 5, z));
        }
    }
    sel(f, region, r, (5, 6, 0), (6, 6, 0));
    for x in [0, 11] {
        let mut z = 2;
        while z <= 12 {
            sel(f, region, r, (x, 4, z), (x, 5, z));
            z += 2;
        }
        sel(f, region, r, (x, 6, 5), (x, 6, 5));
        sel(f, region, r, (x, 6, 9), (x, 6, 9));
    }
    for (x, z) in [(2, 2), (9, 2), (2, 12), (9, 12)] {
        sel(f, region, r, (x, 7, z), (x, 9, z));
    }
    for (x, z) in [(4, 4), (7, 4), (4, 10), (7, 10)] {
        sel(f, region, r, (x, 9, z), (x, 9, z));
    }
    sel(f, region, r, (5, 9, 7), (6, 9, 7));
    let stairs = |dir: &str| st(facts, "cobblestone_stairs", &[("facing", dir)]);
    let (e, wst, s, n) = (
        stairs("east"),
        stairs("west"),
        stairs("south"),
        stairs("north"),
    );
    for (st, x, y, z) in [
        (n, 5, 9, 6),
        (n, 6, 9, 6),
        (s, 5, 9, 8),
        (s, 6, 9, 8),
        (n, 4, 0, 0),
        (n, 5, 0, 0),
        (n, 6, 0, 0),
        (n, 7, 0, 0),
        (n, 4, 1, 8),
        (n, 4, 2, 9),
        (n, 4, 3, 10),
        (n, 7, 1, 8),
        (n, 7, 2, 9),
        (n, 7, 3, 10),
    ] {
        f.set(region, st, x, y, z);
    }
    sel(f, region, r, (4, 1, 9), (4, 1, 9));
    sel(f, region, r, (7, 1, 9), (7, 1, 9));
    sel(f, region, r, (4, 1, 10), (7, 2, 10));
    sel(f, region, r, (5, 4, 5), (6, 4, 5));
    f.set(region, e, 4, 4, 5);
    f.set(region, wst, 7, 4, 5);
    for i in 0..4 {
        f.set(region, s, 5, -i, 6 + i);
        f.set(region, s, 6, -i, 6 + i);
        airbox(f, region, (5, -i, 7 + i), (6, -i, 9 + i));
    }
    airbox(f, region, (1, -3, 12), (10, -1, 13));
    airbox(f, region, (1, -3, 1), (3, -1, 13));
    airbox(f, region, (1, -3, 1), (9, -1, 5));
    let mut i = 1;
    while i <= 13 {
        sel(f, region, r, (1, -3, i), (1, -2, i));
        i += 2;
    }
    let mut i = 2;
    while i <= 12 {
        sel(f, region, r, (1, -1, i), (3, -1, i));
        i += 2;
    }
    sel(f, region, r, (2, -2, 1), (5, -2, 1));
    sel(f, region, r, (7, -2, 1), (9, -2, 1));
    sel(f, region, r, (6, -3, 1), (6, -3, 1));
    sel(f, region, r, (6, -1, 1), (6, -1, 1));
    // The tripwire trap.
    let hook = |dir: &str| {
        st(
            facts,
            "tripwire_hook",
            &[("facing", dir), ("attached", "true")],
        )
    };
    let wire = st(facts, "tripwire", &[("attached", "true")]);
    let dust = st(facts, "redstone_wire", &[]);
    f.set(region, hook("east"), 1, -3, 8);
    f.set(region, hook("west"), 4, -3, 8);
    f.set(region, wire, 2, -3, 8);
    f.set(region, wire, 3, -3, 8);
    for z in 1..=7 {
        f.set(region, dust, 5, -3, z);
    }
    f.set(region, dust, 4, -3, 1);
    f.set(region, mossy, 3, -3, 1);
    f.dispenser(
        region,
        r,
        3,
        -2,
        1,
        Dir::North,
        "minecraft:dispensers/jungle_temple",
    );
    f.set(region, st(facts, "vine", &[("south", "true")]), 3, -2, 2);
    f.set(region, hook("north"), 7, -3, 1);
    f.set(region, hook("south"), 7, -3, 5);
    for z in 2..=4 {
        f.set(region, wire, 7, -3, z);
    }
    for (x, z) in [(8, 6), (9, 6), (9, 5)] {
        f.set(region, dust, x, -3, z);
    }
    f.set(region, mossy, 9, -3, 4);
    f.set(region, dust, 9, -2, 4);
    f.dispenser(
        region,
        r,
        9,
        -2,
        3,
        Dir::West,
        "minecraft:dispensers/jungle_temple",
    );
    f.set(region, st(facts, "vine", &[("east", "true")]), 8, -1, 3);
    f.set(region, st(facts, "vine", &[("east", "true")]), 8, -2, 3);
    f.chest(region, r, 8, -3, 3, "minecraft:chests/jungle_temple");
    for (x, y, z) in [
        (9, -3, 2),
        (8, -3, 1),
        (4, -3, 5),
        (5, -2, 5),
        (5, -1, 5),
        (6, -3, 5),
        (7, -2, 5),
        (7, -1, 5),
        (8, -3, 5),
    ] {
        f.set(region, mossy, x, y, z);
    }
    sel(f, region, r, (9, -1, 1), (9, -1, 5));
    airbox(f, region, (8, -3, 8), (10, -1, 10));
    // The lever puzzle.
    let bricks = st(facts, "chiseled_stone_bricks", &[]);
    let lever = st(facts, "lever", &[("face", "wall"), ("facing", "north")]);
    for x in 8..=10 {
        f.set(region, bricks, x, -2, 11);
        f.set(region, lever, x, -2, 12);
    }
    sel(f, region, r, (8, -3, 8), (8, -3, 10));
    sel(f, region, r, (10, -3, 8), (10, -3, 10));
    f.set(region, mossy, 10, -2, 9);
    f.set(region, dust, 8, -2, 9);
    f.set(region, dust, 8, -2, 10);
    f.set(region, dust, 10, -1, 9);
    f.set(
        region,
        st(facts, "sticky_piston", &[("facing", "up")]),
        9,
        -2,
        8,
    );
    f.set(
        region,
        st(facts, "sticky_piston", &[("facing", "west")]),
        10,
        -2,
        8,
    );
    f.set(
        region,
        st(facts, "sticky_piston", &[("facing", "west")]),
        10,
        -1,
        8,
    );
    f.set(
        region,
        st(facts, "repeater", &[("facing", "north")]),
        10,
        -2,
        10,
    );
    f.chest(region, r, 9, -3, 10, "minecraft:chests/jungle_temple");
}
