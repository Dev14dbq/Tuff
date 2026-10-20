//! Jigsaw structures (minecraft.wiki, "Jigsaw structure", "Template
//! pool"): a start piece from the start pool, then pieces joined at jigsaw
//! blocks, breadth first by priority, each in the space no other piece
//! took, up to a depth and a distance from the start.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use mc_tick::Dir;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use super::template::{BBox, Processor, Rotation, Settings, TBlock, Template};
use super::{Ctx2, Structures};
use crate::feature::Loader;
use crate::placement::Placed;
use crate::rand::WorldgenRandom;
use crate::{P, Region};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    Rigid,
    TerrainMatching,
}

#[derive(Debug)]
pub enum Element {
    Single {
        template: String,
        processors: Vec<Arc<Processor>>,
        projection: Projection,
        legacy: bool,
        waterlog: bool,
    },
    Feature {
        feature: Arc<Placed>,
        projection: Projection,
    },
    List(Vec<Arc<Element>>, Projection),
    Empty,
}

#[derive(Debug, Default)]
pub struct Pool {
    /// Each element as often as its weight.
    pub templates: Vec<Arc<Element>>,
    pub fallback: String,
}

/// A jigsaw block of a placed element.
#[derive(Clone, Debug)]
pub struct Jigsaw {
    pub pos: P,
    pub front: Dir,
    pub top: Dir,
    pub name: String,
    pub target: String,
    pub pool: String,
    pub rollable: bool,
    pub placement_priority: i32,
    pub selection_priority: i32,
}

fn dir(s: &str) -> Dir {
    Dir::from_name(s).unwrap_or(Dir::North)
}

fn jigsaw_of(facts: &crate::Facts, b: &TBlock) -> Jigsaw {
    let orientation = facts.property(b.state, "orientation").unwrap_or("north_up");
    let (front, top) = orientation.split_once('_').unwrap_or(("north", "up"));
    let (front, top) = (dir(front), dir(top));
    let s = |k: &str| match b.nbt.as_ref().and_then(|n| n.get(k)) {
        Some(mc_nbt::Tag::String(v)) => v.clone(),
        _ => String::new(),
    };
    let i = |k: &str| match b.nbt.as_ref().and_then(|n| n.get(k)) {
        Some(mc_nbt::Tag::Int(v)) => *v,
        _ => 0,
    };
    let joint = s("joint");
    let rollable = if joint.is_empty() {
        !matches!(front, Dir::North | Dir::South | Dir::East | Dir::West)
    } else {
        joint == "rollable"
    };
    Jigsaw {
        pos: b.pos,
        front,
        top,
        name: s("name"),
        target: s("target"),
        pool: s("pool"),
        rollable,
        placement_priority: i("placement_priority"),
        selection_priority: i("selection_priority"),
    }
}

fn projection(v: &Value) -> Projection {
    if v["projection"].as_str() == Some("terrain_matching") {
        Projection::TerrainMatching
    } else {
        Projection::Rigid
    }
}

