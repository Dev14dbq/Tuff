//! Structure templates (`data/minecraft/structure/*.nbt`, minecraft.wiki
//! "Structure file"): blocks with positions, one or more palettes, block
//! entity data; placed rotated and mirrored around a pivot, through
//! processors (minecraft.wiki "Processor list").

use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;

use mc_nbt::{Compound, Tag};
use mc_worldgen::random::{RandomSource, position_seed};
use serde_json::Value;

use crate::blocks::Facts;
use crate::predicate::{BlockSet, RuleTest};
use crate::rand::WorldgenRandom;
use crate::value::IntProvider;
use crate::{Heightmap, P, Region};
use mc_tick::Dir;

/// A box of blocks, both corners included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BBox {
    pub min: P,
    pub max: P,
}

impl BBox {
    pub fn from_corners(a: P, b: P) -> Self {
        Self {
            min: P::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: P::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    pub fn moved(self, dx: i32, dy: i32, dz: i32) -> Self {
        Self {
            min: self.min.offset(dx, dy, dz),
            max: self.max.offset(dx, dy, dz),
        }
    }

    pub fn inside(&self, p: P) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    pub fn intersects(&self, o: &BBox) -> bool {
        self.max.x >= o.min.x
            && self.min.x <= o.max.x
            && self.max.z >= o.min.z
            && self.min.z <= o.max.z
            && self.max.y >= o.min.y
            && self.min.y <= o.max.y
    }

    pub fn encapsulate(&mut self, p: P) {
        self.min = P::new(
            self.min.x.min(p.x),
            self.min.y.min(p.y),
            self.min.z.min(p.z),
        );
        self.max = P::new(
            self.max.x.max(p.x),
            self.max.y.max(p.y),
            self.max.z.max(p.z),
        );
    }

    pub fn union(self, o: BBox) -> BBox {
        let mut b = self;
        b.encapsulate(o.min);
        b.encapsulate(o.max);
        b
    }

    pub fn center(&self) -> P {
        P::new(
            self.min.x + (self.max.x - self.min.x + 1) / 2,
            self.min.y + (self.max.y - self.min.y + 1) / 2,
            self.min.z + (self.max.z - self.min.z + 1) / 2,
        )
    }

    pub fn x_span(&self) -> i32 {
        self.max.x - self.min.x + 1
    }

    pub fn y_span(&self) -> i32 {
        self.max.y - self.min.y + 1
    }

    pub fn z_span(&self) -> i32 {
        self.max.z - self.min.z + 1
    }

    /// The chunk's column as a box.
    pub fn chunk(cx: i32, cz: i32, min_y: i32, max_y: i32) -> Self {
        Self {
            min: P::new(cx * 16, min_y, cz * 16),
            max: P::new(cx * 16 + 15, max_y, cz * 16 + 15),
        }
    }
}

/// Quarter turns clockwise: none, 90°, 180°, 270°.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rotation(pub u8);

impl Rotation {
    pub const ALL: [Rotation; 4] = [Rotation(0), Rotation(1), Rotation(2), Rotation(3)];

    pub fn then(self, o: Rotation) -> Rotation {
        Rotation((self.0 + o.0) % 4)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mirror {
    #[default]
    None,
    /// z → −z.
    LeftRight,
    /// x → −x.
    FrontBack,
}

/// Where a template's relative position ends up.
pub fn transform(p: P, mirror: Mirror, rot: Rotation, pivot: P) -> P {
    let (mut i, j, mut k) = (p.x, p.y, p.z);
    match mirror {
        Mirror::LeftRight => k = -k,
        Mirror::FrontBack => i = -i,
        Mirror::None => {}
    }
    let (l, m) = (pivot.x, pivot.z);
    match rot.0 {
        3 => P::new(l - m + k, j, l + m - i),
        1 => P::new(l + m - k, j, m - l + i),
        2 => P::new(l + l - i, j, m + m - k),
        _ => P::new(i, j, k),
    }
}

const HORIZONTAL: [&str; 4] = ["north", "east", "south", "west"];

fn turn(d: &str, rot: Rotation) -> &'static str {
    match HORIZONTAL.iter().position(|h| *h == d) {
        Some(i) => HORIZONTAL[(i + rot.0 as usize) % 4],
        None => match d {
            "up" => "up",
            "down" => "down",
            _ => "north",
        },
    }
}

fn flip(d: &str, mirror: Mirror) -> &'static str {
    match (mirror, d) {
        (Mirror::LeftRight, "north") => "south",
        (Mirror::LeftRight, "south") => "north",
        (Mirror::FrontBack, "east") => "west",
        (Mirror::FrontBack, "west") => "east",
        (_, "north") => "north",
        (_, "south") => "south",
        (_, "east") => "east",
        (_, "west") => "west",
        (_, "up") => "up",
        _ => "down",
    }
}

/// Rail shapes: `north_south`, `ascending_east`, `south_west`...
fn map_shape(shape: &str, f: &dyn Fn(&str) -> &'static str) -> String {
    if let Some(d) = shape.strip_prefix("ascending_") {
        return format!("ascending_{}", f(d));
    }
    let Some((a, b)) = shape.split_once('_') else {
        return shape.to_owned();
    };
    if !HORIZONTAL.contains(&a) || !HORIZONTAL.contains(&b) {
        return shape.to_owned();
    }
    let (a, b) = (f(a), f(b));
    // Canonical names: north_south, east_west, south_east, south_west,
    // north_west, north_east.
    let has = |d: &str| a == d || b == d;
    if has("north") && has("south") {
        "north_south".into()
    } else if has("east") && has("west") {
        "east_west".into()
    } else {
        let ns = if has("north") { "north" } else { "south" };
        let ew = if has("east") { "east" } else { "west" };
        format!("{ns}_{ew}")
    }
}

/// The state turned and mirrored as the template is (`mirror` first).
pub fn transform_state(facts: &Facts, state: u32, mirror: Mirror, rot: Rotation) -> u32 {
    if mirror == Mirror::None && rot.0 == 0 {
        return state;
    }
    let Some(key) = facts.blocks.describe(state) else {
        return state;
    };
    let name = facts.block_name(state);
    let Some(inner) = key.find('[').map(|i| &key[i + 1..key.len() - 1]) else {
        return state;
    };
    if inner.is_empty() {
        return state;
    }
    let mut props: Vec<(String, String)> = inner
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let get = |props: &[(String, String)], k: &str| {
        props.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    };
    let set = |props: &mut Vec<(String, String)>, k: &str, v: String| {
        if let Some(e) = props.iter_mut().find(|(n, _)| n == k) {
            e.1 = v;
        }
    };
    // Mirror.
    if mirror != Mirror::None {
        let facing = get(&props, "facing");
        let flipped_axis = facing.as_deref().is_some_and(|f| match mirror {
            Mirror::LeftRight => f == "north" || f == "south",
            Mirror::FrontBack => f == "east" || f == "west",
            Mirror::None => false,
        });
        if let Some(f) = &facing {
            set(&mut props, "facing", flip(f, mirror).to_owned());
        }
        if name.ends_with("_stairs")
            && flipped_axis
            && let Some(shape) = get(&props, "shape")
        {
            let s = match shape.as_str() {
                "inner_left" => "inner_right",
                "inner_right" => "inner_left",
                "outer_left" => "outer_right",
                "outer_right" => "outer_left",
                o => o,
            };
            set(&mut props, "shape", s.to_owned());
        }
        if name.ends_with("_door")
            && let Some(h) = get(&props, "hinge")
        {
            set(
                &mut props,
                "hinge",
                if h == "left" { "right" } else { "left" }.to_owned(),
            );
        }
        // A chest keeps its `type` when mirrored, so a mirrored double
        // chest no longer pairs up and falls apart once placed (see
        // `settle`), as vanilla mansions show.
        if let Some(r) = get(&props, "rotation").and_then(|r| r.parse::<i32>().ok()) {
            let j = if r > 8 { r - 16 } else { r };
            let v = match mirror {
                Mirror::FrontBack => (16 - j).rem_euclid(16),
                Mirror::LeftRight => (8 - j + 16).rem_euclid(16),
                Mirror::None => r,
            };
            set(&mut props, "rotation", v.to_string());
        }
        let sides: Vec<(String, String)> = props
            .iter()
            .filter(|(k, _)| HORIZONTAL.contains(&k.as_str()))
            .cloned()
            .collect();
        for (k, v) in sides {
            set(&mut props, flip(&k, mirror), v);
        }
        if let Some(shape) = get(&props, "shape")
            && !name.ends_with("_stairs")
        {
            set(&mut props, "shape", map_shape(&shape, &|d| flip(d, mirror)));
        }
        if let Some(o) = get(&props, "orientation") {
            let parts: Vec<&str> = o.split('_').map(|p| flip(p, mirror)).collect();
            set(&mut props, "orientation", parts.join("_"));
        }
    }
    // Rotation.
    if rot.0 != 0 {
        if let Some(f) = get(&props, "facing") {
            set(&mut props, "facing", turn(&f, rot).to_owned());
        }
        if rot.0 % 2 == 1
            && let Some(a) = get(&props, "axis")
        {
            let v = match a.as_str() {
                "x" => "z",
                "z" => "x",
                o => o,
            };
            set(&mut props, "axis", v.to_owned());
        }
        if let Some(r) = get(&props, "rotation").and_then(|r| r.parse::<i32>().ok()) {
            set(
                &mut props,
                "rotation",
                ((r + 4 * i32::from(rot.0)) % 16).to_string(),
            );
        }
        let sides: Vec<(String, String)> = props
            .iter()
            .filter(|(k, _)| HORIZONTAL.contains(&k.as_str()))
            .cloned()
            .collect();
        for (k, v) in sides {
            set(&mut props, turn(&k, rot), v);
        }
        if let Some(shape) = get(&props, "shape")
            && !name.ends_with("_stairs")
        {
            set(&mut props, "shape", map_shape(&shape, &|d| turn(d, rot)));
        }
        if let Some(o) = get(&props, "orientation") {
            let parts: Vec<&str> = o.split('_').map(|p| turn(p, rot)).collect();
            set(&mut props, "orientation", parts.join("_"));
        }
    }
    let list: Vec<(&str, &str)> = props
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    facts.state(name, &list).unwrap_or(state)
}

/// One block of a template.
#[derive(Clone, Debug)]
pub struct TBlock {
    pub pos: P,
    pub state: u32,
    pub nbt: Option<Compound>,
}

#[derive(Debug)]
pub struct Template {
    pub size: P,
    pub palettes: Vec<Vec<TBlock>>,
    /// Entities (relative position and data).
    pub entities: Vec<(P, Compound)>,
}

fn ints(t: Option<&Tag>) -> Option<Vec<i32>> {
    match t? {
        Tag::List(l) => l
            .iter()
            .map(|v| match v {
                Tag::Int(i) => Some(*i),
                Tag::Double(d) => Some(d.floor() as i32),
                _ => None,
            })
            .collect(),
        Tag::IntArray(a) => Some(a.clone()),
        _ => None,
    }
}

fn palette_state(facts: &Facts, c: &Compound) -> u32 {
    // 26.3 writes `id` and `properties`, older files `Name` and `Properties`.
    let Some(Tag::String(name)) = c.get("id").or_else(|| c.get("Name")) else {
        return facts.air;
    };
    let Some(mut state) = facts.default_state(name) else {
        return facts.air;
    };
    if let Some(Tag::Compound(props)) = c.get("properties").or_else(|| c.get("Properties")) {
        for (k, v) in props.iter() {
            if let Tag::String(v) = v {
                state = facts.with(state, k, v);
            }
        }
    }
    state
}

impl Template {
    pub fn load(path: &Path, facts: &Facts) -> Option<Self> {
        let raw = std::fs::read(path).ok()?;
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(&raw[..])
            .read_to_end(&mut bytes)
            .ok()?;
        let (_, Tag::Compound(root), _) = mc_nbt::read_named(&bytes).ok()? else {
            return None;
        };
        let size = ints(root.get("size"))?;
        let palette_states = |list: &Tag| -> Vec<u32> {
            match list {
                Tag::List(l) => l
                    .iter()
                    .map(|c| match c {
                        Tag::Compound(c) => palette_state(facts, c),
                        _ => facts.air,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let mut palettes_states: Vec<Vec<u32>> = Vec::new();
        if let Some(p) = root.get("palette") {
            palettes_states.push(palette_states(p));
        } else if let Some(Tag::List(ps)) = root.get("palettes") {
            for p in ps {
                palettes_states.push(palette_states(p));
            }
        }
        let blocks: Vec<(P, i32, Option<Compound>)> = match root.get("blocks") {
            Some(Tag::List(l)) => l
                .iter()
                .filter_map(|b| {
                    let Tag::Compound(b) = b else { return None };
                    let pos = ints(b.get("pos"))?;
                    let state = match b.get("state") {
                        Some(Tag::Int(s)) => *s,
                        _ => return None,
                    };
                    let nbt = match b.get("nbt") {
                        Some(Tag::Compound(n)) => Some(n.clone()),
                        _ => None,
                    };
                    Some((P::new(pos[0], pos[1], pos[2]), state, nbt))
                })
                .collect(),
            _ => Vec::new(),
        };
        let palettes = palettes_states
            .iter()
            .map(|states| {
                let (mut full, mut other, mut entities) = (Vec::new(), Vec::new(), Vec::new());
                for (pos, i, nbt) in &blocks {
                    let state = states.get(*i as usize).copied().unwrap_or(facts.air);
                    let b = TBlock {
                        pos: *pos,
                        state,
                        nbt: nbt.clone(),
                    };
                    if nbt.is_some() {
                        entities.push(b);
                    } else if facts.solid_render(state) {
                        full.push(b);
                    } else {
                        other.push(b);
                    }
                }
                let key = |b: &TBlock| (b.pos.y, b.pos.x, b.pos.z);
                full.sort_by_key(key);
                other.sort_by_key(key);
                entities.sort_by_key(key);
                full.extend(other);
                full.extend(entities);
                full
            })
            .collect();
        let entities = match root.get("entities") {
            Some(Tag::List(l)) => l
                .iter()
                .filter_map(|e| {
                    let Tag::Compound(e) = e else { return None };
                    let pos = ints(e.get("blockPos"))?;
                    let Some(Tag::Compound(nbt)) = e.get("nbt") else {
                        return None;
                    };
                    Some((P::new(pos[0], pos[1], pos[2]), nbt.clone()))
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            size: P::new(size[0], size[1], size[2]),
            palettes,
            entities,
        })
    }

    /// The box the template covers placed at `pos`.
    pub fn bbox(&self, pos: P, rot: Rotation, pivot: P, mirror: Mirror) -> BBox {
        let v = P::new(self.size.x - 1, self.size.y - 1, self.size.z - 1);
        let a = transform(P::new(0, 0, 0), mirror, rot, pivot);
        let b = transform(v, mirror, rot, pivot);
        BBox::from_corners(a, b).moved(pos.x, pos.y, pos.z)
    }

    fn palette(&self, settings: &Settings, pos: P) -> &[TBlock] {
        match self.palettes.len() {
            0 => &[],
            1 => &self.palettes[0],
            n => &self.palettes[settings.with_random(pos, |r| r.next_int_bound(n as i32)) as usize],
        }
    }

    /// Blocks of one kind (jigsaws), placed at `pos` with `settings`.
    pub fn filter(&self, facts: &Facts, pos: P, settings: &Settings, block: u32) -> Vec<TBlock> {
        self.palette(settings, pos)
            .iter()
            .filter(|b| facts.block(b.state) == block)
            .map(|b| TBlock {
                pos: transform(b.pos, settings.mirror, settings.rotation, settings.pivot).add(pos),
                state: transform_state(facts, b.state, settings.mirror, settings.rotation),
                nbt: b.nbt.clone(),
            })
            .collect()
    }

    /// Data markers (structure blocks in data mode): where they end up and
    /// what they say.
    pub fn data_markers(&self, facts: &Facts, offset: P, settings: &Settings) -> Vec<(P, String)> {
        self.palette(settings, offset)
            .iter()
            .filter(|b| facts.is(b.state, "minecraft:structure_block"))
            .filter_map(|b| {
                let nbt = b.nbt.as_ref()?;
                let mode = match nbt.get("mode") {
                    Some(Tag::String(m)) => m.as_str(),
                    _ => "",
                };
                if !mode.eq_ignore_ascii_case("data") {
                    return None;
                }
                let Some(Tag::String(meta)) = nbt.get("metadata") else {
                    return None;
                };
                let pos = transform(b.pos, settings.mirror, settings.rotation, settings.pivot)
                    .add(offset);
                Some((pos, meta.clone()))
            })
            .collect()
    }

    /// Places the template with its corner at `offset`; `world_pivot` is
    /// what processors measure positions from.
    pub fn place(
        &self,
        facts: &Facts,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        offset: P,
        world_pivot: P,
        settings: &Settings,
    ) -> bool {
        let palette = self.palette(settings, offset);
        if palette.is_empty() || self.size.x < 1 || self.size.y < 1 || self.size.z < 1 {
            return false;
        }
        let mut infos: Vec<(TBlock, TBlock)> = Vec::with_capacity(palette.len());
        for b in palette {
            let pos =
                transform(b.pos, settings.mirror, settings.rotation, settings.pivot).add(offset);
            let mut current = Some(TBlock {
                pos,
                state: b.state,
                nbt: b.nbt.clone(),
            });
            for p in &settings.processors {
                let Some(c) = current.take() else { break };
                current = p.process(facts, region, offset, world_pivot, b, c, settings);
            }
            if let Some(c) = current {
                infos.push((b.clone(), c));
            }
        }
        for p in &settings.processors {
            p.finalize(facts, region, settings, &mut infos);
        }
        let mut placed = false;
        let mut settled = Vec::new();
        for (_, info) in infos {
            if let Some(b) = &settings.bbox
                && !b.inside(info.pos)
            {
                continue;
            }
            let mut state = transform_state(facts, info.state, settings.mirror, settings.rotation);
            if settings.waterlog
                && facts.has_property(state, "waterlogged")
                && facts.is_water_source(region.block(info.pos))
            {
                state = facts.with(state, "waterlogged", "true");
            }
            if !region.set(info.pos, state) {
                continue;
            }
            placed = true;
            let name = facts.block_name(state);
            if name.ends_with("chest") || name == "minecraft:ladder" || name.ends_with("wall_torch")
            {
                settled.push(info.pos);
            }
            if let Some(mut nbt) = info.nbt {
                for k in ["x", "y", "z"] {
                    nbt.remove(k);
                }
                if nbt.get("LootTable").is_some() {
                    nbt.insert("LootTableSeed", Tag::Long(r.next_long()));
                }
                if nbt.get("id").is_none()
                    && let Some(id) = block_entity_id(facts.block_name(state))
                {
                    nbt.insert("id", Tag::String(id.into()));
                }
                region.set_block_entity(info.pos, nbt);
            }
        }
        settle(facts, region, &settled);
        placed
    }
}

/// What the shape update after placing a template does: the half of a
/// double chest whose partner is not there becomes single, and a ladder
/// or wall torch with nothing behind it drops. Blocks of the piece in a
/// chunk not decorated yet are not there, so these split at chunk edges
/// too, as in vanilla mansions.
fn settle(facts: &Facts, region: &mut dyn Region, placed: &[P]) {
    let facing = |state: u32| {
        facts
            .blocks
            .property(state, "facing")
            .and_then(Dir::from_name)
    };
    let partner = |state: u32| -> Option<Dir> {
        let f = facing(state)?;
        match facts.blocks.property(state, "type")? {
            "left" => Some(f.cw()),
            "right" => Some(f.ccw()),
            _ => None,
        }
    };
    let at = |p: P, d: Dir| {
        let (x, y, z) = d.offset();
        P::new(p.x + x, p.y + y, p.z + z)
    };
    for &pos in placed {
        let state = region.block(pos);
        let name = facts.block_name(state);
        if let Some(d) = partner(state) {
            let o = region.block(at(pos, d));
            let paired = facts.block_name(o) == name
                && facing(o) == facing(state)
                && partner(o) == Some(d.opposite());
            if !paired {
                region.set(pos, facts.with(state, "type", "single"));
            }
        } else if (name == "minecraft:ladder" || name.ends_with("wall_torch"))
            && let Some(f) = facing(state)
            && !facts.sturdy(region.block(at(pos, f.opposite())), f)
        {
            region.set(pos, facts.air);
        }
    }
}

/// The block entity a block has, for template data without an id.
fn block_entity_id(block: &str) -> Option<&'static str> {
    let b = block.trim_start_matches("minecraft:");
    Some(match b {
        "chest" => "minecraft:chest",
        "trapped_chest" => "minecraft:trapped_chest",
        "barrel" => "minecraft:barrel",
        "furnace" => "minecraft:furnace",
        "blast_furnace" => "minecraft:blast_furnace",
        "smoker" => "minecraft:smoker",
        "spawner" => "minecraft:mob_spawner",
        "dispenser" => "minecraft:dispenser",
        "dropper" => "minecraft:dropper",
        "hopper" => "minecraft:hopper",
        "brewing_stand" => "minecraft:brewing_stand",
        "lectern" => "minecraft:lectern",
        "bell" => "minecraft:bell",
        "suspicious_sand" | "suspicious_gravel" => "minecraft:brushable_block",
        "decorated_pot" => "minecraft:decorated_pot",
        "jigsaw" => "minecraft:jigsaw",
        _ if b.ends_with("_sign") => "minecraft:sign",
        _ if b.ends_with("_bed") => "minecraft:bed",
        _ if b.ends_with("_banner") => "minecraft:banner",
        _ if b.ends_with("shulker_box") => "minecraft:shulker_box",
        _ => return None,
    })
}

/// How a template is placed.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub rotation: Rotation,
    pub mirror: Mirror,
    /// Rotation pivot (relative).
    pub pivot: P,
    /// Only blocks inside are placed (the chunk being decorated).
    pub bbox: Option<BBox>,
    pub processors: Vec<std::sync::Arc<Processor>>,
    pub waterlog: bool,
    /// A fixed random source instead of one per position.
    pub seed: Option<i64>,
    /// One source for all blocks in turn (fossils), instead of one per
    /// position.
    pub shared: Option<std::sync::Arc<std::sync::Mutex<WorldgenRandom>>>,
}

impl Settings {
    /// Runs `f` with the random source a processor uses at `pos`.
    pub fn with_random<T>(&self, pos: P, f: impl FnOnce(&mut WorldgenRandom) -> T) -> T {
        match &self.shared {
            Some(r) => f(&mut r.lock().expect("random lock")),
            None => f(&mut self.random_at(pos)),
        }
    }

    /// The random source a processor uses at `pos`.
    pub fn random_at(&self, pos: P) -> WorldgenRandom {
        match self.seed {
            Some(s) => WorldgenRandom::legacy(s),
            None => WorldgenRandom::legacy(position_seed(pos.x, pos.y, pos.z)),
        }
    }
}

/// A block entity change a rule makes.
#[derive(Debug, Clone)]
pub enum NbtModifier {
    Keep,
    Clear,
    AppendLoot(String),
    AppendStatic(Compound),
}

#[derive(Debug, Clone)]
pub enum PosTest {
    Always,
    /// Chance growing from `min_chance` at `min_dist` to `max_chance` at
    /// `max_dist` from the pivot (`axis`: only along one axis).
    Linear {
        min_chance: f32,
        max_chance: f32,
        min_dist: i32,
        max_dist: i32,
        axis: Option<char>,
    },
}

#[derive(Debug, Clone)]
pub struct Rule {
    input: RuleTest,
    location: RuleTest,
    position: PosTest,
    output: u32,
    nbt: NbtModifier,
}

#[derive(Debug)]
pub enum Processor {
    Rule(Vec<Rule>),
    BlockRot {
        integrity: f32,
        rottable: Option<BlockSet>,
    },
    Protected(BlockSet),
    Capped {
        delegate: Box<Processor>,
        limit: IntProvider,
    },
    /// Drops structure blocks, and air too when `air` is set.
    Ignore {
        air: bool,
    },
    /// Jigsaw blocks become what they say they turn into.
    JigsawReplace,
    /// Each block goes to the ground: heightmap top plus `offset`.
    Gravity {
        map: Heightmap,
        offset: i32,
    },
}

fn parse_rule(v: &Value, facts: &Facts) -> Option<Rule> {
    let position = match v.get("position_predicate") {
        Some(p) => {
            let ty = p["predicate_type"]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("minecraft:");
            match ty {
                "linear_pos" | "axis_aligned_linear_pos" => PosTest::Linear {
                    min_chance: p["min_chance"].as_f64().unwrap_or(0.0) as f32,
                    max_chance: p["max_chance"].as_f64().unwrap_or(0.0) as f32,
                    min_dist: p["min_dist"].as_i64().unwrap_or(0) as i32,
                    max_dist: p["max_dist"].as_i64().unwrap_or(0) as i32,
                    axis: if ty == "axis_aligned_linear_pos" {
                        Some(
                            p["axis"]
                                .as_str()
                                .unwrap_or("y")
                                .chars()
                                .next()
                                .unwrap_or('y'),
                        )
                    } else {
                        None
                    },
                },
                _ => PosTest::Always,
            }
        }
        None => PosTest::Always,
    };
    let nbt = match v.get("block_entity_modifier") {
        Some(m) => match m["type"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("minecraft:")
        {
            "append_loot" => NbtModifier::AppendLoot(m["loot_table"].as_str()?.to_owned()),
            "clear" => NbtModifier::Clear,
            "append_static" => {
                NbtModifier::AppendStatic(match crate::structure::json_to_nbt(&m["data"]) {
                    Tag::Compound(c) => c,
                    _ => Compound::new(),
                })
            }
            _ => NbtModifier::Keep,
        },
        None => NbtModifier::Keep,
    };
    Some(Rule {
        input: RuleTest::parse(&v["input_predicate"], facts)?,
        location: RuleTest::parse(&v["location_predicate"], facts)?,
        position,
        output: crate::state::parse_state(&v["output_state"], facts)?,
        nbt,
    })
}

impl Rule {
    /// Replaces what `input` matches with `output`, anywhere.
    pub fn simple(input: RuleTest, output: u32) -> Self {
        Self {
            input,
            location: RuleTest::Always,
            position: PosTest::Always,
            output,
            nbt: NbtModifier::Keep,
        }
    }
}

impl Processor {
    pub fn parse(v: &Value, facts: &Facts) -> Option<Self> {
        let ty = v["processor_type"]
            .as_str()?
            .trim_start_matches("minecraft:");
        Some(match ty {
            "rule" => Processor::Rule(
                v["rules"]
                    .as_array()?
                    .iter()
                    .filter_map(|r| parse_rule(r, facts))
                    .collect(),
            ),
            "block_rot" => Processor::BlockRot {
                integrity: v["integrity"].as_f64().unwrap_or(1.0) as f32,
                rottable: v
                    .get("rottable_blocks")
                    .and_then(|b| BlockSet::parse(b, facts)),
            },
            "protected_blocks" => Processor::Protected(BlockSet::parse(&v["value"], facts)?),
            "capped" => Processor::Capped {
                delegate: Box::new(Processor::parse(&v["delegate"], facts)?),
                limit: IntProvider::parse(&v["limit"]).unwrap_or(IntProvider::Constant(1)),
            },
            "gravity" => Processor::Gravity {
                map: Heightmap::parse(v["heightmap"].as_str().unwrap_or("WORLD_SURFACE_WG"))?,
                offset: v["offset"].as_i64().unwrap_or(0) as i32,
            },
            _ => return None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn process(
        &self,
        facts: &Facts,
        region: &dyn Region,
        _offset: P,
        pivot: P,
        original: &TBlock,
        current: TBlock,
        settings: &Settings,
    ) -> Option<TBlock> {
        match self {
            Processor::Rule(rules) => {
                let mut r = WorldgenRandom::legacy(position_seed(
                    current.pos.x,
                    current.pos.y,
                    current.pos.z,
                ));
                let world = region.block(current.pos);
                for rule in rules {
                    if !rule.input.test(facts, current.state, current.pos.y, &mut r)
                        || !rule.location.test(facts, world, current.pos.y, &mut r)
                        || !pos_test(&rule.position, original.pos, current.pos, pivot, &mut r)
                    {
                        continue;
                    }
                    let nbt = match &rule.nbt {
                        NbtModifier::Keep => current.nbt.clone(),
                        NbtModifier::Clear => None,
                        NbtModifier::AppendLoot(table) => {
                            let mut c = current.nbt.clone().unwrap_or_default();
                            c.insert("LootTable", Tag::String(table.clone()));
                            c.insert("LootTableSeed", Tag::Long(r.next_long()));
                            Some(c)
                        }
                        NbtModifier::AppendStatic(data) => {
                            let mut c = current.nbt.clone().unwrap_or_default();
                            for (k, v) in data.iter() {
                                c.insert(k, v.clone());
                            }
                            Some(c)
                        }
                    };
                    return Some(TBlock {
                        pos: current.pos,
                        state: rule.output,
                        nbt,
                    });
                }
                Some(current)
            }
            Processor::BlockRot {
                integrity,
                rottable,
            } => {
                let mut r = settings.random_at(current.pos);
                if rottable
                    .as_ref()
                    .is_none_or(|s| s.contains(facts, original.state))
                    && r.next_float() > *integrity
                {
                    return None;
                }
                Some(current)
            }
            Processor::Protected(set) => {
                if set.contains(facts, region.block(current.pos)) {
                    None
                } else {
                    Some(current)
                }
            }
            Processor::Capped { .. } => Some(current),
            Processor::Ignore { air } => {
                let name = facts.block_name(current.state);
                if name == "minecraft:structure_block"
                    || (*air && facts.is(current.state, "minecraft:air"))
                {
                    None
                } else {
                    Some(current)
                }
            }
            Processor::JigsawReplace => {
                if !facts.is(current.state, "minecraft:jigsaw") {
                    return Some(current);
                }
                let final_state = match current.nbt.as_ref().and_then(|n| n.get("final_state")) {
                    Some(Tag::String(s)) => parse_block_string(facts, s),
                    _ => Some(facts.air),
                }?;
                if facts.is(final_state, "minecraft:structure_void") {
                    return None;
                }
                Some(TBlock {
                    pos: current.pos,
                    state: final_state,
                    nbt: None,
                })
            }
            Processor::Gravity { map, offset } => {
                let map = match map {
                    Heightmap::WorldSurfaceWg => Heightmap::WorldSurfaceWg,
                    Heightmap::OceanFloorWg => Heightmap::OceanFloorWg,
                    m => *m,
                };
                let h = region.height(map, current.pos.x, current.pos.z) + offset;
                Some(TBlock {
                    pos: P::new(current.pos.x, h + original.pos.y, current.pos.z),
                    state: current.state,
                    nbt: current.nbt,
                })
            }
        }
    }

    /// After all blocks: the capped processor applies its delegate to at
    /// most `limit` of them, chosen at random.
    fn finalize(
        &self,
        facts: &Facts,
        region: &dyn Region,
        settings: &Settings,
        infos: &mut [(TBlock, TBlock)],
    ) {
        let Processor::Capped { delegate, limit } = self else {
            return;
        };
        if infos.is_empty() {
            return;
        }
        let mut r = settings.random_at(infos[0].1.pos);
        let n = limit.sample(&mut r).max(0) as usize;
        let mut idx: Vec<usize> = (0..infos.len()).collect();
        r.shuffle(&mut idx);
        let mut done = 0;
        for i in idx {
            if done >= n {
                break;
            }
            let (orig, cur) = &infos[i];
            let before = cur.state;
            if let Some(new) =
                delegate.process(facts, region, cur.pos, cur.pos, orig, cur.clone(), settings)
                && new.state != before
            {
                infos[i].1 = new;
                done += 1;
            }
        }
    }
}

fn pos_test(t: &PosTest, local: P, world: P, pivot: P, r: &mut WorldgenRandom) -> bool {
    match t {
        PosTest::Always => true,
        PosTest::Linear {
            min_chance,
            max_chance,
            min_dist,
            max_dist,
            axis,
        } => {
            let _ = local;
            let d = match axis {
                None => {
                    ((world.x - pivot.x).abs()
                        + (world.y - pivot.y).abs()
                        + (world.z - pivot.z).abs()) as f32
                }
                Some('x') => (world.x - pivot.x).abs() as f32,
                Some('z') => (world.z - pivot.z).abs() as f32,
                Some(_) => (world.y - pivot.y).abs() as f32,
            };
            let f = r.next_float();
            let t =
                ((d - *min_dist as f32) / ((*max_dist - *min_dist).max(1)) as f32).clamp(0.0, 1.0);
            f <= min_chance + t * (max_chance - min_chance)
        }
    }
}

/// `minecraft:oak_log[axis=y]` as a state.
pub fn parse_block_string(facts: &Facts, s: &str) -> Option<u32> {
    let (name, props) = match s.find('[') {
        Some(i) => (&s[..i], &s[i + 1..s.len().saturating_sub(1)]),
        None => (s, ""),
    };
    let name = if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    };
    let mut state = facts.default_state(&name)?;
    for kv in props.split(',') {
        if let Some((k, v)) = kv.split_once('=') {
            state = facts.with(state, k.trim(), v.trim());
        }
    }
    Some(state)
}

/// Templates read once each.
#[derive(Debug, Default)]
pub struct Templates {
    cache: std::sync::Mutex<HashMap<String, Option<std::sync::Arc<Template>>>>,
}

impl Templates {
    pub fn get(&self, root: &Path, facts: &Facts, id: &str) -> Option<std::sync::Arc<Template>> {
        let mut cache = self.cache.lock().expect("templates lock");
        if let Some(t) = cache.get(id) {
            return t.clone();
        }
        let path = root.join(format!("{}.nbt", id.trim_start_matches("minecraft:")));
        let t = Template::load(&path, facts).map(std::sync::Arc::new);
        cache.insert(id.to_owned(), t.clone());
        t
    }
}
