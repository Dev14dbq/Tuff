//! Trees (minecraft.wiki, "Tree", "Configured feature" → `tree`): a trunk
//! placer grows the logs and says where foliage goes, a foliage placer
//! grows leaves around those points, an optional root placer grows roots
//! below, and decorators add vines, cocoa, bee nests and leaf litter.
//! Leaves get their distance to the nearest log, as a grown tree has.

use std::collections::{HashMap, HashSet, VecDeque};

use mc_tick::Dir;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::Facts;
use crate::feature::{Ctx, Loader};
use crate::predicate::{BlockSet, Predicate};
use crate::rand::WorldgenRandom;
use crate::state::StateProvider;
use crate::value::IntProvider;
use crate::{Heightmap, P, Region};

const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

fn random_horizontal(r: &mut WorldgenRandom) -> Dir {
    HORIZONTAL[r.next_int_bound(4) as usize]
}

fn int(v: &Value, k: &str, default: i32) -> i32 {
    v.get(k)
        .and_then(Value::as_i64)
        .map_or(default, |n| n as i32)
}

fn ip(v: &Value, k: &str) -> IntProvider {
    IntProvider::parse(&v[k]).unwrap_or(IntProvider::Constant(0))
}

fn chance(v: &Value, k: &str) -> f32 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0) as f32
}

#[derive(Debug)]
enum Size {
    Two {
        limit: i32,
        lower: i32,
        upper: i32,
        min_clipped: Option<i32>,
    },
    Three {
        limit: i32,
        upper_limit: i32,
        lower: i32,
        middle: i32,
        upper: i32,
        min_clipped: Option<i32>,
    },
}

impl Size {
    fn parse(v: &Value) -> Self {
        let clipped = v
            .get("min_clipped_height")
            .and_then(Value::as_i64)
            .map(|n| n as i32);
        if v["type"].as_str() == Some("minecraft:three_layers_feature_size") {
            Size::Three {
                limit: int(v, "limit", 1),
                upper_limit: int(v, "upper_limit", 1),
                lower: int(v, "lower_size", 0),
                middle: int(v, "middle_size", 1),
                upper: int(v, "upper_size", 1),
                min_clipped: clipped,
            }
        } else {
            Size::Two {
                limit: int(v, "limit", 1),
                lower: int(v, "lower_size", 0),
                upper: int(v, "upper_size", 1),
                min_clipped: clipped,
            }
        }
    }

    fn at(&self, height: i32, y: i32) -> i32 {
        match *self {
            Size::Two {
                limit,
                lower,
                upper,
                ..
            } => {
                if y < limit {
                    lower
                } else {
                    upper
                }
            }
            Size::Three {
                limit,
                upper_limit,
                lower,
                middle,
                upper,
                ..
            } => {
                if y < limit {
                    lower
                } else if y >= height - upper_limit {
                    upper
                } else {
                    middle
                }
            }
        }
    }

    fn min_clipped(&self) -> Option<i32> {
        match *self {
            Size::Two { min_clipped, .. } | Size::Three { min_clipped, .. } => min_clipped,
        }
    }
}

#[derive(Debug)]
enum TrunkKind {
    Straight,
    Forking,
    Giant,
    MegaJungle,
    DarkOak,
    Fancy,
    Bending {
        min_height_for_leaves: i32,
        bend_length: IntProvider,
    },
    UpwardsBranching {
        extra_branch_steps: IntProvider,
        place_branch_chance: f32,
        extra_branch_length: IntProvider,
        can_grow_through: Option<BlockSet>,
    },
    Cherry {
        branch_count: IntProvider,
        horizontal_length: IntProvider,
        start: (i32, i32),
        end: IntProvider,
    },
    Poplar {
        branch_amount: IntProvider,
        above_branches: i32,
    },
}

#[derive(Debug)]
struct Trunk {
    base: i32,
    rand_a: i32,
    rand_b: i32,
    kind: TrunkKind,
}

#[derive(Debug)]
enum FoliageKind {
    Blob {
        height: i32,
    },
    Fancy {
        height: i32,
    },
    Bush {
        height: i32,
    },
    Acacia,
    DarkOak,
    Pine {
        height: IntProvider,
    },
    Spruce {
        trunk_height: IntProvider,
    },
    MegaPine {
        crown_height: IntProvider,
    },
    Jungle {
        height: i32,
    },
    RandomSpread {
        height: IntProvider,
        attempts: i32,
    },
    Cherry {
        height: IntProvider,
        wide_hole: f32,
        corner_hole: f32,
        hanging: f32,
        hanging_extension: f32,
    },
    Poplar {
        height: IntProvider,
        side_hole: f32,
    },
}

#[derive(Debug)]
struct Foliage {
    radius: IntProvider,
    offset: IntProvider,
    kind: FoliageKind,
}

#[derive(Debug)]
struct Roots {
    trunk_offset_y: IntProvider,
    provider: StateProvider,
    above: Option<(f32, StateProvider)>,
    can_grow_through: Option<BlockSet>,
    muddy_in: Option<BlockSet>,
    muddy: Option<StateProvider>,
    max_length: i32,
    max_width: i32,
    skew: f32,
}

#[derive(Debug)]
enum Decorator {
    Beehive(f32),
    PlaceOnGround {
        tries: i32,
        radius: i32,
        height: i32,
        provider: StateProvider,
    },
    LeaveVine(f32),
    TrunkVine,
    Cocoa(f32),
    AlterGround(StateProvider),
    AttachedToLeaves {
        chance: f32,
        xz: i32,
        y: i32,
        provider: StateProvider,
        empty: i32,
        dirs: Vec<Dir>,
    },
    AttachedToLogs {
        chance: f32,
        provider: StateProvider,
        dirs: Vec<Dir>,
    },
    PaleMoss {
        ground: f32,
        leaves: f32,
        trunk: f32,
    },
    CreakingHeart(f32),
    ShelfMushroom(f32),
}

#[derive(Debug)]
pub struct Tree {
    trunk_provider: StateProvider,
    foliage_provider: StateProvider,
    below_trunk: Option<StateProvider>,
    trunk: Trunk,
    foliage: Foliage,
    roots: Option<Roots>,
    size: Size,
    decorators: Vec<Decorator>,
    ignore_vines: bool,
}

#[derive(Debug)]
pub struct FallenTree {
    trunk_provider: StateProvider,
    length: IntProvider,
    stump: Vec<Decorator>,
    log: Vec<Decorator>,
}

fn dirs(v: &Value) -> Vec<Dir> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str().and_then(Dir::from_name))
        .collect()
}

fn decorators(v: &Value, l: &Loader) -> Vec<Decorator> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let ty = d["type"].as_str()?.trim_start_matches("minecraft:");
            Some(match ty {
                "beehive" => Decorator::Beehive(chance(d, "probability")),
                "place_on_ground" => Decorator::PlaceOnGround {
                    tries: int(d, "tries", 128),
                    radius: int(d, "radius", 2),
                    height: int(d, "height", 1),
                    provider: l.provider(&d["block_state_provider"])?,
                },
                "leave_vine" => Decorator::LeaveVine(chance(d, "probability")),
                "trunk_vine" => Decorator::TrunkVine,
                "cocoa" => Decorator::Cocoa(chance(d, "probability")),
                "alter_ground" => Decorator::AlterGround(l.provider(&d["provider"])?),
                "attached_to_leaves" => Decorator::AttachedToLeaves {
                    chance: chance(d, "probability"),
                    xz: int(d, "exclusion_radius_xz", 0),
                    y: int(d, "exclusion_radius_y", 0),
                    provider: l.provider(&d["block_provider"])?,
                    empty: int(d, "required_empty_blocks", 1),
                    dirs: dirs(&d["directions"]),
                },
                "attached_to_logs" => Decorator::AttachedToLogs {
                    chance: chance(d, "probability"),
                    provider: l.provider(&d["block_provider"])?,
                    dirs: dirs(&d["directions"]),
                },
                "pale_moss" => Decorator::PaleMoss {
                    ground: chance(d, "ground_probability"),
                    leaves: chance(d, "leaves_probability"),
                    trunk: chance(d, "trunk_probability"),
                },
                "creaking_heart" => Decorator::CreakingHeart(chance(d, "probability")),
                "shelf_mushroom" => Decorator::ShelfMushroom(chance(d, "probability")),
                _ => return None,
            })
        })
        .collect()
}

