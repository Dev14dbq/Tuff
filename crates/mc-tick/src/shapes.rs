//! Blocks whose state depends on their neighbours (post-placement updates):
//! fences, panes and bars, stairs, snowy dirt-like blocks, and the halves of
//! two-block blocks.
//!
//! Rules come from the public block documentation (minecraft.wiki) and from
//! watching vanilla 26.3: which faces fences attach to is observed for every
//! state (`cargo xtask observe attach`), the rest is checked on the parity
//! bench (`cargo xtask parity`).

use std::collections::HashMap;

use mc_data::{McData, resolve_tags};

use crate::dir::{BlockPos, Dir};
use crate::level::Level;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Other,
    /// `wooden` fences join each other, nether brick fences only each other.
    Fence {
        wooden: bool,
    },
    FenceGate,
    /// Glass panes and bars.
    Pane,
    Wall,
    Stairs,
    /// Grass block, podzol, mycelium: `snowy` when snow lies on top.
    Snowy,
    Snow,
    /// Doors and tall plants: `half` is `lower` or `upper`.
    TwoTall,
    /// Beds: `part` is `foot` or `head`.
    Bed,
}

/// Per block state facts for shape updates.
#[derive(Debug)]
pub struct Shapes {
    kind: Vec<Kind>,
    /// Faces fences attach to (north 1, east 2, south 4, west 8), observed.
    attach: Vec<u8>,
    observed: bool,
    conductor: Vec<bool>,
    blocks_of: Vec<u32>,
    blocks: mc_data::Blocks,
    air: u32,
    /// Note block instrument each state gives the note block on it (an index
    /// in [`crate::redstone::INSTRUMENTS`]), observed.
    instrument: Vec<u8>,
    /// Blocks that raise a wall's post under them (`#wall_post_override`).
    post_override: Vec<bool>,
}

impl Shapes {
    /// `attach` is the observed table (`observed/fence_attach.json`); without
    /// it, every face of a block that is not air, a fluid or a known partial
    /// shape counts as attachable.
    pub fn new(data: &McData, attach: Option<Vec<i32>>, conductor: Option<Vec<i32>>) -> Self {
        let blocks = &data.blocks;
        let states = blocks.state_count() as usize;
        let registry = data
            .builtin
            .get("minecraft:block")
            .cloned()
            .unwrap_or_default();
        let names: HashMap<i32, String> = registry.iter().map(|(n, &i)| (i, n.clone())).collect();
        let tags = data
            .data_pack()
            .tags("minecraft:block")
            .map(|t| resolve_tags(&t, |name| registry.get(name).copied()).0)
            .unwrap_or_default();
        let tagged = |tag: &str| -> Vec<String> {
            tags.get(tag)
                .into_iter()
                .flatten()
                .filter_map(|id| names.get(id).cloned())
                .collect()
        };
        let mut kind = vec![Kind::Other; states];
        let mut mark = |names: Vec<String>, k: Kind| {
            for name in names {
                if let Some(info) = blocks.get(&name) {
                    for s in info.first_state..=info.last_state {
                        kind[s as usize] = k;
                    }
                }
            }
        };
        mark(tagged("minecraft:fences"), Kind::Fence { wooden: false });
        mark(
            tagged("minecraft:wooden_fences"),
            Kind::Fence { wooden: true },
        );
        mark(tagged("minecraft:fence_gates"), Kind::FenceGate);
        mark(tagged("minecraft:walls"), Kind::Wall);
        mark(tagged("minecraft:stairs"), Kind::Stairs);
        mark(tagged("minecraft:beds"), Kind::Bed);
        let all_names: Vec<String> = registry.keys().cloned().collect();
        let named = |f: &dyn Fn(&str) -> bool| -> Vec<String> {
            all_names.iter().filter(|n| f(n)).cloned().collect()
        };
        mark(
            named(&|n| n.ends_with("_pane") || n.ends_with("_bars") || n == "minecraft:iron_bars"),
            Kind::Pane,
        );
        mark(
            named(&|n| {
                matches!(
                    n,
                    "minecraft:grass_block" | "minecraft:podzol" | "minecraft:mycelium"
                )
            }),
            Kind::Snowy,
        );
        mark(
            named(&|n| n == "minecraft:snow" || n == "minecraft:snow_block"),
            Kind::Snow,
        );
        // Doors and tall plants: anything with half = lower/upper.
        for state in 0..states as u32 {
            if kind[state as usize] == Kind::Other
                && matches!(blocks.property(state, "half"), Some("lower" | "upper"))
            {
                kind[state as usize] = Kind::TwoTall;
            }
        }

        let mut post_override = vec![false; states];
        for name in tagged("minecraft:wall_post_override") {
            if let Some(info) = blocks.get(&name) {
                for s in info.first_state..=info.last_state {
                    post_override[s as usize] = true;
                }
            }
        }
        let observed = attach.as_ref().is_some_and(|a| a.len() == states);
        let attach: Vec<u8> = match attach.filter(|a| a.len() == states) {
            Some(table) => table
                .iter()
                .map(|&b| if b < 0 { 0 } else { (b & 15) as u8 })
                .collect(),
            None => (0..states as u32)
                .map(|s| {
                    let partial = blocks.is_air(s)
                        || blocks.is_fluid(s)
                        || kind[s as usize] != Kind::Other
                        || blocks.property(s, "waterlogged").is_some();
                    if partial { 0 } else { 15 }
                })
                .collect(),
        };
        let conductor = match conductor.filter(|c| c.len() == states) {
            Some(table) => table.iter().map(|&c| c >= 0 && c & 1 == 1).collect(),
            None => (0..states)
                .map(|s| {
                    attach[s] == 15
                        && !matches!(
                            blocks.block_name(s as u32),
                            Some(
                                "minecraft:redstone_block"
                                    | "minecraft:observer"
                                    | "minecraft:piston"
                                    | "minecraft:sticky_piston"
                                    | "minecraft:glass"
                            )
                        )
                })
                .collect(),
        };
        let mut blocks_of = vec![0; states];
        let mut block_names: Vec<String> = Vec::new();
        for state in 0..states as u32 {
            let name = blocks.block_name(state).unwrap_or("");
            let index = match block_names.iter().rposition(|n| n == name) {
                Some(i) => i,
                None => {
                    block_names.push(name.to_owned());
                    block_names.len() - 1
                }
            };
            blocks_of[state as usize] = index as u32;
        }
        Self {
            instrument: vec![0; states],
            post_override,
            kind,
            attach,
            observed,
            conductor,
            blocks_of,
            blocks: blocks.clone(),
            air: blocks.default_state("minecraft:air").unwrap_or(0),
        }
    }

