//! Collision boxes of the blocks that are not full cubes (minecraft.wiki
//! pages of each block, "Hitbox" sections), for what needs to know where
//! a block really is: the rays of an explosion's exposure.
//!
//! Boxes are inside the block, as `[x0, y0, z0, x1, y1, z1]` in 0..1 of
//! its cell (a fence is 1.5 high, but only the part in its own cell is
//! kept). A state without boxes here is a full cube if it blocks motion.

use std::sync::OnceLock;

use crate::context::ServerContext;

pub type Box6 = [f64; 6];

const P: f64 = 1.0 / 16.0;

fn px(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Box6 {
    [a * P, b * P, c * P, d * P, e * P, f * P]
}

/// The boxes of every block state (`None`: a full cube or nothing, as
/// `blocks_motion` says), built once.
pub fn table(ctx: &ServerContext) -> &'static [Option<Vec<Box6>>] {
    static TABLE: OnceLock<Vec<Option<Vec<Box6>>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..ctx.data.blocks.state_count())
            .map(|s| boxes_of(ctx, s))
            .collect()
    })
}

/// The box `b` (made for a thing facing north) turned to face `dir`.
fn turned(b: Box6, dir: &str) -> Box6 {
    match dir {
        "south" => [1.0 - b[3], b[1], 1.0 - b[5], 1.0 - b[0], b[4], 1.0 - b[2]],
        // Clockwise a quarter turn: north → east.
        "east" => [1.0 - b[5], b[1], b[0], 1.0 - b[2], b[4], b[3]],
        "west" => [b[2], b[1], 1.0 - b[3], b[5], b[4], 1.0 - b[0]],
        _ => b,
    }
}

/// The box `b` (made for a thing pointing up) turned to point `dir`.
fn pointing(b: Box6, dir: &str) -> Box6 {
    match dir {
        "down" => [b[0], 1.0 - b[4], b[2], b[3], 1.0 - b[1], b[5]],
        "north" => [b[0], b[2], 1.0 - b[4], b[3], b[5], 1.0 - b[1]],
        "south" => [b[0], b[2], b[1], b[3], b[5], b[4]],
        "east" => [b[1], b[0], b[2], b[4], b[3], b[5]],
        "west" => [1.0 - b[4], b[0], b[2], 1.0 - b[1], b[3], b[5]],
        _ => b,
    }
}

