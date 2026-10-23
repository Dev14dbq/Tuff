//! Axis-aligned boxes moving through blocks.

/// What an entity can bump into.
pub trait Collide {
    /// Whether the block at these coordinates stops entities.
    fn solid(&self, x: i32, y: i32, z: i32) -> bool;
    /// The bottom and top of the block's collision box within its cell, in
    /// blocks (a bottom slab `(0, 0.5)`, a fence `(0, 1.5)`); `None` when
    /// nothing stops entities there. Collision boxes fill the cell sideways.
    fn span(&self, x: i32, y: i32, z: i32) -> Option<(f64, f64)> {
        self.solid(x, y, z).then_some((0.0, 1.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    fn shifted(self, axis: usize, by: f64) -> Self {
        let mut b = self;
        b.min[axis] += by;
        b.max[axis] += by;
        b
    }
}

const EPSILON: f64 = 1e-7;

/// The collision boxes of the blocks a box moving by `want` along `axis`
/// may touch (tall blocks reach up from the cell below).
fn boxes_near(b: Aabb, axis: usize, want: f64, world: &dyn Collide) -> Vec<Aabb> {
    let mut lo = b.min;
    let mut hi = b.max;
    if want > 0.0 {
        hi[axis] += want;
    } else {
        lo[axis] += want;
    }
    let mut out = Vec::new();
    for x in (lo[0] - EPSILON).floor() as i32..=(hi[0] + EPSILON).floor() as i32 {
        for z in (lo[2] - EPSILON).floor() as i32..=(hi[2] + EPSILON).floor() as i32 {
            for y in (lo[1] - EPSILON).floor() as i32 - 1..=(hi[1] + EPSILON).floor() as i32 {
                if let Some((bottom, top)) = world.span(x, y, z) {
                    out.push(Aabb {
                        min: [f64::from(x), f64::from(y) + bottom, f64::from(z)],
                        max: [f64::from(x + 1), f64::from(y) + top, f64::from(z + 1)],
                    });
                }
            }
        }
    }
    out
}

/// How far a box can move along `axis` (signed `want`) before touching a
/// block.
fn sweep(b: Aabb, axis: usize, want: f64, world: &dyn Collide) -> f64 {
    if want == 0.0 {
        return 0.0;
    }
    let mut allowed = want;
    for c in boxes_near(b, axis, want, world) {
        // Overlapping on the other two axes.
        let beside = (0..3)
            .filter(|&a| a != axis)
            .all(|a| c.max[a] > b.min[a] + EPSILON && c.min[a] < b.max[a] - EPSILON);
        if !beside {
            continue;
        }
        if want > 0.0 && c.min[axis] >= b.max[axis] - EPSILON {
            allowed = allowed.min((c.min[axis] - b.max[axis]).max(0.0));
        } else if want < 0.0 && c.max[axis] <= b.min[axis] + EPSILON {
            allowed = allowed.max((c.max[axis] - b.min[axis]).min(0.0));
        }
    }
    allowed
}

fn slide(b: Aabb, motion: [f64; 3], world: &dyn Collide) -> (Aabb, [f64; 3]) {
    let mut b = b;
    let mut moved = [0.0; 3];
    let order = if motion[0].abs() >= motion[2].abs() {
        [1, 0, 2]
    } else {
        [1, 2, 0]
    };
    for axis in order {
        let d = sweep(b, axis, motion[axis], world);
        b = b.shifted(axis, d);
        moved[axis] = d;
    }
    (b, moved)
}

/// Moves a box by `motion` through blocks: vertical first, then the larger
/// horizontal axis, then the other. Returns the movement done and whether the
/// box landed on something.
pub fn move_entity(b: Aabb, motion: [f64; 3], world: &dyn Collide) -> ([f64; 3], bool) {
    move_stepping(b, motion, world, 0.0, false)
}

/// As [`move_entity`], but a box on the ground blocked sideways climbs
/// onto what stops it when that is no higher than `step` (0.6 for mobs:
/// slabs, carpets, paths), as vanilla's entities do.
pub fn move_stepping(
    b: Aabb,
    motion: [f64; 3],
    world: &dyn Collide,
    step: f64,
    on_ground: bool,
) -> ([f64; 3], bool) {
    let (_, moved) = slide(b, motion, world);
    let landed = motion[1] < 0.0 && moved[1] > motion[1] + EPSILON;
    let blocked = (moved[0] - motion[0]).abs() > EPSILON || (moved[2] - motion[2]).abs() > EPSILON;
    if step > 0.0 && blocked && (on_ground || landed) {
        // Up by the step, across, then back down onto what is there.
        let up = sweep(b, 1, step, world);
        let raised = b.shifted(1, up);
        let (across, side) = slide(raised, [motion[0], 0.0, motion[2]], world);
        let down = sweep(across, 1, -(up + EPSILON.max(-motion[1].min(0.0))), world);
        let total = [side[0], up + down, side[2]];
        if total[0] * total[0] + total[2] * total[2]
            > moved[0] * moved[0] + moved[2] * moved[2] + EPSILON
        {
            return (total, true);
        }
    }
    (moved, landed)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Floor(i32);
    impl Collide for Floor {
        fn solid(&self, _x: i32, y: i32, _z: i32) -> bool {
            y <= self.0
        }
    }

    #[test]
    fn lands_on_the_floor() {
        let b = Aabb {
            min: [0.01, 5.0, 0.01],
            max: [0.99, 5.98, 0.99],
        };
        let (moved, ground) = move_entity(b, [0.0, -10.0, 0.0], &Floor(0));
        assert!((moved[1] + 4.0).abs() < 1e-9, "{moved:?}");
        assert!(ground);
        let (moved, ground) = move_entity(b, [0.3, -0.5, 0.0], &Floor(0));
        assert_eq!((moved, ground), ([0.3, -0.5, 0.0], false));
    }

    #[test]
    fn steps_onto_a_slab_but_not_a_fence() {
        struct Step(f64);
        impl Collide for Step {
            fn solid(&self, x: i32, y: i32, _z: i32) -> bool {
                y < 0 || x >= 2 && y == 0
            }
            fn span(&self, x: i32, y: i32, z: i32) -> Option<(f64, f64)> {
                if x >= 2 && y == 0 {
                    return Some((0.0, self.0));
                }
                self.solid(x, y, z).then_some((0.0, 1.0))
            }
        }
        let b = Aabb {
            min: [1.0, 0.0, 0.0],
            max: [1.9, 0.9, 0.9],
        };
        let (moved, ground) = move_stepping(b, [0.3, -0.08, 0.0], &Step(0.5), 0.6, true);
        assert!(
            ground && (moved[1] - 0.5).abs() < 1e-6 && moved[0] > 0.29,
            "{moved:?}"
        );
        let (moved, _) = move_stepping(b, [0.3, -0.08, 0.0], &Step(1.5), 0.6, true);
        assert!(moved[0] < 0.11 && moved[1].abs() < 1e-6, "{moved:?}");
        // A fence's top reaches into the cell above: no walking over it.
        let high = Aabb {
            min: [1.0, 1.0, 0.0],
            max: [1.9, 1.9, 0.9],
        };
        let (moved, _) = move_entity(high, [0.3, 0.0, 0.0], &Step(1.5));
        assert!(moved[0] < 0.11, "{moved:?}");
    }

    #[test]
    fn walls_stop_sideways_motion() {
        struct Wall;
        impl Collide for Wall {
            fn solid(&self, x: i32, y: i32, _z: i32) -> bool {
                x >= 2 || y < 0
            }
        }
        let b = Aabb {
            min: [1.0, 0.0, 0.0],
            max: [1.5, 0.5, 0.5],
        };
        let (moved, _) = move_entity(b, [1.0, 0.0, 0.0], &Wall);
        assert!((moved[0] - 0.5).abs() < 1e-9, "{moved:?}");
    }
}