    pub fn is_air(&self, state: u32) -> bool {
        self.blocks.is_air(state)
    }

    pub fn block_name(&self, state: u32) -> Option<&str> {
        self.blocks.block_name(state)
    }

    pub fn property(&self, state: u32, name: &str) -> Option<&str> {
        self.blocks.property(state, name)
    }

    /// A state by block name and properties (others at their defaults).
    pub fn state(&self, name: &str, properties: &[(&str, &str)]) -> Option<u32> {
        self.blocks.state_id(name, properties)
    }

    /// Conducts redstone power (observed; without the table, full faces on
    /// every side count).
    pub fn conducts(&self, state: u32) -> bool {
        self.conductor.get(state as usize).copied().unwrap_or(false)
    }

    /// A number for the block a state belongs to (for scheduled block ticks).
    pub fn block_index(&self, state: u32) -> u32 {
        self.blocks_of
            .get(state as usize)
            .copied()
            .unwrap_or(u32::MAX)
    }

    /// Whether the fence table came from observation.
    pub fn observed(&self) -> bool {
        self.observed
    }

    pub fn kind(&self, state: u32) -> Kind {
        self.kind
            .get(state as usize)
            .copied()
            .unwrap_or(Kind::Other)
    }

    fn same_block(&self, a: u32, b: u32) -> bool {
        self.blocks_of.get(a as usize) == self.blocks_of.get(b as usize)
    }

    fn prop(&self, state: u32, name: &str) -> Option<&str> {
        self.blocks.property(state, name)
    }

    pub fn with(&self, state: u32, name: &str, value: &str) -> u32 {
        self.blocks
            .with_property(state, name, value)
            .unwrap_or(state)
    }

    /// Whether the face of `neighbor` that looks along `towards` (from the
    /// connecting block) can be attached to.
    pub fn face_attaches(&self, neighbor: u32, towards: Dir) -> bool {
        let face = towards.opposite();
        self.attach.get(neighbor as usize).copied().unwrap_or(0) & face.face_bit() != 0
    }

    fn fence_connects(&self, wooden: bool, neighbor: u32, dir: Dir) -> bool {
        match self.kind(neighbor) {
            Kind::Fence { wooden: other } => other == wooden,
            Kind::FenceGate => self
                .prop(neighbor, "facing")
                .and_then(Dir::from_name)
                .is_some_and(|f| f.axis() != dir.axis()),
            Kind::Pane | Kind::Wall => false,
            _ => self.face_attaches(neighbor, dir),
        }
    }