impl Element {
    pub fn parse(v: &Value, l: &Loader, lists: &HashMap<String, Value>) -> Element {
        let ty = v["element_type"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("minecraft:");
        match ty {
            "single_pool_element" | "legacy_single_pool_element" => Element::Single {
                template: v["location"].as_str().unwrap_or("").to_owned(),
                processors: processors(&v["processors"], l, lists),
                projection: projection(v),
                legacy: ty == "legacy_single_pool_element",
                waterlog: v["override_liquid_settings"].as_str() != Some("ignore_waterlogging"),
            },
            "feature_pool_element" => Element::Feature {
                feature: l.placed(&v["feature"]),
                projection: projection(v),
            },
            "list_pool_element" => Element::List(
                v["elements"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| Arc::new(Element::parse(e, l, lists)))
                    .collect(),
                projection(v),
            ),
            _ => Element::Empty,
        }
    }

    pub fn projection(&self) -> Projection {
        match self {
            Element::Single { projection, .. }
            | Element::Feature { projection, .. }
            | Element::List(_, projection) => *projection,
            Element::Empty => Projection::Rigid,
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Element::Empty)
    }

    fn template(&self, s: &Structures) -> Option<Arc<Template>> {
        match self {
            Element::Single { template, .. } => s.template(template),
            _ => None,
        }
    }

    pub fn bbox(&self, s: &Structures, pos: P, rot: Rotation) -> BBox {
        match self {
            Element::Single { .. } => match self.template(s) {
                Some(t) => t.bbox(pos, rot, P::default(), super::template::Mirror::None),
                None => BBox { min: pos, max: pos },
            },
            Element::List(list, _) => list
                .iter()
                .map(|e| e.bbox(s, pos, rot))
                .reduce(BBox::union)
                .unwrap_or(BBox { min: pos, max: pos }),
            Element::Feature { .. } | Element::Empty => BBox { min: pos, max: pos },
        }
    }

    /// Jigsaw blocks, shuffled, then by selection priority.
    pub fn jigsaws(
        &self,
        s: &Structures,
        pos: P,
        rot: Rotation,
        r: &mut WorldgenRandom,
    ) -> Vec<Jigsaw> {
        let facts = s.facts();
        let mut list: Vec<Jigsaw> = match self {
            Element::Single { .. } => {
                let Some(t) = self.template(s) else {
                    return Vec::new();
                };
                let settings = Settings {
                    rotation: rot,
                    ..Settings::default()
                };
                let Some(jigsaw) = facts.block_id("minecraft:jigsaw") else {
                    return Vec::new();
                };
                t.filter(facts, pos, &settings, jigsaw)
                    .iter()
                    .map(|b| jigsaw_of(facts, b))
                    .collect()
            }
            Element::List(list, _) => match list.first() {
                Some(e) => return e.jigsaws(s, pos, rot, r),
                None => Vec::new(),
            },
            Element::Feature { .. } => {
                return vec![Jigsaw {
                    pos,
                    front: Dir::Down,
                    top: Dir::South,
                    name: "minecraft:bottom".into(),
                    target: "minecraft:empty".into(),
                    pool: "minecraft:empty".into(),
                    rollable: true,
                    placement_priority: 0,
                    selection_priority: 0,
                }];
            }
            Element::Empty => Vec::new(),
        };
        r.shuffle(&mut list);
        list.sort_by_key(|j| std::cmp::Reverse(j.selection_priority));
        list
    }

    /// Places the element (the part inside `chunk`).
    #[allow(clippy::too_many_arguments)]
    pub fn place(
        &self,
        cx: &Ctx2,
        region: &mut dyn Region,
        r: &mut WorldgenRandom,
        pos: P,
        pivot: P,
        rot: Rotation,
        chunk: BBox,
    ) -> bool {
        let s = cx.structures;
        match self {
            Element::Single {
                processors,
                projection,
                legacy,
                waterlog,
                ..
            } => {
                let Some(t) = self.template(s) else {
                    return false;
                };
                let mut list: Vec<Arc<Processor>> = Vec::new();
                if !legacy {
                    list.push(Arc::new(Processor::Ignore { air: false }));
                }
                list.push(Arc::new(Processor::JigsawReplace));
                list.extend(processors.iter().cloned());
                if *projection == Projection::TerrainMatching {
                    list.push(Arc::new(Processor::Gravity {
                        map: crate::Heightmap::WorldSurfaceWg,
                        offset: -1,
                    }));
                }
                if *legacy {
                    list.push(Arc::new(Processor::Ignore { air: true }));
                }
                let settings = Settings {
                    rotation: rot,
                    bbox: Some(chunk),
                    processors: list,
                    waterlog: *waterlog,
                    ..Settings::default()
                };
                t.place(s.facts(), region, r, pos, pivot, &settings)
            }
            Element::Feature { feature, .. } => {
                feature.place(&cx.features_ctx(), region, r, pos, None)
            }
            Element::List(list, _) => {
                let mut ok = true;
                for e in list {
                    ok &= e.place(cx, region, r, pos, pivot, rot, chunk);
                }
                ok
            }
            Element::Empty => true,
        }
    }
}

fn processors(v: &Value, l: &Loader, lists: &HashMap<String, Value>) -> Vec<Arc<Processor>> {
    let def = match v {
        Value::String(id) => lists
            .get(id.trim_start_matches("minecraft:"))
            .cloned()
            .unwrap_or(Value::Null),
        o => o.clone(),
    };
    let arr = if def.is_array() {
        def.clone()
    } else {
        def["processors"].clone()
    };
    arr.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Processor::parse(p, l.facts).map(Arc::new))
        .collect()
}

