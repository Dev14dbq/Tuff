//! Rails (minecraft.wiki: "Rail", "Powered Rail", "Detector Rail",
//! "Activator Rail"): the shape a rail takes from the rails around it, and
//! how powered and activator rails pass power along a line of up to 8 more.
//!
//! A rail looks for rails beside it on the same level, one up or one down. It
//! joins up to two; a neighbour that already has two joins only if one of them
//! is this rail. Straight-only rails (powered, detector, activator) never
//! bend. On placement the rail also tells the rails it joined to join back.

use crate::dir::{BlockPos, Dir};
use crate::level::Level;
use crate::redstone;
use crate::updates::{Ctx, Flags, set_block};

/// Longest run of powered rails that pass power on.
const POWER_RANGE: u32 = 8;
const DETECTOR_TICKS: i64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Powered,
    Activator,
    Detector,
}

pub fn kind(ctx: Ctx, state: u32) -> Option<Kind> {
    match ctx.shapes.block_name(state)? {
        "minecraft:rail" => Some(Kind::Plain),
        "minecraft:powered_rail" => Some(Kind::Powered),
        "minecraft:activator_rail" => Some(Kind::Activator),
        "minecraft:detector_rail" => Some(Kind::Detector),
        _ => None,
    }
}

fn straight_only(k: Kind) -> bool {
    k != Kind::Plain
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    NorthSouth,
    EastWest,
    AscEast,
    AscWest,
    AscNorth,
    AscSouth,
    SouthEast,
    SouthWest,
    NorthWest,
    NorthEast,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::NorthSouth => "north_south",
            Shape::EastWest => "east_west",
            Shape::AscEast => "ascending_east",
            Shape::AscWest => "ascending_west",
            Shape::AscNorth => "ascending_north",
            Shape::AscSouth => "ascending_south",
            Shape::SouthEast => "south_east",
            Shape::SouthWest => "south_west",
            Shape::NorthWest => "north_west",
            Shape::NorthEast => "north_east",
        }
    }

    fn from_name(name: &str) -> Option<Shape> {
        Some(match name {
            "north_south" => Shape::NorthSouth,
            "east_west" => Shape::EastWest,
            "ascending_east" => Shape::AscEast,
            "ascending_west" => Shape::AscWest,
            "ascending_north" => Shape::AscNorth,
            "ascending_south" => Shape::AscSouth,
            "south_east" => Shape::SouthEast,
            "south_west" => Shape::SouthWest,
            "north_west" => Shape::NorthWest,
            "north_east" => Shape::NorthEast,
            _ => return None,
        })
    }

    fn ascending(self) -> bool {
        matches!(
            self,
            Shape::AscEast | Shape::AscWest | Shape::AscNorth | Shape::AscSouth
        )
    }

    /// The two spots this shape reaches towards.
    fn connections(self, p: BlockPos) -> [BlockPos; 2] {
        let n = p.offset(Dir::North);
        let s = p.offset(Dir::South);
        let w = p.offset(Dir::West);
        let e = p.offset(Dir::East);
        let up = |b: BlockPos| b.offset(Dir::Up);
        match self {
            Shape::NorthSouth => [n, s],
            Shape::EastWest => [w, e],
            Shape::AscEast => [w, up(e)],
            Shape::AscWest => [up(w), e],
            Shape::AscNorth => [up(n), s],
            Shape::AscSouth => [n, up(s)],
            Shape::SouthEast => [s, e],
            Shape::SouthWest => [s, w],
            Shape::NorthWest => [n, w],
            Shape::NorthEast => [n, e],
        }
    }
}

fn shape_of(ctx: Ctx, state: u32) -> Option<Shape> {
    ctx.shapes
        .property(state, "shape")
        .and_then(Shape::from_name)
}

/// A rail in the world as the shape logic sees it.
struct Rail {
    pos: BlockPos,
    kind: Kind,
    conns: Vec<BlockPos>,
}

impl Rail {
    fn read(ctx: Ctx, pos: BlockPos, state: u32) -> Option<Rail> {
        let kind = kind(ctx, state)?;
        let shape = shape_of(ctx, state)?;
        Some(Rail {
            pos,
            kind,
            conns: shape.connections(pos).to_vec(),
        })
    }

    fn has_connection(&self, p: BlockPos) -> bool {
        self.conns.contains(&p)
            || self.conns.contains(&p.offset(Dir::Up))
            || self.conns.contains(&p.offset(Dir::Down))
    }

    fn can_connect_to(&self, other: BlockPos) -> bool {
        self.conns.contains(&other) || self.conns.len() != 2
    }

