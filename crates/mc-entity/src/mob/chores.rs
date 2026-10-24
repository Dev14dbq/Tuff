//! What mobs do with the blocks around them and the lead they are on
//! (minecraft.wiki, "Zombie", "Enderman", "Cat", "Fox", "Lead"):
//! - a zombie that may break doors bashes a closed wooden door it walks
//!   into, on Hard: the cracks grow over 240 ticks, then the door goes;
//! - an enderman picks up an `#enderman_holdable` block it sees close by
//!   (1 in 20 a tick) and sets it down again (1 in 2000), and teleports:
//!   at random in daylight, away from a player staring at it up close and
//!   towards its target when that is far;
//! - a tamed cat lies on beds and sits on chests and lit furnaces;
//! - foxes look for shade by day and sleep under cover;
//! - a mob on a lead keeps near whoever holds it and is pulled when it
//!   strays more than 6 blocks.

use super::body::params;
use super::{Body, Mob, MobEvent, Senses, Species, cell_of, dist_sq};
use crate::nav::{Cell, Liquid, Perch, standable};

/// Ticks a zombie bashes at a door.
pub(super) const DOOR_TIME: i32 = 240;

fn centre(c: Cell) -> [f64; 3] {
    [
        f64::from(c[0]) + 0.5,
        f64::from(c[1]) + 0.5,
        f64::from(c[2]) + 0.5,
    ]
}

/// Whether the door at `c` is opened by hand, and then whether it is open.
fn hand_door(s: &Senses, c: Cell) -> Option<bool> {
    s.world
        .door(c[0], c[1], c[2])
        .filter(|d| d.by_hand)
        .map(|d| d.open)
}

/// The closed wooden door a zombie on its way has run into: the upper half
/// of a door on its path within 1.5 blocks, or of one where it stands.
pub(super) fn door_ahead(m: &Mob, b: &Body, s: &Senses) -> Option<Cell> {
    let n = (m.nav.index + 2).min(m.nav.path.len());
    for c in &m.nav.path[..n] {
        let door = [c[0], c[1] + 1, c[2]];
        let d = (f64::from(door[0]) - b.pos[0]).powi(2) + (f64::from(door[2]) - b.pos[2]).powi(2);
        if d <= 2.25
            && let Some(open) = hand_door(s, door)
        {
            return (!open).then_some(door);
        }
    }
    let c = cell_of(b.pos);
    let door = [c[0], c[1] + 1, c[2]];
    (hand_door(s, door) == Some(false)).then_some(door)
}

/// Whether a zombie goes on bashing the door.
pub(super) fn door_holds(b: &Body, s: &Senses, door: Cell, time: i32) -> bool {
    time <= DOOR_TIME
        && s.difficulty >= 3
        && hand_door(s, door) == Some(false)
        && dist_sq(centre(door), b.pos) < 4.0
}

/// One tick of bashing a door: it shakes now and then, cracks, and goes.
pub(super) fn bash_door(
    m: &mut Mob,
    s: &Senses,
    door: Cell,
    time: &mut i32,
    stage: &mut i32,
    out: &mut Vec<MobEvent>,
) {
    if m.below(20) == 0 {
        out.push(MobEvent::DoorBash { cell: door });
        out.push(MobEvent::Swing);
    }
    *time += 1;
    let now = (*time as f32 / DOOR_TIME as f32 * 10.0) as i32;
    if now != *stage {
        *stage = now;
        out.push(MobEvent::DoorCrack {
            cell: door,
            stage: now,
        });
    }
    if *time == DOOR_TIME && s.difficulty >= 3 {
        out.push(MobEvent::BreakDoor { cell: door });
    }
}

/// The first block a line from `from` to the centre of `to` runs into,
/// if it is `to` itself.
fn sees_block(s: &Senses, from: [f64; 3], to: Cell) -> bool {
    let end = centre(to);
    let d = dist_sq(from, end).sqrt();
    let steps = (d * 8.0).ceil().max(1.0) as i32;
    for i in 0..=steps {
        let t = f64::from(i) / f64::from(steps);
        let c = cell_of([
            from[0] + (end[0] - from[0]) * t,
            from[1] + (end[1] - from[1]) * t,
            from[2] + (end[2] - from[2]) * t,
        ]);
        if c == to {
            return true;
        }
        if s.world.solid(c[0], c[1], c[2]) {
            return false;
        }
    }
    false
}

