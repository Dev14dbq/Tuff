//! Collision boxes that do not fill the whole cell (minecraft.wiki, the
//! hitbox of each block): stairs, panes, fences, walls, doors and the short
//! blocks a mob already steps onto. Numbers are sixteenths of a block; a
//! fence post reaches 24, a block and a half.

use mc_entity::Aabb;

/// A block-state lookup. The string lives as long as the state, not the key.
type Prop<'a> = dyn Fn(&str) -> Option<&'a str> + 'a;

/// Boxes for `name`, or `None` when the block is a full cube (or nothing
/// here) and the caller should use its own column.
pub fn parts<'a>(name: &str, prop: &Prop<'a>) -> Option<Vec<[i32; 6]>> {
    let name = name.trim_start_matches("minecraft:");
    let mut out = Vec::new();
    if name.ends_with("_stairs") {
        stairs(prop, &mut out);
        return Some(out);
    }
    if name.ends_with("_pane") || name.ends_with("_bars") {
        bars(prop, 0, 16, &mut out);
        return Some(out);
    }
    if name.ends_with("_fence") && !name.ends_with("_fence_gate") {
        bars_post(prop, 6, 10, 7, 9, 24, &mut out);
        return Some(out);
    }
    if name.ends_with("_wall") {
        wall(prop, &mut out);
        return Some(out);
    }
    if name.ends_with("_door") {
        door(prop, &mut out);
        return Some(out);
    }
    if name.ends_with("_trapdoor") {
        trapdoor(prop, &mut out);
        return Some(out);
    }
    if name.ends_with("_slab") {
        let (y0, y1) = match prop("type") {
            Some("top") => (8, 16),
            Some("double") => return None,
            _ => (0, 8),
        };
        return Some(vec![column(y0, y1)]);
    }
    if name.ends_with("_fence_gate") {
        return if prop("open") == Some("true") {
            Some(Vec::new())
        } else {
            Some(vec![[0, 0, 0, 16, 24, 16]])
        };
    }
    if name.ends_with("carpet") || name == "lily_pad" {
        return Some(vec![column(0, 1)]);
    }
    if name.ends_with("_bed") {
        return Some(vec![column(0, 9)]);
    }
    let span = match name {
        "snow" => {
            let layers: i32 = prop("layers").and_then(|l| l.parse().ok()).unwrap_or(1);
            (0, (layers - 1) * 2)
        }
        "soul_sand" | "mud" | "chest" | "trapped_chest" | "ender_chest" => (0, 14),
        "farmland" | "dirt_path" | "honey_block" => (0, 15),
        "enchanting_table" => (0, 12),
        "end_portal_frame" => (0, 13),
        "stonecutter" => (0, 9),
        "daylight_detector" => (0, 6),
        "cake" => (0, 8),
        "repeater" | "comparator" => (0, 2),
        "cactus" => {
            out.push([1, 0, 1, 15, 16, 15]);
            return Some(out);
        }
        _ => return None,
    };
    if span.1 <= span.0 {
        return Some(Vec::new());
    }
    Some(vec![column(span.0, span.1)])
}

/// Appends the boxes of this block. `fallback` is a full-width column
/// (bottom, top) used when the block has no shape of its own.
pub fn push<'a>(
    name: &str,
    prop: &Prop<'a>,
    at: [i32; 3],
    fallback: Option<(f64, f64)>,
    out: &mut Vec<Aabb>,
) {
    let [x, y, z] = at;
    if let Some(parts) = parts(name, prop) {
        for p in parts {
            out.push(at_cell(x, y, z, p));
        }
        return;
    }
    if let Some((lo, hi)) = fallback {
        out.push(Aabb {
            min: [f64::from(x), f64::from(y) + lo, f64::from(z)],
            max: [f64::from(x) + 1.0, f64::from(y) + hi, f64::from(z) + 1.0],
        });
    }
}

fn column(y0: i32, y1: i32) -> [i32; 6] {
    [0, y0, 0, 16, y1, 16]
}

