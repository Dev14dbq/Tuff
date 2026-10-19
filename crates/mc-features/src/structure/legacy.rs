//! Structures with their own piece types (minecraft.wiki: "Igloo",
//! "Shipwreck", "Ocean Ruins", "Buried Treasure", "Ruined Portal", "Swamp
//! Hut", "Desert Pyramid", "Jungle Pyramid", "Mineshaft").

use std::sync::{Arc, OnceLock};

use mc_nbt::{Compound, Tag};
use mc_tick::Dir;
use mc_worldgen::random::RandomSource;
use serde_json::Value;

use super::template::{BBox, Mirror, Processor, Rotation, Settings, transform};
use super::{Ctx2, Ground, Start, Structures};
use crate::blocks::Facts;
use crate::predicate::{BlockSet, RuleTest};
use crate::rand::WorldgenRandom;
use crate::value::Heights;
use crate::{Heightmap, P, Region};

/// A piece of one of these structures.
#[derive(Debug, Clone)]
pub struct Piece {
    pub bbox: BBox,
    pub kind: Arc<Kind>,
}

#[derive(Debug)]
pub enum Kind {
    Template(TemplatePiece),
    Treasure(P),
    Built(Built),
    Mineshaft(super::mineshaft::Part),
    Stronghold(super::stronghold::Part),
    Monument(super::observed::Monument),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Post {
    Plain,
    Igloo {
        offset: P,
        top: bool,
    },
    Shipwreck {
        beached: bool,
    },
    OceanRuin {
        large: bool,
        warm: bool,
    },
    Mansion,
    Portal {
        placement: u8,
        air_pocket: bool,
        mossiness: f32,
        cold: bool,
    },
}

#[derive(Debug)]
pub struct TemplatePiece {
    pub id: String,
    pub pos: P,
    pub settings: Settings,
    pub post: Post,
    /// Where it ends up once the ground is known (first chunk placing it).
    pub fixed: OnceLock<P>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltKind {
    SwampHut,
    DesertPyramid,
    JunglePyramid,
}

#[derive(Debug)]
pub struct Built {
    pub kind: BuiltKind,
    pub facing: Dir,
    pub bbox: BBox,
    pub ground: OnceLock<i32>,
}

fn random_rotation(r: &mut WorldgenRandom) -> Rotation {
    Rotation(r.next_int_bound(4) as u8)
}

const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

fn tall(b: BBox, h: Heights) -> BBox {
    BBox {
        min: P::new(b.min.x, h.min_y, b.min.z),
        max: P::new(b.max.x, h.min_y + h.height - 1, b.max.z),
    }
}

/// `onTopOfChunkCenter`: the chunk's middle column, on its top block.
fn chunk_center(ground: &mut dyn Ground, x: i32, z: i32, ocean_floor: bool) -> P {
    let (mx, mz) = (x * 16 + 8, z * 16 + 8);
    P::new(mx, ground.height(mx, mz, ocean_floor) - 1, mz)
}

pub(super) fn template_piece(
    s: &Structures,
    id: &str,
    pos: P,
    settings: Settings,
    post: Post,
    h: Heights,
) -> Option<Piece> {
    let t = s.template(id)?;
    let bbox = t.bbox(pos, settings.rotation, settings.pivot, settings.mirror);
    Some(Piece {
        bbox: tall(bbox, h),
        kind: Arc::new(Kind::Template(TemplatePiece {
            id: id.to_owned(),
            pos,
            settings,
            post,
            fixed: OnceLock::new(),
        })),
    })
}

fn ignore(air: bool) -> Arc<Processor> {
    Arc::new(Processor::Ignore { air })
}

/// The pieces of a structure of type `ty` starting in chunk `x, z`, and
/// where its biome is checked.
#[allow(clippy::too_many_arguments)]
pub fn generate(
    s: &Structures,
    ty: &str,
    v: &Value,
    r: &mut WorldgenRandom,
    seed: i64,
    x: i32,
    z: i32,
    h: Heights,
    ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    let corner = P::new(x * 16, 90, z * 16);
    match ty {
        "igloo" => {
            let at = chunk_center(ground, x, z, false);
            let rot = random_rotation(r);
            let mut out = Vec::new();
            let piece = |name: &str, down: i32| {
                let (offset, pivot) = match name {
                    "top" => (P::new(0, 0, 0), P::new(3, 5, 5)),
                    "middle" => (P::new(2, -3, 4), P::new(1, 3, 1)),
                    _ => (P::new(0, -3, -2), P::new(3, 6, 7)),
                };
                let settings = Settings {
                    rotation: rot,
                    pivot,
                    processors: vec![ignore(false)],
                    ..Settings::default()
                };
                template_piece(
                    s,
                    &format!("minecraft:igloo/{name}"),
                    corner.add(offset).below(down),
                    settings,
                    Post::Igloo {
                        offset,
                        top: name == "top",
                    },
                    h,
                )
            };
            if r.next_double() < 0.5 {
                let i = r.next_int_bound(8) + 4;
                out.extend(piece("bottom", i * 3));
                for j in 0..i - 1 {
                    out.extend(piece("middle", j * 3));
                }
            }
            out.extend(piece("top", 0));
            Some((out, at))
        }
        "shipwreck" => {
            let beached = v["is_beached"].as_bool().unwrap_or(false);
            let at = chunk_center(ground, x, z, !beached);
            let rot = random_rotation(r);
            const OCEAN: [&str; 20] = [
                "with_mast",
                "upsidedown_full",
                "upsidedown_fronthalf",
                "upsidedown_backhalf",
                "sideways_full",
                "sideways_fronthalf",
                "sideways_backhalf",
                "rightsideup_full",
                "rightsideup_fronthalf",
                "rightsideup_backhalf",
                "with_mast_degraded",
                "upsidedown_full_degraded",
                "upsidedown_fronthalf_degraded",
                "upsidedown_backhalf_degraded",
                "sideways_full_degraded",
                "sideways_fronthalf_degraded",
                "sideways_backhalf_degraded",
                "rightsideup_full_degraded",
                "rightsideup_fronthalf_degraded",
                "rightsideup_backhalf_degraded",
            ];
            const BEACHED: [&str; 11] = [
                "with_mast",
                "sideways_full",
                "sideways_fronthalf",
                "sideways_backhalf",
                "rightsideup_full",
                "rightsideup_fronthalf",
                "rightsideup_backhalf",
                "with_mast_degraded",
                "rightsideup_full_degraded",
                "rightsideup_fronthalf_degraded",
                "rightsideup_backhalf_degraded",
            ];
            let name = if beached {
                *r.pick(&BEACHED)
            } else {
                *r.pick(&OCEAN)
            };
            let settings = Settings {
                rotation: rot,
                pivot: P::new(4, 0, 15),
                processors: vec![ignore(true)],
                ..Settings::default()
            };
            let p = template_piece(
                s,
                &format!("minecraft:shipwreck/{name}"),
                corner,
                settings,
                Post::Shipwreck { beached },
                h,
            )?;
            Some((vec![p], at))
        }
        "ocean_ruin" => {
            let warm = v["biome_temp"].as_str() == Some("warm");
            let large_p = v["large_probability"].as_f64().unwrap_or(0.3) as f32;
            let cluster_p = v["cluster_probability"].as_f64().unwrap_or(0.9) as f32;
            let at = chunk_center(ground, x, z, true);
            let rot = random_rotation(r);
            let mut out = Vec::new();
            let large = r.next_float() <= large_p;
            let integrity = if large { 0.9 } else { 0.8 };
            ruin(s, &mut out, r, corner, rot, warm, large, integrity, h);
            if large && r.next_float() <= cluster_p {
                cluster(s, &mut out, r, rot, corner, warm, h);
            }
            Some((out, at))
        }
        "buried_treasure" => {
            let at = chunk_center(ground, x, z, true);
            let p = P::new(x * 16 + 9, 90, z * 16 + 9);
            Some((
                vec![Piece {
                    bbox: tall(BBox { min: p, max: p }, h),
                    kind: Arc::new(Kind::Treasure(p)),
                }],
                at,
            ))
        }
        "ruined_portal" => portal(s, v, r, x, z, h, ground),
        "swamp_hut" | "desert_pyramid" | "jungle_temple" => {
            let at = chunk_center(ground, x, z, false);
            let (kind, w, hgt, d) = match ty {
                "swamp_hut" => (BuiltKind::SwampHut, 7, 7, 9),
                "desert_pyramid" => (BuiltKind::DesertPyramid, 21, 15, 21),
                _ => (BuiltKind::JunglePyramid, 12, 10, 15),
            };
            let facing = HORIZONTAL[r.next_int_bound(4) as usize];
            let y = 64;
            let bbox = if matches!(facing, Dir::North | Dir::South) {
                BBox::from_corners(
                    P::new(x * 16, y, z * 16),
                    P::new(x * 16 + w - 1, y + hgt - 1, z * 16 + d - 1),
                )
            } else {
                BBox::from_corners(
                    P::new(x * 16, y, z * 16),
                    P::new(x * 16 + d - 1, y + hgt - 1, z * 16 + w - 1),
                )
            };
            if kind == BuiltKind::DesertPyramid {
                // It stands on sand: not where the ground is too uneven.
                let _ = seed;
            }
            Some((
                vec![Piece {
                    bbox: tall(bbox, h),
                    kind: Arc::new(Kind::Built(Built {
                        kind,
                        facing,
                        bbox,
                        ground: OnceLock::new(),
                    })),
                }],
                at,
            ))
        }
        "mineshaft" => super::mineshaft::generate(v, r, x, z, h, ground),
        "stronghold" => super::stronghold::generate(seed, x, z, h),
        "ocean_monument" => {
            super::observed::monument(s.observed(), &s.monument_biomes, r, x, z, h, ground)
        }
        "woodland_mansion" => super::observed::mansion(s, r, x, z, h, ground),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn ruin(
    s: &Structures,
    out: &mut Vec<Piece>,
    r: &mut WorldgenRandom,
    pos: P,
    rot: Rotation,
    warm: bool,
    large: bool,
    integrity: f32,
    h: Heights,
) {
    let add = |name: String, integrity: f32, out: &mut Vec<Piece>| {
        let mut processors = vec![Arc::new(Processor::BlockRot {
            integrity,
            rottable: None,
        })];
        processors.push(ignore(true));
        let settings = Settings {
            rotation: rot,
            processors,
            ..Settings::default()
        };
        if let Some(p) = template_piece(
            s,
            &format!("minecraft:underwater_ruin/{name}"),
            pos,
            settings,
            Post::OceanRuin { large, warm },
            h,
        ) {
            out.push(p);
        }
    };
    if warm {
        let name = if large {
            format!("big_warm_{}", 4 + r.next_int_bound(4))
        } else {
            format!("warm_{}", 1 + r.next_int_bound(8))
        };
        add(name, integrity, out);
    } else {
        let big = [1, 2, 3, 8];
        let (n, prefix) = if large {
            (big[r.next_int_bound(4) as usize], "big_")
        } else {
            (1 + r.next_int_bound(8), "")
        };
        add(format!("{prefix}brick_{n}"), integrity, out);
        add(format!("{prefix}cracked_{n}"), 0.7, out);
        add(format!("{prefix}mossy_{n}"), 0.5, out);
    }
}

fn cluster(
    s: &Structures,
    out: &mut Vec<Piece>,
    r: &mut WorldgenRandom,
    rot: Rotation,
    pos: P,
    warm: bool,
    h: Heights,
) {
    let bp = P::new(pos.x, 90, pos.z);
    let bp2 = transform(P::new(15, 0, 15), Mirror::None, rot, P::default()).add(bp);
    let big = BBox::from_corners(bp, bp2);
    let bp3 = P::new(bp.x.min(bp2.x), bp.y, bp.z.min(bp2.z));
    let ni = |r: &mut WorldgenRandom, a: i32, b: i32| r.int_between(a, b);
    let mut list = vec![
        bp3.offset(-16 + ni(r, 1, 8), 0, 16 + ni(r, 1, 7)),
        bp3.offset(-16 + ni(r, 1, 8), 0, ni(r, 1, 7)),
        bp3.offset(-16 + ni(r, 1, 8), 0, -16 + ni(r, 4, 8)),
        bp3.offset(ni(r, 1, 7), 0, 16 + ni(r, 1, 7)),
        bp3.offset(ni(r, 1, 7), 0, -16 + ni(r, 4, 6)),
        bp3.offset(16 + ni(r, 1, 7), 0, 16 + ni(r, 3, 8)),
        bp3.offset(16 + ni(r, 1, 7), 0, ni(r, 1, 7)),
        bp3.offset(16 + ni(r, 1, 7), 0, -16 + ni(r, 4, 8)),
    ];
    let n = r.int_between(4, 8);
    for _ in 0..n {
        if list.is_empty() {
            continue;
        }
        let k = r.next_int_bound(list.len() as i32) as usize;
        let p = list.remove(k);
        let rot2 = random_rotation(r);
        let p2 = transform(P::new(5, 0, 6), Mirror::None, rot2, P::default()).add(p);
        if BBox::from_corners(p, p2).intersects(&big) {
            continue;
        }
        ruin(s, out, r, p, rot2, warm, false, 0.8, h);
    }
}

fn portal(
    s: &Structures,
    v: &Value,
    r: &mut WorldgenRandom,
    x: i32,
    z: i32,
    h: Heights,
    ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    let setups: Vec<&Value> = v["setups"].as_array()?.iter().collect();
    let setup = if setups.len() > 1 {
        let total: f32 = setups
            .iter()
            .map(|s| s["weight"].as_f64().unwrap_or(1.0) as f32)
            .sum();
        let mut g = r.next_float();
        let mut chosen = setups[setups.len() - 1];
        for st in &setups {
            g -= st["weight"].as_f64().unwrap_or(1.0) as f32 / total;
            if g < 0.0 {
                chosen = st;
                break;
            }
        }
        chosen
    } else {
        setups.first()?
    };
    let chance = |r: &mut WorldgenRandom, p: f32| {
        if p == 0.0 {
            false
        } else if p == 1.0 {
            true
        } else {
            r.next_float() < p
        }
    };
    let air_pocket = chance(
        r,
        setup["air_pocket_probability"].as_f64().unwrap_or(0.0) as f32,
    );
    let mossiness = setup["mossiness"].as_f64().unwrap_or(0.0) as f32;
    let name = if r.next_float() < 0.05 {
        format!("giant_portal_{}", 1 + r.next_int_bound(3))
    } else {
        format!("portal_{}", 1 + r.next_int_bound(10))
    };
    let id = format!("minecraft:ruined_portal/{name}");
    let t = s.template(&id)?;
    let rot = random_rotation(r);
    let mirror = if r.next_float() < 0.5 {
        Mirror::None
    } else {
        Mirror::FrontBack
    };
    let pivot = P::new(t.size.x / 2, 0, t.size.z / 2);
    let corner = P::new(x * 16, 0, z * 16);
    let bbox = t.bbox(corner, rot, pivot, mirror);
    let c = bbox.center();
    let placement = match setup["placement"].as_str() {
        Some("underground") => 1,
        Some("partly_buried") => 2,
        Some("in_mountain") => 3,
        Some("on_ocean_floor") => 4,
        Some("in_nether") => 5,
        _ => 0,
    };
    let ocean = placement == 4;
    let i = ground.height(c.x, c.z, ocean) - 1;
    let span = bbox.y_span();
    let low = h.min_y + 15;
    let l = match placement {
        3 => r.int_between(70, i - span),
        1 => r.int_between(low, i - span),
        2 => i - span + r.int_between(2, 8),
        _ => i,
    };
    let corners = [
        (bbox.min.x, bbox.min.z),
        (bbox.max.x, bbox.min.z),
        (bbox.min.x, bbox.max.z),
        (bbox.max.x, bbox.max.z),
    ];
    let tops: Vec<i32> = corners
        .iter()
        .map(|&(cx, cz)| ground.height(cx, cz, ocean))
        .collect();
    let mut y = l;
    while y > low {
        if tops.iter().filter(|&&t| t > y).count() >= 3 {
            break;
        }
        y -= 1;
    }
    let pos = P::new(corner.x, y, corner.z);
    let at = P::new(c.x, y, c.z);
    let mut processors: Vec<Arc<Processor>> = Vec::new();
    let facts = s.facts();
    let rule = |input: &str, chance: f32, output: &str| -> Option<super::template::Rule> {
        let i = facts.block_id(input)?;
        super::template::Rule::new(
            RuleTest::RandomBlock(i, chance),
            facts.default_state(output)?,
        )
    };
    let mut rules = Vec::new();
    rules.extend(rule("minecraft:gold_block", 0.3, "minecraft:air"));
    rules.extend(rule(
        "minecraft:stone_bricks",
        mossiness,
        "minecraft:mossy_stone_bricks",
    ));
    rules.extend(rule(
        "minecraft:stone_brick_stairs",
        mossiness,
        "minecraft:mossy_stone_brick_stairs",
    ));
    rules.extend(rule(
        "minecraft:stone_brick_slab",
        mossiness,
        "minecraft:mossy_stone_brick_slab",
    ));
    rules.extend(rule(
        "minecraft:stone_brick_wall",
        mossiness,
        "minecraft:mossy_stone_brick_wall",
    ));
    rules.extend(rule("minecraft:obsidian", 0.1, "minecraft:crying_obsidian"));
    processors.push(Arc::new(Processor::Rule(rules)));
    if let Some(t) = facts.tag("minecraft:features_cannot_replace") {
        processors.push(Arc::new(Processor::Protected(BlockSet::Tag(t))));
    }
    processors.push(ignore(!air_pocket));
    let settings = Settings {
        rotation: rot,
        mirror,
        pivot,
        processors,
        ..Settings::default()
    };
    let cold = setup["can_be_cold"].as_bool().unwrap_or(false) && {
        let b = ground.biome(at.x >> 2, at.y >> 2, at.z >> 2);
        s.cold_biomes.contains(&b)
    };
    let p = template_piece(
        s,
        &id,
        pos,
        settings,
        Post::Portal {
            placement,
            air_pocket,
            mossiness,
            cold,
        },
        h,
    )?;
    Some((vec![p], at))
}

/// Places the part of `piece` inside `chunk`.
pub fn place(
    cx: &Ctx2,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    piece: &Piece,
    chunk: BBox,
    start: &Start,
) {
    let _ = start;
    let s = cx.structures;
    let facts = s.facts();
    match &*piece.kind {
        Kind::Template(t) => place_template(s, region, r, t, chunk),
        Kind::Treasure(p) => treasure(facts, region, r, *p, chunk),
        Kind::Built(b) => super::built::place(facts, region, r, b, chunk),
        Kind::Mineshaft(part) => super::mineshaft::place(facts, region, r, part, chunk),
        Kind::Stronghold(part) => super::stronghold::place(facts, region, r, part, chunk),
        Kind::Monument(m) => super::observed::place_monument(s.observed(), facts, region, m, chunk),
    }
}

fn loot_chest(facts: &Facts, region: &mut dyn Region, r: &mut WorldgenRandom, p: P, table: &str) {
    let s = region.block(p);
    if !facts.is(s, "minecraft:chest")
        && !facts.is(s, "minecraft:barrel")
        && !facts.is(s, "minecraft:trapped_chest")
    {
        return;
    }
    let mut be = Compound::new();
    be.insert(
        "id",
        Tag::String(
            if facts.is(s, "minecraft:barrel") {
                "minecraft:barrel"
            } else {
                "minecraft:chest"
            }
            .into(),
        ),
    );
    be.insert("LootTable", Tag::String(table.into()));
    be.insert("LootTableSeed", Tag::Long(r.next_long()));
    region.set_block_entity(p, be);
}

fn place_template(
    s: &Structures,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    t: &TemplatePiece,
    chunk: BBox,
) {
    let facts = s.facts();
    let Some(tpl) = s.template(&t.id) else { return };
    let mut settings = t.settings.clone();
    settings.bbox = Some(chunk);
    let pos = *t.fixed.get_or_init(|| match t.post {
        Post::Plain | Post::Mansion | Post::Portal { .. } => t.pos,
        Post::Igloo { offset, .. } => {
            let rel = transform(
                P::new(3 - offset.x, 0, -offset.z),
                settings.mirror,
                settings.rotation,
                settings.pivot,
            );
            let q = t.pos.add(rel);
            let i = region.height(Heightmap::WorldSurfaceWg, q.x, q.z);
            t.pos.offset(0, i - 90 - 1, 0)
        }
        Post::Shipwreck { beached } => {
            let map = if beached {
                Heightmap::WorldSurfaceWg
            } else {
                Heightmap::OceanFloorWg
            };
            let (sx, sz) = (tpl.size.x, tpl.size.z);
            let (mut sum, mut min) = (0, i32::MAX);
            for dz in 0..sz {
                for dx in 0..sx {
                    let hh = region.height(map, t.pos.x + dx, t.pos.z + dz);
                    sum += hh;
                    min = min.min(hh);
                }
            }
            let y = if beached {
                min - tpl.size.y / 2 - r.next_int_bound(3)
            } else {
                sum / (sx * sz).max(1)
            };
            P::new(t.pos.x, y, t.pos.z)
        }
        Post::OceanRuin { .. } => {
            let i = region.height(Heightmap::OceanFloorWg, t.pos.x, t.pos.z);
            let p = P::new(t.pos.x, i, t.pos.z);
            let far = transform(
                P::new(tpl.size.x - 1, 0, tpl.size.z - 1),
                Mirror::None,
                settings.rotation,
                P::default(),
            )
            .add(p);
            P::new(p.x, ruin_height(facts, region, p, far), p.z)
        }
    });
    tpl.place(facts, region, r, pos, pos, &settings);
    for (mp, marker) in tpl.data_markers(facts, pos, &settings) {
        if !chunk.inside(mp) {
            continue;
        }
        match (&t.post, marker.as_str()) {
            (Post::Igloo { .. }, "chest") => {
                region.set(mp, facts.air);
                loot_chest(
                    facts,
                    region,
                    r,
                    mp.below(1),
                    "minecraft:chests/igloo_chest",
                );
            }
            (Post::Shipwreck { .. }, m) => {
                let table = match m {
                    "map_chest" => "minecraft:chests/shipwreck_map",
                    "treasure_chest" => "minecraft:chests/shipwreck_treasure",
                    "supply_chest" => "minecraft:chests/shipwreck_supply",
                    _ => continue,
                };
                loot_chest(facts, region, r, mp.below(1), table);
            }
            (Post::OceanRuin { large, .. }, "chest") => {
                if let Some(chest) = facts.state("minecraft:chest", &[("waterlogged", "true")]) {
                    let wet = facts.is_water_source(region.block(mp));
                    region.set(
                        mp,
                        facts.with(chest, "waterlogged", if wet { "true" } else { "false" }),
                    );
                    let table = if *large {
                        "minecraft:chests/underwater_ruin_big"
                    } else {
                        "minecraft:chests/underwater_ruin_small"
                    };
                    loot_chest(facts, region, r, mp, table);
                }
            }
            (Post::Mansion, m) if m.starts_with("Chest") => {
                let facing = match m {
                    "ChestWest" => "west",
                    "ChestEast" => "east",
                    "ChestNorth" => "north",
                    _ => "south",
                };
                if let Some(chest) = facts.state("minecraft:chest", &[("facing", facing)]) {
                    let chest = super::template::transform_state(
                        facts,
                        chest,
                        settings.mirror,
                        settings.rotation,
                    );
                    region.set(mp, chest);
                    loot_chest(facts, region, r, mp, "minecraft:chests/woodland_mansion");
                }
            }
            _ => {
                region.set(mp, facts.air);
            }
        }
    }
    match t.post {
        Post::Igloo { top: true, .. } => {
            let rel = transform(
                P::new(3, 0, 5),
                settings.mirror,
                settings.rotation,
                settings.pivot,
            );
            let p = pos.add(rel);
            let below = region.block(p.below(1));
            if chunk.inside(p)
                && !facts.is_air(below)
                && !facts.is(below, "minecraft:ladder")
                && let Some(snow) = facts.default_state("minecraft:snow_block")
            {
                region.set(p, snow);
            }
        }
        Post::Portal {
            placement, cold, ..
        } => portal_ground(
            facts, region, r, &tpl, pos, &settings, chunk, placement, cold,
        ),
        _ => {}
    }
}

fn ruin_height(facts: &Facts, region: &dyn Region, pos: P, far: P) -> i32 {
    let mut i = pos.y;
    let mut j = 512;
    let k = i - 1;
    let mut l = 0;
    let min = region.heights().min_y;
    let (x0, x1) = (pos.x.min(far.x), pos.x.max(far.x));
    let (z0, z1) = (pos.z.min(far.z), pos.z.max(far.z));
    for z in z0..=z1 {
        for x in x0..=x1 {
            let mut o = pos.y - 1;
            loop {
                let s = region.block(P::new(x, o, z));
                let open = facts.is_air(s)
                    || !facts.fluid(s).is_none()
                    || facts.in_named(s, "minecraft:ice");
                if !(open && o > min + 1) {
                    break;
                }
                o -= 1;
            }
            j = j.min(o);
            if o < k - 2 {
                l += 1;
            }
        }
    }
    let p = (pos.x - far.x).abs();
    if k - j > 2 && l > p - 2 {
        i = j + 1;
    }
    i
}

/// Netherrack (and some magma) spread around a ruined portal's base in a
/// rough diamond, with drips hanging below it.
#[allow(clippy::too_many_arguments)]
fn portal_ground(
    facts: &Facts,
    region: &mut dyn Region,
    r: &mut WorldgenRandom,
    tpl: &super::template::Template,
    pos: P,
    settings: &Settings,
    chunk: BBox,
    placement: u8,
    cold: bool,
) {
    let bbox = tpl.bbox(pos, settings.rotation, settings.pivot, settings.mirror);
    let (Some(netherrack), Some(magma)) = (
        facts.default_state("minecraft:netherrack"),
        facts.default_state("minecraft:magma_block"),
    ) else {
        return;
    };
    let cannot = facts.tag("minecraft:features_cannot_replace");
    let replaceable = |region: &dyn Region, p: P| {
        let s = region.block(p);
        !facts.is(s, "minecraft:air")
            && !facts.is(s, "minecraft:obsidian")
            && cannot.is_none_or(|t| !facts.in_tag(s, t))
            && (placement == 5 || !facts.is(s, "minecraft:lava"))
    };
    // Every chunk the portal reaches spreads it again, unclipped, as the
    // game does.
    let _ = chunk;
    let put = |region: &mut dyn Region, r: &mut WorldgenRandom, p: P| {
        let s = if !cold && r.next_float() < 0.07 {
            magma
        } else {
            netherrack
        };
        region.set(p, s);
    };
    let drip = |region: &mut dyn Region, r: &mut WorldgenRandom, mut p: P| {
        put(region, r, p);
        let mut i = 8;
        while i > 0 && r.next_float() < 0.5 {
            p = p.below(1);
            i -= 1;
            put(region, r, p);
        }
    };
    let surface = placement == 0 || placement == 4;
    let c = bbox.center();
    const FALLOFF: [f32; 14] = [
        1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.9, 0.9, 0.8, 0.7, 0.6, 0.4, 0.2,
    ];
    let n = FALLOFF.len() as i32;
    let j = (bbox.x_span() + bbox.z_span()) / 2;
    let k = r.next_int_bound((8 - j / 2).max(1));
    let map = if placement == 4 {
        Heightmap::OceanFloorWg
    } else {
        Heightmap::WorldSurfaceWg
    };
    for x in c.x - n..=c.x + n {
        for z in c.z - n..=c.z + n {
            let o = (x - c.x).abs() + (z - c.z).abs();
            let p = (o + k).max(0);
            if p >= n {
                continue;
            }
            if r.next_double() >= f64::from(FALLOFF[p as usize]) {
                continue;
            }
            let q = region.height(map, x, z) - 1;
            let y = if surface { q } else { bbox.min.y.min(q) };
            let m = P::new(x, y, z);
            if (y - (bbox.min.y - 1)).abs() > 3 || !replaceable(region, m) {
                continue;
            }
            put(region, r, m);
            drip(region, r, m.below(1));
        }
    }
    for x in bbox.min.x + 1..bbox.max.x {
        for z in bbox.min.z + 1..bbox.max.z {
            let p = P::new(x, bbox.min.y, z);
            if facts.is(region.block(p), "minecraft:netherrack") {
                drip(region, r, p.below(1));
            }
        }
    }
}

fn treasure(facts: &Facts, region: &mut dyn Region, r: &mut WorldgenRandom, p: P, chunk: BBox) {
    if !chunk.inside(P::new(p.x, chunk.min.y, p.z)) {
        return;
    }
    let min = region.heights().min_y;
    let mut m = P::new(p.x, region.height(Heightmap::OceanFloorWg, p.x, p.z), p.z);
    let liquid = |s: u32| {
        !facts.fluid(s).is_none()
            && (facts.is(s, "minecraft:water") || facts.is(s, "minecraft:lava"))
    };
    while m.y > min {
        let s = region.block(m);
        let below = region.block(m.below(1));
        let ok = [
            "minecraft:sandstone",
            "minecraft:stone",
            "minecraft:andesite",
            "minecraft:granite",
            "minecraft:diorite",
        ]
        .iter()
        .any(|n| facts.is(below, n) && below == facts.default_state(n).unwrap_or(u32::MAX));
        if ok {
            let sand = facts.default_state("minecraft:sand").unwrap_or(s);
            let cover = if facts.is_air(s) || liquid(s) {
                sand
            } else {
                s
            };
            for d in [
                Dir::Down,
                Dir::Up,
                Dir::North,
                Dir::South,
                Dir::West,
                Dir::East,
            ] {
                let q = m.rel(d);
                let qs = region.block(q);
                if !facts.is_air(qs) && !liquid(qs) {
                    continue;
                }
                let q2 = region.block(q.below(1));
                if (facts.is_air(q2) || liquid(q2)) && d != Dir::Up {
                    region.set(q, below);
                } else {
                    region.set(q, cover);
                }
            }
            if let Some(chest) = facts.default_state("minecraft:chest") {
                region.set(m, chest);
                loot_chest(facts, region, r, m, "minecraft:chests/buried_treasure");
            }
            return;
        }
        m = m.below(1);
    }
}

impl super::template::Rule {
    pub fn new(input: RuleTest, output: u32) -> Option<Self> {
        Some(Self::simple(input, output))
    }
}