/// An enderman reaches for a random block near it.
pub(super) fn take_block(m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let x = (b.pos[0] - 2.0 + m.next_f64() * 4.0).floor() as i32;
    let y = (b.pos[1] + m.next_f64() * 3.0).floor() as i32;
    let z = (b.pos[2] - 2.0 + m.next_f64() * 4.0).floor() as i32;
    if !s.world.holdable(x, y, z) {
        return;
    }
    let from = [
        b.pos[0].floor() + 0.5,
        f64::from(y) + 0.5,
        b.pos[2].floor() + 0.5,
    ];
    if sees_block(s, from, [x, y, z]) {
        out.push(MobEvent::TakeBlock { cell: [x, y, z] });
    }
}

/// An enderman tries a spot next to it for the block it carries (the world
/// checks that the spot takes it).
pub(super) fn leave_block(m: &mut Mob, b: &Body, out: &mut Vec<MobEvent>) {
    let x = (b.pos[0] - 1.0 + m.next_f64() * 2.0).floor() as i32;
    let y = (b.pos[1] + m.next_f64() * 2.0).floor() as i32;
    let z = (b.pos[2] - 1.0 + m.next_f64() * 2.0).floor() as i32;
    out.push(MobEvent::PlaceBlock { cell: [x, y, z] });
}

/// Moves the mob to the ground under `to`, if it can stand there dry.
fn teleport_to(m: &mut Mob, b: &mut Body, s: &Senses, to: [f64; 3]) -> bool {
    let p = params(m);
    let c = cell_of(to);
    for y in (c[1] - 32..=c[1]).rev() {
        let cell = [c[0], y, c[2]];
        if s.world.solid(cell[0], y - 1, cell[2]) {
            if standable(s.world, cell, &p)
                && (0..p.height).all(|dy| s.world.liquid(c[0], y + dy, c[2]) == Liquid::None)
            {
                b.pos = [to[0], crate::nav::floor(s.world, cell), to[2]];
                b.vel = [0.0; 3];
                m.nav.stop();
                return true;
            }
            return false;
        }
    }
    false
}

/// An enderman teleports up to 32 blocks away.
pub(super) fn teleport_random(m: &mut Mob, b: &mut Body, s: &Senses) -> bool {
    let to = [
        b.pos[0] + (m.next_f64() - 0.5) * 64.0,
        b.pos[1] + f64::from(m.below(64) as i32 - 32),
        b.pos[2] + (m.next_f64() - 0.5) * 64.0,
    ];
    teleport_to(m, b, s, to)
}

/// An enderman teleports up to 16 blocks towards eyes at `eye`.
pub(super) fn teleport_towards(m: &mut Mob, b: &mut Body, s: &Senses, eye: [f64; 3]) -> bool {
    let mid = b.pos[1] + m.size().1 / 2.0;
    let v = [b.pos[0] - eye[0], mid - eye[1], b.pos[2] - eye[2]];
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-6 {
        return false;
    }
    let v = v.map(|c| c / len);
    let to = [
        b.pos[0] + (m.next_f64() - 0.5) * 8.0 - v[0] * 16.0,
        b.pos[1] + f64::from(m.below(16) as i32 - 8) - v[1] * 16.0,
        b.pos[2] + (m.next_f64() - 0.5) * 8.0 - v[2] * 16.0,
    ];
    teleport_to(m, b, s, to)
}

/// How bright light looks to a mob (vanilla's light-dependent value: 1 at
/// light 15, 0.5 at 12).
pub(super) fn brightness(light: u8) -> f64 {
    let f = f64::from(light) / 15.0;
    f / (4.0 - 3.0 * f)
}

/// An enderman out in daylight teleports away now and then.
pub(super) fn daylight_teleport(m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    if !s.day || m.target.is_some() && m.age < m.target_since + 600 {
        return;
    }
    let c = cell_of(b.pos);
    let eye = (b.pos[1] + m.eye_height()).floor() as i32;
    let f = brightness(s.world.light(c[0], eye, c[2]));
    if f > 0.5
        && s.world.sky_light(c[0], c[1], c[2]) >= 15
        && m.next_f64() * 30.0 < (f - 0.4) * 2.0
    {
        m.target = None;
        if teleport_random(m, b, s) {
            out.push(MobEvent::Teleported);
        }
    }
}