fn at_cell(x: i32, y: i32, z: i32, p: [i32; 6]) -> Aabb {
    let s = |n: i32| f64::from(n) / 16.0;
    Aabb {
        min: [
            f64::from(x) + s(p[0]),
            f64::from(y) + s(p[1]),
            f64::from(z) + s(p[2]),
        ],
        max: [
            f64::from(x) + s(p[3]),
            f64::from(y) + s(p[4]),
            f64::from(z) + s(p[5]),
        ],
    }
}

/// Local space is a north-facing stair: the high side is north (z 0–8),
/// and +x is east. `facing` is the direction the stair climbs, the way the
/// unrotated model is turned.
fn place(facing: &str, x0: i32, z0: i32, x1: i32, z1: i32, y0: i32, y1: i32) -> [i32; 6] {
    let map = |x: i32, z: i32| match facing {
        "south" => (16 - x, 16 - z),
        "east" => (16 - z, x),
        "west" => (z, 16 - x),
        _ => (x, z),
    };
    let (a, b) = map(x0, z0);
    let (c, d) = map(x1, z1);
    [a.min(c), y0, b.min(d), a.max(c), y1, b.max(d)]
}

fn stairs<'a>(prop: &Prop<'a>, out: &mut Vec<[i32; 6]>) {
    let facing = prop("facing").unwrap_or("north");
    let top = prop("half") == Some("top");
    let (by0, by1, sy0, sy1) = if top { (8, 16, 0, 8) } else { (0, 8, 8, 16) };
    out.push(place(facing, 0, 0, 16, 16, by0, by1));
    // (x0, z0, x1, z1) of each raised piece, in the north-facing model.
    // Left and right are the hands of someone looking the way it climbs.
    let pieces: &[(i32, i32, i32, i32)] = match prop("shape").unwrap_or("straight") {
        "outer_left" => &[(0, 0, 8, 8)],
        "outer_right" => &[(8, 0, 16, 8)],
        "inner_left" => &[(0, 0, 16, 8), (0, 8, 8, 16)],
        "inner_right" => &[(0, 0, 16, 8), (8, 8, 16, 16)],
        _ => &[(0, 0, 16, 8)],
    };
    for &(x0, z0, x1, z1) in pieces {
        out.push(place(facing, x0, z0, x1, z1, sy0, sy1));
    }
}

fn flag<'a>(prop: &Prop<'a>, side: &str) -> bool {
    prop(side) == Some("true")
}

/// A centre post and arms to the sides that connect. `post` is the post's
/// inset, `arm` the thinner arm's inset, `top` the height.
fn bars_post<'a>(
    prop: &Prop<'a>,
    post0: i32,
    post1: i32,
    arm0: i32,
    arm1: i32,
    top: i32,
    out: &mut Vec<[i32; 6]>,
) {
    out.push([post0, 0, post0, post1, top, post1]);
    if flag(prop, "north") {
        out.push([arm0, 0, 0, arm1, top, post0]);
    }
    if flag(prop, "south") {
        out.push([arm0, 0, post1, arm1, top, 16]);
    }
    if flag(prop, "west") {
        out.push([0, 0, arm0, post0, top, arm1]);
    }
    if flag(prop, "east") {
        out.push([post1, 0, arm0, 16, top, arm1]);
    }
}

fn bars<'a>(prop: &Prop<'a>, _y0: i32, top: i32, out: &mut Vec<[i32; 6]>) {
    bars_post(prop, 7, 9, 7, 9, top, out);
}

fn wall<'a>(prop: &Prop<'a>, out: &mut Vec<[i32; 6]>) {
    let side = |d: &str| -> &'a str { prop(d).unwrap_or("none") };
    let tall = ["north", "south", "east", "west"]
        .iter()
        .any(|d| side(d) == "tall");
    let post = if prop("up") == Some("true") || tall {
        24
    } else {
        16
    };
    out.push([4, 0, 4, 12, post, 12]);
    let h = |d: &str| if side(d) == "tall" { 24 } else { 14 };
    if side("north") != "none" {
        out.push([5, 0, 0, 11, h("north"), 4]);
    }
    if side("south") != "none" {
        out.push([5, 0, 12, 11, h("south"), 16]);
    }
    if side("west") != "none" {
        out.push([0, 0, 5, 4, h("west"), 11]);
    }
    if side("east") != "none" {
        out.push([12, 0, 5, 16, h("east"), 11]);
    }
}

