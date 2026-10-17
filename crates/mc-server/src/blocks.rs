//! What placing and breaking a block does, before block behaviours exist
//! (ROADMAP Ф3): which state an item places, where it goes and which blocks
//! can be replaced.
//!
//! The rules come from watching vanilla 26.3 (`cargo xtask blocks`) and from
//! block tags in the data pack. They are a first approximation: no neighbour
//! updates (stair shapes, fences), and block shapes are assumed to be full
//! cubes unless the block is known to have no collision.

use std::collections::HashMap;

use anyhow::{Context as _, Result};
use mc_data::{McData, resolve_tags};
use mc_protocol::Position;

/// The six faces, in Player Action / Use Item On order.
pub const FACES: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];
const FACE_NAMES: [&str; 6] = ["down", "up", "north", "south", "west", "east"];

pub fn offset(p: Position, face: usize) -> Position {
    let (dx, dy, dz) = FACES[face];
    Position::new(p.x + dx, p.y + dy, p.z + dz)
}

/// Per block state facts the rules need.
#[derive(Debug)]
pub struct BlockRules {
    /// Can be replaced by placing a block into it (air, water, grass, ...).
    replaceable: Vec<bool>,
    /// No collision: does not stop a placement next to a player and does not
    /// count for the motion-blocking heightmaps.
    passable: Vec<bool>,
    leaves: Vec<bool>,
    /// Item registry id → block it places.
    item_blocks: HashMap<i32, String>,
    /// Block registry id → name.
    block_names: Vec<String>,
    look_facing: Vec<bool>,
    pub air: u32,
    pub water: u32,
}

/// Blocks without collision that no tag covers.
const PASSABLE: &[&str] = &[
    "minecraft:torch",
    "minecraft:wall_torch",
    "minecraft:soul_torch",
    "minecraft:soul_wall_torch",
    "minecraft:redstone_torch",
    "minecraft:redstone_wall_torch",
    "minecraft:copper_torch",
    "minecraft:copper_wall_torch",
    "minecraft:redstone_wire",
    "minecraft:tripwire",
    "minecraft:tripwire_hook",
    "minecraft:lever",
    "minecraft:sugar_cane",
    "minecraft:nether_wart",
    "minecraft:cobweb",
    "minecraft:end_portal",
    "minecraft:nether_portal",
    "minecraft:light",
    "minecraft:structure_void",
    "minecraft:kelp",
    "minecraft:kelp_plant",
    "minecraft:sweet_berry_bush",
    "minecraft:big_dripleaf_stem",
    "minecraft:hanging_roots",
    "minecraft:spore_blossom",
];
const PASSABLE_TAGS: &[&str] = &[
    "minecraft:replaceable",
    "minecraft:flowers",
    "minecraft:saplings",
    "minecraft:crops",
    "minecraft:buttons",
    "minecraft:pressure_plates",
    "minecraft:rails",
    "minecraft:all_signs",
    "minecraft:banners",
    "minecraft:cave_vines",
    "minecraft:fire",
    "minecraft:climbable",
    "minecraft:corals",
    "minecraft:wall_corals",
];
/// Blocks whose `facing` is the way the player looks; others face the player.
const LOOK_FACING_TAGS: &[&str] = &[
    "minecraft:stairs",
    "minecraft:doors",
    "minecraft:fence_gates",
    "minecraft:beds",
];

impl BlockRules {
    pub fn new(data: &McData) -> Result<Self> {
        let blocks = &data.blocks;
        let states = blocks.state_count() as usize;
        let registry = data
            .builtin
            .get("minecraft:block")
            .context("minecraft:block missing from the registries report")?;
        let mut block_names = vec![String::new(); registry.len()];
        for (name, &id) in registry {
            if let Some(slot) = usize::try_from(id)
                .ok()
                .and_then(|i| block_names.get_mut(i))
            {
                slot.clone_from(name);
            }
        }
        let pack = data.data_pack();
        let (tags, _) = resolve_tags(&pack.tags("minecraft:block")?, |name| {
            registry.get(name).copied()
        });
        let per_state = |names: &mut dyn Iterator<Item = &str>| -> Vec<bool> {
            let mut flags = vec![false; states];
            for name in names {
                if let Some(info) = blocks.get(name) {
                    for state in info.first_state..=info.last_state {
                        flags[state as usize] = true;
                    }
                }
            }
            flags
        };
        let tagged = |tag_names: &[&str]| -> Vec<&str> {
            tag_names
                .iter()
                .filter_map(|t| tags.get(*t))
                .flatten()
                .filter_map(|&id| block_names.get(id as usize).map(String::as_str))
                .collect()
        };
        let mut replaceable = per_state(&mut tagged(&["minecraft:replaceable"]).into_iter());
        for state in 0..states as u32 {
            if blocks.is_air(state) {
                replaceable[state as usize] = true;
            }
        }
        let mut passable_names = tagged(PASSABLE_TAGS);
        passable_names.extend_from_slice(PASSABLE);
        let mut passable = per_state(&mut passable_names.into_iter());
        for state in 0..states as u32 {
            if blocks.is_air(state)
                || blocks
                    .block_name(state)
                    .is_some_and(|n| n == "minecraft:water" || n == "minecraft:lava")
            {
                passable[state as usize] = true;
            }
        }
        let leaves = per_state(&mut tagged(&["minecraft:leaves"]).into_iter());
        let look_facing = per_state(&mut tagged(LOOK_FACING_TAGS).into_iter());

        // Most block items share their block's name.
        let items = data
            .builtin
            .get("minecraft:item")
            .context("minecraft:item missing from the registries report")?;
        let item_blocks = items
            .iter()
            .filter(|(name, _)| blocks.get(name).is_some())
            .map(|(name, &id)| (id, name.clone()))
            .collect();
        Ok(Self {
            replaceable,
            passable,
            leaves,
            item_blocks,
            block_names,
            look_facing,
            air: blocks.default_state("minecraft:air").context("no air")?,
            water: blocks
                .default_state("minecraft:water")
                .context("no water")?,
        })
    }