/// Whether a cat may settle on the block at `c`.
pub(super) fn perch_ok(s: &Senses, c: Cell, bed: bool) -> bool {
    let kind = s.world.perch(c[0], c[1], c[2]);
    let fits = if bed {
        kind == Perch::Bed
    } else {
        matches!(kind, Perch::Chest | Perch::Furnace | Perch::Bed)
    };
    fits && s.world.span(c[0], c[1] + 1, c[2]).is_none()
        && !s.world.solid(c[0], c[1] + 1, c[2])
}

/// The nearest block within 8 a cat could lie on (a bed) or sit on.
pub(super) fn find_perch(b: &Body, s: &Senses, bed: bool) -> Option<Cell> {
    let c = cell_of(b.pos);
    let mut best: Option<(f64, Cell)> = None;
    for dy in -1..=1 {
        for dx in -8..=8 {
            for dz in -8..=8 {
                let p = [c[0] + dx, c[1] + dy, c[2] + dz];
                if !perch_ok(s, p, bed) {
                    continue;
                }
                let d = dist_sq(centre(p), b.pos);
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, p));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Whether the mob has a roof over its head (no sky to be seen above).
pub(super) fn sheltered(m: &Mob, b: &Body, s: &Senses) -> bool {
    let c = cell_of(b.pos);
    let top = (b.pos[1] + m.size().1).floor() as i32;
    s.world.sky_light(c[0], top.max(c[1]), c[2]) < 15
}

/// A shady spot within 10 blocks for a fox in the sun: no sky over it and
/// dim (light 12 or less).
pub(super) fn shade(m: &mut Mob, b: &Body, s: &Senses) -> Option<Cell> {
    let from = cell_of(b.pos);
    let p = params(m);
    for _ in 0..10 {
        let c = [
            from[0] + m.below(21) as i32 - 10,
            from[1] + m.below(7) as i32 - 3,
            from[2] + m.below(21) as i32 - 10,
        ];
        if standable(s.world, c, &p)
            && s.world.sky_light(c[0], c[1], c[2]) < 15
            && brightness(s.world.light(c[0], c[1], c[2])) < 0.5
        {
            return Some(c);
        }
    }
    None
}

/// A lava cell near a strider out of lava to walk back to.
pub(super) fn lava(m: &mut Mob, b: &Body, s: &Senses) -> Option<Cell> {
    let from = cell_of(b.pos);
    let p = params(m);
    for _ in 0..40 {
        let c = [
            from[0] + m.below(17) as i32 - 8,
            from[1] + m.below(9) as i32 - 4,
            from[2] + m.below(17) as i32 - 8,
        ];
        if s.world.liquid(c[0], c[1], c[2]) == Liquid::Lava && standable(s.world, c, &p) {
            return Some(c);
        }
    }
    None
}

/// Whether someone a sleeping fox would wake for is near: a player or a
/// hostile mob within 12 blocks (6 up or down).
pub(super) fn fox_alerted(b: &Body, s: &Senses) -> bool {
    let near = |p: [f64; 3]| {
        (p[0] - b.pos[0]).abs() <= 12.0
            && (p[1] - b.pos[1]).abs() <= 6.0
            && (p[2] - b.pos[2]).abs() <= 12.0
    };
    s.players.iter().any(|p| p.targetable && near(p.pos))
        || s.mobs.iter().any(|o| {
            o.alive
                && o.id != b.id
                && o.species.is_some_and(|k| k.category() == super::Category::Monster)
                && near(o.pos)
        })
}

/// The pull of a lead held at `holder`: past 6 blocks the mob is dragged
/// along; closer, it walks up to 2 blocks from the holder (unless it panics).
pub(super) fn follow_lead(m: &mut Mob, b: &mut Body, holder: [f64; 3]) {
    let d = [
        holder[0] - b.pos[0],
        holder[1] - b.pos[1],
        holder[2] - b.pos[2],
    ];
    let f = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if f < 1e-6 {
        return;
    }
    if f > 6.0 {
        for (v, c) in b.vel.iter_mut().zip(d) {
            let k = c / f;
            *v += (k * k * 0.4).copysign(k);
        }
        m.goto = None;
        if m.fall_distance > 0.0 && b.vel[1] > -0.5 {
            m.fall_distance = 0.0;
        }
        return;
    }
    let panicking = m.revenge.is_some() && m.species.category() == super::Category::Creature;
    if panicking || m.species == Species::Parrot && m.sitting {
        return;
    }
    let k = (f - 2.0).max(0.0) / f;
    m.goto = (k > 0.0).then(|| ([b.pos[0] + d[0] * k, b.pos[1] + d[1] * k, b.pos[2] + d[2] * k], 1.0));
}