impl Pool {
    pub fn parse(v: &Value, l: &Loader, lists: &HashMap<String, Value>) -> Pool {
        let mut templates = Vec::new();
        for e in v["elements"].as_array().into_iter().flatten() {
            let el = Arc::new(Element::parse(&e["element"], l, lists));
            for _ in 0..e["weight"].as_i64().unwrap_or(1).max(0) {
                templates.push(el.clone());
            }
        }
        Pool {
            templates,
            fallback: v["fallback"]
                .as_str()
                .unwrap_or("minecraft:empty")
                .to_owned(),
        }
    }

    fn shuffled(&self, r: &mut WorldgenRandom) -> Vec<Arc<Element>> {
        let mut list = self.templates.clone();
        r.shuffle(&mut list);
        list
    }

    fn max_size(&self, s: &Structures) -> i32 {
        self.templates
            .iter()
            .map(|e| e.bbox(s, P::default(), Rotation(0)).y_span())
            .max()
            .unwrap_or(0)
    }
}

/// A connection point where terrain is shaped to meet a piece.
#[derive(Clone, Copy, Debug)]
pub struct Junction {
    pub x: i32,
    pub ground_y: i32,
    pub z: i32,
}

/// A placed jigsaw piece.
#[derive(Debug, Clone)]
pub struct Piece {
    pub element: Arc<Element>,
    pub pos: P,
    pub rotation: Rotation,
    pub bbox: BBox,
    pub ground_delta: i32,
    pub junctions: Vec<Junction>,
}

impl Piece {
    fn moved(&mut self, dy: i32) {
        self.pos = self.pos.offset(0, dy, 0);
        self.bbox = self.bbox.moved(0, dy, 0);
    }
}

/// Where children may still go: a box minus the pieces placed in it.
#[derive(Debug)]
struct Free {
    /// Continuous box: min included, max excluded.
    min: P,
    max: P,
    taken: Vec<BBox>,
}

impl Free {
    fn fits(&self, b: &BBox) -> bool {
        b.min.x >= self.min.x
            && b.min.y >= self.min.y
            && b.min.z >= self.min.z
            && b.max.x < self.max.x
            && b.max.y < self.max.y
            && b.max.z < self.max.z
            && !self.taken.iter().any(|t| t.intersects(b))
    }
}

/// The world's surface for placing pieces.
pub trait Ground {
    fn height(&mut self, x: i32, z: i32) -> i32;
}

/// Assembles a jigsaw structure from its start pool; `pos` is the start
/// chunk's corner at the start height. Returns the pieces and the start's
/// centre (for the biome check).
#[allow(clippy::too_many_arguments)]
pub fn assemble(
    s: &Structures,
    r: &mut WorldgenRandom,
    ground: &mut dyn Ground,
    start_pool: &str,
    max_depth: i32,
    pos: P,
    expansion_hack: bool,
    project: bool,
    max_distance: i32,
    min_y: i32,
    max_y: i32,
) -> Option<(Vec<Piece>, P)> {
    let rotation = Rotation(r.next_int_bound(4) as u8);
    let pool = s.pool(start_pool)?;
    if pool.templates.is_empty() {
        return None;
    }
    let element = pool.templates[r.next_int_bound(pool.templates.len() as i32) as usize].clone();
    if element.is_empty() {
        return None;
    }
    let bbox = element.bbox(s, pos, rotation);
    let mut start = Piece {
        element: element.clone(),
        pos,
        rotation,
        bbox,
        ground_delta: 1,
        junctions: Vec::new(),
    };
    let i = (bbox.max.x + bbox.min.x) / 2;
    let j = (bbox.max.z + bbox.min.z) / 2;
    let k = if project {
        pos.y + ground.height(i, j)
    } else {
        pos.y
    };
    let l = bbox.min.y + start.ground_delta;
    start.moved(k - l);
    if start.bbox.min.y < min_y || start.bbox.max.y > max_y {
        return None;
    }
    let center = P::new(i, k, j);
    let mut pieces = vec![start];
    if max_depth <= 0 {
        return Some((pieces, center));
    }
    expand(
        s,
        r,
        ground,
        &mut pieces,
        max_depth,
        expansion_hack,
        max_distance,
        (min_y, max_y),
        center,
    );
    Some((pieces, center))
}

