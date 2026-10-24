//! Land animals (Cursor): goats. A goat rams one target it can see, and
//! long-jumps a wall, a hole or powder snow (minecraft.wiki, "Goat").

use super::body::params;
use super::{Body, Mob, MobEvent, PLAYER_EYE, Senses, Species, cell_of, dist_sq, line_clear};
use crate::nav::{Feature, standable};

/// What [`super::goals::Goal::Land`] is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Lowers its head, then leaps. `left` is the windup.
    LongJump { left: i32, spot: Option<[f64; 3]> },
    /// Stomps, then charges a locked spot. `phase` counts the windup.
    Ram {
        phase: i32,
        at: Option<[f64; 3]>,
        who: Option<i32>,
    },
}

/// Extra state on [`super::Quirks`]. Horns start present; a summoned goat
/// is not screaming.
#[derive(Clone, Debug, PartialEq)]
pub struct Land {
    pub screaming: bool,
    pub left_horn: bool,
    pub right_horn: bool,
    /// Ticks until it may ram again.
    pub ram_cd: i32,
    /// Ticks until it may long-jump again.
    pub jump_cd: i32,
    /// Long-jump velocity, applied once on the next grounded tick.
    pub leap_vy: f64,
    pub leap_vx: f64,
    pub leap_vz: f64,
    /// Shrunk hitbox and the long-jump pose.
    pub jumping: bool,
    /// Horn style 0–3, or 255 until the first horn picks one.
    pub horn_group: u8,
    /// The ram connected or missed: shed the charge speed.
    pub brake: bool,
}

impl Default for Land {
    fn default() -> Self {
        Self {
            screaming: false,
            left_horn: true,
            right_horn: true,
            ram_cd: 0,
            jump_cd: 0,
            leap_vy: 0.0,
            leap_vx: 0.0,
            leap_vz: 0.0,
            jumping: false,
            horn_group: 255,
            brake: false,
        }
    }
}

pub(super) fn pulse(m: &mut Mob) {
    if m.species != Species::Goat {
        return;
    }
    let land = &mut m.quirks.land;
    if land.ram_cd > 0 {
        land.ram_cd -= 1;
    }
    if land.jump_cd > 0 {
        land.jump_cd -= 1;
    }
}

pub(super) fn can_use(act: &mut Act, m: &mut Mob, b: &Body, s: &Senses) -> bool {
    if m.species != Species::Goat || m.is_dead() {
        return false;
    }
    match act {
        Act::LongJump { spot, .. } => {
            if !b.on_ground || m.quirks.land.jumping {
                return false;
            }
            let stuck = in_powder(s, b.pos);
            if m.quirks.land.jump_cd > 0 && !stuck {
                return false;
            }
            let Some(p) = jump_spot(m, b, s, stuck) else {
                return false;
            };
            *spot = Some(p);
            true
        }
        Act::Ram { who, .. } => {
            if m.quirks.land.ram_cd > 0 || !b.on_ground {
                return false;
            }
            let Some(id) = ram_target(m, b, s) else {
                return false;
            };
            *who = Some(id);
            true
        }
    }
}

pub(super) fn can_continue(act: &Act, m: &Mob) -> bool {
    match act {
        Act::LongJump { left, spot } => *left > 0 && spot.is_some() && !m.is_dead(),
        Act::Ram { phase, .. } => *phase < 80 && !m.is_dead(),
    }
}

pub(super) fn start(act: &mut Act, m: &mut Mob, out: &mut Vec<MobEvent>) {
    match act {
        Act::LongJump { left, .. } => *left = 20,
        Act::Ram { phase, .. } => {
            *phase = 0;
            // The wait starts now, so wheat cancelling the windup still
            // counts as this attempt. A screamer rams every 5–15s; the
            // others every 30s–5min.
            m.quirks.land.ram_cd = if m.quirks.land.screaming {
                m.rand_range(100, 300)
            } else {
                m.rand_range(600, 6000)
            };
            out.push(voice(m, "prepare_ram", 0.8));
        }
    }
}

pub(super) fn stop(act: &Act, m: &mut Mob) {
    if matches!(act, Act::Ram { .. }) {
        m.goto = None;
        m.nav.stop();
        m.quirks.land.brake = true;
    }
}