    /// The rail at `p`, one above or one below.
    fn find(level: &dyn Level, ctx: Ctx, p: BlockPos) -> Option<Rail> {
        for q in [p, p.offset(Dir::Up), p.offset(Dir::Down)] {
            if let Some(r) = Rail::read(ctx, q, level.block(q)) {
                return Some(r);
            }
        }
        None
    }

    /// Drops joins to rails that do not join back.
    fn remove_soft(&mut self, level: &dyn Level, ctx: Ctx) {
        let me = self.pos;
        let conns = std::mem::take(&mut self.conns);
        self.conns = conns
            .into_iter()
            .filter_map(|c| {
                Rail::find(level, ctx, c).and_then(|r| r.has_connection(me).then_some(r.pos))
            })
            .collect();
    }

    fn neighbor_rail(&self, level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
        Rail::find(level, ctx, p).is_some_and(|mut r| {
            r.remove_soft(level, ctx);
            r.can_connect_to(self.pos)
        })
    }

    fn is_rail(level: &dyn Level, ctx: Ctx, p: BlockPos) -> bool {
        kind(ctx, level.block(p)).is_some()
    }

    fn with_slope(&self, level: &dyn Level, ctx: Ctx, mut shape: Shape) -> Shape {
        let up = |d: Dir| self.pos.offset(d).offset(Dir::Up);
        if shape == Shape::NorthSouth {
            if Rail::is_rail(level, ctx, up(Dir::North)) {
                shape = Shape::AscNorth;
            }
            if Rail::is_rail(level, ctx, up(Dir::South)) {
                shape = Shape::AscSouth;
            }
        }
        if shape == Shape::EastWest {
            if Rail::is_rail(level, ctx, up(Dir::East)) {
                shape = Shape::AscEast;
            }
            if Rail::is_rail(level, ctx, up(Dir::West)) {
                shape = Shape::AscWest;
            }
        }
        shape
    }

    /// The shape this rail takes from its neighbours.
    fn place(&mut self, level: &dyn Level, ctx: Ctx, first: bool, current: Shape) -> Shape {
        let n = self.neighbor_rail(level, ctx, self.pos.offset(Dir::North));
        let s = self.neighbor_rail(level, ctx, self.pos.offset(Dir::South));
        let w = self.neighbor_rail(level, ctx, self.pos.offset(Dir::West));
        let e = self.neighbor_rail(level, ctx, self.pos.offset(Dir::East));
        let straight = straight_only(self.kind);
        let mut shape = None;
        if (n || s) && !w && !e {
            shape = Some(Shape::NorthSouth);
        }
        if (w || e) && !n && !s {
            shape = Some(Shape::EastWest);
        }
        if !straight {
            if s && e && !n && !w {
                shape = Some(Shape::SouthEast);
            }
            if s && w && !n && !e {
                shape = Some(Shape::SouthWest);
            }
            if n && w && !s && !e {
                shape = Some(Shape::NorthWest);
            }
            if n && e && !s && !w {
                shape = Some(Shape::NorthEast);
            }
        }
        if shape.is_none() {
            if n || s {
                shape = Some(Shape::NorthSouth);
            }
            if w || e {
                shape = Some(Shape::EastWest);
            }
            if !straight {
                if first {
                    if s && e {
                        shape = Some(Shape::SouthEast);
                    }
                    if w && s {
                        shape = Some(Shape::SouthWest);
                    }
                    if e && n {
                        shape = Some(Shape::NorthEast);
                    }
                    if n && w {
                        shape = Some(Shape::NorthWest);
                    }
                } else {
                    if n && w {
                        shape = Some(Shape::NorthWest);
                    }
                    if e && n {
                        shape = Some(Shape::NorthEast);
                    }
                    if w && s {
                        shape = Some(Shape::SouthWest);
                    }
                    if s && e {
                        shape = Some(Shape::SouthEast);
                    }
                }
            }
        }
        let _ = current;
        let shape = self.with_slope(level, ctx, shape.unwrap_or(Shape::NorthSouth));
        self.conns = shape.connections(self.pos).to_vec();
        shape
    }

    /// A rail joined to this one: take a shape that includes the new join.
    fn connect_to(&mut self, ctx: Ctx, level: &dyn Level, other: BlockPos) -> Shape {
        self.conns.push(other);
        let has = |d: Dir, s: &Rail| s.has_connection(s.pos.offset(d));
        let (n, s, w, e) = (
            has(Dir::North, self),
            has(Dir::South, self),
            has(Dir::West, self),
            has(Dir::East, self),
        );
        let straight = straight_only(self.kind);
        let mut shape = None;
        if n || s {
            shape = Some(Shape::NorthSouth);
        }
        if w || e {
            shape = Some(Shape::EastWest);
        }
        if !straight {
            if s && e && !n && !w {
                shape = Some(Shape::SouthEast);
            }
            if s && w && !n && !e {
                shape = Some(Shape::SouthWest);
            }
            if n && w && !s && !e {
                shape = Some(Shape::NorthWest);
            }
            if n && e && !s && !w {
                shape = Some(Shape::NorthEast);
            }
        }
        self.with_slope(level, ctx, shape.unwrap_or(Shape::NorthSouth))
    }

