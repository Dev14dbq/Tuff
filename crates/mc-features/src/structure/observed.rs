//! Ocean monuments and woodland mansions, built as a vanilla server built
//! the ones `cargo xtask observe-structures` recorded
//! (`observed/monuments.json`, `observed/mansions.json`): the wiki does not
//! describe how their rooms are laid out, so one of the recorded layouts is
//! picked and turned. Where they are, how high, and what they stand on
//! follows the wiki ("Ocean Monument", "Woodland Mansion").

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use mc_worldgen::random::RandomSource;
use serde_json::Value;

use super::Ground;
use super::legacy::{Kind, Piece};
use super::template::{BBox, Mirror, Rotation, parse_block_string};
use crate::blocks::{Facts, Fluid};
use crate::rand::WorldgenRandom;
use crate::value::Heights;
use crate::{P, Region};

pub const SIDE: i32 = 58;
pub const TALL: i32 = 23;

#[derive(Debug, Default)]
pub struct Observed {
    pub monuments: Vec<ObservedMonument>,
    pub mansions: Vec<ObservedMansion>,
}

#[derive(Debug)]
pub struct ObservedMonument {
    /// Per block (`(y * 58 + z) * 58 + x`): the state, or none where the
    /// monument left the ground alone.
    blocks: Vec<Option<u32>>,
    /// Pillar columns going down to the sea floor (`z * 58 + x`).
    pillars: Vec<Option<u32>>,
    /// The start chunk it was recorded at.
    chunk: (i32, i32),
}

#[derive(Debug)]
pub struct ObservedMansion {
    /// The start (chunk corner + 7) at the height of the first floor.
    origin: P,
    rotation: Rotation,
    chunk: (i32, i32),
    pieces: Vec<(String, P, Rotation, Mirror)>,
}

fn rotation(s: &str) -> Rotation {
    Rotation(match s {
        "CLOCKWISE_90" => 1,
        "CLOCKWISE_180" => 2,
        "COUNTERCLOCKWISE_90" => 3,
        _ => 0,
    })
}

fn mirror(s: &str) -> Mirror {
    match s {
        "LEFT_RIGHT" => Mirror::LeftRight,
        "FRONT_BACK" => Mirror::FrontBack,
        _ => Mirror::None,
    }
}

fn ints(v: &Value) -> Vec<i32> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_i64().unwrap_or(0) as i32).collect())
        .unwrap_or_default()
}

impl Observed {
    pub fn load(dir: &Path, facts: &Facts) -> Self {
        let read = |name: &str| -> Value {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null)
        };
        let mut out = Observed::default();
        for m in read("monuments.json")["monuments"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let palette: Vec<Option<u32>> = m["palette"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| parse_block_string(facts, s.as_str().unwrap_or("")))
                .collect();
            let state = |i: i64| -> Option<u32> {
                usize::try_from(i)
                    .ok()
                    .and_then(|i| palette.get(i).copied().flatten())
            };
            let blocks: Vec<Option<u32>> = m["blocks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| state(v.as_i64().unwrap_or(-1)))
                .collect();
            if blocks.len() != (SIDE * SIDE * TALL) as usize {
                continue;
            }
            let mut pillars = vec![None; (SIDE * SIDE) as usize];
            for p in m["pillars"].as_array().into_iter().flatten() {
                let p = ints(p);
                if let [x, z, i] = p[..] {
                    pillars[(z * SIDE + x) as usize] = state(i64::from(i));
                }
            }
            let chunk = ints(&m["chunk"]);
            out.monuments.push(ObservedMonument {
                blocks,
                pillars,
                chunk: (chunk[0], chunk[1]),
            });
        }
        for m in read("mansions.json")["mansions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let chunk = ints(&m["chunk"]);
            let pieces: Vec<(String, P, Rotation, Mirror)> = m["pieces"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| {
                    let pos = ints(&p["pos"]);
                    (
                        format!(
                            "minecraft:woodland_mansion/{}",
                            p["template"].as_str().unwrap_or("")
                        ),
                        P::new(pos[0], pos[1], pos[2]),
                        rotation(p["rotation"].as_str().unwrap_or("")),
                        mirror(p["mirror"].as_str().unwrap_or("")),
                    )
                })
                .collect();
            let Some(entrance) = pieces.iter().find(|p| p.0.ends_with("/entrance")) else {
                continue;
            };
            out.mansions.push(ObservedMansion {
                origin: P::new(chunk[0] * 16 + 7, entrance.1.y, chunk[1] * 16 + 7),
                rotation: entrance.2,
                chunk: (chunk[0], chunk[1]),
                pieces: pieces.clone(),
            });
        }
        out
    }
}