fn boxes_of(ctx: &ServerContext, s: u32) -> Option<Vec<Box6>> {
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(s)?.trim_start_matches("minecraft:");
    let prop = |k: &str| blocks.property(s, k);
    let is = |k: &str, v: &str| prop(k) == Some(v);
    // A post with arms toward the connected sides: (width, arm width, height).
    let arms = |post: f64, arm: f64, height: f64| {
        let (a, b) = (8.0 - post / 2.0, 8.0 + post / 2.0);
        let (c, d) = (8.0 - arm / 2.0, 8.0 + arm / 2.0);
        let h = height.min(16.0);
        let mut out = vec![px(a, 0.0, a, b, h, b)];
        for (side, dir) in [("north", 0), ("south", 1), ("west", 2), ("east", 3)] {
            let on = prop(side).is_some_and(|v| v != "false" && v != "none");
            if !on {
                continue;
            }
            out.push(match dir {
                0 => px(c, 0.0, 0.0, d, h, 8.0),
                1 => px(c, 0.0, 8.0, d, h, 16.0),
                2 => px(0.0, 0.0, c, 8.0, h, d),
                _ => px(8.0, 0.0, c, 16.0, h, d),
            });
        }
        out
    };
    Some(match name {
        n if n.ends_with("_slab") => match prop("type")? {
            "bottom" => vec![[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]],
            "top" => vec![[0.0, 0.5, 0.0, 1.0, 1.0, 1.0]],
            _ => return None,
        },
        n if n.ends_with("_stairs") => {
            let top = is("half", "top");
            let facing = prop("facing")?;
            let (lo, hi) = if top { (0.5, 1.0) } else { (0.0, 0.5) };
            let mut out = vec![[0.0, lo, 0.0, 1.0, hi, 1.0]];
            let (up0, up1) = if top { (0.0, 0.5) } else { (0.5, 1.0) };
            // Quarters of the upper layer, for a stair facing north: the
            // back (north) half, and the left (west) / right (east) ones.
            let q = |x: bool, z: bool| {
                [
                    if x { 0.5 } else { 0.0 },
                    up0,
                    if z { 0.5 } else { 0.0 },
                    if x { 1.0 } else { 0.5 },
                    up1,
                    if z { 1.0 } else { 0.5 },
                ]
            };
            // North-facing parts: (x east?, z south?).
            let parts: Vec<(bool, bool)> = match prop("shape")? {
                "outer_left" => vec![(false, false)],
                "outer_right" => vec![(true, false)],
                "inner_left" => vec![(false, false), (true, false), (false, true)],
                "inner_right" => vec![(false, false), (true, false), (true, true)],
                _ => vec![(false, false), (true, false)],
            };
            for (x, z) in parts {
                out.push(turned(q(x, z), facing));
            }
            out
        }
        n if n.ends_with("carpet") => vec![px(0.0, 0.0, 0.0, 16.0, 1.0, 16.0)],
        "snow" => {
            let layers: f64 = prop("layers")?.parse().ok()?;
            if layers <= 1.0 {
                return Some(Vec::new());
            }
            vec![[0.0, 0.0, 0.0, 1.0, (layers - 1.0) * 2.0 * P, 1.0]]
        }
        "farmland" | "dirt_path" => vec![px(0.0, 0.0, 0.0, 16.0, 15.0, 16.0)],
        "soul_sand" | "mud" => vec![px(0.0, 0.0, 0.0, 16.0, 14.0, 16.0)],
        "honey_block" => vec![px(1.0, 0.0, 1.0, 15.0, 15.0, 15.0)],
        "cactus" => vec![px(1.0, 0.0, 1.0, 15.0, 15.0, 15.0)],
        "chest" | "trapped_chest" | "ender_chest" | "copper_chest" => {
            vec![px(1.0, 0.0, 1.0, 15.0, 14.0, 15.0)]
        }
        n if n.ends_with("_bed") => vec![px(0.0, 3.0, 0.0, 16.0, 9.0, 16.0)],
        "cake" => {
            let bites: f64 = prop("bites")?.parse().ok()?;
            vec![px(1.0 + bites * 2.0, 0.0, 1.0, 15.0, 8.0, 15.0)]
        }
        "enchanting_table" => vec![px(0.0, 0.0, 0.0, 16.0, 12.0, 16.0)],
        "daylight_detector" => vec![px(0.0, 0.0, 0.0, 16.0, 6.0, 16.0)],
        "end_portal_frame" => vec![px(0.0, 0.0, 0.0, 16.0, 13.0, 16.0)],
        "stonecutter" => vec![px(0.0, 0.0, 0.0, 16.0, 9.0, 16.0)],
        "sculk_sensor" | "calibrated_sculk_sensor" => vec![px(0.0, 0.0, 0.0, 16.0, 8.0, 16.0)],
        "campfire" | "soul_campfire" => vec![px(0.0, 0.0, 0.0, 16.0, 7.0, 16.0)],
        "flower_pot" => vec![px(5.0, 0.0, 5.0, 11.0, 6.0, 11.0)],
        "decorated_pot" => vec![px(1.0, 0.0, 1.0, 15.0, 16.0, 15.0)],
        "brewing_stand" => vec![
            px(7.0, 0.0, 7.0, 9.0, 14.0, 9.0),
            px(9.0, 0.0, 5.0, 15.0, 2.0, 11.0),
            px(2.0, 0.0, 1.0, 8.0, 2.0, 7.0),
            px(2.0, 0.0, 9.0, 8.0, 2.0, 15.0),
        ],
        n if n == "cauldron" || n.ends_with("_cauldron") => vec![
            // Legs at the corners, then the floor.
            px(0.0, 0.0, 0.0, 4.0, 3.0, 2.0),
            px(0.0, 0.0, 0.0, 2.0, 3.0, 4.0),
            px(12.0, 0.0, 0.0, 16.0, 3.0, 2.0),
            px(14.0, 0.0, 0.0, 16.0, 3.0, 4.0),
            px(0.0, 0.0, 14.0, 4.0, 3.0, 16.0),
            px(0.0, 0.0, 12.0, 2.0, 3.0, 16.0),
            px(12.0, 0.0, 14.0, 16.0, 3.0, 16.0),
            px(14.0, 0.0, 12.0, 16.0, 3.0, 16.0),
            px(0.0, 3.0, 0.0, 16.0, 4.0, 16.0),
            px(0.0, 4.0, 0.0, 16.0, 16.0, 2.0),
            px(0.0, 4.0, 14.0, 16.0, 16.0, 16.0),
            px(0.0, 4.0, 2.0, 2.0, 16.0, 14.0),
            px(14.0, 4.0, 2.0, 16.0, 16.0, 14.0),
        ],
        "composter" => vec![
            px(0.0, 0.0, 0.0, 16.0, 2.0, 16.0),
            px(0.0, 2.0, 0.0, 16.0, 16.0, 2.0),
            px(0.0, 2.0, 14.0, 16.0, 16.0, 16.0),
            px(0.0, 2.0, 2.0, 2.0, 16.0, 14.0),
            px(14.0, 2.0, 2.0, 16.0, 16.0, 14.0),
        ],
        "hopper" => vec![
            px(0.0, 10.0, 0.0, 16.0, 11.0, 16.0),
            px(0.0, 11.0, 0.0, 16.0, 16.0, 2.0),
            px(0.0, 11.0, 14.0, 16.0, 16.0, 16.0),
            px(0.0, 11.0, 2.0, 2.0, 16.0, 14.0),
            px(14.0, 11.0, 2.0, 16.0, 16.0, 14.0),
            px(4.0, 4.0, 4.0, 12.0, 10.0, 12.0),
        ],
        "lantern" | "soul_lantern" => {
            if is("hanging", "true") {
                vec![
                    px(5.0, 1.0, 5.0, 11.0, 8.0, 11.0),
                    px(6.0, 8.0, 6.0, 10.0, 10.0, 10.0),
                ]
            } else {
                vec![
                    px(5.0, 0.0, 5.0, 11.0, 7.0, 11.0),
                    px(6.0, 7.0, 6.0, 10.0, 9.0, 10.0),
                ]
            }
        }
        n if n.ends_with("_skull") || n.ends_with("_head") => {
            if n.contains("wall") {
                return None;
            }
            vec![px(4.0, 0.0, 4.0, 12.0, 8.0, 12.0)]
        }
        n if n.ends_with("_trapdoor") => {
            if is("open", "true") {
                let thin = px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0);
                vec![turned(thin, prop("facing")?)]
            } else if is("half", "top") {
                vec![px(0.0, 13.0, 0.0, 16.0, 16.0, 16.0)]
            } else {
                vec![px(0.0, 0.0, 0.0, 16.0, 3.0, 16.0)]
            }
        }
        n if n.ends_with("_door") => {
            let facing = prop("facing")?;
            let closed = is("open", "false");
            let right = is("hinge", "right");
            let side = if closed {
                facing
            } else {
                match (facing, right) {
                    ("east", true) => "north",
                    ("east", false) => "south",
                    ("south", true) => "east",
                    ("south", false) => "west",
                    ("west", true) => "south",
                    ("west", false) => "north",
                    ("north", true) => "west",
                    _ => "east",
                }
            };
            vec![match side {
                "east" => px(0.0, 0.0, 0.0, 3.0, 16.0, 16.0),
                "south" => px(0.0, 0.0, 0.0, 16.0, 16.0, 3.0),
                "west" => px(13.0, 0.0, 0.0, 16.0, 16.0, 16.0),
                _ => px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0),
            }]
        }
        "anvil" | "chipped_anvil" | "damaged_anvil" => {
            let along_x = matches!(prop("facing")?, "north" | "south");
            let parts = [
                px(2.0, 0.0, 2.0, 14.0, 4.0, 14.0),
                px(4.0, 4.0, 3.0, 12.0, 5.0, 13.0),
                px(6.0, 5.0, 4.0, 10.0, 10.0, 12.0),
                px(3.0, 10.0, 0.0, 13.0, 16.0, 16.0),
            ];
            // The long top runs across the way it faces.
            parts
                .into_iter()
                .map(|b| {
                    if along_x {
                        b
                    } else {
                        [b[2], b[1], b[0], b[5], b[4], b[3]]
                    }
                })
                .collect()
        }
        n if n.ends_with("chain") => {
            let b = px(6.5, 0.0, 6.5, 9.5, 16.0, 9.5);
            vec![match prop("axis")? {
                "x" => [b[1], b[0], b[2], b[4], b[3], b[5]],
                "z" => [b[0], b[2], b[1], b[3], b[5], b[4]],
                _ => b,
            }]
        }
        "end_rod" | "lightning_rod" => {
            vec![pointing(
                px(6.0, 0.0, 6.0, 10.0, 16.0, 10.0),
                prop("facing")?,
            )]
        }
        "ladder" => vec![turned(
            px(0.0, 0.0, 13.0, 16.0, 16.0, 16.0),
            prop("facing")?,
        )],
        "lily_pad" => vec![px(1.0, 0.0, 1.0, 15.0, 1.5, 15.0)],
        "turtle_egg" => {
            if prop("eggs")? == "1" {
                vec![px(3.0, 0.0, 3.0, 12.0, 7.0, 12.0)]
            } else {
                vec![px(1.0, 0.0, 1.0, 15.0, 7.0, 15.0)]
            }
        }
        "sea_pickle" => vec![match prop("pickles")? {
            "1" => px(6.0, 0.0, 6.0, 10.0, 6.0, 10.0),
            "2" => px(3.0, 0.0, 3.0, 13.0, 6.0, 13.0),
            "3" => px(2.0, 0.0, 2.0, 14.0, 6.0, 14.0),
            _ => px(2.0, 0.0, 2.0, 14.0, 7.0, 14.0),
        }],
        "dragon_egg" => vec![px(1.0, 0.0, 1.0, 15.0, 16.0, 15.0)],
        "conduit" => vec![px(5.0, 5.0, 5.0, 11.0, 11.0, 11.0)],
        "azalea" | "flowering_azalea" => {
            vec![
                px(0.0, 8.0, 0.0, 16.0, 16.0, 16.0),
                px(6.0, 0.0, 6.0, 10.0, 8.0, 10.0),
            ]
        }
        "piston" | "sticky_piston" => {
            if !is("extended", "true") {
                return None;
            }
            vec![pointing(
                px(0.0, 0.0, 0.0, 16.0, 12.0, 16.0),
                prop("facing")?,
            )]
        }
        "piston_head" => {
            let facing = prop("facing")?;
            vec![
                pointing(px(0.0, 12.0, 0.0, 16.0, 16.0, 16.0), facing),
                pointing(px(6.0, 0.0, 6.0, 10.0, 12.0, 10.0), facing),
            ]
        }
        n if n.ends_with("_fence") => arms(4.0, 2.0, 24.0),
        n if n.ends_with("_wall") => {
            let mut out = arms(8.0, 6.0, 24.0);
            // Without the post, a straight wall has none.
            if is("up", "false") {
                out.remove(0);
            }
            out
        }
        n if n.ends_with("_fence_gate") => {
            if is("open", "true") {
                return Some(Vec::new());
            }
            let along_x = matches!(prop("facing")?, "north" | "south");
            vec![if along_x {
                px(0.0, 0.0, 6.0, 16.0, 16.0, 10.0)
            } else {
                px(6.0, 0.0, 0.0, 10.0, 16.0, 16.0)
            }]
        }
        "iron_bars" | "glass_pane" => arms(2.0, 2.0, 16.0),
        n if n.ends_with("_glass_pane") => arms(2.0, 2.0, 16.0),
        _ => return None,
    })
}