    fn count_potential(&self, level: &dyn Level, ctx: Ctx) -> usize {
        Dir::HORIZONTAL
            .into_iter()
            .filter(|&d| self.neighbor_rail(level, ctx, self.pos.offset(d)))
            .count()
    }
}

/// Reshapes the rail at `p` from its neighbours. `first` is a fresh placement.
fn update_shape(level: &mut dyn Level, ctx: Ctx, p: BlockPos, first: bool) {
    let state = level.block(p);
    let Some(mut rail) = Rail::read(ctx, p, state) else {
        return;
    };
    let current = shape_of(ctx, state).unwrap_or(Shape::NorthSouth);
    let shape = rail.place(level, ctx, first, current);
    let new = ctx.shapes.with(state, "shape", shape.name());
    if first || new != state {
        set_block(level, ctx, p, new, Flags::ALL);
        for c in rail.conns.clone() {
            if let Some(mut other) = Rail::find(level, ctx, c) {
                other.remove_soft(level, ctx);
                if other.can_connect_to(p) {
                    let s = other.connect_to(ctx, level, p);
                    let st = level.block(other.pos);
                    let ns = ctx.shapes.with(st, "shape", s.name());
                    if ns != st {
                        set_block(level, ctx, other.pos, ns, Flags::ALL);
                    }
                }
            }
        }
    }
}

fn powered_prop(ctx: Ctx, state: u32) -> bool {
    ctx.shapes.property(state, "powered") == Some("true")
}

/// Whether a powered or activator rail at `p` of the same kind carries
/// power along `shape`'s line.
fn same_rail_with_power(
    level: &dyn Level,
    ctx: Ctx,
    p: BlockPos,
    forward: bool,
    depth: u32,
    shape: Shape,
    k: Kind,
) -> bool {
    let state = level.block(p);
    if kind(ctx, state) != Some(k) {
        return false;
    }
    let Some(other) = shape_of(ctx, state) else {
        return false;
    };
    let across = |a: Shape| {
        matches!(a, Shape::EastWest)
            && matches!(other, Shape::NorthSouth | Shape::AscNorth | Shape::AscSouth)
            || matches!(a, Shape::NorthSouth)
                && matches!(other, Shape::EastWest | Shape::AscEast | Shape::AscWest)
    };
    if across(shape) || !powered_prop(ctx, state) {
        return false;
    }
    redstone::received(level, ctx, p, true) > 0
        || find_powered(level, ctx, p, state, forward, depth + 1)
}

fn find_powered(
    level: &dyn Level,
    ctx: Ctx,
    p: BlockPos,
    state: u32,
    forward: bool,
    depth: u32,
) -> bool {
    if depth >= POWER_RANGE {
        return false;
    }
    let (Some(shape), Some(k)) = (shape_of(ctx, state), kind(ctx, state)) else {
        return false;
    };
    let (mut x, mut y, mut z) = (p.x, p.y, p.z);
    let mut below = true;
    match shape {
        Shape::NorthSouth => z += if forward { 1 } else { -1 },
        Shape::EastWest => x += if forward { 1 } else { -1 },
        Shape::AscEast => {
            if forward {
                x -= 1;
            } else {
                x += 1;
                y += 1;
                below = false;
            }
        }
        Shape::AscWest => {
            if forward {
                x -= 1;
                y += 1;
                below = false;
            } else {
                x += 1;
            }
        }
        Shape::AscNorth => {
            if forward {
                z += 1;
            } else {
                z -= 1;
                y += 1;
                below = false;
            }
        }
        Shape::AscSouth => {
            if forward {
                z += 1;
                y += 1;
                below = false;
            } else {
                z -= 1;
            }
        }
        _ => return false,
    }
    same_rail_with_power(level, ctx, BlockPos::new(x, y, z), forward, depth, shape, k)
        || (below
            && same_rail_with_power(
                level,
                ctx,
                BlockPos::new(x, y - 1, z),
                forward,
                depth,
                shape,
                k,
            ))
}