impl Tree {
    pub fn parse(v: &Value, l: &Loader) -> Option<Self> {
        let t = &v["trunk_placer"];
        let set = |x: &Value| BlockSet::parse(x, l.facts);
        let kind = match t["type"].as_str()?.trim_start_matches("minecraft:") {
            "straight_trunk_placer" => TrunkKind::Straight,
            "forking_trunk_placer" => TrunkKind::Forking,
            "giant_trunk_placer" => TrunkKind::Giant,
            "mega_jungle_trunk_placer" => TrunkKind::MegaJungle,
            "dark_oak_trunk_placer" => TrunkKind::DarkOak,
            "fancy_trunk_placer" => TrunkKind::Fancy,
            "bending_trunk_placer" => TrunkKind::Bending {
                min_height_for_leaves: int(t, "min_height_for_leaves", 1),
                bend_length: ip(t, "bend_length"),
            },
            "upwards_branching_trunk_placer" => TrunkKind::UpwardsBranching {
                extra_branch_steps: ip(t, "extra_branch_steps"),
                place_branch_chance: chance(t, "place_branch_per_log_probability"),
                extra_branch_length: ip(t, "extra_branch_length"),
                can_grow_through: set(&t["can_grow_through"]),
            },
            "cherry_trunk_placer" => {
                let s = &t["branch_start_offset_from_top"];
                TrunkKind::Cherry {
                    branch_count: ip(t, "branch_count"),
                    horizontal_length: ip(t, "branch_horizontal_length"),
                    start: (int(s, "min_inclusive", -4), int(s, "max_inclusive", -3)),
                    end: ip(t, "branch_end_offset_from_top"),
                }
            }
            "poplar_trunk_placer" => TrunkKind::Poplar {
                branch_amount: ip(t, "branch_amount"),
                above_branches: int(t, "trunk_height_above_branches", 3),
            },
            _ => return None,
        };
        let trunk = Trunk {
            base: int(t, "base_height", 4),
            rand_a: int(t, "height_rand_a", 0),
            rand_b: int(t, "height_rand_b", 0),
            kind,
        };
        let f = &v["foliage_placer"];
        let fkind = match f["type"].as_str()?.trim_start_matches("minecraft:") {
            "blob_foliage_placer" => FoliageKind::Blob {
                height: int(f, "height", 3),
            },
            "fancy_foliage_placer" => FoliageKind::Fancy {
                height: int(f, "height", 4),
            },
            "bush_foliage_placer" => FoliageKind::Bush {
                height: int(f, "height", 2),
            },
            "acacia_foliage_placer" => FoliageKind::Acacia,
            "dark_oak_foliage_placer" => FoliageKind::DarkOak,
            "pine_foliage_placer" => FoliageKind::Pine {
                height: ip(f, "height"),
            },
            "spruce_foliage_placer" => FoliageKind::Spruce {
                trunk_height: ip(f, "trunk_height"),
            },
            "mega_pine_foliage_placer" => FoliageKind::MegaPine {
                crown_height: ip(f, "crown_height"),
            },
            "jungle_foliage_placer" => FoliageKind::Jungle {
                height: int(f, "height", 2),
            },
            "random_spread_foliage_placer" => FoliageKind::RandomSpread {
                height: ip(f, "foliage_height"),
                attempts: int(f, "leaf_placement_attempts", 50),
            },
            "cherry_foliage_placer" => FoliageKind::Cherry {
                height: ip(f, "height"),
                wide_hole: chance(f, "wide_bottom_layer_hole_chance"),
                corner_hole: chance(f, "corner_hole_chance"),
                hanging: chance(f, "hanging_leaves_chance"),
                hanging_extension: chance(f, "hanging_leaves_extension_chance"),
            },
            "poplar_foliage_placer" => FoliageKind::Poplar {
                height: ip(f, "height"),
                side_hole: chance(f, "side_hole_chance"),
            },
            _ => return None,
        };
        let foliage = Foliage {
            radius: ip(f, "radius"),
            offset: ip(f, "offset"),
            kind: fkind,
        };
        let roots = match v.get("root_placer") {
            Some(rp) if rp.is_object() => {
                let m = &rp["mangrove_root_placement"];
                let a = &rp["above_root_placement"];
                Some(Roots {
                    trunk_offset_y: ip(rp, "trunk_offset_y"),
                    provider: l.provider(&rp["root_provider"])?,
                    above: if a.is_object() {
                        Some((
                            chance(a, "above_root_placement_chance"),
                            l.provider(&a["above_root_provider"])?,
                        ))
                    } else {
                        None
                    },
                    can_grow_through: set(&m["can_grow_through"]),
                    muddy_in: set(&m["muddy_roots_in"]),
                    muddy: l.provider(&m["muddy_roots_provider"]),
                    max_length: int(m, "max_root_length", 15),
                    max_width: int(m, "max_root_width", 8),
                    skew: chance(m, "random_skew_chance"),
                })
            }
            _ => None,
        };
        Some(Self {
            trunk_provider: l.provider(&v["trunk_provider"])?,
            foliage_provider: l.provider(&v["foliage_provider"])?,
            below_trunk: v
                .get("below_trunk_provider")
                .or_else(|| v.get("dirt_provider"))
                .and_then(|p| l.provider(p)),
            trunk,
            foliage,
            roots,
            size: Size::parse(&v["minimum_size"]),
            decorators: decorators(&v["decorators"], l),
            ignore_vines: v["ignore_vines"].as_bool().unwrap_or(false),
        })
    }
}

impl FallenTree {
    pub fn parse(v: &Value, l: &Loader) -> Option<Self> {
        Some(Self {
            trunk_provider: l.provider(&v["trunk_provider"])?,
            length: ip(v, "log_length"),
            stump: decorators(&v["stump_decorators"], l),
            log: decorators(&v["log_decorators"], l),
        })
    }
}

/// Where foliage grows: a point, how much wider the foliage is there, and
/// whether it sits on a 2×2 trunk.
#[derive(Clone, Copy, Debug)]
struct Attachment {
    pos: P,
    radius_offset: i32,
    double: bool,
}

/// What a tree placed: positions in placement order, and the foliage as a
/// set for "is there a leaf here".
struct Built<'r> {
    region: &'r mut dyn Region,
    roots: Vec<P>,
    trunks: Vec<P>,
    foliage: Vec<P>,
    foliage_set: HashSet<P>,
    decorations: Vec<P>,
}

impl Built<'_> {
    fn set(&mut self, p: P, state: u32) -> bool {
        self.region.set(p, state)
    }
}

fn valid_tree_pos(facts: &Facts, region: &dyn Region, p: P) -> bool {
    let s = region.block(p);
    facts.is_air(s) || facts.in_named(s, "minecraft:replaceable_by_trees")
}

fn is_air_or_leaves(facts: &Facts, region: &dyn Region, p: P) -> bool {
    let s = region.block(p);
    facts.is_air(s) || facts.in_named(s, "minecraft:leaves")
}

impl Tree {
    fn tree_height(&self, r: &mut WorldgenRandom) -> i32 {
        self.trunk.base
            + r.next_int_bound(self.trunk.rand_a + 1)
            + r.next_int_bound(self.trunk.rand_b + 1)
    }

    fn valid(&self, facts: &Facts, region: &dyn Region, p: P) -> bool {
        if valid_tree_pos(facts, region, p) {
            return true;
        }
        if let TrunkKind::UpwardsBranching {
            can_grow_through: Some(set),
            ..
        } = &self.trunk.kind
        {
            return set.contains(facts, region.block(p));
        }
        false
    }

    fn is_free(&self, facts: &Facts, region: &dyn Region, p: P) -> bool {
        self.valid(facts, region, p) || facts.in_named(region.block(p), "minecraft:logs")
    }

    fn place_log_with(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        p: P,
        axis: Option<&str>,
    ) -> bool {
        if !self.valid(cx.facts, b.region, p) {
            return false;
        }
        let Some(mut state) = self.trunk_provider.get(b.region, cx.facts, r, p) else {
            return false;
        };
        if let Some(a) = axis {
            state = cx.facts.with(state, "axis", a);
        }
        b.set(p, state);
        b.trunks.push(p);
        true
    }

