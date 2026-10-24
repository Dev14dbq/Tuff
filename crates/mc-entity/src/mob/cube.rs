//! Sulfur cubes (minecraft.wiki, "Sulfur Cube"; the values are those of the
//! `sulfur_cube_archetype` data): a large one that swallows a full block
//! stops hopping and becomes a ball of that block's archetype. Hit, it
//! takes no harm but flies away from the blow; it bounces off the ground
//! and the walls by its archetype's bounciness and slides by its friction.

use super::{Body, Mob, MobEvent, Species};

/// What a block does to the cube that holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Archetype {
    pub name: &'static str,
    pub bounce: f64,
    pub friction: f64,
    pub drag: f64,
    pub push_h: f64,
    pub push_v: f64,
    /// TNT: lit, it goes off after 120 ticks with power 3.
    pub explosive: bool,
    /// Magma: whoever touches it takes 1 damage.
    pub hot: bool,
}

const fn a(
    name: &'static str,
    bounce: f64,
    friction: f64,
    drag: f64,
    push_h: f64,
    push_v: f64,
) -> Archetype {
    Archetype {
        name,
        bounce,
        friction,
        drag,
        push_h,
        push_v,
        explosive: false,
        hot: false,
    }
}

/// The twelve archetypes; a mob holds an index plus one (0: no block).
pub const ARCHETYPES: [Archetype; 12] = [
    a("bouncy", 0.9, -0.7, -0.99, 0.4125, 0.105),
    Archetype {
        explosive: true,
        ..a("explosive", 0.5, -0.7, -0.7, 0.4125, 0.09)
    },
    a("fast_flat", 0.5, -0.8, -0.99, 0.9125, 0.09),
    a("fast_sliding", 0.1, -0.95, -0.99, 0.6625, 0.09),
    a("high_resistance", 0.2, 0.0, -0.99, 0.4125, 0.09),
    Archetype {
        hot: true,
        ..a("hot", 0.5, -0.7, -0.9, 0.4125, 0.09)
    },
    a("light", 1.0, -0.7, 0.8, 0.4125, 0.18),
    a("regular", 0.5, -0.7, -0.9, 0.4125, 0.09),
    a("slow_bouncy", 0.6, -0.7, -0.95, 0.4125, 0.24),
    a("slow_flat", 0.4, -0.6, -0.9, 0.4125, 0.105),
    a("slow_sliding", 0.1, -0.95, -0.99, 0.4125, 0.09),
    a("sticky", 0.0, 1.0, -0.99, 0.4125, 0.09),
];

/// The index plus one of an archetype by name.
pub fn index_of(name: &str) -> Option<u8> {
    ARCHETYPES
        .iter()
        .position(|a| a.name == name)
        .map(|i| i as u8 + 1)
}

/// The archetype a cube holds, if it holds a block.
pub fn of(m: &Mob) -> Option<&'static Archetype> {
    if m.species != Species::SulfurCube {
        return None;
    }
    let i = usize::from(m.quirks.air.archetype);
    (i > 0).then(|| &ARCHETYPES[i - 1])
}

/// Ticks of an explosive cube's fuse.
pub const FUSE: i32 = 120;

/// Flies off from a blow at `from` (a point or an entity): the push of its
/// archetype, harder for a harder blow and for Knockback.
pub fn launch(m: &Mob, vel: &mut [f64; 3], pos: [f64; 3], from: [f64; 3], damage: f32, extra: f64) {
    let Some(a) = of(m) else { return };
    let (dx, dz) = (pos[0] - from[0], pos[2] - from[2]);
    let len = dx.hypot(dz).max(1e-4);
    let power = 1.0 + f64::from(damage) / 4.0 + extra * 0.5;
    vel[0] += dx / len * a.push_h * power;
    vel[2] += dz / len * a.push_h * power;
    vel[1] += a.push_v * power + 0.1;
}

/// One tick of a cube that holds a block: it does not think, only rolls.
pub(super) fn roll(m: &mut Mob, b: &mut Body, s: &super::Senses, out: &mut Vec<MobEvent>) {
    let Some(a) = of(m).copied() else { return };
    let before = b.vel;
    let was_ground = b.on_ground;
    super::body::physics(m, b, s, None, out);
    // Off the walls and the ground.
    for axis in [0, 2] {
        if before[axis].abs() > 0.05 && b.vel[axis].abs() < before[axis].abs() * 0.1 {
            b.vel[axis] = -before[axis] * a.bounce;
        }
    }
    if b.on_ground && !was_ground && before[1] < -0.12 {
        b.vel[1] = -before[1] * a.bounce;
        b.on_ground = a.bounce < 0.05;
    }
    // Sliding: the default ground friction is 0.546 a tick.
    if b.on_ground {
        let mine = (1.0 - 0.454 * (1.0 + a.friction)).clamp(0.05, 0.995);
        let k = mine / 0.546;
        b.vel[0] *= k;
        b.vel[2] *= k;
    } else {
        let k = (1.0 - 0.01 * (1.0 + a.drag).max(0.0)).clamp(0.9, 1.0);
        b.vel[0] *= k;
        b.vel[2] *= k;
    }
    if a.explosive && m.quirks.air.fuse >= 0 {
        m.quirks.air.fuse -= 1;
        if m.quirks.air.fuse < 0 {
            out.push(MobEvent::Explode { power: 3.0 });
        }
    }
}
