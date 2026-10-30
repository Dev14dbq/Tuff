//! What a crowd does to its members and what some effects leave behind
//! when the one who carries them is killed: the arithmetic, apart from the
//! world it happens in.

/// Whether a body is crushed: with `max_entity_cramming` above zero, more
/// than `cap - 1` other pushable bodies that are not riding anything share
/// its space. (Whether this tick is the one — one in four — is rolled by the
/// caller, before it counts.)
pub fn is_crushed(cap: i32, others: usize) -> bool {
    cap > 0 && others as i64 > i64::from(cap) - 1
}

/// How many slimes an oozing victim leaves: what was asked for, but never
/// more than room remains under the cramming cap for `nearby` slimes
/// within two blocks. A cap below one lifts the limit.
pub fn slimes_to_spawn(cap: i32, nearby: i32, requested: i32) -> i32 {
    if cap < 1 {
        requested
    } else {
        (cap - nearby).max(0).min(requested)
    }
}

/// How many slimes within reach need counting to know the answer: counting
/// stops at the cap.
pub fn slime_count_limit(cap: i32) -> usize {
    cap.max(0) as usize
}

/// Weaving: the cells, out of the candidates (random cells around the
/// victim, in the order tried), that get a cobweb. A cell qualifies when
/// `fits` says so (replaceable, on a block that holds a top face up), each
/// cell once, until `count` are found.
pub fn cobweb_cells(
    candidates: &[[i32; 3]],
    count: usize,
    mut fits: impl FnMut([i32; 3]) -> bool,
) -> Vec<[i32; 3]> {
    let mut cells: Vec<[i32; 3]> = Vec::new();
    for &cell in candidates {
        if !cells.contains(&cell) && fits(cell) {
            cells.push(cell);
            if cells.len() >= count {
                break;
            }
        }
    }
    cells
}

/// The strength of the gust a wind-charged victim bursts into, from a roll
/// in [0, 1): 3 to 5.
pub fn gust_strength(roll: f32) -> f32 {
    3.0 + roll * 2.0
}

/// Infested: the chance that a hurt victim lets silverfish out.
pub const INFESTED_CHANCE: f32 = 0.1;

/// The push a silverfish gets as it comes out: the way the victim looks,
/// 0.3 long and half as much again upwards, turned by `angle` (radians,
/// from -pi/2 to pi/2) about the vertical.
pub fn silverfish_push(look: [f64; 3], angle: f32) -> [f64; 3] {
    let (x, y, z) = (look[0] * 0.3, look[1] * 0.3 * 1.5, look[2] * 0.3);
    let (s, c) = (f64::from(angle).sin(), f64::from(angle).cos());
    [x * c + z * s, y, -x * s + z * c]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crushing_needs_a_crowd_past_the_cap() {
        assert!(!is_crushed(24, 23));
        assert!(is_crushed(24, 24));
        assert!(!is_crushed(0, 1000));
        assert!(!is_crushed(-1, 1000));
        assert!(is_crushed(1, 1));
        assert!(!is_crushed(1, 0));
    }

    #[test]
    fn oozing_stops_at_the_cramming_cap() {
        // Plenty of room: both slimes.
        assert_eq!(slimes_to_spawn(24, 0, 2), 2);
        // Room for one more.
        assert_eq!(slimes_to_spawn(24, 23, 2), 1);
        assert_eq!(slimes_to_spawn(24, 24, 2), 0);
        // Counting stops at the cap, but be safe if it did not.
        assert_eq!(slimes_to_spawn(24, 30, 2), 0);
        // No cap, no limit.
        assert_eq!(slimes_to_spawn(0, 500, 2), 2);
        assert_eq!(slimes_to_spawn(-5, 500, 2), 2);
    }

    #[test]
    fn cobwebs_take_each_fitting_cell_once_up_to_the_count() {
        let cells = [[0, 0, 0], [0, 0, 0], [1, 0, 0], [2, 0, 0], [3, 0, 0]];
        let got = cobweb_cells(&cells, 2, |c| c[0] != 1);
        assert_eq!(got, vec![[0, 0, 0], [2, 0, 0]]);
        let got = cobweb_cells(&cells, 3, |c| c[0] != 3);
        assert_eq!(got, vec![[0, 0, 0], [1, 0, 0], [2, 0, 0]]);
        assert!(cobweb_cells(&cells, 3, |_| false).is_empty());
    }

    #[test]
    fn silverfish_come_out_ahead_and_a_little_up() {
        let p = silverfish_push([0.0, 1.0, 1.0], 0.0);
        assert!((p[0]).abs() < 1e-12 && (p[1] - 0.45).abs() < 1e-12 && (p[2] - 0.3).abs() < 1e-12);
        // A quarter turn swings the horizontal push to the side.
        let q = silverfish_push([0.0, 0.0, 1.0], std::f32::consts::FRAC_PI_2);
        assert!((q[0] - 0.3).abs() < 1e-6 && q[2].abs() < 1e-6);
    }

    #[test]
    fn a_gust_is_three_to_five() {
        assert_eq!(gust_strength(0.0), 3.0);
        assert_eq!(gust_strength(0.5), 4.0);
    }
}