fn edge(facing: &str) -> [i32; 6] {
    match facing {
        "south" => [0, 0, 13, 16, 16, 16],
        "west" => [0, 0, 0, 3, 16, 16],
        "east" => [13, 0, 0, 16, 16, 16],
        _ => [0, 0, 0, 16, 16, 3],
    }
}

fn door<'a>(prop: &Prop<'a>, out: &mut Vec<[i32; 6]>) {
    let facing = prop("facing").unwrap_or("north");
    if prop("open") != Some("true") {
        out.push(edge(facing));
        return;
    }
    // The panel swings onto the hinge's side. Left is the left hand of
    // someone looking the way the door faces.
    let left = prop("hinge") != Some("right");
    let plate = match (facing, left) {
        ("north", true) | ("south", false) => [0, 0, 0, 3, 16, 16],
        ("north", false) | ("south", true) => [13, 0, 0, 16, 16, 16],
        ("east", true) | ("west", false) => [0, 0, 0, 16, 16, 3],
        _ => [0, 0, 13, 16, 16, 16],
    };
    out.push(plate);
}

fn trapdoor<'a>(prop: &Prop<'a>, out: &mut Vec<[i32; 6]>) {
    if prop("open") == Some("true") {
        out.push(edge(prop("facing").unwrap_or("north")));
        return;
    }
    let (y0, y1) = if prop("half") == Some("top") {
        (13, 16)
    } else {
        (0, 3)
    };
    out.push(column(y0, y1));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<&'a str> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| *v)
    }

    #[test]
    fn a_stair_is_a_slab_plus_the_high_half() {
        let p = prop(&[
            ("facing", "north"),
            ("half", "bottom"),
            ("shape", "straight"),
        ]);
        let b = parts("minecraft:oak_stairs", &p).unwrap();
        assert!(b.contains(&[0, 0, 0, 16, 8, 16]));
        assert!(b.contains(&[0, 8, 0, 16, 16, 8]), "{b:?}");
        let p = prop(&[
            ("facing", "east"),
            ("half", "bottom"),
            ("shape", "straight"),
        ]);
        let b = parts("oak_stairs", &p).unwrap();
        assert!(
            b.contains(&[8, 8, 0, 16, 16, 16]),
            "the high side is east, the way an east-facing stair climbs, {b:?}"
        );
    }

    #[test]
    fn a_fence_post_leaves_the_corners_open_and_a_pane_arm_reaches_the_side() {
        let none = prop(&[]);
        let fence = parts("minecraft:oak_fence", &none).unwrap();
        assert_eq!(fence, vec![[6, 0, 6, 10, 24, 10]]);
        let east = prop(&[("east", "true")]);
        let pane = parts("minecraft:glass_pane", &east).unwrap();
        assert!(pane.contains(&[7, 0, 7, 9, 16, 9]));
        assert!(pane.contains(&[9, 0, 7, 16, 16, 9]), "{pane:?}");
    }

    #[test]
    fn an_open_door_is_a_plate_on_the_hinge() {
        let p = prop(&[("facing", "north"), ("open", "true"), ("hinge", "left")]);
        assert_eq!(
            parts("minecraft:oak_door", &p).unwrap(),
            vec![[0, 0, 0, 3, 16, 16]]
        );
        let shut = prop(&[("facing", "north"), ("open", "false"), ("hinge", "left")]);
        assert_eq!(
            parts("minecraft:oak_door", &shut).unwrap(),
            vec![[0, 0, 0, 16, 16, 3]]
        );
    }
}
