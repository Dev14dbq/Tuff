//! What features need to know about block states: names and properties,
//! block tags from the data pack, fluids, solidity and sturdy faces (the
//! observed facts the tick logic uses), and whether a block survives where
//! it is put (the tick logic's support rules).

use std::collections::HashMap;

use mc_data::{McData, resolve_tags};
use mc_tick::{BlockPos, BlockRules, Dir, Shapes};

use crate::{P, Region};

/// The fluid in a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fluid {
    None,
    Water { source: bool },
    Lava { source: bool },
}

impl Fluid {
    pub fn is_none(self) -> bool {
        self == Fluid::None
    }

    /// The fluid's registry name (`minecraft:water` for a source,
    /// `minecraft:flowing_water` otherwise).
    pub fn name(self) -> &'static str {
        match self {
            Fluid::None => "minecraft:empty",
            Fluid::Water { source: true } => "minecraft:water",
            Fluid::Water { source: false } => "minecraft:flowing_water",
            Fluid::Lava { source: true } => "minecraft:lava",
            Fluid::Lava { source: false } => "minecraft:flowing_lava",
        }
    }
}

/// A block tag, resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag(usize);

#[derive(Debug)]
pub struct Facts {
    pub blocks: mc_data::Blocks,
    block_of: Vec<u32>,
    block_ids: HashMap<String, u32>,
    block_names: Vec<String>,
    defaults: Vec<u32>,
    tag_index: HashMap<String, usize>,
    tag_members: Vec<Vec<bool>>,
    fluid: Vec<Fluid>,
    pub shapes: Shapes,
    pub rules: BlockRules,
    pub air: u32,
    pub cave_air: u32,
    pub water: u32,
    pub lava: u32,
    pub stone: u32,
    pub bedrock_block: u32,
    pub vine_block: u32,
}

impl Facts {
    pub fn new(data: &McData, shapes: Shapes, rules: BlockRules) -> Result<Self, String> {
        let blocks = data.blocks.clone();
        let registry = data
            .builtin
            .get("minecraft:block")
            .ok_or("minecraft:block missing from the registries report")?;
        let mut block_names = vec![String::new(); registry.len()];
        for (name, &id) in registry {
            if let Some(slot) = block_names.get_mut(id as usize) {
                slot.clone_from(name);
            }
        }
        let states = blocks.state_count() as usize;
        let mut block_of = vec![0u32; states];
        let mut defaults = vec![0u32; block_names.len()];
        for (id, name) in block_names.iter().enumerate() {
            if let Some(info) = blocks.get(name) {
                defaults[id] = info.default_state;
                for s in info.first_state..=info.last_state {
                    block_of[s as usize] = id as u32;
                }
            }
        }
        let pack = data.data_pack();
        let raw = pack.tags("minecraft:block").map_err(|e| e.to_string())?;
        let (tags, _) = resolve_tags(&raw, |name| registry.get(name).copied());
        let mut tag_index = HashMap::new();
        let mut tag_members = Vec::new();
        for (name, members) in &tags {
            let mut set = vec![false; block_names.len()];
            for &m in members {
                if let Some(slot) = set.get_mut(m as usize) {
                    *slot = true;
                }
            }
            tag_index.insert(name.clone(), tag_members.len());
            tag_members.push(set);
        }
        let mut fluid = vec![Fluid::None; states];
        for (s, f) in fluid.iter_mut().enumerate() {
            let s = s as u32;
            let name = blocks.block_name(s).unwrap_or("");
            let level = blocks
                .property(s, "level")
                .and_then(|l| l.parse::<u8>().ok());
            *f = match name {
                "minecraft:water" => Fluid::Water {
                    source: level == Some(0),
                },
                "minecraft:lava" => Fluid::Lava {
                    source: level == Some(0),
                },
                "minecraft:bubble_column"
                | "minecraft:kelp"
                | "minecraft:kelp_plant"
                | "minecraft:seagrass"
                | "minecraft:tall_seagrass" => Fluid::Water { source: true },
                _ if blocks.property(s, "waterlogged") == Some("true") => {
                    Fluid::Water { source: true }
                }
                _ => Fluid::None,
            };
        }
        let state = |n: &str| blocks.default_state(n).ok_or(format!("no {n}"));
        Ok(Self {
            air: state("minecraft:air")?,
            cave_air: state("minecraft:cave_air")?,
            water: state("minecraft:water")?,
            lava: state("minecraft:lava")?,
            stone: state("minecraft:stone")?,
            bedrock_block: registry["minecraft:bedrock"] as u32,
            vine_block: registry["minecraft:vine"] as u32,
            block_ids: registry
                .iter()
                .map(|(k, &v)| (k.clone(), v as u32))
                .collect(),
            blocks,
            block_of,
            block_names,
            defaults,
            tag_index,
            tag_members,
            fluid,
            shapes,
            rules,
        })
    }

    /// Block registry id of a state.
    pub fn block(&self, state: u32) -> u32 {
        self.block_of.get(state as usize).copied().unwrap_or(0)
    }

    pub fn block_id(&self, name: &str) -> Option<u32> {
        self.block_ids.get(name).copied()
    }

    pub fn block_name(&self, state: u32) -> &str {
        self.block_names
            .get(self.block(state) as usize)
            .map_or("", String::as_str)
    }