pub(super) fn tick(act: &mut Act, m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match act {
        Act::LongJump { left, spot } => {
            let Some(spot) = *spot else {
                *left = 0;
                return;
            };
            m.look = Some(spot);
            *left -= 1;
            if *left > 0 {
                return;
            }
            launch(m, b.pos, spot, out);
        }
        Act::Ram { phase, at, who } => tick_ram(phase, at, who, m, b, s, out),
    }
}

fn launch(m: &mut Mob, from: [f64; 3], to: [f64; 3], out: &mut Vec<MobEvent>) {
    let (vy, vx, vz) = leap_velocity(from, to);
    m.quirks.land.leap_vy = vy;
    m.quirks.land.leap_vx = vx;
    m.quirks.land.leap_vz = vz;
    m.quirks.land.jumping = true;
    m.quirks.land.jump_cd = m.rand_range(600, 1200);
    m.jump = true;
    m.dirty = true;
    m.goto = None;
    out.push(voice(m, "long_jump", 0.8));
}

/// Ballistic hop: gravity 0.08 and the 0.98 drag, high enough for the
/// blocks of rise and far enough for the horizontal gap.
fn leap_velocity(from: [f64; 3], to: [f64; 3]) -> (f64, f64, f64) {
    let dx = to[0] - from[0];
    let dz = to[2] - from[2];
    let dy = (to[1] - from[1]).clamp(-5.0, 10.0);
    let horizontal = dx.hypot(dz).max(0.01);
    let vy = (0.42 + dy.max(0.0) * 0.115).clamp(0.42, 1.55);
    let ticks = 8.0 + dy.max(0.0) * 1.4;
    let speed = (horizontal / ticks).clamp(0.05, 0.55);
    (vy, dx / horizontal * speed, dz / horizontal * speed)
}

fn tick_ram(
    phase: &mut i32,
    at: &mut Option<[f64; 3]>,
    who: &mut Option<i32>,
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    out: &mut Vec<MobEvent>,
) {
    *phase += 1;
    let Some(id) = *who else {
        *phase = 80;
        return;
    };
    if *phase < 20 {
        if let Some(t) = s.seen(id) {
            m.look = Some([t.pos[0], t.pos[1] + t.eye, t.pos[2]]);
        }
        return;
    }
    if *phase == 20 {
        // Locked here, so a target that moved during the windup is missed.
        *at = s.seen(id).map(|t| t.pos);
        if at.is_none() {
            *phase = 80;
            return;
        }
    }
    let Some(spot) = *at else {
        *phase = 80;
        return;
    };
    m.look = Some(spot);
    m.goto = Some((spot, 2.5));
    if s.seen(id).is_some_and(|t| near(b.pos, t.pos)) {
        let hard = s.difficulty >= 3;
        let damage = match (m.is_baby(), hard) {
            (false, false) => 2.0,
            (false, true) => 3.0,
            (true, false) => 1.0,
            (true, true) => 1.5,
        };
        // `knockback` levels on top of the basic 0.4: 3.2 and 1.2 make a
        // shove of strength 2 and 1, about nine blocks and four and a half.
        let knockback = if m.is_baby() { 1.2 } else { 3.2 };
        out.push(MobEvent::Ram {
            target: id,
            damage,
            knockback,
        });
        out.push(voice(m, "ram_impact", 0.8));
        end_ram(phase, m);
        return;
    }
    if m.bumped {
        if !m.is_baby() && (m.quirks.land.left_horn || m.quirks.land.right_horn) && horn_block(m, b, s)
        {
            out.push(MobEvent::Horn);
        }
        end_ram(phase, m);
        return;
    }
    if near(b.pos, spot) || *phase > 50 {
        end_ram(phase, m);
    }
}

fn end_ram(phase: &mut i32, m: &mut Mob) {
    *phase = 80;
    m.goto = None;
    m.nav.stop();
    m.quirks.land.brake = true;
}

fn near(a: [f64; 3], b: [f64; 3]) -> bool {
    let (dx, dz) = (a[0] - b[0], a[2] - b[2]);
    dx * dx + dz * dz < 2.25 && (a[1] - b[1]).abs() < 2.0
}