/// Where the segment `from → to` first enters one of `boxes` placed at the
/// cell `at`, as a fraction of the segment. Starting inside a box is not
/// entering it.
pub fn enters(at: [i32; 3], boxes: &[Box6], from: [f64; 3], to: [f64; 3]) -> Option<f64> {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let mut best: Option<f64> = None;
    for b in boxes {
        let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
        let mut hit = true;
        for i in 0..3 {
            let lo = f64::from(at[i]) + b[i];
            let hi = f64::from(at[i]) + b[i + 3];
            if d[i].abs() < 1e-12 {
                if from[i] < lo || from[i] > hi {
                    hit = false;
                    break;
                }
            } else {
                let (a, c) = ((lo - from[i]) / d[i], (hi - from[i]) / d[i]);
                t0 = t0.max(a.min(c));
                t1 = t1.min(a.max(c));
            }
        }
        if hit && t0 <= t1 && (0.0..=1.0).contains(&t0) {
            best = Some(best.map_or(t0, |m: f64| m.min(t0)));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    #[test]
    fn partial_blocks_have_partial_boxes() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let t = table(&ctx);
        let boxes = |name: &str, props: &[(&str, &str)]| {
            let s = ctx.data.blocks.state_id(name, props).unwrap();
            t[s as usize].clone()
        };
        assert_eq!(
            boxes("minecraft:oak_slab", &[("type", "bottom")]),
            Some(vec![[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]])
        );
        assert_eq!(boxes("minecraft:oak_slab", &[("type", "double")]), None);
        assert_eq!(boxes("minecraft:stone", &[]), None);
        // A stair facing east has its back half on the east.
        let stairs = boxes(
            "minecraft:oak_stairs",
            &[
                ("facing", "east"),
                ("half", "bottom"),
                ("shape", "straight"),
            ],
        )
        .unwrap();
        assert!(
            stairs.contains(&[0.5, 0.5, 0.0, 1.0, 1.0, 0.5]),
            "{stairs:?}"
        );
        assert!(
            stairs.contains(&[0.5, 0.5, 0.5, 1.0, 1.0, 1.0]),
            "{stairs:?}"
        );
        // A fence with a neighbour to the east has an arm that way.
        let fence = boxes(
            "minecraft:oak_fence",
            &[
                ("east", "true"),
                ("north", "false"),
                ("south", "false"),
                ("west", "false"),
            ],
        )
        .unwrap();
        assert_eq!(fence.len(), 2);
        // An anvil facing north is wide across east-west at the top.
        let anvil = boxes("minecraft:anvil", &[("facing", "north")]).unwrap();
        assert_eq!(anvil.len(), 4);
        assert!(
            anvil.contains(&px(3.0, 10.0, 0.0, 13.0, 16.0, 16.0)),
            "{anvil:?}"
        );
        let east = boxes("minecraft:anvil", &[("facing", "east")]).unwrap();
        assert!(
            east.contains(&px(0.0, 10.0, 3.0, 16.0, 16.0, 13.0)),
            "{east:?}"
        );
        // Rods point where they face; a closed piston is a cube.
        assert_eq!(
            boxes("minecraft:end_rod", &[("facing", "east")]),
            Some(vec![[0.0, 6.0 * P, 6.0 * P, 1.0, 10.0 * P, 10.0 * P]])
        );
        assert_eq!(
            boxes(
                "minecraft:piston",
                &[("extended", "false"), ("facing", "up")]
            ),
            None
        );
        assert_eq!(
            boxes(
                "minecraft:piston",
                &[("extended", "true"), ("facing", "down")]
            ),
            Some(vec![[0.0, 0.25, 0.0, 1.0, 1.0, 1.0]])
        );
        assert_eq!(
            boxes(
                "minecraft:ladder",
                &[("facing", "east"), ("waterlogged", "false")]
            )
            .unwrap()
            .len(),
            1
        );
        // A ray through the air above a bottom slab passes; one through it
        // does not.
        let slab = [[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]];
        assert!(enters([0, 0, 0], &slab, [0.5, 0.8, -1.0], [0.5, 0.8, 2.0]).is_none());
        assert!(enters([0, 0, 0], &slab, [0.5, 0.3, -1.0], [0.5, 0.3, 2.0]).is_some());
        // Starting inside is not entering.
        assert!(enters([0, 0, 0], &slab, [0.5, 0.3, 0.5], [0.5, 0.3, 2.0]).is_none());
    }
}
