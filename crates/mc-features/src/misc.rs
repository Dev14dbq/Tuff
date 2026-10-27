//! The other feature types: ores, disks, lakes, springs, snow and ice,
//! columns (cactus, sugar cane, kelp), ground patches (moss, clay),
//! lichen, bamboo, huge mushrooms, rocks, ice spikes, vines, dungeons,
//! geodes, magma, rooted azaleas, corals, piles and dripstone. Behaviour as
//! minecraft.wiki describes each feature.

use std::sync::Arc;

use mc_tick::Dir;
use mc_worldgen::noise::{NoiseParameters, NormalNoise};
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use crate::blocks::Facts;
use crate::feature::{Ctx, Loader};
use crate::placement::Placed;
use crate::predicate::{BlockSet, Predicate, RuleTest};
use crate::rand::WorldgenRandom;
use crate::state::{StateProvider, parse_state};
use crate::value::{FloatProvider, IntProvider};
use crate::{Heightmap, P, Region};

const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];
/// `Direction.values()` order.
const ALL: [Dir; 6] = [
    Dir::Down,
    Dir::Up,
    Dir::North,
    Dir::South,
    Dir::West,
    Dir::East,
];

#[derive(Debug)]
pub struct Ore {
    size: i32,
    discard: f32,
    targets: Vec<(RuleTest, u32)>,
    scattered: bool,
}

#[derive(Debug)]
pub enum Misc {
    Ore(Ore),
    Disk {
        provider: StateProvider,
        target: Predicate,
        radius: IntProvider,
        half_height: i32,
    },
    Lake {
        fluid: StateProvider,
        barrier: StateProvider,
        can_place: Predicate,
        replace_air_fluid: Predicate,
        replace_barrier: Predicate,
    },
    Spring {
        state: u32,
        rock: i32,
        hole: i32,
        below: bool,
        valid: BlockSet,
    },
    FreezeTopLayer,
    Column {
        dir: Dir,
        allowed: Predicate,
        tip: bool,
        layers: Vec<(IntProvider, StateProvider)>,
    },
    Patch {
        replaceable: BlockSet,
        ground: StateProvider,
        vegetation: Arc<Placed>,
        ceiling: bool,
        depth: IntProvider,
        extra_bottom: f32,
        vertical: i32,
        chance: f32,
        radius: IntProvider,
        edge: f32,
        waterlogged: bool,
    },
    Multiface {
        block: u32,
        spread: f32,
        floor: bool,
        ceiling: bool,
        wall: bool,
        on: BlockSet,
    },
    BlueIce,
    Bamboo(f32),
    HugeMushroom {
        red: bool,
        on: Predicate,
        cap: StateProvider,
        stem: StateProvider,
        radius: i32,
    },
    Blob {
        state: u32,
        on: Predicate,
    },
    Spike {
        state: u32,
        on: Predicate,
        replace: Predicate,
    },
    Vines,
    MonsterRoom,
    Geode(Box<Geode>),
    Magma {
        range: i32,
        radius: i32,
        chance: f32,
    },
    RootSystem(Box<RootSystem>),
    Coral {
        claw: bool,
        block: Arc<Placed>,
    },
    Pile(StateProvider),
    Iceberg(u32),
    Speleothem {
        base: u32,
        pointed: u32,
        replaceable: BlockSet,
    },
    SpeleothemCluster(Box<Cluster>),
    SculkPatch(crate::sculk::Patch),
    Template {
        templates: Vec<(String, i32)>,
        processors: Vec<Arc<crate::structure::template::Processor>>,
    },
    Fossil {
        fossils: Vec<String>,
        overlays: Vec<String>,
        fossil_processors: Vec<Arc<crate::structure::template::Processor>>,
        overlay_processors: Vec<Arc<crate::structure::template::Processor>>,
        max_empty_corners: i32,
    },
    LargeDripstone(Box<LargeDripstone>),
}

#[derive(Debug)]
pub struct LargeDripstone {
    search: i32,
    radius: IntProvider,
    radius_range: (i32, i32),
    height_scale: FloatProvider,
    ratio: f32,
    stalactite_bluntness: FloatProvider,
    stalagmite_bluntness: FloatProvider,
    wind_speed: FloatProvider,
    min_radius_for_wind: i32,
    min_bluntness_for_wind: f32,
}

#[derive(Debug)]
pub struct Geode {
    filling: StateProvider,
    inner: StateProvider,
    alternate: StateProvider,
    middle: StateProvider,
    outer: StateProvider,
    placements: Vec<u32>,
    cannot_replace: BlockSet,
    invalid: BlockSet,
    layers: [f64; 4],
    crack_chance: f64,
    crack_size: f64,
    crack_offset: i32,
    noise_multiplier: f64,
    alternate_chance: f64,
    require_alternate: bool,
    potential_chance: f64,
    outer_wall: IntProvider,
    points: IntProvider,
    point_offset: IntProvider,
    min_offset: i32,
    max_offset: i32,
    invalid_threshold: i32,
}

#[derive(Debug)]
pub struct RootSystem {
    tree: Arc<Placed>,
    required_space: i32,
    root_radius: i32,
    root_replaceable: BlockSet,
    root_provider: StateProvider,
    root_attempts: i32,
    column_height: i32,
    hanging_radius: i32,
    hanging_span: i32,
    hanging_provider: StateProvider,
    hanging_attempts: i32,
    water: i32,
    allowed: Predicate,
}

#[derive(Debug)]
pub struct Cluster {
    base: u32,
    pointed: u32,
    replaceable: BlockSet,
    search: i32,
    height: IntProvider,
    radius: IntProvider,
    height_diff: i32,
    deviation: i32,
    thickness: IntProvider,
    density: FloatProvider,
    wetness: FloatProvider,
    edge_chance: f32,
    edge_distance: i32,
    bias_distance: i32,
}

fn int(v: &Value, k: &str, d: i32) -> i32 {
    v.get(k).and_then(Value::as_i64).map_or(d, |n| n as i32)
}

fn float(v: &Value, k: &str, d: f32) -> f32 {
    v.get(k).and_then(Value::as_f64).map_or(d, |n| n as f32)
}

fn ip(v: &Value, k: &str, d: i32) -> IntProvider {
    IntProvider::parse(&v[k]).unwrap_or(IntProvider::Constant(d))
}

fn pred(v: &Value, facts: &Facts) -> Predicate {
    if v.is_null() {
        Predicate::True
    } else {
        Predicate::parse(v, facts).unwrap_or(Predicate::True)
    }
}