    pub fn default_of(&self, block: u32) -> u32 {
        self.defaults
            .get(block as usize)
            .copied()
            .unwrap_or(self.air)
    }

    pub fn default_state(&self, name: &str) -> Option<u32> {
        self.blocks.default_state(name)
    }

    pub fn is(&self, state: u32, name: &str) -> bool {
        self.block_name(state) == name
    }

    pub fn tag(&self, name: &str) -> Option<Tag> {
        let name = name.trim_start_matches('#');
        let full = if name.contains(':') {
            name.to_owned()
        } else {
            format!("minecraft:{name}")
        };
        self.tag_index.get(&full).copied().map(Tag)
    }

    pub fn in_tag(&self, state: u32, tag: Tag) -> bool {
        self.tag_members[tag.0]
            .get(self.block(state) as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Whether the state is in the named tag (false for unknown tags).
    pub fn in_named(&self, state: u32, tag: &str) -> bool {
        self.tag(tag).is_some_and(|t| self.in_tag(state, t))
    }

    /// Blocks of a tag.
    pub fn tag_blocks(&self, tag: Tag) -> Vec<u32> {
        self.tag_members[tag.0]
            .iter()
            .enumerate()
            .filter(|(_, m)| **m)
            .map(|(i, _)| i as u32)
            .collect()
    }

    pub fn is_air(&self, state: u32) -> bool {
        self.blocks.is_air(state)
    }

    pub fn fluid(&self, state: u32) -> Fluid {
        self.fluid
            .get(state as usize)
            .copied()
            .unwrap_or(Fluid::None)
    }

    pub fn is_water_source(&self, state: u32) -> bool {
        self.fluid(state) == Fluid::Water { source: true }
    }

    /// The legacy "solid" flag: blocks movement.
    pub fn solid(&self, state: u32) -> bool {
        !self.is_air(state) && self.rules.blocks_motion(state)
    }

    pub fn blocks_motion(&self, state: u32) -> bool {
        self.rules.blocks_motion(state)
    }

    pub fn is_leaves(&self, state: u32) -> bool {
        self.rules.is_leaves(state)
    }

    pub fn replaceable(&self, state: u32) -> bool {
        self.rules.is_replaceable(state)
    }

    /// Whether the face of `state` on side `dir` is full and sturdy.
    pub fn sturdy(&self, state: u32, dir: Dir) -> bool {
        use mc_tick::support::{BOTTOM_FULL, TOP_FULL};
        match dir {
            Dir::Up => self.shapes.support(state) & TOP_FULL != 0,
            Dir::Down => self.shapes.support(state) & BOTTOM_FULL != 0,
            d => self.shapes.face_attaches(state, d.opposite()),
        }
    }

    /// A full opaque cube (grows things on top, holds leaf litter).
    pub fn solid_render(&self, state: u32) -> bool {
        self.sturdy(state, Dir::Up) && self.sturdy(state, Dir::Down) && !self.is_leaves(state)
    }

    pub fn property<'a>(&'a self, state: u32, name: &str) -> Option<&'a str> {
        self.blocks.property(state, name)
    }

    /// The state with a property changed; the state itself if it has no
    /// such property.
    pub fn with(&self, state: u32, name: &str, value: &str) -> u32 {
        self.blocks
            .with_property(state, name, value)
            .unwrap_or(state)
    }

    pub fn has_property(&self, state: u32, name: &str) -> bool {
        self.property(state, name).is_some()
    }

    pub fn state(&self, name: &str, properties: &[(&str, &str)]) -> Option<u32> {
        self.blocks.state_id(name, properties)
    }

    /// Whether `state` stays at `p` (plants on their soil, torches...).
    pub fn survives(&self, region: &dyn Region, p: P, state: u32) -> bool {
        let view = View { region };
        let ctx = mc_tick::Ctx {
            shapes: &self.shapes,
            facts: &self.rules,
        };
        mc_tick::support::can_survive(&view, ctx, BlockPos::new(p.x, p.y, p.z), state)
    }

    /// Dirt-like blocks trees and plants grow on (`#dirt`).
    pub fn is_dirt(&self, state: u32) -> bool {
        self.in_named(state, "minecraft:dirt")
    }

    /// Grass block, dirt and the like (`Feature.isGrassOrDirt`).
    pub fn is_grass_or_dirt(&self, state: u32) -> bool {
        self.is_dirt(state) || self.is(state, "minecraft:farmland")
    }
}

/// A region seen by the tick logic's support rules.
struct View<'a> {
    region: &'a dyn Region,
}

impl mc_tick::Level for View<'_> {
    fn block(&self, p: BlockPos) -> u32 {
        self.region.block(P::new(p.x, p.y, p.z))
    }
    fn set(&mut self, _p: BlockPos, _state: u32) -> bool {
        false
    }
    fn game_time(&self) -> i64 {
        0
    }
    fn schedule_fluid_tick(&mut self, _p: BlockPos, _fluid: u32, _delay: i64) {}
    fn schedule_block_tick(&mut self, _p: BlockPos, _block: u32, _delay: i64, _priority: i32) {}
    fn is_block_tick_scheduled(&self, _p: BlockPos, _block: u32) -> bool {
        false
    }
    fn in_world(&self, p: BlockPos) -> bool {
        let h = self.region.heights();
        p.y >= h.min_y && p.y < h.min_y + h.height
    }
    fn spawn_falling_block(&mut self, _p: BlockPos, _state: u32) {}
}