/// Starts a jigsaw at a named connector (`/place jigsaw`): a random piece
/// of `start_pool` goes where its jigsaw named `target` is at `anchor`.
/// `Err(true)` for a pool that does not exist, `Err(false)` when nothing
/// could start.
#[allow(clippy::too_many_arguments)]
pub fn assemble_at(
    s: &Structures,
    r: &mut WorldgenRandom,
    ground: &mut dyn Ground,
    start_pool: &str,
    target: &str,
    max_depth: i32,
    anchor: P,
    min_y: i32,
    max_y: i32,
) -> Result<Vec<Piece>, bool> {
    let rotation = Rotation(r.next_int_bound(4) as u8);
    let pool = s.pool(start_pool).ok_or(true)?;
    if pool.templates.is_empty() {
        return Err(false);
    }
    let element = pool.templates[r.next_int_bound(pool.templates.len() as i32) as usize].clone();
    if element.is_empty() {
        return Err(false);
    }
    let local = element
        .jigsaws(s, P::default(), rotation, r)
        .into_iter()
        .find(|j| j.name == target)
        .ok_or(false)?;
    let pos = P::new(
        anchor.x - local.pos.x,
        anchor.y - local.pos.y,
        anchor.z - local.pos.z,
    );
    let bbox = element.bbox(s, pos, rotation);
    let start = Piece {
        element,
        pos,
        rotation,
        bbox,
        ground_delta: 1,
        junctions: Vec::new(),
    };
    let center = P::new(
        (bbox.max.x + bbox.min.x) / 2,
        bbox.min.y,
        (bbox.max.z + bbox.min.z) / 2,
    );
    let mut pieces = vec![start];
    if max_depth > 0 {
        expand(s, r, ground, &mut pieces, max_depth, false, 128, (min_y, max_y), center);
    }
    Ok(pieces)
}