    fn place_log(&self, cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, p: P) -> bool {
        self.place_log_with(cx, b, r, p, None)
    }

    fn place_log_if_free(&self, cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, p: P) {
        if self.is_free(cx.facts, b.region, p) {
            self.place_log(cx, b, r, p);
        }
    }

    fn set_dirt(&self, cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, p: P) {
        if let Some(provider) = &self.below_trunk
            && let Some(state) = provider.get(b.region, cx.facts, r, p)
        {
            // Through the trunk's setter: it counts as part of the trunk
            // for the decorators.
            b.set(p, state);
            b.trunks.push(p);
        }
    }

    fn max_free_height(&self, cx: &Ctx, region: &dyn Region, height: i32, p: P) -> i32 {
        for i in 0..=height + 1 {
            let j = self.size.at(height, i);
            for dx in -j..=j {
                for dz in -j..=j {
                    let q = p.offset(dx, i, dz);
                    if !self.is_free(cx.facts, region, q)
                        || (!self.ignore_vines && cx.facts.is(region.block(q), "minecraft:vine"))
                    {
                        return i - 2;
                    }
                }
            }
        }
        height
    }

    pub fn place(&self, cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
        let mut b = Built {
            region,
            roots: Vec::new(),
            trunks: Vec::new(),
            foliage: Vec::new(),
            foliage_set: HashSet::new(),
            decorations: Vec::new(),
        };
        if !self.grow(cx, &mut b, r, p) || (b.trunks.is_empty() && b.foliage.is_empty()) {
            return false;
        }
        for d in &self.decorators {
            decorate(cx, &mut b, r, d);
        }
        update_leaves(cx.facts, &mut b);
        true
    }

    fn grow(&self, cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, p: P) -> bool {
        let height = self.tree_height(r);
        let foliage_height = self.foliage_height(r, height);
        let trunk_part = height - foliage_height;
        let radius = self.foliage_radius(r, trunk_part);
        let origin = match &self.roots {
            Some(roots) => p.above(roots.trunk_offset_y.sample(r)),
            None => p,
        };
        let h = b.region.heights();
        let lo = p.y.min(origin.y);
        let hi = p.y.max(origin.y) + height + 1;
        if lo < h.min_y + 1 || hi > h.min_y + h.height {
            return false;
        }
        let free = self.max_free_height(cx, b.region, height, origin);
        if free < height && self.size.min_clipped().is_none_or(|m| free < m) {
            return false;
        }
        if let Some(roots) = &self.roots
            && !place_roots(cx, b, r, roots, p, origin)
        {
            return false;
        }
        let attachments = self.place_trunk(cx, b, r, free, origin);
        for a in attachments {
            self.create_foliage(cx, b, r, free, a, foliage_height, radius);
        }
        true
    }

    fn foliage_height(&self, r: &mut WorldgenRandom, height: i32) -> i32 {
        match &self.foliage.kind {
            FoliageKind::Blob { height: h }
            | FoliageKind::Fancy { height: h }
            | FoliageKind::Bush { height: h }
            | FoliageKind::Jungle { height: h } => *h,
            FoliageKind::Acacia => 0,
            FoliageKind::DarkOak => 4,
            FoliageKind::Pine { height: h }
            | FoliageKind::RandomSpread { height: h, .. }
            | FoliageKind::Cherry { height: h, .. }
            | FoliageKind::Poplar { height: h, .. } => h.sample(r),
            FoliageKind::Spruce { trunk_height } => (height - trunk_height.sample(r)).max(4),
            FoliageKind::MegaPine { crown_height } => crown_height.sample(r),
        }
    }

    fn foliage_radius(&self, r: &mut WorldgenRandom, trunk: i32) -> i32 {
        let base = self.foliage.radius.sample(r);
        match self.foliage.kind {
            FoliageKind::Pine { .. } => base + r.next_int_bound((trunk + 1).max(1)),
            _ => base,
        }
    }

    // ---- trunks ----