impl Misc {
    pub fn parse(ty: &str, v: &Value, l: &Loader) -> Option<Self> {
        let facts = l.facts;
        let set = |x: &Value| BlockSet::parse(x, facts);
        Some(match ty {
            "ore" | "scattered_ore" => Misc::Ore(Ore {
                size: int(v, "size", 8),
                discard: float(v, "discard_chance_on_air_exposure", 0.0),
                targets: v["targets"]
                    .as_array()?
                    .iter()
                    .filter_map(|t| {
                        Some((
                            RuleTest::parse(&t["target"], facts)?,
                            parse_state(&t["state"], facts)?,
                        ))
                    })
                    .collect(),
                scattered: ty == "scattered_ore",
            }),
            "disk" => Misc::Disk {
                provider: l.provider(&v["state_provider"])?,
                target: pred(&v["target"], facts),
                radius: ip(v, "radius", 2),
                half_height: int(v, "half_height", 1),
            },
            "lake" => Misc::Lake {
                fluid: l.provider(&v["fluid"])?,
                barrier: l.provider(&v["barrier"])?,
                can_place: pred(&v["can_place_feature"], facts),
                replace_air_fluid: pred(&v["can_replace_with_air_or_fluid"], facts),
                replace_barrier: pred(&v["can_replace_with_barrier"], facts),
            },
            "spring_feature" => Misc::Spring {
                state: parse_state(&v["state"], facts).map(|s| facts.with(s, "level", "0"))?,
                rock: int(v, "rock_count", 4),
                hole: int(v, "hole_count", 1),
                below: v["requires_block_below"].as_bool().unwrap_or(true),
                valid: set(&v["valid_blocks"])?,
            },
            "freeze_top_layer" => Misc::FreezeTopLayer,
            "block_column" => Misc::Column {
                dir: Dir::from_name(v["direction"].as_str().unwrap_or("up"))?,
                allowed: pred(&v["allowed_placement"], facts),
                tip: v["prioritize_tip"].as_bool().unwrap_or(false),
                layers: v["layers"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| {
                        Some((
                            IntProvider::parse(&e["height"])?,
                            l.provider(&e["provider"])?,
                        ))
                    })
                    .collect(),
            },
            "vegetation_patch" | "waterlogged_vegetation_patch" => Misc::Patch {
                replaceable: set(&v["replaceable"])?,
                ground: l.provider(&v["ground_state"])?,
                vegetation: l.placed(&v["vegetation_feature"]),
                ceiling: v["surface"].as_str() == Some("ceiling"),
                depth: ip(v, "depth", 1),
                extra_bottom: float(v, "extra_bottom_block_chance", 0.0),
                vertical: int(v, "vertical_range", 1),
                chance: float(v, "vegetation_chance", 0.0),
                radius: ip(v, "xz_radius", 1),
                edge: float(v, "extra_edge_column_chance", 0.0),
                waterlogged: ty == "waterlogged_vegetation_patch",
            },
            "multiface_growth" => Misc::Multiface {
                block: facts.block_id(v["block"].as_str().unwrap_or("minecraft:glow_lichen"))?,
                spread: float(v, "chance_of_spreading", 0.5),
                floor: v["can_place_on_floor"].as_bool().unwrap_or(false),
                ceiling: v["can_place_on_ceiling"].as_bool().unwrap_or(false),
                wall: v["can_place_on_wall"].as_bool().unwrap_or(false),
                on: set(&v["can_be_placed_on"])?,
            },
            "blue_ice" => Misc::BlueIce,
            "bamboo" => Misc::Bamboo(float(v, "probability", 0.0)),
            "huge_red_mushroom" | "huge_brown_mushroom" => Misc::HugeMushroom {
                red: ty == "huge_red_mushroom",
                on: pred(&v["can_place_on"], facts),
                cap: l.provider(&v["cap_provider"])?,
                stem: l.provider(&v["stem_provider"])?,
                radius: int(v, "foliage_radius", 2),
            },
            "block_blob" | "forest_rock" => Misc::Blob {
                state: parse_state(&v["state"], facts)?,
                on: pred(&v["can_place_on"], facts),
            },
            "spike" => Misc::Spike {
                state: parse_state(&v["state"], facts)?,
                on: pred(&v["can_place_on"], facts),
                replace: pred(&v["can_replace"], facts),
            },
            "vines" => Misc::Vines,
            "monster_room" => Misc::MonsterRoom,
            "geode" => {
                let b = &v["blocks"];
                let layers = &v["layers"];
                let crack = &v["crack"];
                Misc::Geode(Box::new(Geode {
                    filling: l.provider(&b["filling_provider"])?,
                    inner: l.provider(&b["inner_layer_provider"])?,
                    alternate: l.provider(&b["alternate_inner_layer_provider"])?,
                    middle: l.provider(&b["middle_layer_provider"])?,
                    outer: l.provider(&b["outer_layer_provider"])?,
                    placements: b["inner_placements"]
                        .as_array()?
                        .iter()
                        .filter_map(|s| parse_state(s, facts))
                        .collect(),
                    cannot_replace: set(&b["cannot_replace"])?,
                    invalid: set(&b["invalid_blocks"])?,
                    layers: [
                        layers["filling"].as_f64().unwrap_or(1.7),
                        layers["inner_layer"].as_f64().unwrap_or(2.2),
                        layers["middle_layer"].as_f64().unwrap_or(3.2),
                        layers["outer_layer"].as_f64().unwrap_or(4.2),
                    ],
                    crack_chance: crack["generate_crack_chance"].as_f64().unwrap_or(1.0),
                    crack_size: crack["base_crack_size"].as_f64().unwrap_or(2.0),
                    crack_offset: int(crack, "crack_point_offset", 2),
                    noise_multiplier: v["noise_multiplier"].as_f64().unwrap_or(0.05),
                    alternate_chance: v["use_alternate_layer0_chance"].as_f64().unwrap_or(0.0),
                    require_alternate: v["placements_require_layer0_alternate"]
                        .as_bool()
                        .unwrap_or(true),
                    potential_chance: v["use_potential_placements_chance"]
                        .as_f64()
                        .unwrap_or(0.35),
                    outer_wall: ip(v, "outer_wall_distance", 4),
                    points: IntProvider::parse(&v["distribution_points"])
                        .unwrap_or(IntProvider::Uniform(3, 4)),
                    point_offset: IntProvider::parse(&v["point_offset"])
                        .unwrap_or(IntProvider::Uniform(1, 2)),
                    min_offset: int(v, "min_gen_offset", -16),
                    max_offset: int(v, "max_gen_offset", 16),
                    invalid_threshold: int(v, "invalid_blocks_threshold", 0),
                }))
            }
            "underwater_magma" => Misc::Magma {
                range: int(v, "floor_search_range", 5),
                radius: int(v, "placement_radius_around_floor", 1),
                chance: float(v, "placement_probability_per_valid_position", 0.5),
            },
            "root_system" => Misc::RootSystem(Box::new(RootSystem {
                tree: l.placed(&v["feature"]),
                required_space: int(v, "required_vertical_space_for_tree", 3),
                root_radius: int(v, "root_radius", 3),
                root_replaceable: set(&v["root_replaceable"])?,
                root_provider: l.provider(&v["root_state_provider"])?,
                root_attempts: int(v, "root_placement_attempts", 20),
                column_height: int(v, "root_column_max_height", 100),
                hanging_radius: int(v, "hanging_root_radius", 3),
                hanging_span: int(v, "hanging_roots_vertical_span", 2),
                hanging_provider: l.provider(&v["hanging_root_state_provider"])?,
                hanging_attempts: int(v, "hanging_root_placement_attempts", 20),
                water: int(v, "allowed_vertical_water_for_tree", 2),
                allowed: pred(&v["allowed_tree_position"], facts),
            })),
            "coral_tree" | "coral_claw" => Misc::Coral {
                claw: ty == "coral_claw",
                block: l.placed(&v["feature"]),
            },
            "block_pile" => Misc::Pile(l.provider(&v["state_provider"])?),
            "iceberg" => Misc::Iceberg(parse_state(&v["state"], facts)?),
            "speleothem" | "pointed_dripstone" => Misc::Speleothem {
                base: parse_state(&v["base_block"], facts)
                    .or_else(|| facts.default_state("minecraft:dripstone_block"))?,
                pointed: parse_state(&v["pointed_block"], facts)
                    .or_else(|| facts.default_state("minecraft:pointed_dripstone"))?,
                replaceable: set(&v["replaceable_blocks"]).or_else(|| {
                    facts
                        .tag("minecraft:dripstone_replaceable_blocks")
                        .map(BlockSet::Tag)
                })?,
            },
            "speleothem_cluster" | "dripstone_cluster" => {
                Misc::SpeleothemCluster(Box::new(Cluster {
                    base: parse_state(&v["base_block"], facts)
                        .or_else(|| facts.default_state("minecraft:dripstone_block"))?,
                    pointed: parse_state(&v["pointed_block"], facts)
                        .or_else(|| facts.default_state("minecraft:pointed_dripstone"))?,
                    replaceable: set(&v["replaceable_blocks"]).or_else(|| {
                        facts
                            .tag("minecraft:dripstone_replaceable_blocks")
                            .map(BlockSet::Tag)
                    })?,
                    search: int(v, "floor_to_ceiling_search_range", 12),
                    height: ip(v, "height", 3),
                    radius: ip(v, "radius", 2),
                    height_diff: int(v, "max_stalagmite_stalactite_height_diff", 1),
                    deviation: int(v, "height_deviation", 3),
                    thickness: IntProvider::parse(&v["speleothem_block_layer_thickness"])
                        .or_else(|| IntProvider::parse(&v["dripstone_block_layer_thickness"]))
                        .unwrap_or(IntProvider::Uniform(2, 4)),
                    density: FloatProvider::parse(&v["density"])
                        .unwrap_or(FloatProvider::Constant(0.5)),
                    wetness: FloatProvider::parse(&v["wetness"])
                        .unwrap_or(FloatProvider::Constant(0.0)),
                    edge_chance: v
                        .get("chance_of_speleothem_at_max_distance_from_center")
                        .or_else(|| v.get("chance_of_dripstone_column_at_max_distance_from_center"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.1) as f32,
                    edge_distance: v
                        .get("max_distance_from_edge_affecting_chance_of_speleothem")
                        .or_else(|| {
                            v.get("max_distance_from_edge_affecting_chance_of_dripstone_column")
                        })
                        .and_then(Value::as_i64)
                        .unwrap_or(3) as i32,
                    bias_distance: int(v, "max_distance_from_center_affecting_height_bias", 8),
                }))
            }
            "template" => Misc::Template {
                templates: v["templates"]
                    .as_array()?
                    .iter()
                    .filter_map(|e| {
                        let id = e["data"]["id"].as_str().or_else(|| e["data"].as_str())?;
                        Some((id.to_owned(), e["weight"].as_i64().unwrap_or(1) as i32))
                    })
                    .collect(),
                processors: l.processors(&v["processors"]),
            },
            "fossil" => {
                let names = |k: &str| -> Vec<String> {
                    v[k].as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                };
                Misc::Fossil {
                    fossils: names("fossil_structures"),
                    overlays: names("overlay_structures"),
                    fossil_processors: l.processors(&v["fossil_processors"]),
                    overlay_processors: l.processors(&v["overlay_processors"]),
                    max_empty_corners: int(v, "max_empty_corners_allowed", 4),
                }
            }
            "large_dripstone" => {
                let radius = ip(v, "column_radius", 3);
                let radius_range = match &radius {
                    IntProvider::Clamped(_, lo, hi) => (*lo, *hi),
                    IntProvider::Uniform(lo, hi) => (*lo, *hi),
                    p => (p.max(), p.max()),
                };
                let fp = |k: &str, d: f32| {
                    FloatProvider::parse(&v[k]).unwrap_or(FloatProvider::Constant(d))
                };
                Misc::LargeDripstone(Box::new(LargeDripstone {
                    search: int(v, "floor_to_ceiling_search_range", 30),
                    radius,
                    radius_range,
                    height_scale: fp("height_scale", 1.0),
                    ratio: float(v, "max_column_radius_to_cave_height_ratio", 0.4),
                    stalactite_bluntness: fp("stalactite_bluntness", 0.5),
                    stalagmite_bluntness: fp("stalagmite_bluntness", 0.5),
                    wind_speed: fp("wind_speed", 0.0),
                    min_radius_for_wind: int(v, "min_radius_for_wind", 4),
                    min_bluntness_for_wind: float(v, "min_bluntness_for_wind", 0.6),
                }))
            }
            "sculk_patch" => Misc::SculkPatch(crate::sculk::Patch {
                charges: int(v, "charge_count", 10),
                amount: int(v, "amount_per_charge", 32),
                attempts: int(v, "spread_attempts", 64),
                spread_rounds: int(v, "spread_rounds", 1),
                growth_rounds: int(v, "growth_rounds", 0),
            }),
            _ => return None,
        })
    }

    pub fn place(&self, cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
        let facts = cx.facts;
        match self {
            Misc::Ore(ore) => {
                if ore.scattered {
                    scattered_ore(facts, region, r, p, ore)
                } else {
                    place_ore(facts, region, r, p, ore)
                }
            }
            Misc::Disk {
                provider,
                target,
                radius,
                half_height,
            } => {
                let top = p.y + half_height;
                let bottom = p.y - half_height - 1;
                let rad = radius.sample(r);
                let mut any = false;
                for dz in -rad..=rad {
                    for dx in -rad..=rad {
                        if dx * dx + dz * dz > rad * rad {
                            continue;
                        }
                        let mut y = top;
                        while y > bottom {
                            let q = P::new(p.x + dx, y, p.z + dz);
                            if target.test(region, facts, q)
                                && let Some(s) = provider.get(region, facts, r, q)
                            {
                                region.set(q, s);
                                any = true;
                            }
                            y -= 1;
                        }
                    }
                }
                any
            }
            Misc::Lake {
                fluid,
                barrier,
                can_place,
                replace_air_fluid,
                replace_barrier,
            } => lake(
                cx,
                region,
                r,
                p,
                fluid,
                barrier,
                can_place,
                replace_air_fluid,
                replace_barrier,
            ),
            Misc::Spring {
                state,
                rock,
                hole,
                below,
                valid,
            } => {
                let is_valid = |q: P| valid.contains(facts, region.block(q));
                if !is_valid(p.above(1)) || (*below && !is_valid(p.below(1))) {
                    return false;
                }
                let here = region.block(p);
                if !facts.is_air(here) && !valid.contains(facts, here) {
                    return false;
                }
                let sides = [
                    p.rel(Dir::West),
                    p.rel(Dir::East),
                    p.rel(Dir::North),
                    p.rel(Dir::South),
                    p.below(1),
                ];
                let rocks = sides.iter().filter(|q| is_valid(**q)).count() as i32;
                let holes = sides
                    .iter()
                    .filter(|q| facts.is_air(region.block(**q)))
                    .count() as i32;
                if rocks == *rock && holes == *hole {
                    region.set(p, *state);
                    region.schedule_tick(p);
                    true
                } else {
                    false
                }
            }
            Misc::FreezeTopLayer => {
                for dx in 0..16 {
                    for dz in 0..16 {
                        let x = p.x + dx;
                        let z = p.z + dz;
                        let y = region.height(Heightmap::MotionBlocking, x, z);
                        let top = P::new(x, y, z);
                        let below = top.below(1);
                        let biome = region.biome(top);
                        if cx.features.should_freeze(region, facts, biome, below)
                            && let Some(ice) = facts.default_state("minecraft:ice")
                        {
                            region.set(below, ice);
                        }
                        if cx.features.should_snow(region, facts, biome, top) {
                            if let Some(snow) = facts.default_state("minecraft:snow") {
                                region.set(top, snow);
                            }
                            let b = region.block(below);
                            if facts.has_property(b, "snowy") {
                                region.set(below, facts.with(b, "snowy", "true"));
                            }
                        }
                    }
                }
                true
            }
            Misc::Column {
                dir,
                allowed,
                tip,
                layers,
            } => {
                let mut heights: Vec<i32> = layers.iter().map(|(h, _)| h.sample(r)).collect();
                let total: i32 = heights.iter().sum();
                if total == 0 {
                    return false;
                }
                let mut ahead = p.rel(*dir);
                for l in 0..total {
                    if !allowed.test(region, facts, ahead) {
                        truncate(&mut heights, total, l, *tip);
                        break;
                    }
                    ahead = ahead.rel(*dir);
                }
                let mut m = p;
                for (i, &h) in heights.iter().enumerate() {
                    for _ in 0..h {
                        if let Some(s) = layers[i].1.get(region, facts, r, m) {
                            let s = crate::feature::waterlog(facts, region, m, s);
                            region.set(m, s);
                        }
                        m = m.rel(*dir);
                    }
                }
                true
            }
            Misc::Patch { .. } => patch(cx, region, r, p, self),
            Misc::Multiface {
                block,
                spread,
                floor,
                ceiling,
                wall,
                on,
            } => multiface(
                facts, region, r, p, *block, *spread, *floor, *ceiling, *wall, on,
            ),
            Misc::BlueIce => blue_ice(cx, region, r, p),
            Misc::Bamboo(chance) => bamboo(facts, region, r, p, *chance),
            Misc::HugeMushroom {
                red,
                on,
                cap,
                stem,
                radius,
            } => huge_mushroom(facts, region, r, p, *red, on, cap, stem, *radius),
            Misc::Blob { state, on } => {
                let h = region.heights();
                let mut q = p;
                while q.y > h.min_y + 3 {
                    let below = q.below(1);
                    if !facts.is_air(region.block(below)) && on.test(region, facts, below) {
                        break;
                    }
                    q = below;
                }
                if q.y <= h.min_y + 3 {
                    return false;
                }
                for _ in 0..3 {
                    let j = r.next_int_bound(2);
                    let k = r.next_int_bound(2);
                    let l = r.next_int_bound(2);
                    let f = (j + k + l) as f32 * 0.333 + 0.5;
                    for z in -l..=l {
                        for y in -k..=k {
                            for x in -j..=j {
                                if ((x * x + y * y + z * z) as f64) <= f64::from(f * f) {
                                    region.set(q.offset(x, y, z), *state);
                                }
                            }
                        }
                    }
                    q = q.offset(
                        -1 + r.next_int_bound(2),
                        -r.next_int_bound(2),
                        -1 + r.next_int_bound(2),
                    );
                }
                true
            }
            Misc::Spike { state, on, replace } => spike(facts, region, r, p, *state, on, replace),
            Misc::Vines => {
                if !facts.is_air(region.block(p)) {
                    return false;
                }
                for d in ALL {
                    if d == Dir::Down {
                        continue;
                    }
                    if facts.sturdy(region.block(p.rel(d)), d.opposite())
                        && let Some(v) = facts.state("minecraft:vine", &[(d.name(), "true")])
                    {
                        region.set(p, v);
                        return true;
                    }
                }
                false
            }
            Misc::MonsterRoom => monster_room(facts, region, r, p),
            Misc::Geode(g) => geode(facts, region, r, p, g),
            Misc::Magma {
                range,
                radius,
                chance,
            } => {
                let water_or_air = |region: &dyn Region, q: P| {
                    let s = region.block(q);
                    facts.is(s, "minecraft:water") || facts.is_air(s)
                };
                if !facts.is(region.block(p), "minecraft:water") {
                    return false;
                }
                let mut q = p;
                let mut i = 1;
                while i < *range && facts.is(region.block(q), "minecraft:water") {
                    q = q.below(1);
                    i += 1;
                }
                if facts.is(region.block(q), "minecraft:water") {
                    return false;
                }
                let floor = q;
                let Some(magma) = facts.default_state("minecraft:magma_block") else {
                    return false;
                };
                let mut placed = 0;
                for z in -radius..=*radius {
                    for y in -radius..=*radius {
                        for x in -radius..=*radius {
                            let c = floor.offset(x, y, z);
                            if r.next_float() >= *chance {
                                continue;
                            }
                            if water_or_air(region, c)
                                || water_or_air(region, c.below(1))
                                || HORIZONTAL.iter().any(|d| water_or_air(region, c.rel(*d)))
                            {
                                continue;
                            }
                            region.set(c, magma);
                            placed += 1;
                        }
                    }
                }
                placed > 0
            }
            Misc::RootSystem(rs) => root_system(cx, region, r, p, rs),
            Misc::Coral { claw, block } => {
                if *claw {
                    coral_claw(cx, region, r, p, block)
                } else {
                    coral_tree(cx, region, r, p, block)
                }
            }
            Misc::Pile(provider) => {
                if p.y < region.heights().min_y + 5 {
                    return false;
                }
                let i = 2 + r.next_int_bound(2);
                let j = 2 + r.next_int_bound(2);
                for z in -j..=j {
                    for y in 0..=1 {
                        for x in -i..=i {
                            let q = p.offset(x, y, z);
                            let d2 = (x * x + z * z) as f32;
                            if d2 <= r.next_float() * 10.0 - r.next_float() * 6.0
                                || r.next_float() < 0.031
                            {
                                pile_block(facts, region, r, q, provider);
                            }
                        }
                    }
                }
                true
            }
            Misc::Iceberg(state) => iceberg(cx, region, r, p, *state),
            Misc::Speleothem {
                base,
                pointed,
                replaceable,
            } => speleothem(facts, region, r, p, *base, *pointed, replaceable),
            Misc::SpeleothemCluster(c) => cluster(facts, region, r, p, c),
            Misc::SculkPatch(c) => crate::sculk::patch(facts, region, r, p, c),
            Misc::Template {
                templates,
                processors,
            } => {
                let Some(id) = crate::value::weighted(templates, r) else {
                    return false;
                };
                let Some(t) = cx.features.structures.template(id) else {
                    return false;
                };
                let rotation = crate::structure::template::Rotation(r.next_int_bound(4) as u8);
                let pivot = P::new(t.size.x / 2, 0, t.size.z / 2);
                let settings = crate::structure::template::Settings {
                    rotation,
                    pivot,
                    processors: processors.clone(),
                    waterlog: true,
                    ..Default::default()
                };
                let corner = p.offset(-pivot.x, 0, -pivot.z);
                t.place(facts, region, r, corner, corner, &settings)
            }
            Misc::Fossil {
                fossils,
                overlays,
                fossil_processors,
                overlay_processors,
                max_empty_corners,
            } => fossil(
                cx,
                region,
                r,
                p,
                fossils,
                overlays,
                fossil_processors,
                overlay_processors,
                *max_empty_corners,
            ),
            Misc::LargeDripstone(c) => large_dripstone(facts, region, r, p, c),
        }
    }
}

fn truncate(heights: &mut [i32], total: i32, placeable: i32, tip: bool) {
    let mut k = total - placeable;
    let n = heights.len() as i32;
    let (step, start, end) = if tip { (1, 0, n) } else { (-1, n - 1, -1) };
    let mut o = start;
    while o != end && k > 0 {
        let h = heights[o as usize];
        let q = h.min(k);
        k -= q;
        heights[o as usize] -= q;
        o += step;
    }
}

fn can_place_ore(
    facts: &Facts,
    region: &dyn Region,
    r: &mut WorldgenRandom,
    ore: &Ore,
    test: &RuleTest,
    state: u32,
    q: P,
) -> bool {
    if !test.test(facts, state, q.y, r) {
        return false;
    }
    let skip_air = if ore.discard <= 0.0 {
        true
    } else if ore.discard >= 1.0 {
        false
    } else {
        r.next_float() >= ore.discard
    };
    if skip_air {
        return true;
    }
    !ALL.iter().any(|d| facts.is_air(region.block(q.rel(*d))))
}

fn place_ore(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    ore: &Ore,
) -> bool {
    let f = r.next_float() * std::f32::consts::PI;
    let g = ore.size as f32 / 8.0;
    let i = ((ore.size as f32 / 16.0 * 2.0 + 1.0) / 2.0).ceil() as i32;
    let (sin, cos) = (f64::from(f.sin()), f64::from(f.cos()));
    let x1 = f64::from(p.x) + sin * f64::from(g);
    let x2 = f64::from(p.x) - sin * f64::from(g);
    let z1 = f64::from(p.z) + cos * f64::from(g);
    let z2 = f64::from(p.z) - cos * f64::from(g);
    let y1 = f64::from(p.y + r.next_int_bound(3) - 2);
    let y2 = f64::from(p.y + r.next_int_bound(3) - 2);
    let gc = g.ceil() as i32;
    let min_x = p.x - gc - i;
    let min_y = p.y - 2 - i;
    let min_z = p.z - gc - i;
    let w = 2 * (gc + i);
    let h = 2 * (2 + i);
    for x in min_x..=min_x + w {
        for z in min_z..=min_z + w {
            if min_y <= region.height(Heightmap::OceanFloorWg, x, z) {
                return ore_blob(
                    facts,
                    region,
                    r,
                    ore,
                    [x1, x2, z1, z2, y1, y2],
                    min_x,
                    min_y,
                    min_z,
                    w,
                    h,
                );
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn ore_blob(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    ore: &Ore,
    c: [f64; 6],
    min_x: i32,
    min_y: i32,
    min_z: i32,
    w: i32,
    h: i32,
) -> bool {
    let [x1, x2, z1, z2, y1, y2] = c;
    let n = ore.size as usize;
    let mut ds = vec![0.0f64; n * 4];
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    for q in 0..n {
        let t = q as f32 / n as f32;
        let s = lerp(f64::from(t), x1, x2);
        let yy = lerp(f64::from(t), y1, y2);
        let u = lerp(f64::from(t), z1, z2);
        let v = r.next_double() * n as f64 / 16.0;
        let rad = (f64::from((std::f32::consts::PI * t).sin() + 1.0) * v + 1.0) / 2.0;
        ds[q * 4] = s;
        ds[q * 4 + 1] = yy;
        ds[q * 4 + 2] = u;
        ds[q * 4 + 3] = rad;
    }
    for q in 0..n.saturating_sub(1) {
        if ds[q * 4 + 3] <= 0.0 {
            continue;
        }
        for x in q + 1..n {
            if ds[x * 4 + 3] <= 0.0 {
                continue;
            }
            let s = ds[q * 4] - ds[x * 4];
            let t = ds[q * 4 + 1] - ds[x * 4 + 1];
            let u = ds[q * 4 + 2] - ds[x * 4 + 2];
            let v = ds[q * 4 + 3] - ds[x * 4 + 3];
            if v * v > s * s + t * t + u * u {
                if v > 0.0 {
                    ds[x * 4 + 3] = -1.0;
                } else {
                    ds[q * 4 + 3] = -1.0;
                }
            }
        }
    }
    let size = (w * w * h).max(1) as usize;
    let mut done = vec![false; size + (w * h * 2 + w * 2 + 2) as usize];
    let mut count = 0;
    let hgt = region.heights();
    for q in 0..n {
        let s = ds[q * 4 + 3];
        if s < 0.0 {
            continue;
        }
        let (t, u, v) = (ds[q * 4], ds[q * 4 + 1], ds[q * 4 + 2]);
        let x0 = ((t - s).floor() as i32).max(min_x);
        let y0 = ((u - s).floor() as i32).max(min_y);
        let z0 = ((v - s).floor() as i32).max(min_z);
        let x1 = ((t + s).floor() as i32).max(x0);
        let y1 = ((u + s).floor() as i32).max(y0);
        let z1 = ((v + s).floor() as i32).max(z0);
        for x in x0..=x1 {
            let af = (f64::from(x) + 0.5 - t) / s;
            if af * af >= 1.0 {
                continue;
            }
            for y in y0..=y1 {
                let ah = (f64::from(y) + 0.5 - u) / s;
                if af * af + ah * ah >= 1.0 {
                    continue;
                }
                for z in z0..=z1 {
                    let aj = (f64::from(z) + 0.5 - v) / s;
                    if af * af + ah * ah + aj * aj >= 1.0
                        || y < hgt.min_y
                        || y >= hgt.min_y + hgt.height
                    {
                        continue;
                    }
                    let idx = (x - min_x) + (y - min_y) * w + (z - min_z) * w * h;
                    let Some(slot) = usize::try_from(idx).ok().and_then(|i| done.get_mut(i)) else {
                        continue;
                    };
                    if *slot {
                        continue;
                    }
                    *slot = true;
                    let qp = P::new(x, y, z);
                    let state = region.block(qp);
                    for (test, ore_state) in &ore.targets {
                        if can_place_ore(facts, region, r, ore, test, state, qp) {
                            if region.set(qp, *ore_state) {
                                count += 1;
                            }
                            break;
                        }
                    }
                }
            }
        }
    }
    count > 0
}

fn scattered_ore(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    ore: &Ore,
) -> bool {
    let n = r.next_int_bound(ore.size + 1);
    for j in 0..n {
        let spread = j.min(7) as f32;
        let mut axis = || ((r.next_float() - r.next_float()) * spread).round() as i32;
        let (dx, dy, dz) = (axis(), axis(), axis());
        let q = p.offset(dx, dy, dz);
        let state = region.block(q);
        for (test, ore_state) in &ore.targets {
            if can_place_ore(facts, region, r, ore, test, state, q) {
                region.set(q, *ore_state);
                break;
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn lake(
    cx: &Ctx,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    fluid: &StateProvider,
    barrier: &StateProvider,
    can_place: &Predicate,
    replace_air_fluid: &Predicate,
    replace_barrier: &Predicate,
) -> bool {
    let facts = cx.facts;
    if p.y <= region.heights().min_y + 4 || !can_place.test(region, facts, p) {
        return false;
    }
    // The lake's 16×8×16 box starts 8 blocks west and north of the spot.
    let o = p.offset(-8, -4, -8);
    let mut shape = vec![false; 2048];
    let at = |x: i32, z: i32, y: i32| ((x * 16 + z) * 8 + y) as usize;
    let n = r.next_int_bound(4) + 4;
    for _ in 0..n {
        let d = r.next_double() * 6.0 + 3.0;
        let e = r.next_double() * 4.0 + 2.0;
        let f = r.next_double() * 6.0 + 3.0;
        let g = r.next_double() * (16.0 - d - 2.0) + 1.0 + d / 2.0;
        let h = r.next_double() * (8.0 - e - 4.0) + 2.0 + e / 2.0;
        let k = r.next_double() * (16.0 - f - 2.0) + 1.0 + f / 2.0;
        for x in 1..15 {
            for z in 1..15 {
                for y in 1..7 {
                    let a = (f64::from(x) - g) / (d / 2.0);
                    let b = (f64::from(y) - h) / (e / 2.0);
                    let c = (f64::from(z) - k) / (f / 2.0);
                    if a * a + b * b + c * c < 1.0 {
                        shape[at(x, z, y)] = true;
                    }
                }
            }
        }
    }
    let edge = |shape: &[bool], x: i32, z: i32, y: i32| {
        !shape[at(x, z, y)]
            && (x < 15 && shape[at(x + 1, z, y)]
                || x > 0 && shape[at(x - 1, z, y)]
                || z < 15 && shape[at(x, z + 1, y)]
                || z > 0 && shape[at(x, z - 1, y)]
                || y < 7 && shape[at(x, z, y + 1)]
                || y > 0 && shape[at(x, z, y - 1)])
    };
    let Some(fluid_state) = fluid.get(region, facts, r, o) else {
        return false;
    };
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                if !edge(&shape, x, z, y) {
                    continue;
                }
                let s = region.block(o.offset(x, y, z));
                if y >= 4 && !facts.fluid(s).is_none() {
                    return false;
                }
                if y < 4 && !facts.solid(s) && s != fluid_state {
                    return false;
                }
            }
        }
    }
    for x in 0..16 {
        for z in 0..16 {
            for y in 0..8 {
                let q = o.offset(x, y, z);
                if !shape[at(x, z, y)] || !replace_air_fluid.test(region, facts, q) {
                    continue;
                }
                let upper = y >= 4;
                region.set(q, if upper { facts.cave_air } else { fluid_state });
                if !upper {
                    region.schedule_tick(q);
                }
            }
        }
    }
    if let Some(b) = barrier.get(region, facts, r, o)
        && !facts.is_air(b)
    {
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..8 {
                    if !edge(&shape, x, z, y) || (y >= 4 && r.next_int_bound(2) == 0) {
                        continue;
                    }
                    let q = o.offset(x, y, z);
                    if !facts.solid(region.block(q)) || !replace_barrier.test(region, facts, q) {
                        continue;
                    }
                    region.set(q, b);
                }
            }
        }
    }
    if matches!(facts.fluid(fluid_state), crate::Fluid::Water { .. })
        && let Some(ice) = facts.default_state("minecraft:ice")
    {
        for x in 0..16 {
            for z in 0..16 {
                let q = o.offset(x, 4, z);
                let biome = region.biome(q);
                if cx.features.should_freeze(region, facts, biome, q)
                    && replace_air_fluid.test(region, facts, q)
                {
                    region.set(q, ice);
                }
            }
        }
    }
    true
}

fn patch(cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P, m: &Misc) -> bool {
    let Misc::Patch {
        replaceable,
        ground,
        vegetation,
        ceiling,
        depth,
        extra_bottom,
        vertical,
        chance,
        radius,
        edge,
        waterlogged,
    } = m
    else {
        return false;
    };
    let facts = cx.facts;
    let dir = if *ceiling { Dir::Up } else { Dir::Down };
    let opp = dir.opposite();
    let i = radius.sample(r) + 1;
    let j = radius.sample(r) + 1;
    let mut set: Vec<P> = Vec::new();
    for k in -i..=i {
        let bx = k == -i || k == i;
        for l in -j..=j {
            let bz = l == -j || l == j;
            let edge_col = bx || bz;
            let corner = bx && bz;
            let side = edge_col && !corner;
            if corner || side && (*edge == 0.0 || r.next_float() > *edge) {
                continue;
            }
            let mut q = p.offset(k, 0, l);
            let mut n = 0;
            while facts.is_air(region.block(q)) && n < *vertical {
                q = q.rel(dir);
                n += 1;
            }
            n = 0;
            while !facts.is_air(region.block(q)) && n < *vertical {
                q = q.rel(opp);
                n += 1;
            }
            let ground_pos = q.rel(dir);
            if !facts.is_air(region.block(q)) || !facts.sturdy(region.block(ground_pos), opp) {
                continue;
            }
            let extra = *extra_bottom > 0.0 && r.next_float() < *extra_bottom;
            let d = depth.sample(r) + i32::from(extra);
            let mut g = ground_pos;
            let mut ok = true;
            for step in 0..d {
                let Some(s) = ground.get(region, facts, r, g) else {
                    break;
                };
                let here = region.block(g);
                if facts.block(s) != facts.block(here) {
                    if !replaceable.contains(facts, here) {
                        ok = step != 0;
                        break;
                    }
                    region.set(g, s);
                    g = g.rel(dir);
                }
            }
            if ok {
                set.push(ground_pos);
            }
        }
    }
    let mut set = java_hash_order(&set);
    if *waterlogged {
        let exposed = |region: &dyn Region, q: P| {
            [Dir::North, Dir::East, Dir::South, Dir::West, Dir::Down]
                .iter()
                .any(|d| !facts.sturdy(region.block(q.rel(*d)), d.opposite()))
        };
        let kept: Vec<P> = set
            .iter()
            .copied()
            .filter(|q| !exposed(region, *q))
            .collect();
        for q in &kept {
            region.set(*q, facts.water);
        }
        set = java_hash_order(&kept);
    }
    for q in &set {
        if *chance > 0.0 && r.next_float() < *chance {
            if *waterlogged {
                // The plant goes where the ground was, now water.
                if vegetation.place(cx, region, r, *q, None) {
                    let s = region.block(*q);
                    if facts.property(s, "waterlogged") == Some("false") {
                        region.set(*q, facts.with(s, "waterlogged", "true"));
                    }
                }
            } else {
                vegetation.place(cx, region, r, q.rel(opp), None);
            }
        }
    }
    !set.is_empty()
}

/// The order a `java.util.HashSet` of block positions, filled in this
/// order, hands them back: by bucket of the position's hash, then as
/// added.
pub(crate) fn java_hash_order(list: &[P]) -> Vec<P> {
    let mut cap = 16usize;
    while list.len() * 4 > cap * 3 {
        cap *= 2;
    }
    let bucket = |p: &P| {
        let h =
            p.y.wrapping_add(p.z.wrapping_mul(31))
                .wrapping_mul(31)
                .wrapping_add(p.x);
        ((h ^ ((h as u32) >> 16) as i32) as u32 as usize) & (cap - 1)
    };
    let mut out: Vec<(usize, usize, P)> = list
        .iter()
        .enumerate()
        .map(|(i, p)| (bucket(p), i, *p))
        .collect();
    out.sort_by_key(|&(b, i, _)| (b, i));
    out.into_iter().map(|(_, _, p)| p).collect()
}

#[allow(clippy::too_many_arguments)]
fn multiface(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    block: u32,
    spread: f32,
    floor: bool,
    ceiling: bool,
    wall: bool,
    on: &BlockSet,
) -> bool {
    let air_or_water = |s: u32| facts.is_air(s) || facts.is(s, "minecraft:water");
    if !air_or_water(region.block(p)) {
        return false;
    }
    let mut valid = Vec::new();
    if ceiling {
        valid.push(Dir::Up);
    }
    if floor {
        valid.push(Dir::Down);
    }
    if wall {
        valid.extend(HORIZONTAL);
    }
    let mut dirs = valid.clone();
    r.shuffle(&mut dirs);
    let spreader = crate::multiface::Spreader::new(facts, facts.default_of(block));
    // `placeGrowthIfPossible`: on the first face that holds it, then
    // perhaps on toward a random direction.
    let grow = |region: &mut dyn Region, r: &mut WorldgenRandom, q: P, dirs: &[Dir]| -> bool {
        let old = region.block(q);
        for d in dirs {
            if !on.contains(facts, region.block(q.rel(*d))) {
                continue;
            }
            let Some(new) = spreader.state_for_placement(region, old, q, *d) else {
                return false;
            };
            region.set(q, new);
            if r.next_float() < spread {
                spreader.spread_from_face(region, r, new, q, *d);
            }
            return true;
        }
        false
    };
    if grow(region, r, p, &dirs) {
        return true;
    }
    // Each direction looks at the neighbouring block only: the game's
    // search starts from the origin again on every step.
    for d in dirs.clone() {
        let mut others: Vec<Dir> = valid
            .iter()
            .copied()
            .filter(|e| *e != d.opposite())
            .collect();
        r.shuffle(&mut others);
        let q = p.rel(d);
        let s = region.block(q);
        if !air_or_water(s) && facts.block(s) != block {
            continue;
        }
        if grow(region, r, q, &others) {
            return true;
        }
    }
    false
}

fn blue_ice(cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
    let facts = cx.facts;
    let sea = region.heights().sea_level;
    let is = |region: &dyn Region, q: P, n: &str| facts.is(region.block(q), n);
    if p.y > sea - 1 {
        return false;
    }
    if !is(region, p, "minecraft:water") && !is(region, p.below(1), "minecraft:water") {
        return false;
    }
    if !ALL
        .iter()
        .any(|d| *d != Dir::Down && is(region, p.rel(*d), "minecraft:packed_ice"))
    {
        return false;
    }
    let Some(blue) = facts.default_state("minecraft:blue_ice") else {
        return false;
    };
    region.set(p, blue);
    for _ in 0..200 {
        let j = r.next_int_bound(5) - r.next_int_bound(6);
        let mut k = 3;
        if j < 2 {
            k += j / 2;
        }
        if k < 1 {
            continue;
        }
        let dx = r.next_int_bound(k) - r.next_int_bound(k);
        let dz = r.next_int_bound(k) - r.next_int_bound(k);
        let q = p.offset(dx, j, dz);
        let s = region.block(q);
        if !facts.is_air(s)
            && !facts.is(s, "minecraft:water")
            && !facts.is(s, "minecraft:packed_ice")
            && !facts.is(s, "minecraft:ice")
        {
            continue;
        }
        if ALL
            .iter()
            .any(|d| is(region, q.rel(*d), "minecraft:blue_ice"))
        {
            region.set(q, blue);
        }
    }
    true
}

fn bamboo(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    chance: f32,
) -> bool {
    if !facts.is_air(region.block(p)) {
        return false;
    }
    let st = |props: &[(&str, &str)]| facts.state("minecraft:bamboo", props);
    let (Some(trunk), Some(final_large), Some(top_large), Some(top_small)) = (
        st(&[("age", "1"), ("leaves", "none"), ("stage", "0")]),
        st(&[("age", "1"), ("leaves", "large"), ("stage", "1")]),
        st(&[("age", "1"), ("leaves", "large")]),
        st(&[("age", "1"), ("leaves", "small")]),
    ) else {
        return false;
    };
    if facts.survives(region, p, trunk) {
        let height = r.next_int_bound(12) + 5;
        if r.next_float() < chance
            && let Some(podzol) = facts.default_state("minecraft:podzol")
        {
            let k = r.next_int_bound(4) + 1;
            for x in p.x - k..=p.x + k {
                for z in p.z - k..=p.z + k {
                    let (dx, dz) = (x - p.x, z - p.z);
                    if dx * dx + dz * dz > k * k {
                        continue;
                    }
                    let q = P::new(x, region.height(Heightmap::WorldSurface, x, z) - 1, z);
                    if facts.is_dirt(region.block(q)) {
                        region.set(q, podzol);
                    }
                }
            }
        }
        let mut m = p;
        let mut k = 0;
        while k < height && facts.is_air(region.block(m)) {
            region.set(m, trunk);
            m = m.above(1);
            k += 1;
        }
        if m.y - p.y >= 3 {
            region.set(m, final_large);
            region.set(m.below(1), top_large);
            region.set(m.below(2), top_small);
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn huge_mushroom(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    red: bool,
    on: &Predicate,
    cap: &StateProvider,
    stem: &StateProvider,
    radius: i32,
) -> bool {
    let mut height = r.next_int_bound(3) + 4;
    if r.next_int_bound(12) == 0 {
        height *= 2;
    }
    let h = region.heights();
    if p.y < h.min_y + 1 || p.y + height + 1 > h.min_y + h.height - 1 {
        return false;
    }
    if !on.test(region, facts, p.below(1)) {
        return false;
    }
    let radius_at = |k: i32| {
        if red {
            if k < height && k >= height - 3 || k == height {
                radius
            } else {
                0
            }
        } else if k <= 3 {
            0
        } else {
            radius
        }
    };
    for k in 0..=height {
        let l = radius_at(k);
        for dx in -l..=l {
            for dz in -l..=l {
                let s = region.block(p.offset(dx, k, dz));
                if !facts.is_air(s) && !facts.is_leaves(s) {
                    return false;
                }
            }
        }
    }
    let set_cap =
        |region: &mut dyn Region, r: &mut WorldgenRandom, q: P, sides: [(&str, bool); 5]| {
            if facts.solid_render(region.block(q)) {
                return;
            }
            let Some(mut s) = cap.get(region, facts, r, p) else {
                return;
            };
            for (name, v) in sides {
                if facts.has_property(s, name) {
                    s = facts.with(s, name, if v { "true" } else { "false" });
                }
            }
            region.set(q, s);
        };
    if red {
        for y in height - 3..=height {
            let k = if y < height { radius } else { radius - 1 };
            let l = radius - 2;
            for dx in -k..=k {
                for dz in -k..=k {
                    let ex = dx == -k || dx == k;
                    let ez = dz == -k || dz == k;
                    if y < height && ex == ez {
                        continue;
                    }
                    set_cap(
                        region,
                        r,
                        p.offset(dx, y, dz),
                        [
                            ("up", y >= height - 1),
                            ("west", dx < -l),
                            ("east", dx > l),
                            ("north", dz < -l),
                            ("south", dz > l),
                        ],
                    );
                }
            }
        }
    } else {
        let j = radius;
        for dx in -j..=j {
            for dz in -j..=j {
                let (w, e, n, s) = (dx == -j, dx == j, dz == -j, dz == j);
                let ex = w || e;
                let ez = n || s;
                if ex && ez {
                    continue;
                }
                let west = w || ez && dx == 1 - j;
                let east = e || ez && dx == j - 1;
                let north = n || ex && dz == 1 - j;
                let south = s || ex && dz == j - 1;
                set_cap(
                    region,
                    r,
                    p.offset(dx, height, dz),
                    [
                        ("up", true),
                        ("west", west),
                        ("east", east),
                        ("north", north),
                        ("south", south),
                    ],
                );
            }
        }
    }
    for j in 0..height {
        let q = p.above(j);
        if facts.solid_render(region.block(q)) {
            continue;
        }
        if let Some(s) = stem.get(region, facts, r, p) {
            region.set(q, s);
        }
    }
    true
}

fn spike(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    state: u32,
    on: &Predicate,
    replace: &Predicate,
) -> bool {
    let h = region.heights();
    let mut p = p;
    while facts.is_air(region.block(p)) && p.y > h.min_y + 2 {
        p = p.below(1);
    }
    if !on.test(region, facts, p) {
        return false;
    }
    p = p.above(r.next_int_bound(4));
    let i = r.next_int_bound(4) + 7;
    let j = i / 4 + r.next_int_bound(2);
    if j > 1 && r.next_int_bound(60) == 0 {
        p = p.above(10 + r.next_int_bound(30));
    }
    let can = |region: &dyn Region, q: P| replace.test(region, facts, q);
    for k in 0..i {
        let f = (1.0 - k as f32 / i as f32) * j as f32;
        let l = f.ceil() as i32;
        for m in -l..=l {
            let g = m.abs() as f32 - 0.25;
            for n in -l..=l {
                let hh = n.abs() as f32 - 0.25;
                if (m != 0 || n != 0) && g * g + hh * hh > f * f
                    || (m == -l || m == l || n == -l || n == l) && r.next_float() > 0.75
                {
                    continue;
                }
                let q = p.offset(m, k, n);
                if can(region, q) {
                    region.set(q, state);
                }
                if k != 0 && l > 1 {
                    let q = p.offset(m, -k, n);
                    if can(region, q) {
                        region.set(q, state);
                    }
                }
            }
        }
    }
    let k = (j - 1).clamp(0, 1);
    for o in -k..=k {
        for l in -k..=k {
            let mut q = p.offset(o, -1, l);
            let mut n = if o.abs() == 1 && l.abs() == 1 {
                r.next_int_bound(5)
            } else {
                50
            };
            while q.y > 50 {
                if !can(region, q) && !facts.is(region.block(q), "minecraft:packed_ice") {
                    break;
                }
                region.set(q, state);
                q = q.below(1);
                n -= 1;
                if n <= 0 {
                    q = q.below(r.next_int_bound(5) + 1);
                    n = r.next_int_bound(5);
                }
            }
        }
    }
    true
}

fn monster_room(facts: &Facts, region: &mut dyn Region, r: &mut WorldgenRandom, p: P) -> bool {
    let cannot = facts.tag("minecraft:features_cannot_replace");
    let replaceable =
        |region: &dyn Region, q: P| cannot.is_none_or(|t| !facts.in_tag(region.block(q), t));
    let j = r.next_int_bound(2) + 2;
    let (k, l) = (-j - 1, j + 1);
    let o = r.next_int_bound(2) + 2;
    let (pp, q) = (-o - 1, o + 1);
    let mut holes = 0;
    for s in k..=l {
        for t in -1..=4 {
            for u in pp..=q {
                let bp = p.offset(s, t, u);
                let solid = facts.solid(region.block(bp));
                if (t == -1 || t == 4) && !solid {
                    return false;
                }
                if (s == k || s == l || u == pp || u == q)
                    && t == 0
                    && facts.is_air(region.block(bp))
                    && facts.is_air(region.block(bp.above(1)))
                {
                    holes += 1;
                }
            }
        }
    }
    if !(1..=5).contains(&holes) {
        return false;
    }
    let state = |n: &str| facts.default_state(n).unwrap_or(facts.air);
    let (cobble, mossy, chest, spawner) = (
        state("minecraft:cobblestone"),
        state("minecraft:mossy_cobblestone"),
        state("minecraft:chest"),
        state("minecraft:spawner"),
    );
    let h = region.heights();
    for s in k..=l {
        for t in (-1..=3).rev() {
            for u in pp..=q {
                let bp = p.offset(s, t, u);
                let here = region.block(bp);
                if s == k || t == -1 || u == pp || s == l || t == 4 || u == q {
                    if bp.y >= h.min_y && !facts.solid(region.block(bp.below(1))) {
                        region.set(bp, facts.cave_air);
                        continue;
                    }
                    if !facts.solid(here) || facts.is(here, "minecraft:chest") {
                        continue;
                    }
                    let block = if t == -1 && r.next_int_bound(4) != 0 {
                        mossy
                    } else {
                        cobble
                    };
                    if replaceable(region, bp) {
                        region.set(bp, block);
                    }
                    continue;
                }
                if facts.is(here, "minecraft:chest") || facts.is(here, "minecraft:spawner") {
                    continue;
                }
                if replaceable(region, bp) {
                    region.set(bp, facts.cave_air);
                }
            }
        }
    }
    'chests: for _ in 0..2 {
        for _ in 0..3 {
            let x = p.x + r.next_int_bound(j * 2 + 1) - j;
            let z = p.z + r.next_int_bound(o * 2 + 1) - o;
            let bp = P::new(x, p.y, z);
            if !facts.is_air(region.block(bp)) {
                continue;
            }
            let walls = HORIZONTAL
                .iter()
                .filter(|d| facts.solid(region.block(bp.rel(**d))))
                .count();
            if walls != 1 {
                continue;
            }
            if replaceable(region, bp) {
                let mut facing = Dir::North;
                for d in HORIZONTAL {
                    if facts.solid_render(region.block(bp.rel(d))) {
                        facing = d.opposite();
                    }
                }
                region.set(bp, facts.with(chest, "facing", facing.name()));
                let mut be = mc_nbt::Compound::new();
                be.insert("id", mc_nbt::Tag::String("minecraft:chest".into()));
                be.insert(
                    "LootTable",
                    mc_nbt::Tag::String("minecraft:chests/simple_dungeon".into()),
                );
                be.insert("LootTableSeed", mc_nbt::Tag::Long(r.next_long()));
                region.set_block_entity(bp, be);
            }
            continue 'chests;
        }
    }
    if replaceable(region, p) {
        region.set(p, spawner);
        let mob = [
            "minecraft:skeleton",
            "minecraft:zombie",
            "minecraft:zombie",
            "minecraft:spider",
        ][r.next_int_bound(4) as usize];
        region.set_block_entity(p, spawner_entity(mob));
    }
    true
}

/// A spawner's block entity for `mob`.
pub fn spawner_entity(mob: &str) -> mc_nbt::Compound {
    use mc_nbt::{Compound, Tag};
    let mut entity = Compound::new();
    entity.insert("id", Tag::String(mob.into()));
    let mut data = Compound::new();
    data.insert("entity", Tag::Compound(entity));
    let mut be = Compound::new();
    be.insert("id", Tag::String("minecraft:mob_spawner".into()));
    be.insert("SpawnData", Tag::Compound(data));
    be.insert("Delay", Tag::Short(20));
    be.insert("MinSpawnDelay", Tag::Short(200));
    be.insert("MaxSpawnDelay", Tag::Short(800));
    be.insert("SpawnCount", Tag::Short(4));
    be.insert("MaxNearbyEntities", Tag::Short(6));
    be.insert("RequiredPlayerRange", Tag::Short(16));
    be.insert("SpawnRange", Tag::Short(4));
    be
}

fn geode(facts: &Facts, region: &mut dyn Region, r: &mut WorldgenRandom, p: P, g: &Geode) -> bool {
    let k = g.points.sample(r);
    let mut wr = WorldgenRandom::legacy(region.seed());
    let noise = NormalNoise::new(
        &mut wr,
        &NoiseParameters {
            first_octave: -4,
            amplitudes: vec![1.0],
        },
        false,
    );
    let d = f64::from(k) / f64::from(g.outer_wall.max().max(1));
    let e = 1.0 / g.layers[0].sqrt();
    let f = 1.0 / (g.layers[1] + d).sqrt();
    let gg = 1.0 / (g.layers[2] + d).sqrt();
    let h = 1.0 / (g.layers[3] + d).sqrt();
    let l = 1.0 / (g.crack_size + r.next_double() / 2.0 + if k > 3 { d } else { 0.0 }).sqrt();
    let crack = f64::from(r.next_float()) < g.crack_chance;
    let mut invalid = 0;
    let mut points = Vec::new();
    for _ in 0..k {
        let o = g.outer_wall.sample(r);
        let pq = g.outer_wall.sample(r);
        let q = g.outer_wall.sample(r);
        let bp = p.offset(o, pq, q);
        let s = region.block(bp);
        if (facts.is_air(s) || g.invalid.contains(facts, s)) && {
            invalid += 1;
            invalid > g.invalid_threshold
        } {
            return false;
        }
        points.push((bp, g.point_offset.sample(r)));
    }
    let mut cracks = Vec::new();
    if crack {
        let side = r.next_int_bound(4);
        let s = k * 2 + 1;
        let (cx, cz) = match side {
            0 => (s, 0),
            1 => (0, s),
            2 => (s, s),
            _ => (0, 0),
        };
        for y in [7, 5, 1] {
            cracks.push(p.offset(cx, y, cz));
        }
    }
    let replaceable =
        |region: &dyn Region, q: P| !g.cannot_replace.contains(facts, region.block(q));
    let dist2 = |a: P, b: P| {
        let (x, y, z) = (
            f64::from(a.x - b.x),
            f64::from(a.y - b.y),
            f64::from(a.z - b.z),
        );
        x * x + y * y + z * z
    };
    let mut potential = Vec::new();
    let (lo, hi) = (g.min_offset, g.max_offset);
    for z in lo..=hi {
        for y in lo..=hi {
            for x in lo..=hi {
                let bp = p.offset(x, y, z);
                let t = noise.value(f64::from(bp.x), f64::from(bp.y), f64::from(bp.z))
                    * g.noise_multiplier;
                let mut u = 0.0;
                let mut v = 0.0;
                for (pt, off) in &points {
                    u += 1.0 / (dist2(bp, *pt) + f64::from(*off)).sqrt() + t;
                }
                for c in &cracks {
                    v += 1.0 / (dist2(bp, *c) + f64::from(g.crack_offset)).sqrt() + t;
                }
                if u < h {
                    continue;
                }
                if crack && v >= l && u < e {
                    if replaceable(region, bp) {
                        region.set(bp, facts.air);
                    }
                    for d in ALL {
                        let q = bp.rel(d);
                        if !facts.fluid(region.block(q)).is_none() {
                            region.schedule_tick(q);
                        }
                    }
                    continue;
                }
                let provider = if u >= e {
                    &g.filling
                } else if u >= f {
                    let alt = f64::from(r.next_float()) < g.alternate_chance;
                    let prov = if alt { &g.alternate } else { &g.inner };
                    if let Some(s) = prov.get(region, facts, r, bp)
                        && replaceable(region, bp)
                    {
                        region.set(bp, s);
                    }
                    if (alt || !g.require_alternate)
                        && f64::from(r.next_float()) < g.potential_chance
                    {
                        potential.push(bp);
                    }
                    continue;
                } else if u >= gg {
                    &g.middle
                } else {
                    &g.outer
                };
                if let Some(s) = provider.get(region, facts, r, bp)
                    && replaceable(region, bp)
                {
                    region.set(bp, s);
                }
            }
        }
    }
    if g.placements.is_empty() {
        return true;
    }
    for bp in potential {
        let mut s = *r.pick(&g.placements);
        for d in ALL {
            if facts.has_property(s, "facing") {
                s = facts.with(s, "facing", d.name());
            }
            let q = bp.rel(d);
            let there = region.block(q);
            if facts.has_property(s, "waterlogged") {
                let wet = matches!(
                    facts.fluid(there),
                    crate::Fluid::Water { source: true } | crate::Fluid::Lava { source: true }
                );
                s = facts.with(s, "waterlogged", if wet { "true" } else { "false" });
            }
            let can_grow = facts.is_air(there)
                || facts.is(there, "minecraft:water") && facts.is_water_source(there);
            if !can_grow {
                continue;
            }
            if replaceable(region, q) {
                region.set(q, s);
            }
            break;
        }
    }
    true
}

fn root_system(
    cx: &Ctx,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    rs: &RootSystem,
) -> bool {
    let facts = cx.facts;
    if !facts.is_air(region.block(p)) {
        return false;
    }
    let space = |region: &dyn Region, q: P| {
        for i in 1..=rs.required_space {
            let s = region.block(q.above(i));
            if facts.is_air(s) {
                continue;
            }
            if i < rs.water && matches!(facts.fluid(s), crate::Fluid::Water { .. }) {
                continue;
            }
            return false;
        }
        true
    };
    let mut m = p;
    let mut grown = None;
    for i in 0..rs.column_height {
        m = m.above(1);
        if !rs.allowed.test(region, facts, m) || !space(region, m) {
            continue;
        }
        let below = region.block(m.below(1));
        if matches!(facts.fluid(below), crate::Fluid::Lava { .. }) || !facts.solid(below) {
            return true;
        }
        if !rs.tree.place(cx, region, r, m, None) {
            continue;
        }
        grown = Some(p.y + i);
        break;
    }
    let Some(top) = grown else { return true };
    for y in p.y..top {
        for _ in 0..rs.root_attempts {
            let k = rs.root_radius.max(1);
            let q = P::new(
                p.x + r.next_int_bound(k) - r.next_int_bound(k),
                y,
                p.z + r.next_int_bound(k) - r.next_int_bound(k),
            );
            if rs.root_replaceable.contains(facts, region.block(q))
                && let Some(s) = rs.root_provider.get(region, facts, r, q)
            {
                region.set(q, s);
            }
        }
    }
    let (i, j) = (rs.hanging_radius.max(1), rs.hanging_span.max(1));
    for _ in 0..rs.hanging_attempts {
        let q = p.offset(
            r.next_int_bound(i) - r.next_int_bound(i),
            r.next_int_bound(j) - r.next_int_bound(j),
            r.next_int_bound(i) - r.next_int_bound(i),
        );
        if !facts.is_air(region.block(q)) {
            continue;
        }
        let Some(s) = rs.hanging_provider.get(region, facts, r, q) else {
            continue;
        };
        if facts.survives(region, q, s) && facts.sturdy(region.block(q.above(1)), Dir::Down) {
            region.set(q, s);
        }
    }
    true
}

fn coral_tree(
    cx: &Ctx,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    block: &Placed,
) -> bool {
    let mut m = p;
    let n = r.next_int_bound(3) + 1;
    for _ in 0..n {
        if !block.place(cx, region, r, m, None) {
            return true;
        }
        m = m.above(1);
    }
    let top = m;
    let k = r.next_int_bound(3) + 2;
    let mut dirs = HORIZONTAL.to_vec();
    r.shuffle(&mut dirs);
    for d in dirs.into_iter().take(k as usize) {
        let mut q = top.rel(d);
        let l = r.next_int_bound(5) + 2;
        let mut run = 0;
        for i in 0..l {
            if !block.place(cx, region, r, q, None) {
                break;
            }
            run += 1;
            q = q.above(1);
            if i == 0 || run >= 2 && r.next_float() < 0.25 {
                q = q.rel(d);
                run = 0;
            }
        }
    }
    true
}

fn coral_claw(
    cx: &Ctx,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    block: &Placed,
) -> bool {
    if !block.place(cx, region, r, p, None) {
        return false;
    }
    let d = HORIZONTAL[r.next_int_bound(4) as usize];
    let i = r.next_int_bound(2) + 2;
    let mut list = vec![d, d.cw(), d.ccw()];
    r.shuffle(&mut list);
    'outer: for e in list.into_iter().take(i as usize) {
        let mut m = p;
        let j = r.next_int_bound(2) + 1;
        m = m.rel(e);
        let (f, k) = if e == d {
            (d, r.next_int_bound(3) + 2)
        } else {
            m = m.above(1);
            let f = if r.next_int_bound(2) == 0 { e } else { Dir::Up };
            (f, r.next_int_bound(3) + 3)
        };
        let mut l = 0;
        while l < j && block.place(cx, region, r, m, None) {
            m = m.rel(f);
            l += 1;
        }
        m = m.rel(f.opposite()).above(1);
        for _ in 0..k {
            m = m.rel(d);
            if !block.place(cx, region, r, m, None) {
                continue 'outer;
            }
            if r.next_float() < 0.25 {
                m = m.above(1);
            }
        }
    }
    true
}

fn pile_block(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    q: P,
    provider: &StateProvider,
) {
    if !facts.is_air(region.block(q)) {
        return;
    }
    let below = region.block(q.below(1));
    let ok = if facts.is(below, "minecraft:dirt_path") {
        r.next_bool()
    } else {
        facts.sturdy(below, Dir::Up)
    };
    if ok && let Some(s) = provider.get(region, facts, r, q) {
        region.set(q, s);
    }
}

/// A simpler iceberg: an icy cone above and below the sea, rounded or
/// pointed, with blue ice spots.
fn iceberg(cx: &Ctx, region: &mut dyn Region, r: &mut WorldgenRandom, p: P, state: u32) -> bool {
    let facts = cx.facts;
    let sea = region.heights().sea_level;
    let origin = p.with_y(sea);
    let snow = facts.default_state("minecraft:snow_block").unwrap_or(state);
    let pointed = r.next_double() > 0.7;
    let height = if pointed {
        r.next_int_bound(6) + 6
    } else {
        r.next_int_bound(15) + 3
    };
    let depth = (height + r.next_int_bound(11)).min(18);
    let radius = (height + r.next_int_bound(7) - r.next_int_bound(5)).clamp(3, 11);
    let can = |region: &dyn Region, q: P| {
        let s = region.block(q);
        facts.is_air(s)
            || facts.is(s, "minecraft:water")
            || facts.is(s, "minecraft:snow")
            || facts.is(s, "minecraft:ice")
    };
    for y in -depth..height {
        let t = if y >= 0 {
            1.0 - y as f32 / height as f32
        } else {
            1.0 - (-y) as f32 / depth as f32
        };
        let rad = (radius as f32 * if pointed { t } else { t.sqrt() }).max(0.0);
        let ri = rad.ceil() as i32;
        for dx in -ri..=ri {
            for dz in -ri..=ri {
                let d = ((dx * dx + dz * dz) as f32).sqrt();
                if d > rad + r.next_float() * 0.5 - 0.25 {
                    continue;
                }
                let q = origin.offset(dx, y, dz);
                if !can(region, q) {
                    continue;
                }
                let top = y >= 0 && (y == height - 1 || d > rad - 1.0 && r.next_int_bound(3) == 0);
                region.set(
                    q,
                    if top && y > 0 && r.next_bool() {
                        snow
                    } else {
                        state
                    },
                );
            }
        }
    }
    true
}

fn pointed_state(facts: &Facts, pointed: u32, dir: Dir, thickness: &str) -> u32 {
    let s = facts.with(pointed, "vertical_direction", dir.name());
    facts.with(s, "thickness", thickness)
}

#[allow(clippy::too_many_arguments)]
fn grow_pointed(
    facts: &Facts,
    region: &mut dyn Region,
    p: P,
    dir: Dir,
    height: i32,
    merge: bool,
    pointed: u32,
    base: &BlockSet,
) {
    if !base.contains(facts, region.block(p.rel(dir.opposite()))) {
        return;
    }
    let mut list = Vec::new();
    if height >= 3 {
        list.push("base");
        list.extend(std::iter::repeat_n("middle", (height - 3) as usize));
    }
    if height >= 2 {
        list.push("frustum");
    }
    if height >= 1 {
        list.push(if merge { "tip_merge" } else { "tip" });
    }
    let mut m = p;
    for t in list {
        let mut s = pointed_state(facts, pointed, dir, t);
        let wet = facts.is(region.block(m), "minecraft:water");
        s = facts.with(s, "waterlogged", if wet { "true" } else { "false" });
        region.set(m, s);
        m = m.rel(dir);
    }
}

fn place_base(
    facts: &Facts,
    region: &mut dyn Region,
    q: P,
    base: u32,
    replaceable: &BlockSet,
) -> bool {
    if replaceable.contains(facts, region.block(q)) {
        region.set(q, base);
        true
    } else {
        false
    }
}

fn empty_or_water(facts: &Facts, s: u32) -> bool {
    facts.is_air(s) || facts.is(s, "minecraft:water")
}

fn speleothem(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    base: u32,
    pointed: u32,
    replaceable: &BlockSet,
) -> bool {
    let up = replaceable.contains(facts, region.block(p.above(1)));
    let down = replaceable.contains(facts, region.block(p.below(1)));
    let dir = match (up, down) {
        (true, true) => {
            if r.next_bool() {
                Dir::Down
            } else {
                Dir::Up
            }
        }
        (true, false) => Dir::Down,
        (false, true) => Dir::Up,
        _ => return false,
    };
    let root = p.rel(dir.opposite());
    place_base(facts, region, root, base, replaceable);
    for d in HORIZONTAL {
        if r.next_float() > 0.7 {
            continue;
        }
        let q = root.rel(d);
        place_base(facts, region, q, base, replaceable);
        if r.next_float() > 0.5 {
            continue;
        }
        let q2 = q.rel(ALL[r.next_int_bound(6) as usize]);
        place_base(facts, region, q2, base, replaceable);
        if r.next_float() > 0.5 {
            continue;
        }
        let q3 = q2.rel(ALL[r.next_int_bound(6) as usize]);
        place_base(facts, region, q3, base, replaceable);
    }
    let height = if r.next_float() < 0.2 && empty_or_water(facts, region.block(p.rel(dir))) {
        2
    } else {
        1
    };
    grow_pointed(facts, region, p, dir, height, false, pointed, replaceable);
    true
}

fn cluster(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    c: &Cluster,
) -> bool {
    if !empty_or_water(facts, region.block(p)) {
        return false;
    }
    let height = c.height.sample(r);
    let wetness = c.wetness.sample(r);
    let density = c.density.sample(r);
    let rx = c.radius.sample(r);
    let rz = c.radius.sample(r);
    for dx in -rx..=rx {
        for dz in -rz..=rz {
            let k = (rx - dx.abs()).min(rz - dz.abs());
            let t = (k as f32 / c.edge_distance.max(1) as f32).clamp(0.0, 1.0);
            let chance = f64::from(c.edge_chance + t * (1.0 - c.edge_chance));
            cluster_column(
                facts,
                region,
                r,
                p.offset(dx, 0, dz),
                dx,
                dz,
                wetness,
                chance,
                height,
                density,
                c,
            );
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn cluster_column(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    dx: i32,
    dz: i32,
    wetness: f32,
    chance: f64,
    height: i32,
    density: f32,
    c: &Cluster,
) {
    if !empty_or_water(facts, region.block(p)) {
        return;
    }
    let scan = |region: &dyn Region, dir: Dir| -> Option<i32> {
        let mut q = p;
        let mut i = 1;
        while i < c.search && empty_or_water(facts, region.block(q)) {
            q = q.rel(dir);
            i += 1;
        }
        (!empty_or_water(facts, region.block(q))).then_some(q.y)
    };
    let ceiling = scan(region, Dir::Up);
    let mut floor = scan(region, Dir::Down);
    if ceiling.is_none() && floor.is_none() {
        return;
    }
    let wet = r.next_float() < wetness;
    if wet && let Some(f) = floor {
        let fp = p.with_y(f);
        let s = region.block(fp);
        let ok = !facts.is(s, "minecraft:water")
            && facts.block(s) != facts.block(c.base)
            && facts.block(s) != facts.block(c.pointed)
            && HORIZONTAL.iter().chain([Dir::Down].iter()).all(|d| {
                let n = region.block(fp.rel(*d));
                facts.in_named(n, "minecraft:base_stone_overworld")
                    || matches!(facts.fluid(n), crate::Fluid::Water { .. })
            });
        if ok {
            region.set(fp, facts.water);
            floor = Some(f - 1);
        }
    }
    let span_height = |r: &mut WorldgenRandom, max: i32| -> i32 {
        if r.next_float() > density {
            return 0;
        }
        let i = dx.abs() + dz.abs();
        let t = (i as f32 / c.bias_distance.max(1) as f32).clamp(0.0, 1.0);
        let mean = max as f32 / 2.0 * (1.0 - t);
        let v = (mean + r.next_gaussian() as f32 * c.deviation as f32).clamp(0.0, max as f32);
        v as i32
    };
    let is_lava = |region: &dyn Region, y: i32| {
        matches!(
            facts.fluid(region.block(p.with_y(y))),
            crate::Fluid::Lava { .. }
        )
    };
    let mut down = 0;
    if let Some(cy) = ceiling
        && r.next_double() < chance
        && !is_lava(region, cy)
    {
        let t = c.thickness.sample(r);
        let mut q = p.with_y(cy);
        for _ in 0..t {
            if !place_base(facts, region, q, c.base, &c.replaceable) {
                break;
            }
            q = q.above(1);
        }
        let l = match floor {
            Some(f) => height.min(cy - f),
            None => height,
        };
        down = span_height(r, l);
    }
    let mut up = 0;
    if let Some(fy) = floor
        && r.next_double() < chance
        && !is_lava(region, fy)
    {
        let t = c.thickness.sample(r);
        let mut q = p.with_y(fy);
        for _ in 0..t {
            if !place_base(facts, region, q, c.base, &c.replaceable) {
                break;
            }
            q = q.below(1);
        }
        up = if ceiling.is_some() {
            (down + r.next_int_bound(2 * c.height_diff + 1) - c.height_diff).max(0)
        } else {
            span_height(r, height)
        };
    }
    let (mut a, mut b) = (down, up);
    if let (Some(cy), Some(fy)) = (ceiling, floor)
        && cy - down <= fy + up
    {
        let lo = (cy - down).max(fy + 1);
        let hi = (fy + up).min(cy - 1);
        let meet = if hi + 1 >= lo {
            lo + r.next_int_bound(hi + 1 - lo + 1)
        } else {
            lo
        };
        a = cy - meet;
        b = meet - 1 - fy;
    }
    let merge = r.next_bool()
        && a > 0
        && b > 0
        && matches!((ceiling, floor), (Some(cy), Some(fy)) if a + b == cy - fy - 1);
    let base = BlockSet::Blocks(vec![facts.block(c.base)]);
    let any_base =
        |s: u32| facts.block(s) == facts.block(c.base) || c.replaceable.contains(facts, s);
    let _ = any_base;
    if let Some(cy) = ceiling {
        grow_pointed(
            facts,
            region,
            p.with_y(cy - 1),
            Dir::Down,
            a,
            merge,
            c.pointed,
            &base,
        );
    }
    if let Some(fy) = floor {
        grow_pointed(
            facts,
            region,
            p.with_y(fy + 1),
            Dir::Up,
            b,
            merge,
            c.pointed,
            &base,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn fossil(
    cx: &Ctx,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    fossils: &[String],
    overlays: &[String],
    fossil_processors: &[Arc<crate::structure::template::Processor>],
    overlay_processors: &[Arc<crate::structure::template::Processor>],
    max_empty_corners: i32,
) -> bool {
    use crate::structure::template::{BBox, Mirror, Rotation, Settings};
    let facts = cx.facts;
    if fossils.is_empty() {
        return false;
    }
    let rot = Rotation(r.next_int_bound(4) as u8);
    let i = r.next_int_bound(fossils.len() as i32) as usize;
    let s = &cx.features.structures;
    let (Some(t), Some(t2)) = (
        s.template(&fossils[i]),
        overlays.get(i).and_then(|o| s.template(o)),
    ) else {
        return false;
    };
    let h = region.heights();
    let (cxk, czk) = (p.x >> 4, p.z >> 4);
    let bbox = BBox {
        min: P::new(cxk * 16 - 16, h.min_y, czk * 16 - 16),
        max: P::new(
            cxk * 16 + 15 + 16,
            h.min_y + h.height - 1,
            czk * 16 + 15 + 16,
        ),
    };
    let (sx, sz) = if rot.0 % 2 == 1 {
        (t.size.z, t.size.x)
    } else {
        (t.size.x, t.size.z)
    };
    let bp = p.offset(-sx / 2, 0, -sz / 2);
    let mut j = p.y;
    for k in 0..sx {
        for l in 0..sz {
            j = j.min(region.height(Heightmap::OceanFloorWg, bp.x + k, bp.z + l));
        }
    }
    let k = (j - 15 - r.next_int_bound(10)).max(h.min_y + 10);
    // The corner that keeps the turned template where it was meant to be.
    let (xi, zj) = (t.size.x - 1, t.size.z - 1);
    let base = bp.with_y(k);
    let zero = match rot.0 {
        3 => base.offset(0, 0, xi),
        1 => base.offset(zj, 0, 0),
        2 => base.offset(xi, 0, zj),
        _ => base,
    };
    let shared = std::sync::Arc::new(std::sync::Mutex::new(r.clone()));
    let mut settings = Settings {
        rotation: rot,
        mirror: Mirror::None,
        bbox: Some(bbox),
        processors: fossil_processors.to_vec(),
        shared: Some(shared.clone()),
        ..Settings::default()
    };
    let fb = t.bbox(zero, rot, P::default(), Mirror::None);
    let corners = [
        P::new(fb.min.x, fb.min.y, fb.min.z),
        P::new(fb.max.x, fb.min.y, fb.min.z),
        P::new(fb.min.x, fb.max.y, fb.min.z),
        P::new(fb.max.x, fb.max.y, fb.min.z),
        P::new(fb.min.x, fb.min.y, fb.max.z),
        P::new(fb.max.x, fb.min.y, fb.max.z),
        P::new(fb.min.x, fb.max.y, fb.max.z),
        P::new(fb.max.x, fb.max.y, fb.max.z),
    ];
    let empty = corners
        .iter()
        .filter(|c| {
            let s = region.block(**c);
            facts.is_air(s) || facts.is(s, "minecraft:water") || facts.is(s, "minecraft:lava")
        })
        .count() as i32;
    if empty > max_empty_corners {
        return false;
    }
    t.place(facts, region, r, zero, zero, &settings);
    settings.processors = overlay_processors.to_vec();
    t2.place(facts, region, r, zero, zero, &settings);
    *r = shared.lock().expect("random lock").clone();
    true
}

fn drip_height(dist: f64, radius: f64, scale: f64, bluntness: f64) -> f64 {
    let dist = dist.max(bluntness);
    let e = dist / radius * 0.384;
    let f = 0.75 * e.powf(4.0 / 3.0);
    let g = e.powf(2.0 / 3.0);
    let h = e.ln() / 3.0;
    let i = (scale * (f - g - h)).max(0.0);
    i / 0.384 * radius
}

struct Drip {
    root: P,
    up: bool,
    radius: i32,
    bluntness: f64,
    scale: f64,
}

impl Drip {
    fn height_at(&self, f: f32) -> i32 {
        drip_height(
            f64::from(f),
            f64::from(self.radius),
            self.scale,
            self.bluntness,
        ) as i32
    }
}

fn large_dripstone(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    p: P,
    c: &LargeDripstone,
) -> bool {
    let empty = |s: u32| facts.is_air(s) || facts.is(s, "minecraft:water");
    let empty_or_lava = |s: u32| empty(s) || facts.is(s, "minecraft:lava");
    let base = |s: u32| {
        facts.in_named(s, "minecraft:dripstone_replaceable_blocks") || facts.is(s, "minecraft:lava")
    };
    if !empty(region.block(p)) {
        return false;
    }
    // The open column through p, with ground below and a roof above.
    let mut ceiling = p;
    let mut i = 1;
    while i < c.search && empty(region.block(ceiling)) {
        ceiling = ceiling.above(1);
        i += 1;
    }
    let mut floor = p;
    let mut i = 1;
    while i < c.search && empty(region.block(floor)) {
        floor = floor.below(1);
        i += 1;
    }
    if !base(region.block(ceiling)) || !base(region.block(floor)) {
        return false;
    }
    let height = ceiling.y - floor.y - 1;
    if height < 4 {
        return false;
    }
    let max = ((height as f32 * c.ratio) as i32).clamp(c.radius_range.0, c.radius_range.1);
    let k = r.int_between(c.radius_range.0, max);
    let _ = &c.radius;
    let mut stalactite = Drip {
        root: p.with_y(ceiling.y - 1),
        up: false,
        radius: k,
        bluntness: f64::from(c.stalactite_bluntness.sample(r)),
        scale: f64::from(c.height_scale.sample(r)),
    };
    let mut stalagmite = Drip {
        root: p.with_y(floor.y + 1),
        up: true,
        radius: k,
        bluntness: f64::from(c.stalagmite_bluntness.sample(r)),
        scale: f64::from(c.height_scale.sample(r)),
    };
    let windy = |d: &Drip| {
        d.radius >= c.min_radius_for_wind && d.bluntness >= f64::from(c.min_bluntness_for_wind)
    };
    let wind = if windy(&stalactite) && windy(&stalagmite) {
        let f = c.wind_speed.sample(r);
        let g = r.float_between(0.0, std::f32::consts::PI);
        Some((p.y, f64::from(g.cos() * f), f64::from(g.sin() * f)))
    } else {
        None
    };
    let offset = |q: P| match wind {
        Some((oy, wx, wz)) => {
            let i = f64::from(oy - q.y);
            q.offset((wx * i).floor() as i32, 0, (wz * i).floor() as i32)
        }
        None => q,
    };
    let settle = |region: &dyn Region, d: &mut Drip| -> bool {
        while d.radius > 1 {
            let mut m = d.root;
            let steps = d.height_at(0.0).min(10);
            for _ in 0..steps {
                if facts.is(region.block(m), "minecraft:lava") {
                    return false;
                }
                // Mostly in stone around: none of the circle's points open.
                let at = offset(m);
                let embedded = !empty_or_lava(region.block(at)) && {
                    let g = 6.0 / d.radius as f32;
                    let mut h = 0.0f32;
                    let mut ok = true;
                    while h < std::f32::consts::TAU {
                        let q = at.offset(
                            (h.cos() * d.radius as f32) as i32,
                            0,
                            (h.sin() * d.radius as f32) as i32,
                        );
                        if empty_or_lava(region.block(q)) {
                            ok = false;
                            break;
                        }
                        h += g;
                    }
                    ok
                };
                if embedded {
                    d.root = m;
                    return true;
                }
                m = if d.up { m.below(1) } else { m.above(1) };
            }
            d.radius /= 2;
        }
        false
    };
    let a = settle(region, &mut stalactite);
    let b = settle(region, &mut stalagmite);
    let Some(dripstone) = facts.default_state("minecraft:dripstone_block") else {
        return false;
    };
    for (ok, d) in [(a, &stalactite), (b, &stalagmite)] {
        if !ok {
            continue;
        }
        for i in -d.radius..=d.radius {
            for j in -d.radius..=d.radius {
                let f = ((i * i + j * j) as f32).sqrt();
                if f > d.radius as f32 {
                    continue;
                }
                let mut k = d.height_at(f);
                if k <= 0 {
                    continue;
                }
                if r.next_float() < 0.2 {
                    k = (k as f32 * r.float_between(0.8, 1.0)) as i32;
                }
                let mut m = d.root.offset(i, 0, j);
                let mut placed = false;
                let limit = if d.up {
                    region.height(Heightmap::WorldSurfaceWg, m.x, m.z)
                } else {
                    i32::MAX
                };
                let mut n = 0;
                while n < k && m.y < limit {
                    let q = offset(m);
                    let s = region.block(q);
                    if empty_or_lava(s) {
                        placed = true;
                        region.set(q, dripstone);
                    } else if placed && facts.in_named(s, "minecraft:base_stone_overworld") {
                        break;
                    }
                    m = if d.up { m.above(1) } else { m.below(1) };
                    n += 1;
                }
            }
        }
    }
    true
}