    fn flag(flags: &[bool], state: u32) -> bool {
        flags.get(state as usize).copied().unwrap_or(false)
    }

    pub fn is_replaceable(&self, state: u32) -> bool {
        Self::flag(&self.replaceable, state)
    }

    /// Stops movement (a full cube unless known otherwise).
    pub fn blocks_motion(&self, state: u32) -> bool {
        !Self::flag(&self.passable, state)
    }

    pub fn is_leaves(&self, state: u32) -> bool {
        Self::flag(&self.leaves, state)
    }

    /// Block name an item places, if it is a block item.
    pub fn block_for_item(&self, item: i32) -> Option<&str> {
        self.item_blocks.get(&item).map(String::as_str)
    }

    pub fn block_name(&self, id: usize) -> Option<&str> {
        self.block_names.get(id).map(String::as_str)
    }
}

/// Everything about one Use Item On that decides the placed state.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub clicked: Position,
    pub face: usize,
    pub cursor: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

/// Horizontal direction a yaw looks at.
fn look_horizontal(yaw: f32) -> &'static str {
    let quarter = ((yaw / 90.0 + 0.5).floor() as i32).rem_euclid(4);
    ["south", "west", "north", "east"][quarter as usize]
}

fn opposite(direction: &str) -> &'static str {
    match direction {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        _ => "up",
    }
}

/// Main direction of the view, vertical included.
fn look_direction(yaw: f32, pitch: f32) -> &'static str {
    if pitch < -45.0 {
        "up"
    } else if pitch > 45.0 {
        "down"
    } else {
        look_horizontal(yaw)
    }
}

/// The blocks one placement sets: the main block and, for two-block blocks
/// (doors, tall plants, beds), the other part.
#[derive(Debug, PartialEq)]
pub struct Placed {
    pub blocks: Vec<(Position, u32)>,
}