    fn pane_connects(&self, neighbor: u32, dir: Dir) -> bool {
        match self.kind(neighbor) {
            Kind::Pane | Kind::Wall => true,
            Kind::Fence { .. } | Kind::FenceGate => false,
            _ => self.face_attaches(neighbor, dir),
        }
    }

    fn wall_connects(&self, neighbor: u32, dir: Dir) -> bool {
        match self.kind(neighbor) {
            Kind::Wall | Kind::Pane => true,
            Kind::FenceGate => self
                .prop(neighbor, "facing")
                .and_then(Dir::from_name)
                .is_some_and(|f| f.axis() != dir.axis()),
            Kind::Fence { .. } => false,
            _ => self.face_attaches(neighbor, dir),
        }
    }

    /// A fence gate between walls sits lower (`in_wall`).
    fn gate_in_wall(&self, level: &dyn Level, pos: BlockPos, state: u32) -> u32 {
        let Some(facing) = self.prop(state, "facing").and_then(Dir::from_name) else {
            return state;
        };
        let wall = [facing.cw(), facing.ccw()]
            .into_iter()
            .any(|d| self.kind(level.block(pos.offset(d))) == Kind::Wall);
        self.with(state, "in_wall", if wall { "true" } else { "false" })
    }

    /// Whether the block above a wall reaches down over its side `dir`.
    fn covers_side(&self, above: u32, dir: Dir) -> bool {
        match self.kind(above) {
            Kind::Wall => self.prop(above, dir.name()).is_some_and(|v| v != "none"),
            Kind::Stairs => self.prop(above, "half") == Some("bottom"),
            _ if self.prop(above, "type") == Some("bottom") => true,
            _ if self
                .blocks
                .block_name(above)
                .is_some_and(|n| n.ends_with("_carpet") || n == "minecraft:snow") =>
            {
                true
            }
            _ => self.attach.get(above as usize).copied().unwrap_or(0) == 15,
        }
    }

    /// A wall's sides (none, low, tall) and post from its neighbours
    /// (minecraft.wiki, "Wall").
    fn wall_shape(&self, level: &dyn Level, pos: BlockPos, state: u32) -> u32 {
        let above = level.block(pos.offset(Dir::Up));
        let mut sides = [("none", Dir::North); 4];
        for (i, d) in Dir::HORIZONTAL.into_iter().enumerate() {
            let value = if !self.wall_connects(level.block(pos.offset(d)), d) {
                "none"
            } else if self.covers_side(above, d) {
                "tall"
            } else {
                "low"
            };
            sides[i] = (value, d);
        }
        let side = |d: Dir| {
            sides
                .iter()
                .find(|(_, s)| *s == d)
                .map_or("none", |(v, _)| *v)
        };
        let (n, e, so, w) = (
            side(Dir::North),
            side(Dir::East),
            side(Dir::South),
            side(Dir::West),
        );
        // A post unless the wall runs straight through (opposite sides
        // alike: a line or a cross), or something on top needs one.
        let alone = [n, e, so, w].iter().all(|v| *v == "none");
        let post = alone
            || n != so
            || e != w
            || (self.kind(above) == Kind::Wall && self.prop(above, "up") == Some("true"))
            || self
                .post_override
                .get(above as usize)
                .copied()
                .unwrap_or(false);
        let state = sides
            .into_iter()
            .fold(state, |st, (v, d)| self.with(st, d.name(), v));
        self.with(state, "up", if post { "true" } else { "false" })
    }

    /// A stairs' shape from the stairs in front of and behind it.
    fn stairs_shape(&self, level: &dyn Level, pos: BlockPos, state: u32) -> u32 {
        let Some(facing) = self.prop(state, "facing").and_then(Dir::from_name) else {
            return state;
        };
        let half = self.prop(state, "half");
        let stairs_at = |p: BlockPos| {
            let s = level.block(p);
            (self.kind(s) == Kind::Stairs && self.prop(s, "half") == half)
                .then(|| self.prop(s, "facing").and_then(Dir::from_name))
                .flatten()
        };
        // A side is free when it does not hold stairs facing and halved like us.
        let free = |side: Dir| stairs_at(pos.offset(side)) != Some(facing);
        let shape = if let Some(back) = stairs_at(pos.offset(facing))
            && back.axis() != facing.axis()
            && free(back.opposite())
        {
            if back == facing.ccw() {
                "outer_left"
            } else {
                "outer_right"
            }
        } else if let Some(front) = stairs_at(pos.offset(facing.opposite()))
            && front.axis() != facing.axis()
            && free(front)
        {
            if front == facing.ccw() {
                "inner_left"
            } else {
                "inner_right"
            }
        } else {
            "straight"
        };
        self.with(state, "shape", shape)
    }

