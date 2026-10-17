//! Axis-aligned boxes moving through blocks.

/// What an entity can bump into.
pub trait Collide {
    /// Whether the block at these coordinates stops entities (a full cube).
    fn solid(&self, x: i32, y: i32, z: i32) -> bool;
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

/// How far a box can move along `axis` (signed `want`) before touching a
/// solid block.
fn sweep(b: Aabb, axis: usize, want: f64, world: &dyn Collide) -> f64 {
    if want == 0.0 {
        return 0.0;
    }
    let others: Vec<usize> = (0..3).filter(|&a| a != axis).collect();
    let range = |a: usize| (b.min[a].floor() as i32)..=((b.max[a] - EPSILON).floor() as i32);
    let mut allowed = want;
    // Every block layer the box would enter along the axis.
    let (start, end) = if want > 0.0 {
        (
            (b.max[axis] - EPSILON).floor() as i32 + 1,
            (b.max[axis] + want).floor() as i32,
        )
    } else {
        (
            (b.min[axis] + want).floor() as i32,
            b.min[axis].floor() as i32 - 1 + i32::from(b.min[axis].fract() > EPSILON),
        )
    };
    let layers: Box<dyn Iterator<Item = i32>> = if want > 0.0 {
        Box::new(start..=end)
    } else {
        Box::new((start..=end).rev())
    };
    for layer in layers {
        let mut hit = false;
        for u in range(others[0]) {
            for v in range(others[1]) {
                let mut c = [0i32; 3];
                c[axis] = layer;
                c[others[0]] = u;
                c[others[1]] = v;
                if world.solid(c[0], c[1], c[2]) {
                    hit = true;
                }
            }
        }
        if hit {
            allowed = if want > 0.0 {
                (f64::from(layer) - b.max[axis]).max(0.0).min(want)
            } else {
                (f64::from(layer + 1) - b.min[axis]).min(0.0).max(want)
            };
            break;
        }
    }
    allowed
}

/// Moves a box by `motion` through blocks: vertical first, then the larger
/// horizontal axis, then the other. Returns the movement done and whether the
/// box landed on something.
pub fn move_entity(b: Aabb, motion: [f64; 3], world: &dyn Collide) -> ([f64; 3], bool) {
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
    let on_ground = motion[1] < 0.0 && moved[1] > motion[1] + EPSILON;
    (moved, on_ground)
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