    fn place_trunk(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        free: i32,
        p: P,
    ) -> Vec<Attachment> {
        let at = |pos: P, radius_offset: i32, double: bool| Attachment {
            pos,
            radius_offset,
            double,
        };
        match &self.trunk.kind {
            TrunkKind::Straight => {
                self.set_dirt(cx, b, r, p.below(1));
                for i in 0..free {
                    self.place_log(cx, b, r, p.above(i));
                }
                vec![at(p.above(free), 0, false)]
            }
            TrunkKind::Forking => {
                self.set_dirt(cx, b, r, p.below(1));
                let mut out = Vec::new();
                let dir = random_horizontal(r);
                let i = free - r.next_int_bound(4) - 1;
                let mut j = 3 - r.next_int_bound(3);
                let (mut x, mut z) = (p.x, p.z);
                let mut top = None;
                for n in 0..free {
                    let y = p.y + n;
                    if n >= i && j > 0 {
                        let (dx, _, dz) = dir.offset();
                        x += dx;
                        z += dz;
                        j -= 1;
                    }
                    if self.place_log(cx, b, r, P::new(x, y, z)) {
                        top = Some(y + 1);
                    }
                }
                if let Some(t) = top {
                    out.push(at(P::new(x, t, z), 1, false));
                }
                let (mut x, mut z) = (p.x, p.z);
                let dir2 = random_horizontal(r);
                if dir2 != dir {
                    let start = i - r.next_int_bound(2) - 1;
                    let mut q = 1 + r.next_int_bound(3);
                    let mut top = None;
                    let mut n = start;
                    while n < free && q > 0 {
                        if n >= 1 {
                            let y = p.y + n;
                            let (dx, _, dz) = dir2.offset();
                            x += dx;
                            z += dz;
                            if self.place_log(cx, b, r, P::new(x, y, z)) {
                                top = Some(y + 1);
                            }
                        }
                        n += 1;
                        q -= 1;
                    }
                    if let Some(t) = top {
                        out.push(at(P::new(x, t, z), 0, false));
                    }
                }
                out
            }
            TrunkKind::Giant | TrunkKind::MegaJungle => {
                let below = p.below(1);
                for q in [
                    below,
                    below.offset(1, 0, 0),
                    below.offset(0, 0, 1),
                    below.offset(1, 0, 1),
                ] {
                    self.set_dirt(cx, b, r, q);
                }
                for i in 0..free {
                    self.place_log_if_free(cx, b, r, p.offset(0, i, 0));
                    if i < free - 1 {
                        self.place_log_if_free(cx, b, r, p.offset(1, i, 0));
                        self.place_log_if_free(cx, b, r, p.offset(1, i, 1));
                        self.place_log_if_free(cx, b, r, p.offset(0, i, 1));
                    }
                }
                let mut out = vec![at(p.above(free), 0, true)];
                if matches!(self.trunk.kind, TrunkKind::MegaJungle) {
                    let mut i = free - 2 - r.next_int_bound(4);
                    while i > free / 2 {
                        let f = r.next_float() * std::f32::consts::TAU;
                        let (mut j, mut k) = (0, 0);
                        for l in 0..5 {
                            j = (1.5 + f.cos() * l as f32) as i32;
                            k = (1.5 + f.sin() * l as f32) as i32;
                            self.place_log(cx, b, r, p.offset(j, i - 3 + l / 2, k));
                        }
                        out.push(at(p.offset(j, i, k), -2, false));
                        i -= 2 + r.next_int_bound(4);
                    }
                }
                out
            }
            TrunkKind::DarkOak => {
                let below = p.below(1);
                for q in [
                    below,
                    below.offset(1, 0, 0),
                    below.offset(0, 0, 1),
                    below.offset(1, 0, 1),
                ] {
                    self.set_dirt(cx, b, r, q);
                }
                let dir = random_horizontal(r);
                let i = free - r.next_int_bound(4);
                let mut j = 2 - r.next_int_bound(3);
                let (k, l, m) = (p.x, p.y, p.z);
                let (mut n, mut o) = (k, m);
                let top = l + free - 1;
                for q in 0..free {
                    if q >= i && j > 0 {
                        let (dx, _, dz) = dir.offset();
                        n += dx;
                        o += dz;
                        j -= 1;
                    }
                    let bp = P::new(n, l + q, o);
                    if is_air_or_leaves(cx.facts, b.region, bp) {
                        self.place_log(cx, b, r, bp);
                        self.place_log(cx, b, r, bp.offset(1, 0, 0));
                        self.place_log(cx, b, r, bp.offset(0, 0, 1));
                        self.place_log(cx, b, r, bp.offset(1, 0, 1));
                    }
                }
                let mut out = vec![at(P::new(n, top, o), 0, true)];
                for q in -1..=2 {
                    for s in -1..=2 {
                        if (0..=1).contains(&q) && (0..=1).contains(&s) || r.next_int_bound(3) > 0 {
                            continue;
                        }
                        let len = r.next_int_bound(3) + 2;
                        for t in 0..len {
                            self.place_log(cx, b, r, P::new(k + q, top - t - 1, m + s));
                        }
                        out.push(at(P::new(k + q, top, m + s), 0, false));
                    }
                }
                out
            }
            TrunkKind::Fancy => self.fancy_trunk(cx, b, r, free, p),
            TrunkKind::Bending {
                min_height_for_leaves,
                bend_length,
            } => {
                let dir = random_horizontal(r);
                let i = free - 1;
                let mut m = p;
                self.set_dirt(cx, b, r, m.below(1));
                let mut out = Vec::new();
                for j in 0..=i {
                    if j + 1 >= i + r.next_int_bound(2) {
                        m = m.rel(dir);
                    }
                    if valid_tree_pos(cx.facts, b.region, m) {
                        self.place_log(cx, b, r, m);
                    }
                    if j >= *min_height_for_leaves {
                        out.push(at(m, 0, false));
                    }
                    m = m.above(1);
                }
                let len = bend_length.sample(r);
                for _ in 0..=len {
                    if valid_tree_pos(cx.facts, b.region, m) {
                        self.place_log(cx, b, r, m);
                    }
                    out.push(at(m, 0, false));
                    m = m.rel(dir);
                }
                out
            }
            TrunkKind::UpwardsBranching {
                extra_branch_steps,
                place_branch_chance,
                extra_branch_length,
                ..
            } => {
                let mut out = Vec::new();
                for i in 0..free {
                    let y = p.y + i;
                    let m = P::new(p.x, y, p.z);
                    if self.place_log(cx, b, r, m)
                        && i < free - 1
                        && r.next_float() < *place_branch_chance
                    {
                        let dir = random_horizontal(r);
                        let k = extra_branch_length.sample(r);
                        let l = (k - extra_branch_length.sample(r) - 1).max(0);
                        let mut n = extra_branch_steps.sample(r);
                        // The branch.
                        let mut top = y + l;
                        let (mut x, mut z) = (m.x, m.z);
                        let mut o = l;
                        while o < free && n > 0 {
                            if o >= 1 {
                                let yy = y + o;
                                let (dx, _, dz) = dir.offset();
                                x += dx;
                                z += dz;
                                top = yy;
                                if self.place_log(cx, b, r, P::new(x, yy, z)) {
                                    top += 1;
                                }
                                out.push(at(P::new(x, yy, z), 0, false));
                            }
                            o += 1;
                            n -= 1;
                        }
                        if top - y > 1 {
                            let bp = P::new(x, top, z);
                            out.push(at(bp, 0, false));
                            out.push(at(bp.below(2), 0, false));
                        }
                    }
                    if i == free - 1 {
                        out.push(at(P::new(p.x, y + 1, p.z), 0, false));
                    }
                }
                out
            }
            TrunkKind::Cherry {
                branch_count,
                horizontal_length,
                start,
                end,
            } => {
                self.set_dirt(cx, b, r, p.below(1));
                let i = (free - 1 + r.next_int_bound(start.1 - start.0 + 1) + start.0).max(0);
                let mut j =
                    (free - 1 + r.next_int_bound(start.1 - 1 - start.0 + 1).max(0) + start.0)
                        .max(0);
                if j >= i {
                    j += 1;
                }
                let k = branch_count.sample(r);
                let three = k == 3;
                let two = k >= 2;
                let l = if three {
                    free
                } else if two {
                    i.max(j) + 1
                } else {
                    i + 1
                };
                for m in 0..l {
                    self.place_log(cx, b, r, p.above(m));
                }
                let mut out = Vec::new();
                if three {
                    out.push(at(p.above(l), 0, false));
                }
                let dir = random_horizontal(r);
                out.push(self.cherry_branch(
                    cx,
                    b,
                    r,
                    free,
                    p,
                    dir,
                    i,
                    i < l - 1,
                    horizontal_length,
                    end,
                ));
                if two {
                    out.push(self.cherry_branch(
                        cx,
                        b,
                        r,
                        free,
                        p,
                        dir.opposite(),
                        j,
                        j < l - 1,
                        horizontal_length,
                        end,
                    ));
                }
                out
            }
            TrunkKind::Poplar {
                branch_amount,
                above_branches,
            } => {
                self.set_dirt(cx, b, r, p.below(1));
                for i in 0..free {
                    self.place_log(cx, b, r, p.above(i));
                }
                let mut out = vec![at(p.above(free), 0, false)];
                let count = branch_amount.sample(r);
                let top = (free - above_branches).max(2);
                for _ in 0..count {
                    let dir = random_horizontal(r);
                    let y = (top / 2) + r.next_int_bound((top - top / 2).max(1));
                    let q = p.above(y).rel(dir);
                    let axis = if dir.axis() == mc_tick::Axis::X {
                        "x"
                    } else {
                        "z"
                    };
                    if self.place_log_with(cx, b, r, q, Some(axis)) {
                        out.push(at(q.above(1), -1, false));
                    }
                }
                out
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cherry_branch(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        free: i32,
        p: P,
        dir: Dir,
        i: i32,
        long: bool,
        horizontal: &IntProvider,
        end: &IntProvider,
    ) -> Attachment {
        let axis = if dir.axis() == mc_tick::Axis::X {
            "x"
        } else {
            "z"
        };
        let mut m = p.above(i);
        let j = free - 1 + end.sample(r);
        let bl2 = long || j < i;
        let k = horizontal.sample(r) + i32::from(bl2);
        let target = p.rel_n(dir, k).above(j);
        let steps = if bl2 { 2 } else { 1 };
        for _ in 0..steps {
            m = m.rel(dir);
            self.place_log_with(cx, b, r, m, Some(axis));
        }
        let up = if target.y > m.y { Dir::Up } else { Dir::Down };
        loop {
            let n = (target.x - m.x).abs() + (target.y - m.y).abs() + (target.z - m.z).abs();
            if n == 0 {
                break;
            }
            let g = (target.y - m.y).abs() as f32 / n as f32;
            let vertical = r.next_float() < g;
            m = m.rel(if vertical { up } else { dir });
            self.place_log_with(cx, b, r, m, if vertical { None } else { Some(axis) });
        }
        Attachment {
            pos: target.above(1),
            radius_offset: 0,
            double: false,
        }
    }

    fn fancy_trunk(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        free: i32,
        p: P,
    ) -> Vec<Attachment> {
        let j = free + 2;
        let k = (f64::from(j) * 0.618).floor() as i32;
        self.set_dirt(cx, b, r, p.below(1));
        let m = p.y + k;
        let mut n = j - 5;
        // (attachment, branch base y)
        let mut list: Vec<(P, i32)> = vec![(p.above(n), m)];
        while n >= 0 {
            let f = tree_shape(j, n);
            if f >= 0.0 {
                let g = f64::from(f) * (f64::from(r.next_float()) + 0.328);
                let h = f64::from(r.next_float() * 2.0) * std::f64::consts::PI;
                let px = g * h.sin() + 0.5;
                let qz = g * h.cos() + 0.5;
                let bp = p.offset(px.floor() as i32, n - 1, qz.floor() as i32);
                let bp2 = bp.above(5);
                if self.make_limb(cx, b, r, bp, bp2, false) {
                    let dx = p.x - bp.x;
                    let dz = p.z - bp.z;
                    let t = f64::from(bp.y) - f64::from(dx * dx + dz * dz).sqrt() * 0.381;
                    let u = if t > f64::from(m) { m } else { t as i32 };
                    let bp3 = P::new(p.x, u, p.z);
                    if self.make_limb(cx, b, r, bp3, bp, false) {
                        list.push((bp, bp3.y));
                    }
                }
            }
            n -= 1;
        }
        self.make_limb(cx, b, r, p, p.above(k), true);
        for &(at, base) in &list {
            let bp = P::new(p.x, base, p.z);
            if bp != at && trim_branches(j, base - p.y) {
                self.make_limb(cx, b, r, bp, at, true);
            }
        }
        list.iter()
            .filter(|(_, base)| trim_branches(j, base - p.y))
            .map(|&(pos, _)| Attachment {
                pos,
                radius_offset: 0,
                double: false,
            })
            .collect()
    }

    fn make_limb(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        from: P,
        to: P,
        make: bool,
    ) -> bool {
        if !make && from == to {
            return true;
        }
        let d = P::new(to.x - from.x, to.y - from.y, to.z - from.z);
        let steps = d.x.abs().max(d.y.abs()).max(d.z.abs());
        if steps == 0 {
            if make {
                self.place_log(cx, b, r, from);
            }
            return true;
        }
        let (f, g, h) = (
            d.x as f32 / steps as f32,
            d.y as f32 / steps as f32,
            d.z as f32 / steps as f32,
        );
        for j in 0..=steps {
            let q = from.offset(
                (0.5 + j as f32 * f).floor() as i32,
                (0.5 + j as f32 * g).floor() as i32,
                (0.5 + j as f32 * h).floor() as i32,
            );
            if make {
                let ax = (q.x - from.x).abs();
                let az = (q.z - from.z).abs();
                let mx = ax.max(az);
                let axis = if mx > 0 {
                    if ax == mx { "x" } else { "z" }
                } else {
                    "y"
                };
                self.place_log_with(cx, b, r, q, Some(axis));
            } else if !self.is_free(cx.facts, b.region, q) {
                return false;
            }
        }
        true
    }

    // ---- foliage ----

    #[allow(clippy::too_many_arguments)]
    fn create_foliage(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        free: i32,
        a: Attachment,
        height: i32,
        radius: i32,
    ) {
        let _ = free;
        let offset = self.foliage.offset.sample(r);
        let pos = a.pos;
        let large = a.double;
        match &self.foliage.kind {
            FoliageKind::Blob { .. } => {
                let mut i = offset;
                while i >= offset - height {
                    let j = (radius + a.radius_offset - 1 - i / 2).max(0);
                    self.leaves_row(cx, b, r, pos, j, i, large);
                    i -= 1;
                }
            }
            FoliageKind::Fancy { .. } => {
                let mut i = offset;
                while i >= offset - height {
                    let j = radius + i32::from(i != offset && i != offset - height);
                    self.leaves_row(cx, b, r, pos, j, i, large);
                    i -= 1;
                }
            }
            FoliageKind::Bush { .. } => {
                let mut i = offset;
                while i >= offset - height {
                    let j = radius + a.radius_offset - 1 - i;
                    self.leaves_row(cx, b, r, pos, j, i, large);
                    i -= 1;
                }
            }
            FoliageKind::Acacia => {
                let bp = pos.above(offset);
                self.leaves_row(cx, b, r, bp, radius + a.radius_offset, -1 - height, large);
                self.leaves_row(cx, b, r, bp, radius - 1, -height, large);
                self.leaves_row(cx, b, r, bp, radius + a.radius_offset - 1, 0, large);
            }
            FoliageKind::DarkOak => {
                let bp = pos.above(offset);
                if large {
                    self.leaves_row(cx, b, r, bp, radius + 2, -1, large);
                    self.leaves_row(cx, b, r, bp, radius + 3, 0, large);
                    self.leaves_row(cx, b, r, bp, radius + 2, 1, large);
                    if r.next_bool() {
                        self.leaves_row(cx, b, r, bp, radius, 2, large);
                    }
                } else {
                    self.leaves_row(cx, b, r, bp, radius + 2, -1, large);
                    self.leaves_row(cx, b, r, bp, radius + 1, 0, large);
                }
            }
            FoliageKind::Pine { .. } => {
                let mut i = 0;
                let mut j = offset;
                while j >= offset - height {
                    self.leaves_row(cx, b, r, pos, i, j, large);
                    if i >= 1 && j == offset - height + 1 {
                        i -= 1;
                    } else if i < radius + a.radius_offset {
                        i += 1;
                    }
                    j -= 1;
                }
            }
            FoliageKind::Spruce { .. } => {
                let mut i = r.next_int_bound(2);
                let mut j = 1;
                let mut k = 0;
                let mut l = offset;
                while l >= -height {
                    self.leaves_row(cx, b, r, pos, i, l, large);
                    if i >= j {
                        i = k;
                        k = 1;
                        j = (j + 1).min(radius + a.radius_offset);
                    } else {
                        i += 1;
                    }
                    l -= 1;
                }
            }
            FoliageKind::MegaPine { .. } => {
                let mut prev = 0;
                for y in pos.y - height + offset..=pos.y + offset {
                    let k = pos.y - y;
                    let l =
                        radius + a.radius_offset + (k as f32 / height as f32 * 3.5).floor() as i32;
                    let m = if k > 0 && l == prev && y & 1 == 0 {
                        l + 1
                    } else {
                        l
                    };
                    self.leaves_row(cx, b, r, pos.with_y(y), m, 0, large);
                    prev = l;
                }
            }
            FoliageKind::Jungle { .. } => {
                let i = if large {
                    height
                } else {
                    1 + r.next_int_bound(2)
                };
                let mut j = offset;
                while j >= offset - i {
                    let k = radius + a.radius_offset + 1 - j;
                    self.leaves_row(cx, b, r, pos, k, j, large);
                    j -= 1;
                }
            }
            FoliageKind::RandomSpread { attempts, .. } => {
                let (rad, hgt) = (radius.max(1), height.max(1));
                for _ in 0..*attempts {
                    let dx = r.next_int_bound(rad) - r.next_int_bound(rad);
                    let dy = r.next_int_bound(hgt) - r.next_int_bound(hgt);
                    let dz = r.next_int_bound(rad) - r.next_int_bound(rad);
                    self.try_leaf(cx, b, r, pos.offset(dx, dy, dz));
                }
            }
            FoliageKind::Cherry {
                hanging,
                hanging_extension,
                ..
            } => {
                let bp = pos.above(offset);
                let i = radius + a.radius_offset - 1;
                self.leaves_row(cx, b, r, bp, i - 2, height - 3, large);
                self.leaves_row(cx, b, r, bp, i - 1, height - 4, large);
                let mut j = height - 5;
                while j >= 0 {
                    self.leaves_row(cx, b, r, bp, i, j, large);
                    j -= 1;
                }
                self.leaves_row_hanging(cx, b, r, bp, i, -1, large, *hanging, *hanging_extension);
                self.leaves_row_hanging(
                    cx,
                    b,
                    r,
                    bp,
                    i - 1,
                    -2,
                    large,
                    *hanging,
                    *hanging_extension,
                );
            }
            FoliageKind::Poplar { .. } => {
                // A tall narrow crown: wider in the middle.
                let mut i = offset;
                let bottom = offset - height - radius / 2;
                while i >= bottom {
                    let from_top = offset - i;
                    let total = offset - bottom;
                    let w = if from_top == 0 || from_top == total {
                        0
                    } else if from_top * 3 < total || from_top * 3 > total * 2 {
                        1
                    } else {
                        2
                    };
                    self.leaves_row(cx, b, r, pos, w, i, large);
                    i -= 1;
                }
            }
        }
    }

    fn skip(
        &self,
        r: &mut WorldgenRandom,
        dx: i32,
        y: i32,
        dz: i32,
        range: i32,
        large: bool,
    ) -> bool {
        match &self.foliage.kind {
            FoliageKind::Blob { .. } => {
                dx == range && dz == range && (r.next_int_bound(2) == 0 || y == 0)
            }
            FoliageKind::Fancy { .. } => {
                (dx as f32 + 0.5).powi(2) + (dz as f32 + 0.5).powi(2) > (range * range) as f32
            }
            FoliageKind::Bush { .. } => dx == range && dz == range && r.next_int_bound(2) == 0,
            FoliageKind::Acacia => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == range && dz == range && range > 0
                }
            }
            FoliageKind::DarkOak => {
                if y == -1 && !large {
                    dx == range && dz == range
                } else if y == 1 {
                    dx + dz > range * 2 - 2
                } else {
                    false
                }
            }
            FoliageKind::Pine { .. } | FoliageKind::Spruce { .. } => {
                dx == range && dz == range && range > 0
            }
            FoliageKind::MegaPine { .. } | FoliageKind::Jungle { .. } => {
                dx + dz >= 7 || dx * dx + dz * dz > range * range
            }
            FoliageKind::RandomSpread { .. } => false,
            FoliageKind::Cherry {
                wide_hole,
                corner_hole,
                ..
            } => {
                if y == -1 && (dx == range || dz == range) && r.next_float() < *wide_hole {
                    return true;
                }
                let corner = dx == range && dz == range;
                if range > 2 {
                    corner || dx + dz > range * 2 - 2 && r.next_float() < *corner_hole
                } else {
                    corner && r.next_float() < *corner_hole
                }
            }
            FoliageKind::Poplar { side_hole, .. } => {
                (dx == range && dz == range && range > 0)
                    || ((dx == range || dz == range) && range > 0 && r.next_float() < *side_hole)
            }
        }
    }

    fn skip_signed(
        &self,
        r: &mut WorldgenRandom,
        dx: i32,
        y: i32,
        dz: i32,
        range: i32,
        large: bool,
    ) -> bool {
        if let FoliageKind::DarkOak = self.foliage.kind
            && y == 0
            && large
            && (dx == -range || dx >= range)
            && (dz == -range || dz >= range)
        {
            return true;
        }
        let (i, j) = if large {
            (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs()))
        } else {
            (dx.abs(), dz.abs())
        };
        self.skip(r, i, y, j, range, large)
    }