fn voice(m: &mut Mob, suffix: &str, volume: f32) -> MobEvent {
    let base = if m.quirks.land.screaming {
        "entity.goat.screaming"
    } else {
        "entity.goat"
    };
    MobEvent::Sound {
        name: format!("{base}.{suffix}"),
        volume,
        pitch: m.voice_pitch(),
    }
}

/// A player (not in creative, and not on peaceful) or any mob but a goat
/// or a ghast, 4 to 16 blocks away and in sight. The charge then locks
/// the spot, which is how a moving target gets out of the way.
fn ram_target(m: &Mob, b: &Body, s: &Senses) -> Option<i32> {
    let eye = [b.pos[0], b.pos[1] + m.eye_height(), b.pos[2]];
    let mut best: Option<(i32, f64)> = None;
    let mut consider = |id: i32, pos: [f64; 3], eye_y: f64| {
        let d = dist_sq(b.pos, pos);
        if !(16.0..=256.0).contains(&d) || best.is_some_and(|(_, bd)| d >= bd) {
            return;
        }
        let at = [pos[0], eye_y, pos[2]];
        if line_clear(s.world, eye, at) {
            best = Some((id, d));
        }
    };
    for o in s.mobs {
        if !o.alive || o.id == b.id {
            continue;
        }
        if matches!(o.species, Some(Species::Goat | Species::Ghast)) {
            continue;
        }
        consider(o.id, o.pos, o.pos[1] + o.height * 0.85);
    }
    if s.difficulty > 0 {
        for p in s.players {
            if p.targetable {
                consider(p.id, p.pos, p.pos[1] + PLAYER_EYE);
            }
        }
    }
    best.map(|(id, _)| id)
}

fn horn_block(m: &Mob, b: &Body, s: &Senses) -> bool {
    let yaw = m.yaw.to_radians();
    let reach = m.size().0 * 0.5 + 0.51;
    let x = b.pos[0] - yaw.sin() * reach;
    let z = b.pos[2] + yaw.cos() * reach;
    let (fx, fz) = (x.floor() as i32, z.floor() as i32);
    let fy = b.pos[1].floor() as i32;
    s.world.feature(fx, fy, fz, Feature::HornSnap)
        || s.world.feature(fx, fy + 1, fz, Feature::HornSnap)
}

fn in_powder(s: &Senses, pos: [f64; 3]) -> bool {
    let c = cell_of(pos);
    s.world.feature(c[0], c[1], c[2], Feature::PowderSnow)
        || s.world
            .feature(c[0], (pos[1] + 0.4).floor() as i32, c[2], Feature::PowderSnow)
}

fn powder_at(s: &Senses, x: i32, y: i32, z: i32) -> bool {
    s.world.feature(x, y, z, Feature::PowderSnow)
}

/// A landing up to 10 blocks up and 5 sideways, and never more than 5 down.
/// Pathfinding already steps one block and drops three, so this is for a
/// real obstacle: a tall step, a deep hole, or powder snow.
fn jump_spot(m: &Mob, b: &Body, s: &Senses, escape: bool) -> Option<[f64; 3]> {
    let here = cell_of(b.pos);
    let p = params(m);
    let yaw = m.yaw.to_radians();
    for turn in [0.0_f32, 0.7, -0.7, 1.5, -1.5, std::f32::consts::PI] {
        let (sin, cos) = (yaw + turn).sin_cos();
        let (dx, dz) = (-f64::from(sin), f64::from(cos));
        for dist in 1..=5 {
            let tx = here[0] + (dx * f64::from(dist)).round() as i32;
            let tz = here[2] + (dz * f64::from(dist)).round() as i32;
            if tx == here[0] && tz == here[2] {
                continue;
            }
            let Some(dy) = (-5..=10)
                .rev()
                .find(|&dy| standable(s.world, [tx, here[1] + dy, tz], &p))
            else {
                continue;
            };
            let snow = (1..=dist).any(|step| {
                let mx = here[0] + (dx * f64::from(step)).round() as i32;
                let mz = here[2] + (dz * f64::from(step)).round() as i32;
                powder_at(s, mx, here[1], mz) || powder_at(s, mx, here[1] - 1, mz)
            });
            if escape || dy >= 2 || snow || (-5..-3).contains(&dy) {
                return Some([
                    f64::from(tx) + 0.5,
                    f64::from(here[1] + dy),
                    f64::from(tz) + 0.5,
                ]);
            }
        }
    }
    None
}