/// Where a block item goes and as which state. `block_at` reads the world.
/// `None`: the placement is refused.
pub fn place(
    data: &McData,
    rules: &BlockRules,
    block: &str,
    p: Placement,
    block_at: &dyn Fn(Position) -> Option<u32>,
) -> Option<Placed> {
    let blocks = &data.blocks;
    let default = blocks.default_state(block)?;
    let clicked_state = block_at(p.clicked)?;

    // A single slab clicked on its open side, or reached from a neighbour,
    // becomes a double slab.
    if blocks.property(default, "type").is_some() && is_slab(blocks, default) {
        let same = |s: u32| blocks.block_name(s) == Some(block);
        let single = |s: u32| matches!(blocks.property(s, "type"), Some("top" | "bottom"));
        if same(clicked_state) && single(clicked_state) {
            let open = match blocks.property(clicked_state, "type") {
                Some("bottom") => p.face == 1,
                _ => p.face == 0,
            };
            if open {
                let double = blocks.with_property(clicked_state, "type", "double")?;
                return Some(Placed {
                    blocks: vec![(p.clicked, double)],
                });
            }
        }
        let target = offset(p.clicked, p.face);
        if let Some(existing) = block_at(target)
            && same(existing)
            && single(existing)
        {
            let double = blocks.with_property(existing, "type", "double")?;
            return Some(Placed {
                blocks: vec![(target, double)],
            });
        }
    }

    let (target, replacing_clicked) = if rules.is_replaceable(clicked_state) {
        (p.clicked, true)
    } else {
        (offset(p.clicked, p.face), false)
    };
    let replaced = block_at(target)?;
    if !rules.is_replaceable(replaced) {
        return None;
    }
    // Clicking a replaceable block acts as clicking the top of the one below.
    let face = if replacing_clicked { 1 } else { p.face };

    let mut state = default;
    let set = |state: &mut u32, property: &str, value: &str| {
        if let Some(changed) = blocks.with_property(*state, property, value) {
            *state = changed;
        }
    };
    if blocks.property(state, "axis").is_some() {
        let axis = match face {
            0 | 1 => "y",
            2 | 3 => "z",
            _ => "x",
        };
        set(&mut state, "axis", axis);
    }
    if blocks.property(state, "facing").is_some() {
        let horizontal_only = blocks.with_property(state, "facing", "up").is_none();
        let look = if horizontal_only {
            look_horizontal(p.yaw)
        } else {
            look_direction(p.yaw, p.pitch)
        };
        let facing = if BlockRules::flag(&rules.look_facing, state) {
            look
        } else {
            opposite(look)
        };
        set(&mut state, "facing", facing);
    }
    let upper_half = match face {
        0 => true,
        1 => false,
        _ => p.cursor[1] > 0.5,
    };
    if is_slab(blocks, state) {
        set(
            &mut state,
            "type",
            if upper_half { "top" } else { "bottom" },
        );
    }
    if matches!(blocks.property(state, "half"), Some("top" | "bottom")) {
        set(
            &mut state,
            "half",
            if upper_half { "top" } else { "bottom" },
        );
    }
    if blocks.property(state, "waterlogged").is_some() && replaced == rules.water {
        set(&mut state, "waterlogged", "true");
    }

    let mut placed = vec![(target, state)];
    // Two-block blocks: doors and tall plants (`half` lower/upper), beds (`part`).
    if blocks.property(state, "half") == Some("lower") {
        let above = offset(target, 1);
        if !block_at(above).is_some_and(|s| rules.is_replaceable(s)) {
            return None;
        }
        placed.push((above, blocks.with_property(state, "half", "upper")?));
    } else if blocks.property(state, "part") == Some("foot") {
        let facing = blocks.property(state, "facing")?;
        let face = FACE_NAMES.iter().position(|f| *f == facing)?;
        let head = offset(target, face);
        if !block_at(head).is_some_and(|s| rules.is_replaceable(s)) {
            return None;
        }
        placed.push((head, blocks.with_property(state, "part", "head")?));
    }
    Some(Placed { blocks: placed })
}

fn is_slab(blocks: &mc_data::Blocks, state: u32) -> bool {
    blocks
        .block_name(state)
        .is_some_and(|n| n.ends_with("_slab"))
        && blocks.property(state, "type").is_some()
}

/// What breaking the block at `position` changes: the block itself (to air,
/// or water if it was waterlogged) and the other part of a two-block block.
pub fn break_block(
    data: &McData,
    rules: &BlockRules,
    position: Position,
    state: u32,
    block_at: &dyn Fn(Position) -> Option<u32>,
) -> Vec<(Position, u32)> {
    let blocks = &data.blocks;
    let left = |s: u32| {
        if blocks.property(s, "waterlogged") == Some("true") {
            rules.water
        } else {
            rules.air
        }
    };
    let mut changes = vec![(position, left(state))];
    let same_block = |other: u32| blocks.block_name(other) == blocks.block_name(state);
    let partner = match (
        blocks.property(state, "half"),
        blocks.property(state, "part"),
    ) {
        (Some("lower"), _) => Some(offset(position, 1)),
        (Some("upper"), _) => Some(offset(position, 0)),
        (_, Some(part)) => blocks
            .property(state, "facing")
            .and_then(|f| FACE_NAMES.iter().position(|n| *n == f))
            .map(|face| {
                if part == "foot" {
                    offset(position, face)
                } else {
                    offset(position, face ^ 1)
                }
            }),
        _ => None,
    };
    if let Some(partner) = partner
        && let Some(other) = block_at(partner)
        && same_block(other)
    {
        changes.push((partner, left(other)));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaw_to_direction() {
        assert_eq!(look_horizontal(0.0), "south");
        assert_eq!(look_horizontal(90.0), "west");
        assert_eq!(look_horizontal(-90.0), "east");
        assert_eq!(look_horizontal(179.0), "north");
        assert_eq!(look_horizontal(-170.0), "north");
        assert_eq!(look_direction(0.0, 80.0), "down");
    }

    #[test]
    fn faces_pair_with_their_opposites() {
        for face in 0..6 {
            let (a, b) = (FACES[face], FACES[face ^ 1]);
            assert_eq!((a.0 + b.0, a.1 + b.1, a.2 + b.2), (0, 0, 0));
            assert_eq!(opposite(FACE_NAMES[face]), FACE_NAMES[face ^ 1]);
        }
    }
}