/// Powered and activator rails: follow power from beside or along the line.
fn update_power(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    let Some(k) = kind(ctx, state) else { return };
    if !matches!(k, Kind::Powered | Kind::Activator) {
        return;
    }
    let was = powered_prop(ctx, state);
    let now = redstone::received(level, ctx, p, true) > 0
        || find_powered(level, ctx, p, state, true, 0)
        || find_powered(level, ctx, p, state, false, 0);
    if now != was {
        let new = ctx
            .shapes
            .with(state, "powered", if now { "true" } else { "false" });
        set_block(level, ctx, p, new, Flags::ALL);
        redstone::update_neighbors_at(level, ctx, p.offset(Dir::Down));
        if shape_of(ctx, new).is_some_and(Shape::ascending) {
            redstone::update_neighbors_at(level, ctx, p.offset(Dir::Up));
        }
    }
}

/// An ascending rail needs a block on the high side to rest on.
fn slope_unsupported(level: &dyn Level, ctx: Ctx, p: BlockPos, state: u32) -> bool {
    let side = match shape_of(ctx, state) {
        Some(Shape::AscEast) => Dir::East,
        Some(Shape::AscWest) => Dir::West,
        Some(Shape::AscNorth) => Dir::North,
        Some(Shape::AscSouth) => Dir::South,
        _ => return false,
    };
    !ctx.facts.blocks_motion(level.block(p.offset(side)))
}

/// A rail was put at `p` (a different block than before).
pub fn on_place(level: &mut dyn Level, ctx: Ctx, p: BlockPos, state: u32) {
    let Some(k) = kind(ctx, state) else { return };
    update_shape(level, ctx, p, true);
    if straight_only(k) {
        // Powered rails look at their power at once.
        update_power(level, ctx, p);
    }
    if k == Kind::Detector {
        let kind_id = ctx.shapes.block_index(level.block(p));
        level.schedule_block_tick(p, kind_id, DETECTOR_TICKS, 0);
    }
}

/// A neighbour of the rail at `p` changed. Returns whether it broke.
pub fn neighbor_changed(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    let Some(k) = kind(ctx, state) else { return };
    if slope_unsupported(level, ctx, p, state) {
        level.destroyed(p, state);
        set_block(level, ctx, p, ctx.shapes.air(), Flags::ALL);
        return;
    }
    match k {
        Kind::Plain => {
            // Only a third rail next to it makes a plain rail look again.
            if let Some(r) = Rail::read(ctx, p, state)
                && r.count_potential(level, ctx) == 3
            {
                update_shape(level, ctx, p, false);
            }
        }
        _ => update_power(level, ctx, p),
    }
}

fn detector_signal(level: &dyn Level, p: BlockPos) -> bool {
    level.minecarts_at(p) > 0
}

/// Detector rail: powered while a minecart is on it.
fn detector_check(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if kind(ctx, state) != Some(Kind::Detector) {
        return;
    }
    let was = powered_prop(ctx, state);
    let now = detector_signal(level, p);
    if now != was {
        let new = ctx
            .shapes
            .with(state, "powered", if now { "true" } else { "false" });
        set_block(level, ctx, p, new, Flags::SHAPES_ONLY);
        redstone::update_neighbors_at(level, ctx, p);
        redstone::update_neighbors_at(level, ctx, p.offset(Dir::Down));
    }
    if now {
        let kind_id = ctx.shapes.block_index(level.block(p));
        level.schedule_block_tick(p, kind_id, DETECTOR_TICKS, 0);
    }
}

/// A minecart is in the rail's block.
pub fn entity_inside(level: &mut dyn Level, ctx: Ctx, p: BlockPos) {
    let state = level.block(p);
    if kind(ctx, state) == Some(Kind::Detector) && !powered_prop(ctx, state) {
        detector_check(level, ctx, p);
    }
}

pub fn block_tick(level: &mut dyn Level, ctx: Ctx, p: BlockPos, kind_id: u32) {
    let state = level.block(p);
    if ctx.shapes.block_index(state) == kind_id && kind(ctx, state) == Some(Kind::Detector) {
        detector_check(level, ctx, p);
    }
}

/// Power a rail gives: a detector rail under a minecart is a strong source
/// for the block below (`toward` is the way from the rail to that block).
pub fn emits(ctx: Ctx, state: u32, toward: Dir, strong: bool) -> u8 {
    if kind(ctx, state) == Some(Kind::Detector) && powered_prop(ctx, state) {
        if strong {
            return if toward == Dir::Down { 15 } else { 0 };
        }
        return 15;
    }
    0
}

/// The comparator reading of a detector rail with a minecart on it (a
/// minecart with a container is not read yet): 15 when pressed.
pub fn comparator_reading(level: &dyn Level, ctx: Ctx, p: BlockPos) -> Option<u8> {
    let state = level.block(p);
    (kind(ctx, state) == Some(Kind::Detector)).then(|| u8::from(level.minecarts_at(p) > 0) * 15)
}
