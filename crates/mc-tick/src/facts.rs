//! Facts about block states the block logic needs and the data reports do
//! not give directly: what can be replaced, what has no collision, leaves,
//! which item places which block. Built from block tags and names; a first
//! approximation until shapes are known (see `observed/` tables).

use std::collections::HashMap;

use mc_data::{McData, resolve_tags};

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

/// Block items whose block has another name (minecraft.wiki, each item's
/// page).
const ITEM_BLOCKS: &[(&str, &str)] = &[
    ("minecraft:wheat_seeds", "minecraft:wheat"),
    ("minecraft:beetroot_seeds", "minecraft:beetroots"),
    ("minecraft:carrot", "minecraft:carrots"),
    ("minecraft:potato", "minecraft:potatoes"),
    ("minecraft:melon_seeds", "minecraft:melon_stem"),
    ("minecraft:pumpkin_seeds", "minecraft:pumpkin_stem"),
    ("minecraft:torchflower_seeds", "minecraft:torchflower_crop"),
    ("minecraft:pitcher_pod", "minecraft:pitcher_crop"),
    ("minecraft:sweet_berries", "minecraft:sweet_berry_bush"),
    ("minecraft:glow_berries", "minecraft:cave_vines"),
    ("minecraft:cocoa_beans", "minecraft:cocoa"),
    ("minecraft:redstone", "minecraft:redstone_wire"),
    ("minecraft:string", "minecraft:tripwire"),
    ("minecraft:bamboo", "minecraft:bamboo_sapling"),
];

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
    pub fn new(data: &McData) -> Result<Self, String> {
        let blocks = &data.blocks;
        let states = blocks.state_count() as usize;
        let registry = data
            .builtin
            .get("minecraft:block")
            .ok_or("minecraft:block missing from the registries report")?;
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
        let block_tags = pack.tags("minecraft:block").map_err(|e| e.to_string())?;
        let (tags, _) = resolve_tags(&block_tags, |name| registry.get(name).copied());
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
            .ok_or("minecraft:item missing from the registries report")?;
        let mut item_blocks: HashMap<i32, String> = items
            .iter()
            .filter(|(name, _)| blocks.get(name).is_some())
            .map(|(name, &id)| (id, name.clone()))
            .collect();
        // The rest are named after what they grow into or lay down.
        for (item, block) in ITEM_BLOCKS {
            if let Some(&id) = items.get(*item)
                && blocks.get(block).is_some()
            {
                item_blocks.insert(id, (*block).to_owned());
            }
        }
        Ok(Self {
            replaceable,
            passable,
            leaves,
            item_blocks,
            block_names,
            look_facing,
            air: blocks.default_state("minecraft:air").ok_or("no air")?,
            water: blocks.default_state("minecraft:water").ok_or("no water")?,
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

    /// Whether a placed block's `facing` is the way the player looks (stairs,
    /// doors, gates, beds) rather than towards the player.
    pub fn faces_look_direction(&self, state: u32) -> bool {
        Self::flag(&self.look_facing, state)
    }

    pub fn block_name(&self, id: usize) -> Option<&str> {
        self.block_names.get(id).map(String::as_str)
    }
}