    /// With the observed note block instruments (`observed/instrument.json`:
    /// per state, filled for default states). Every state of a block gives
    /// what its default state gives.
    pub fn with_instruments(mut self, table: Option<Vec<i32>>) -> Self {
        let Some(table) = table.filter(|t| t.len() == self.instrument.len()) else {
            return self;
        };
        for state in 0..self.instrument.len() as u32 {
            let default = self
                .blocks
                .block_name(state)
                .and_then(|n| self.blocks.default_state(n));
            if let Some(&v) = default.and_then(|d| table.get(d as usize)) {
                self.instrument[state as usize] = v.max(0) as u8;
            }
        }
        self
    }

    /// The instrument a note block on top of `state` plays.
    pub fn instrument_below(&self, state: u32) -> u8 {
        self.instrument.get(state as usize).copied().unwrap_or(0)
    }

    /// The new state of the block at `pos` after its neighbour in direction
    /// `dir` became `neighbor`.
    pub fn update_shape(
        &self,
        level: &dyn Level,
        pos: BlockPos,
        state: u32,
        dir: Dir,
        neighbor: u32,
    ) -> u32 {
        match self.kind(state) {
            Kind::Fence { wooden } if dir.is_horizontal() => {
                let value = if self.fence_connects(wooden, neighbor, dir) {
                    "true"
                } else {
                    "false"
                };
                self.with(state, dir.name(), value)
            }
            Kind::Pane if dir.is_horizontal() => {
                let value = if self.pane_connects(neighbor, dir) {
                    "true"
                } else {
                    "false"
                };
                self.with(state, dir.name(), value)
            }
            Kind::Stairs if dir.is_horizontal() => self.stairs_shape(level, pos, state),
            Kind::Wall if dir != Dir::Down => self.wall_shape(level, pos, state),
            Kind::FenceGate if dir.is_horizontal() => self.gate_in_wall(level, pos, state),
            Kind::Snowy if dir == Dir::Up => {
                let snowy = self.kind(neighbor) == Kind::Snow;
                self.with(state, "snowy", if snowy { "true" } else { "false" })
            }
            Kind::TwoTall => {
                let lower = self.prop(state, "half") == Some("lower");
                let partner_dir = if lower { Dir::Up } else { Dir::Down };
                if dir != partner_dir {
                    return state;
                }
                let partner_ok = self.same_block(state, neighbor)
                    && self.prop(neighbor, "half") == Some(if lower { "upper" } else { "lower" });
                if !partner_ok {
                    return self.air;
                }
                // Doors keep both halves alike.
                let mut new = state;
                for p in ["facing", "open", "hinge", "powered"] {
                    if let Some(v) = self.prop(neighbor, p) {
                        new = self.with(new, p, v);
                    }
                }
                new
            }
            Kind::Bed => {
                let Some(facing) = self.prop(state, "facing").and_then(Dir::from_name) else {
                    return state;
                };
                let foot = self.prop(state, "part") == Some("foot");
                let toward_partner = if foot { facing } else { facing.opposite() };
                if dir != toward_partner {
                    return state;
                }
                let partner_ok = self.same_block(state, neighbor)
                    && self.prop(neighbor, "part") == Some(if foot { "head" } else { "foot" });
                if partner_ok { state } else { self.air }
            }
            _ => state,
        }
    }

    /// The state a block takes when placed at `pos`: connections and shape
    /// as if every neighbour had just updated it.
    pub fn placement_shape(&self, level: &dyn Level, pos: BlockPos, state: u32) -> u32 {
        match self.kind(state) {
            Kind::Fence { .. } | Kind::Pane => Dir::HORIZONTAL.into_iter().fold(state, |s, d| {
                self.update_shape(level, pos, s, d, level.block(pos.offset(d)))
            }),
            Kind::Stairs => self.stairs_shape(level, pos, state),
            Kind::Wall => self.wall_shape(level, pos, state),
            Kind::FenceGate => self.gate_in_wall(level, pos, state),
            Kind::Snowy => {
                self.update_shape(level, pos, state, Dir::Up, level.block(pos.offset(Dir::Up)))
            }
            _ => state,
        }
    }
}