/// Grows `pieces` (just the start) with children up to `max_depth`.
#[allow(clippy::too_many_arguments)]
fn expand(
    s: &Structures,
    r: &mut WorldgenRandom,
    ground: &mut dyn Ground,
    pieces: &mut Vec<Piece>,
    max_depth: i32,
    expansion_hack: bool,
    max_distance: i32,
    (min_y, max_y): (i32, i32),
    center: P,
) {
    let (i, k, j) = (center.x, center.y, center.z);
    let outer = Rc::new(RefCell::new(Free {
        min: P::new(
            i - max_distance,
            (k - max_distance).max(min_y),
            j - max_distance,
        ),
        max: P::new(
            i + max_distance + 1,
            (k + max_distance + 1).min(max_y + 1),
            j + max_distance + 1,
        ),
        taken: vec![pieces[0].bbox],
    }));
    // (piece index, free space, depth), highest priority first, in order.
    // (priority, order, piece, free space, depth)
    type Waiting = (i32, u64, usize, Rc<RefCell<Free>>, i32);
    let mut queue: Vec<Waiting> = Vec::new();
    let mut seq = 0u64;
    let mut current = Some((0usize, outer, 0i32));
    while let Some((index, free, depth)) = current.take() {
        place_children(
            s,
            r,
            ground,
            pieces,
            index,
            free,
            depth,
            max_depth,
            expansion_hack,
            &mut |pi, f, d, prio| {
                queue.push((prio, seq, pi, f, d));
                seq += 1;
            },
        );
        if let Some(best) = queue
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.0.cmp(&b.1.0).then(b.1.1.cmp(&a.1.1)))
            .map(|(i, _)| i)
        {
            let (_, _, pi, f, d) = queue.remove(best);
            current = Some((pi, f, d));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn place_children(
    s: &Structures,
    r: &mut WorldgenRandom,
    ground: &mut dyn Ground,
    pieces: &mut Vec<Piece>,
    index: usize,
    free: Rc<RefCell<Free>>,
    depth: i32,
    max_depth: i32,
    expansion_hack: bool,
    push: &mut dyn FnMut(usize, Rc<RefCell<Free>>, i32, i32),
) {
    let piece = pieces[index].clone();
    let rigid = piece.element.projection() == Projection::Rigid;
    let mut inner: Option<Rc<RefCell<Free>>> = None;
    let box_min_y = piece.bbox.min.y;
    'jigsaws: for jig in piece.element.jigsaws(s, piece.pos, piece.rotation, r) {
        let target = jig.pos.rel(jig.front);
        let j = jig.pos.y - box_min_y;
        let mut k = i32::MIN;
        let Some(pool) = s.pool(&jig.pool) else {
            continue;
        };
        if pool.templates.is_empty() && !is_empty_pool(&jig.pool) {
            continue;
        }
        let Some(fallback) = s.pool(&pool.fallback) else {
            continue;
        };
        if fallback.templates.is_empty() && !is_empty_pool(&pool.fallback) {
            continue;
        }
        let free2 = if piece.bbox.inside(target) {
            inner
                .get_or_insert_with(|| {
                    Rc::new(RefCell::new(Free {
                        min: piece.bbox.min,
                        max: piece.bbox.max.offset(1, 1, 1),
                        taken: Vec::new(),
                    }))
                })
                .clone()
        } else {
            free.clone()
        };
        let mut candidates: Vec<Arc<Element>> = Vec::new();
        if depth != max_depth {
            candidates.extend(pool.shuffled(r));
        }
        candidates.extend(fallback.shuffled(r));
        let l = jig.placement_priority;
        for chosen in candidates {
            if chosen.is_empty() {
                break;
            }
            let mut rotations = Rotation::ALL.to_vec();
            r.shuffle(&mut rotations);
            for rot2 in rotations {
                let list2 = chosen.jigsaws(s, P::default(), rot2, r);
                let box2 = chosen.bbox(s, P::default(), rot2);
                let m = if !expansion_hack || box2.y_span() > 16 {
                    0
                } else {
                    list2
                        .iter()
                        .map(|j2| {
                            if !box2.inside(j2.pos.rel(j2.front)) {
                                return 0;
                            }
                            let a = s.pool(&j2.pool).map_or(0, |p| p.max_size(s));
                            let b = s
                                .pool(&j2.pool)
                                .and_then(|p| s.pool(&p.fallback))
                                .map_or(0, |p| p.max_size(s));
                            a.max(b)
                        })
                        .max()
                        .unwrap_or(0)
                };
                for j2 in &list2 {
                    if !(j2.front == jig.front.opposite()
                        && (jig.rollable || jig.top == j2.top)
                        && jig.target == j2.name)
                    {
                        continue;
                    }
                    let delta = P::new(
                        target.x - j2.pos.x,
                        target.y - j2.pos.y,
                        target.z - j2.pos.z,
                    );
                    let box3 = chosen.bbox(s, delta, rot2);
                    let n = box3.min.y;
                    let rigid2 = chosen.projection() == Projection::Rigid;
                    let o = j2.pos.y;
                    let p = j - o + jig.front.offset().1;
                    let q = if rigid && rigid2 {
                        box_min_y + p
                    } else {
                        if k == i32::MIN {
                            k = ground.height(jig.pos.x, jig.pos.z);
                        }
                        k - o
                    };
                    let rr = q - n;
                    let mut box4 = box3.moved(0, rr, 0);
                    let bp4 = delta.offset(0, rr, 0);
                    if m > 0 {
                        let sy = (m + 1).max(box4.max.y - box4.min.y);
                        box4.encapsulate(P::new(box4.min.x, box4.min.y + sy, box4.min.z));
                    }
                    if !free2.borrow().fits(&box4) {
                        continue;
                    }
                    free2.borrow_mut().taken.push(box4);
                    let sgd = piece.ground_delta;
                    let t = if rigid2 { sgd - p } else { 1 };
                    let u = if rigid {
                        box_min_y + j
                    } else if rigid2 {
                        q + o
                    } else {
                        if k == i32::MIN {
                            k = ground.height(jig.pos.x, jig.pos.z);
                        }
                        k + p / 2
                    };
                    pieces[index].junctions.push(Junction {
                        x: target.x,
                        ground_y: u - j + sgd,
                        z: target.z,
                    });
                    let child = Piece {
                        element: chosen.clone(),
                        pos: bp4,
                        rotation: rot2,
                        bbox: box4,
                        ground_delta: t,
                        junctions: vec![Junction {
                            x: jig.pos.x,
                            ground_y: u - o + t,
                            z: jig.pos.z,
                        }],
                    };
                    pieces.push(child);
                    if depth < max_depth {
                        push(pieces.len() - 1, free2.clone(), depth + 1, l);
                    }
                    continue 'jigsaws;
                }
            }
        }
    }
}

fn is_empty_pool(id: &str) -> bool {
    id == "minecraft:empty" || id == "empty"
}