    #[allow(clippy::too_many_arguments)]
    fn leaves_row(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        pos: P,
        range: i32,
        y: i32,
        large: bool,
    ) {
        let extra = i32::from(large);
        for dx in -range..=range + extra {
            for dz in -range..=range + extra {
                if !self.skip_signed(r, dx, y, dz, range, large) {
                    self.try_leaf(cx, b, r, pos.offset(dx, y, dz));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn leaves_row_hanging(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        pos: P,
        range: i32,
        y: i32,
        large: bool,
        chance: f32,
        extension: f32,
    ) {
        self.leaves_row(cx, b, r, pos, range, y, large);
        let extra = i32::from(large);
        let origin = pos.below(1);
        for d in HORIZONTAL {
            let e = d.cw();
            let (ex, _, ez) = e.offset();
            let positive = ex > 0 || ez > 0;
            let j = if positive { range + extra } else { range };
            let mut m = pos.offset(0, y - 1, 0).rel_n(e, j).rel_n(d, -range);
            for _ in -range..range + extra {
                let above_set = b.foliage_set.contains(&m.above(1));
                if above_set && self.try_extension(cx, b, r, chance, origin, m) {
                    self.try_extension(cx, b, r, extension, origin, m.below(1));
                }
                m = m.rel(d);
            }
        }
    }

    fn try_extension(
        &self,
        cx: &Ctx,
        b: &mut Built,
        r: &mut WorldgenRandom,
        chance: f32,
        origin: P,
        p: P,
    ) -> bool {
        let dist = (p.x - origin.x).abs() + (p.y - origin.y).abs() + (p.z - origin.z).abs();
        if dist >= 7 || r.next_float() > chance {
            return false;
        }
        self.try_leaf(cx, b, r, p)
    }

    fn try_leaf(&self, cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, p: P) -> bool {
        if !valid_tree_pos(cx.facts, b.region, p) {
            return false;
        }
        let Some(mut state) = self.foliage_provider.get(b.region, cx.facts, r, p) else {
            return false;
        };
        if cx.facts.has_property(state, "waterlogged") {
            let wet = cx.facts.is_water_source(b.region.block(p));
            state = cx
                .facts
                .with(state, "waterlogged", if wet { "true" } else { "false" });
        }
        b.set(p, state);
        if b.foliage_set.insert(p) {
            b.foliage.push(p);
        }
        true
    }
}

fn tree_shape(i: i32, j: i32) -> f32 {
    if (j as f32) < i as f32 * 0.3 {
        return -1.0;
    }
    let f = i as f32 / 2.0;
    let g = f - j as f32;
    let mut h = (f * f - g * g).sqrt();
    if g == 0.0 {
        h = f;
    } else if g.abs() >= f {
        return 0.0;
    }
    h * 0.5
}

fn trim_branches(i: i32, j: i32) -> bool {
    f64::from(j) >= f64::from(i) * 0.2
}

fn waterlogged(facts: &Facts, region: &dyn Region, p: P, state: u32) -> u32 {
    if facts.has_property(state, "waterlogged") {
        let wet = matches!(facts.fluid(region.block(p)), crate::Fluid::Water { .. });
        facts.with(state, "waterlogged", if wet { "true" } else { "false" })
    } else {
        state
    }
}

fn place_roots(
    cx: &Ctx,
    b: &mut Built,
    r: &mut WorldgenRandom,
    roots: &Roots,
    p: P,
    origin: P,
) -> bool {
    let facts = cx.facts;
    let can = |region: &dyn Region, q: P| {
        valid_tree_pos(facts, region, q)
            || roots
                .can_grow_through
                .as_ref()
                .is_some_and(|s| s.contains(facts, region.block(q)))
    };
    let mut m = p;
    while m.y < origin.y {
        if !can(b.region, m) {
            return false;
        }
        m = m.above(1);
    }
    let mut list = vec![origin.below(1)];
    for d in HORIZONTAL {
        let start = origin.rel(d);
        let mut found = Vec::new();
        if !simulate_roots(b.region, r, roots, start, d, origin, &mut found, 0, &can) {
            return false;
        }
        list.extend(found);
        list.push(origin.rel(d));
    }
    for q in list {
        if roots
            .muddy_in
            .as_ref()
            .is_some_and(|s| s.contains(facts, b.region.block(q)))
            && let Some(muddy) = &roots.muddy
        {
            if let Some(s) = muddy.get(b.region, facts, r, q) {
                let s = waterlogged(facts, b.region, q, s);
                b.set(q, s);
                b.roots.push(q);
            }
            continue;
        }
        if !can(b.region, q) {
            continue;
        }
        if let Some(s) = roots.provider.get(b.region, facts, r, q) {
            let s = waterlogged(facts, b.region, q, s);
            b.set(q, s);
            b.roots.push(q);
        }
        if let Some((chance, provider)) = &roots.above {
            let above = q.above(1);
            if r.next_float() < *chance
                && facts.is_air(b.region.block(above))
                && let Some(s) = provider.get(b.region, facts, r, above)
            {
                let s = waterlogged(facts, b.region, above, s);
                b.set(above, s);
                b.roots.push(above);
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn simulate_roots(
    region: &dyn Region,
    r: &mut WorldgenRandom,
    roots: &Roots,
    pos: P,
    dir: Dir,
    origin: P,
    list: &mut Vec<P>,
    depth: i32,
    can: &dyn Fn(&dyn Region, P) -> bool,
) -> bool {
    if depth == roots.max_length || list.len() as i32 > roots.max_length {
        return false;
    }
    let below = pos.below(1);
    let side = pos.rel(dir);
    let dist = (pos.x - origin.x).abs() + (pos.y - origin.y).abs() + (pos.z - origin.z).abs();
    let w = roots.max_width;
    let next: Vec<P> = if dist > w - 3 && dist <= w {
        if r.next_float() < roots.skew {
            vec![below, side.below(1)]
        } else {
            vec![below]
        }
    } else if dist > w || r.next_float() < roots.skew {
        vec![below]
    } else if r.next_bool() {
        vec![side]
    } else {
        vec![below]
    };
    for q in next {
        if !can(region, q) {
            continue;
        }
        list.push(q);
        if !simulate_roots(region, r, roots, q, dir, origin, list, depth + 1, can) {
            return false;
        }
    }
    true
}

// ---- decorators ----

fn sorted_by_y(list: &[P]) -> Vec<P> {
    let mut v = list.to_vec();
    v.sort_by_key(|p| p.y);
    v
}

fn lowest(b: &Built) -> Vec<P> {
    let logs = sorted_by_y(&b.trunks);
    let roots = sorted_by_y(&b.roots);
    if roots.is_empty() {
        logs
    } else if !logs.is_empty() && roots[0].y == logs[0].y {
        let mut v = logs;
        v.extend(roots);
        v
    } else {
        roots
    }
}

fn deco_set(b: &mut Built, p: P, state: u32) {
    if b.set(p, state) {
        b.decorations.push(p);
    }
}

fn place_vine(cx: &Ctx, b: &mut Built, p: P, side: &str) {
    if let Some(v) = cx.facts.state("minecraft:vine", &[(side, "true")]) {
        deco_set(b, p, v);
    }
}

fn hanging_vine(cx: &Ctx, b: &mut Built, mut p: P, side: &str) {
    place_vine(cx, b, p, side);
    let mut i = 4;
    p = p.below(1);
    while cx.facts.is_air(b.region.block(p)) && i > 0 {
        place_vine(cx, b, p, side);
        p = p.below(1);
        i -= 1;
    }
}

fn decorate(cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, d: &Decorator) {
    let facts = cx.facts;
    let air = |b: &Built, p: P| facts.is_air(b.region.block(p));
    match d {
        Decorator::Beehive(chance) => {
            let leaves = sorted_by_y(&b.foliage);
            let logs = sorted_by_y(&b.trunks);
            if logs.is_empty() || r.next_float() >= *chance {
                return;
            }
            let y = if let Some(first) = leaves.first() {
                (first.y - 1).max(logs[0].y + 1)
            } else {
                (logs[0].y + 1 + r.next_int_bound(3)).min(logs[logs.len() - 1].y)
            };
            let mut list: Vec<P> = logs
                .iter()
                .filter(|p| p.y == y)
                .flat_map(|p| [Dir::East, Dir::South, Dir::West].map(|d| p.rel(d)))
                .collect();
            if list.is_empty() {
                return;
            }
            r.shuffle(&mut list);
            let Some(spot) = list
                .into_iter()
                .find(|p| air(b, *p) && air(b, p.rel(Dir::South)))
            else {
                return;
            };
            let Some(nest) = facts.state("minecraft:bee_nest", &[("facing", "south")]) else {
                return;
            };
            deco_set(b, spot, nest);
            let count = 2 + r.next_int_bound(2);
            let mut bees = Vec::new();
            for _ in 0..count {
                let ticks = r.next_int_bound(599);
                let mut bee = mc_nbt::Compound::new();
                let mut data = mc_nbt::Compound::new();
                data.insert("id", mc_nbt::Tag::String("minecraft:bee".into()));
                bee.insert("entity_data", mc_nbt::Tag::Compound(data));
                bee.insert("ticks_in_hive", mc_nbt::Tag::Int(ticks));
                bee.insert("min_ticks_in_hive", mc_nbt::Tag::Int(600));
                bees.push(mc_nbt::Tag::Compound(bee));
            }
            let mut be = mc_nbt::Compound::new();
            be.insert("id", mc_nbt::Tag::String("minecraft:beehive".into()));
            be.insert("bees", mc_nbt::Tag::List(bees));
            b.region.set_block_entity(spot, be);
        }
        Decorator::PlaceOnGround {
            tries,
            radius,
            height,
            provider,
        } => {
            let list = lowest(b);
            let Some(first) = list.first().copied() else {
                return;
            };
            let y = first.y;
            let (mut x0, mut x1, mut z0, mut z1) = (first.x, first.x, first.z, first.z);
            for p in &list {
                if p.y == y {
                    x0 = x0.min(p.x);
                    x1 = x1.max(p.x);
                    z0 = z0.min(p.z);
                    z1 = z1.max(p.z);
                }
            }
            let (bx0, bx1) = (x0 - radius, x1 + radius);
            let (by0, by1) = (y - height, y + height);
            let (bz0, bz1) = (z0 - radius, z1 + radius);
            for _ in 0..*tries {
                let p = P::new(
                    r.next_int_bound(bx1 - bx0 + 1) + bx0,
                    r.next_int_bound(by1 - by0 + 1) + by0,
                    r.next_int_bound(bz1 - bz0 + 1) + bz0,
                );
                let above = p.above(1);
                let s = b.region.block(above);
                if (facts.is_air(s) || facts.is(s, "minecraft:vine"))
                    && facts.solid_render(b.region.block(p))
                    && b.region.height(Heightmap::MotionBlockingNoLeaves, p.x, p.z) <= above.y
                    && let Some(state) = provider.get(b.region, facts, r, above)
                {
                    deco_set(b, above, state);
                }
            }
        }
        Decorator::LeaveVine(chance) => {
            for p in sorted_by_y(&b.foliage) {
                for (d, side) in [
                    (Dir::West, "east"),
                    (Dir::East, "west"),
                    (Dir::North, "south"),
                    (Dir::South, "north"),
                ] {
                    if r.next_float() < *chance {
                        let q = p.rel(d);
                        if air(b, q) {
                            hanging_vine(cx, b, q, side);
                        }
                    }
                }
            }
        }
        Decorator::TrunkVine => {
            for p in sorted_by_y(&b.trunks) {
                for (d, side) in [
                    (Dir::West, "east"),
                    (Dir::East, "west"),
                    (Dir::North, "south"),
                    (Dir::South, "north"),
                ] {
                    if r.next_int_bound(3) > 0 {
                        let q = p.rel(d);
                        if air(b, q) {
                            place_vine(cx, b, q, side);
                        }
                    }
                }
            }
        }
        Decorator::Cocoa(chance) => {
            if r.next_float() >= *chance {
                return;
            }
            let logs = sorted_by_y(&b.trunks);
            let Some(first) = logs.first() else { return };
            let base = first.y;
            for p in logs.iter().filter(|p| p.y - base <= 2) {
                for d in HORIZONTAL {
                    if r.next_float() <= 0.25 {
                        let q = p.rel(d.opposite());
                        if air(b, q) {
                            let age = r.next_int_bound(3).to_string();
                            if let Some(s) = facts.state(
                                "minecraft:cocoa",
                                &[("age", age.as_str()), ("facing", d.name())],
                            ) {
                                deco_set(b, q, s);
                            }
                        }
                    }
                }
            }
        }
        Decorator::AlterGround(provider) => {
            let list = lowest(b);
            let Some(first) = list.first() else { return };
            let y = first.y;
            let ground: Vec<P> = list.iter().copied().filter(|p| p.y == y).collect();
            for p in ground {
                for c in [
                    p.offset(-1, 0, -1),
                    p.offset(2, 0, -1),
                    p.offset(-1, 0, 2),
                    p.offset(2, 0, 2),
                ] {
                    alter_circle(cx, b, r, provider, c);
                }
                for _ in 0..5 {
                    let k = r.next_int_bound(64);
                    let (l, m) = (k % 8, k / 8);
                    if l == 0 || l == 7 || m == 0 || m == 7 {
                        alter_circle(cx, b, r, provider, p.offset(-3 + l, 0, -3 + m));
                    }
                }
            }
        }
        Decorator::AttachedToLeaves {
            chance,
            xz,
            y,
            provider,
            empty,
            dirs,
        } => {
            if dirs.is_empty() {
                return;
            }
            let mut leaves = sorted_by_y(&b.foliage);
            r.shuffle(&mut leaves);
            let mut taken: HashSet<P> = HashSet::new();
            for p in leaves {
                let d = *r.pick(dirs);
                let q = p.rel(d);
                if taken.contains(&q) || r.next_float() >= *chance {
                    continue;
                }
                if !(1..=*empty).all(|i| air(b, p.rel_n(d, i))) {
                    continue;
                }
                for dx in -xz..=*xz {
                    for dy in -y..=*y {
                        for dz in -xz..=*xz {
                            taken.insert(q.offset(dx, dy, dz));
                        }
                    }
                }
                if let Some(s) = provider.get(b.region, facts, r, q) {
                    deco_set(b, q, s);
                }
            }
        }
        Decorator::AttachedToLogs {
            chance,
            provider,
            dirs,
        } => {
            if dirs.is_empty() {
                return;
            }
            let mut logs = sorted_by_y(&b.trunks);
            r.shuffle(&mut logs);
            for p in logs {
                let d = *r.pick(dirs);
                let q = p.rel(d);
                if r.next_float() > *chance || !air(b, q) {
                    continue;
                }
                if let Some(s) = provider.get(b.region, facts, r, q) {
                    deco_set(b, q, s);
                }
            }
        }
        Decorator::PaleMoss {
            ground,
            leaves,
            trunk,
        } => {
            let mut logs = sorted_by_y(&b.trunks);
            if logs.is_empty() {
                return;
            }
            r.shuffle(&mut logs);
            let lowest = logs.iter().min_by_key(|p| p.y).copied().unwrap_or_default();
            if r.next_float() < *ground
                && let Some(patch) = cx.features.configured("minecraft:pale_moss_patch")
            {
                patch.place(cx, b.region, r, lowest.above(1));
            }
            for p in sorted_by_y(&b.trunks) {
                if r.next_float() < *trunk {
                    let q = p.below(1);
                    if air(b, q) {
                        moss_hanger(cx, b, r, q);
                    }
                }
            }
            for p in sorted_by_y(&b.foliage) {
                if r.next_float() < *leaves {
                    let q = p.below(1);
                    if air(b, q) {
                        moss_hanger(cx, b, r, q);
                    }
                }
            }
        }
        Decorator::CreakingHeart(chance) => {
            if r.next_float() >= *chance {
                return;
            }
            let logs = sorted_by_y(&b.trunks);
            let candidates: Vec<P> = logs
                .iter()
                .copied()
                .filter(|p| {
                    p.y > logs[0].y + 1
                        && b.trunks.contains(&p.above(1))
                        && b.trunks.contains(&p.below(1))
                })
                .collect();
            if candidates.is_empty() {
                return;
            }
            let p = *r.pick(&candidates);
            if let Some(s) = facts
                .state(
                    "minecraft:creaking_heart",
                    &[("axis", "y"), ("natural", "true")],
                )
                .or_else(|| facts.default_state("minecraft:creaking_heart"))
            {
                deco_set(b, p, s);
            }
        }
        Decorator::ShelfMushroom(chance) => {
            let Some(base) = facts.default_state("minecraft:shelf_mushroom") else {
                return;
            };
            for p in sorted_by_y(&b.trunks) {
                if r.next_float() >= *chance / 4.0 {
                    continue;
                }
                let d = HORIZONTAL[r.next_int_bound(4) as usize];
                let q = p.rel(d);
                if air(b, q) {
                    deco_set(b, q, facts.with(base, "facing", d.name()));
                }
            }
        }
    }
}

fn moss_hanger(cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, mut p: P) {
    let facts = cx.facts;
    let Some(moss) = facts.default_state("minecraft:pale_hanging_moss") else {
        return;
    };
    while facts.is_air(b.region.block(p.below(1))) && r.next_float() >= 0.5 {
        deco_set(b, p, facts.with(moss, "tip", "false"));
        p = p.below(1);
    }
    deco_set(b, p, facts.with(moss, "tip", "true"));
}

fn alter_circle(cx: &Ctx, b: &mut Built, r: &mut WorldgenRandom, provider: &StateProvider, c: P) {
    for i in -2..=2 {
        for j in -2..=2 {
            if i32::abs(i) == 2 && i32::abs(j) == 2 {
                continue;
            }
            let p = c.offset(i, 0, j);
            let mut k = 2;
            while k >= -3 {
                let q = p.above(k);
                let s = b.region.block(q);
                if cx.facts.is_grass_or_dirt(s) {
                    if let Some(state) = provider.get(b.region, cx.facts, r, p) {
                        deco_set(b, q, state);
                    }
                    break;
                }
                if !cx.facts.is_air(s) && k < 0 {
                    break;
                }
                k -= 1;
            }
        }
    }
}

/// Gives the tree's leaves their distance to the nearest log (up to 6;
/// farther leaves keep 7), as `TreeFeature` does.
fn update_leaves(facts: &Facts, b: &mut Built) {
    let all: Vec<P> = b
        .roots
        .iter()
        .chain(&b.trunks)
        .chain(&b.foliage)
        .chain(&b.decorations)
        .copied()
        .collect();
    let Some(first) = all.first() else { return };
    let (mut lo, mut hi) = (*first, *first);
    for p in &all {
        lo = P::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = P::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    let inside = |p: P| {
        p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && p.z >= lo.z && p.z <= hi.z
    };
    let mut done: HashSet<P> = b
        .decorations
        .iter()
        .chain(&b.roots)
        .copied()
        .filter(|p| inside(*p))
        .collect();
    let mut dist: HashMap<P, i32> = HashMap::new();
    let mut queue: VecDeque<(P, i32)> = b.trunks.iter().map(|p| (*p, 0)).collect();
    while let Some((p, k)) = queue.pop_front() {
        if !inside(p) || done.contains(&p) {
            continue;
        }
        if dist.get(&p).is_some_and(|&d| d < k) {
            continue;
        }
        done.insert(p);
        if k != 0 {
            let s = b.region.block(p);
            if facts.has_property(s, "distance") {
                b.region.set(p, facts.with(s, "distance", &k.to_string()));
            }
        }
        for d in Dir::ALL {
            let q = p.rel(d);
            if !inside(q) || done.contains(&q) {
                continue;
            }
            let s = b.region.block(q);
            let existing = if facts.in_named(s, "minecraft:logs") {
                0
            } else if let Some(v) = facts
                .property(s, "distance")
                .and_then(|v| v.parse::<i32>().ok())
            {
                v
            } else {
                continue;
            };
            let l = existing.min(k + 1);
            if l >= 7 {
                continue;
            }
            if dist.get(&q).is_none_or(|&d| l < d) {
                dist.insert(q, l);
                queue.push_back((q, l));
            }
        }
    }
}

impl FallenTree {
    pub fn place(&self, cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
        let facts = cx.facts;
        let mut b = Built {
            region,
            roots: Vec::new(),
            trunks: Vec::new(),
            foliage: Vec::new(),
            foliage_set: HashSet::new(),
            decorations: Vec::new(),
        };
        // The stump.
        if let Some(s) = self.trunk_provider.get(b.region, facts, r, p) {
            b.set(p, s);
        }
        b.trunks = vec![p];
        for d in &self.stump {
            decorate(cx, &mut b, r, d);
        }
        let dir = random_horizontal(r);
        let len = self.length.sample(r) - 2;
        let mut start = p.rel_n(dir, 2 + r.next_int_bound(2));
        let over_solid =
            |region: &dyn Region, q: P| facts.sturdy(region.block(q.below(1)), Dir::Up);
        start = start.above(1);
        for _ in 0..6 {
            if valid_tree_pos(facts, b.region, start) && over_solid(b.region, start) {
                break;
            }
            start = start.below(1);
        }
        // The whole log must fit, over ground with gaps of at most 2.
        let mut gap = 0;
        let mut m = start;
        for _ in 0..len {
            if !valid_tree_pos(facts, b.region, m) {
                return true;
            }
            if !over_solid(b.region, m) {
                gap += 1;
                if gap > 2 {
                    return true;
                }
            } else {
                gap = 0;
            }
            m = m.rel(dir);
        }
        let axis = if dir.axis() == mc_tick::Axis::X {
            "x"
        } else {
            "z"
        };
        let mut logs = Vec::new();
        let mut m = start;
        for _ in 0..len {
            if let Some(s) = self.trunk_provider.get(b.region, facts, r, m) {
                b.set(m, facts.with(s, "axis", axis));
                logs.push(m);
            }
            m = m.rel(dir);
        }
        b.trunks = logs;
        b.decorations.clear();
        for d in &self.log {
            decorate(cx, &mut b, r, d);
        }
        true
    }
}

impl Predicate {
    /// Used by tree-like features: whether trees may grow into `p`.
    pub fn tree_can_replace(facts: &Facts, region: &dyn Region, p: P) -> bool {
        valid_tree_pos(facts, region, p)
    }
}