/// One quarter turn of a monument column.
fn quarter(x: i32, z: i32) -> (i32, i32) {
    (SIDE - 1 - z, x)
}

/// A monument as placed: recorded layout `index`, mirrored (x → −x) and
/// turned `turns` quarter turns, its corner at `origin`.
#[derive(Debug)]
pub struct Monument {
    pub index: usize,
    pub turns: u8,
    pub mirror: bool,
    pub origin: P,
}

impl Monument {
    /// The recorded column a placed column comes from.
    fn source(&self, x: i32, z: i32) -> (i32, i32) {
        let (mut x, mut z) = (x, z);
        for _ in 0..(4 - self.turns) % 4 {
            (x, z) = quarter(x, z);
        }
        if self.mirror {
            (SIDE - 1 - x, z)
        } else {
            (x, z)
        }
    }
}

/// Every biome cell within `radius` blocks of the middle is in `ok`.
fn monument_biomes(
    ground: &mut dyn Ground,
    x: i32,
    y: i32,
    z: i32,
    radius: i32,
    ok: &HashSet<u32>,
) -> bool {
    let qy = y >> 2;
    for qz in (z - radius) >> 2..=(z + radius) >> 2 {
        for qx in (x - radius) >> 2..=(x + radius) >> 2 {
            if !ok.contains(&ground.biome(qx, qy, qz)) {
                return false;
            }
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub fn monument(
    observed: &Observed,
    around: &HashSet<u32>,
    r: &mut WorldgenRandom,
    x: i32,
    z: i32,
    h: Heights,
    ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    if observed.monuments.is_empty() {
        return None;
    }
    let (mx, mz) = (x * 16 + 9, z * 16 + 9);
    let sea = h.sea_level;
    // Deep ocean in the middle is the structure's own biome check; ocean or
    // river 29 blocks around (a vanilla monument stands 17 blocks from
    // plain ocean, so the wiki's 16 blocks of deep ocean no longer hold).
    if !monument_biomes(ground, mx, sea, mz, 29, around) {
        return None;
    }
    let mut turns = r.next_int_bound(4) as u8;
    let mut index = r.next_int_bound(observed.monuments.len() as i32) as usize;
    let mut mirror = r.next_bool();
    // To check the recording against vanilla: the monument recorded here,
    // as it was.
    if std::env::var_os("REPLAY_OBSERVED").is_some()
        && let Some(same) = observed.monuments.iter().position(|m| m.chunk == (x, z))
    {
        (index, turns, mirror) = (same, 0, false);
    }
    let origin = P::new(x * 16 - 29, sea - 24, z * 16 - 29);
    let bbox = BBox::from_corners(
        P::new(origin.x, h.min_y, origin.z),
        P::new(
            origin.x + SIDE - 1,
            origin.y + TALL - 1,
            origin.z + SIDE - 1,
        ),
    );
    let piece = Piece {
        bbox,
        kind: Arc::new(Kind::Monument(Monument {
            index,
            turns,
            mirror,
            origin,
        })),
    };
    Some((vec![piece], P::new(mx, sea, mz)))
}

pub fn place_monument(
    observed: &Observed,
    facts: &Facts,
    region: &mut dyn Region,
    m: &Monument,
    chunk: BBox,
) {
    let Some(o) = observed.monuments.get(m.index) else {
        return;
    };
    let h = region.heights();
    for z in chunk.min.z.max(m.origin.z)..=chunk.max.z.min(m.origin.z + SIDE - 1) {
        for x in chunk.min.x.max(m.origin.x)..=chunk.max.x.min(m.origin.x + SIDE - 1) {
            let (sx, sz) = m.source(x - m.origin.x, z - m.origin.z);
            for y in 0..TALL {
                if let Some(s) = o.blocks[((y * SIDE + sz) * SIDE + sx) as usize] {
                    region.set(P::new(x, m.origin.y + y, z), s);
                }
            }
            if let Some(s) = o.pillars[(sz * SIDE + sx) as usize] {
                let mut y = m.origin.y - 1;
                while y > h.min_y {
                    let p = P::new(x, y, z);
                    let here = region.block(p);
                    if !facts.is_air(here) && facts.fluid(here) == Fluid::None {
                        break;
                    }
                    region.set(p, s);
                    y -= 1;
                }
            }
        }
    }
}

/// The pieces of a mansion starting in chunk `x, z`: a recorded mansion
/// turned to a random rotation, on the lowest of four corners of a 5 × 5
/// square next to the start; none below y 60.
pub fn mansion(
    s: &super::Structures,
    r: &mut WorldgenRandom,
    x: i32,
    z: i32,
    h: Heights,
    ground: &mut dyn Ground,
) -> Option<(Vec<Piece>, P)> {
    let observed = s.observed();
    if observed.mansions.is_empty() {
        return None;
    }
    let rot = Rotation(r.next_int_bound(4) as u8);
    let (i, j) = match rot.0 {
        1 => (-5, 5),
        2 => (-5, -5),
        3 => (5, -5),
        _ => (5, 5),
    };
    let (sx, sz) = (x * 16 + 7, z * 16 + 7);
    let y = [(sx, sz), (sx, sz + j), (sx + i, sz), (sx + i, sz + j)]
        .into_iter()
        .map(|(x, z)| ground.height(x, z, false) - 1)
        .min()?;
    if y < 60 {
        return None;
    }
    let mut o = &observed.mansions[r.next_int_bound(observed.mansions.len() as i32) as usize];
    let mut rot = rot;
    // To check the recording against vanilla: the mansion recorded here,
    // as it was.
    if std::env::var_os("REPLAY_OBSERVED").is_some()
        && let Some(same) = observed.mansions.iter().find(|m| m.chunk == (x, z))
    {
        o = same;
        rot = same.rotation;
    }
    let delta = Rotation((rot.0 + 4 - o.rotation.0) % 4);
    let origin = P::new(sx, y, sz);
    let mut pieces = Vec::new();
    for (id, pos, prot, mir) in &o.pieces {
        let rel = super::template::transform(
            P::new(pos.x - o.origin.x, pos.y - o.origin.y, pos.z - o.origin.z),
            Mirror::None,
            delta,
            P::default(),
        );
        let settings = super::template::Settings {
            rotation: Rotation((prot.0 + delta.0) % 4),
            mirror: *mir,
            processors: vec![Arc::new(super::template::Processor::Ignore { air: false })],
            ..Default::default()
        };
        pieces.extend(super::legacy::template_piece(
            s,
            id,
            origin.add(rel),
            settings,
            super::legacy::Post::Mansion,
            h,
        ));
    }
    Some((pieces, origin))
}

/// Cobblestone under the mansion down to the ground, wherever its floor
/// has something under it to fill.
pub fn mansion_foundation(facts: &Facts, region: &mut dyn Region, bbox: BBox, chunk: BBox) {
    let Some(cobble) = facts.default_state("minecraft:cobblestone") else {
        return;
    };
    let h = region.heights();
    let y0 = bbox.min.y;
    for z in chunk.min.z.max(bbox.min.z)..=chunk.max.z.min(bbox.max.z) {
        for x in chunk.min.x.max(bbox.min.x)..=chunk.max.x.min(bbox.max.x) {
            if facts.is_air(region.block(P::new(x, y0, z))) {
                continue;
            }
            let mut y = y0 - 1;
            while y > h.min_y {
                let p = P::new(x, y, z);
                let here = region.block(p);
                if !facts.is_air(here) && facts.fluid(here) == Fluid::None {
                    break;
                }
                region.set(p, cobble);
                y -= 1;
            }
        }
    }
}
