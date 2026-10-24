//! Flying mobs (flying-mobs): the bat's roost, the allay's errand, the
//! breeze's wind charges and bounds, the shulker's shell and the creaking's
//! stillness (minecraft.wiki, the page of each mob).

use super::goals::{eye, look_at, random_air, target};
use super::{
    Body, Missile, Mob, MobEvent, PLAYER_EYE, Senses, Species, cell_of, dist_sq, line_clear,
};

/// What [`super::goals::Goal::Air`] is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// An allay stays by the player it was given an item by.
    Follow,
    /// Goes where the server sent it (`Air::errand`): a bee to a flower or
    /// its hive.
    Errand,
    /// A breeze fires a wind charge; `timer` counts to the next one.
    Shoot { timer: i32 },
    /// A breeze crouches and leaps; `timer` counts the windup.
    Bounce { timer: i32 },
    /// A shulker peeks and fires bullets; `left` counts to the next one.
    Peek { left: i32 },
    /// A creaking stands still while a player looks at it.
    Freeze,
    /// A bat hangs from the ceiling above `spot` until disturbed.
    Roost { spot: Option<[i32; 3]> },
}

/// Extra state on [`super::Quirks`].
#[derive(Clone, Debug, PartialEq)]
pub struct Air {
    /// Bat: hanging from a block.
    pub hanging: bool,
    /// Allay: the player it works for, and whether it dances.
    pub liked: Option<u128>,
    pub dancing: bool,
    /// Allay: ticks until it may split again.
    pub dup_cd: i32,
    /// Allay: ticks to wait before it looks for items again.
    pub errand_cd: i32,
    /// Shulker: how far its shell is open (0 closed .. 100), face it sits on
    /// (vanilla direction ids; 0 is down) and the dye colour (16 none).
    pub peek: i32,
    pub face: u8,
    pub color: u8,
    /// Creaking: frozen now, awake, and its heart's cell.
    pub frozen: bool,
    pub active: bool,
    pub home: Option<[i32; 3]>,
    /// Bogged: the mushrooms are gone.
    pub sheared: bool,
    /// Breeze: ticks to the next wind charge.
    pub charge_cd: i32,
    /// Bee: carries nectar, has stung, its hive and the flower it works.
    pub nectar: bool,
    pub stung: bool,
    pub hive: Option<[i32; 3]>,
    pub flower: Option<[i32; 3]>,
    /// Bee: ticks it may not enter a hive, ticks since it pollinated, crops
    /// it grew since, ticks circling the flower, ticks since it stung.
    pub cannot_enter: i32,
    pub since_pollination: i32,
    pub crops: i32,
    pub circling: i32,
    pub sting_age: i32,
    /// Copper golem: weathering 0 (unaffected) to 3 (oxidized), the game
    /// time of the next step (-1: not set, -2: waxed) and its pose state.
    pub weather: u8,
    pub next_weather: i64,
    pub state: u8,
    /// Happy ghast: ticks it stays still.
    pub still: i32,
    /// Allay: what it carries (item, count), where it is bound for (set by
    /// the server each tick) and the note block it hears, with the game
    /// time it stops listening.
    pub carried: Option<(i32, i32)>,
    pub errand: Option<[f64; 3]>,
    pub note: Option<([i32; 3], i64)>,
    /// Copper golem: chests it found nothing in (at most nine), the failed
    /// looks since it last put something down, ticks it waits (looking at a
    /// chest, or wandering after ten failures) and the chest it is at.
    pub memory: Vec<[i32; 3]>,
    pub checks: i32,
    pub wait: i32,
    pub at_chest: Option<[i32; 3]>,
    /// Sulfur cube: the archetype of the block it holds (index plus one, 0
    /// for none), the ticks of its fuse (-1 unlit), the ticks before it
    /// swallows again and whether its growing up is stopped.
    pub archetype: u8,
    pub fuse: i32,
    pub pickup_timer: i32,
    pub locked: bool,
}

impl Default for Air {
    fn default() -> Self {
        Self {
            hanging: false,
            liked: None,
            dancing: false,
            dup_cd: 0,
            errand_cd: 0,
            peek: 0,
            face: 0,
            color: 16,
            frozen: false,
            active: false,
            home: None,
            sheared: false,
            charge_cd: 0,
            nectar: false,
            stung: false,
            hive: None,
            flower: None,
            cannot_enter: 0,
            since_pollination: 0,
            crops: 0,
            circling: 0,
            sting_age: 0,
            weather: 0,
            next_weather: -1,
            state: 0,
            still: 0,
            carried: None,
            errand: None,
            note: None,
            memory: Vec::new(),
            checks: 0,
            wait: 0,
            at_chest: None,
            archetype: 0,
            fuse: -1,
            pickup_timer: 0,
            locked: false,
        }
    }
}

/// Whether some player who could be hurt by a creaking looks at the mob
/// within 60 degrees and with nothing in the way.
pub fn watched(m: &Mob, b: &Body, s: &Senses) -> bool {
    let at = [b.pos[0], b.pos[1] + m.eye_height(), b.pos[2]];
    s.players.iter().any(|p| {
        if !p.targetable {
            return false;
        }
        let from = [p.pos[0], p.pos[1] + PLAYER_EYE, p.pos[2]];
        let d = [at[0] - from[0], at[1] - from[1], at[2] - from[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if !(1e-6..=32.0).contains(&len) {
            return false;
        }
        let (yaw, pitch) = (
            f64::from(p.yaw).to_radians(),
            f64::from(p.pitch).to_radians(),
        );
        let look = [
            -yaw.sin() * pitch.cos(),
            -pitch.sin(),
            yaw.cos() * pitch.cos(),
        ];
        let cos = (look[0] * d[0] + look[1] * d[1] + look[2] * d[2]) / len;
        cos > 60f64.to_radians().cos() && line_clear(s.world, from, at)
    })
}

/// Per-tick upkeep outside the goals.
pub(super) fn pulse(m: &mut Mob, b: &Body, s: &Senses, _out: &mut Vec<MobEvent>) {
    let air = &mut m.quirks.air;
    air.dup_cd = (air.dup_cd - 1).max(0);
    air.errand_cd = (air.errand_cd - 1).max(0);
    air.charge_cd = (air.charge_cd - 1).max(0);
    if m.species == Species::Creaking {
        let frozen = watched(m, b, s);
        let air = &mut m.quirks.air;
        if air.frozen != frozen {
            air.frozen = frozen;
            m.dirty = true;
        }
    }
}

pub(super) fn can_use(act: &mut Act, m: &mut Mob, b: &Body, s: &Senses) -> bool {
    if m.is_dead() {
        return false;
    }
    match act {
        Act::Follow => liked(m, s).is_some(),
        Act::Errand => m.quirks.air.errand.is_some(),
        Act::Shoot { .. } => target(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) < 16.0 * 16.0),
        Act::Bounce { .. } => b.on_ground && target(m, s).is_some(),
        Act::Peek { .. } => true,
        Act::Freeze => m.quirks.air.frozen,
        Act::Roost { spot } => {
            if s.players.iter().any(|p| dist_sq(p.pos, b.pos) < 16.0) || m.below(80) != 0 {
                return false;
            }
            let c = cell_of(b.pos);
            let up = [c[0], c[1] + 1, c[2]];
            if s.world.solid(up[0], up[1], up[2]) {
                *spot = Some(up);
                return true;
            }
            false
        }
    }
}

pub(super) fn can_continue(act: &Act, m: &Mob, b: &Body, s: &Senses) -> bool {
    match act {
        Act::Follow => liked(m, s).is_some(),
        Act::Errand => m.quirks.air.errand.is_some(),
        Act::Shoot { .. } | Act::Bounce { .. } => m.target.is_some() && !m.is_dead(),
        Act::Peek { .. } => !m.is_dead(),
        Act::Freeze => m.quirks.air.frozen,
        Act::Roost { spot } => {
            !s.players.iter().any(|p| dist_sq(p.pos, b.pos) < 16.0)
                && spot.is_some_and(|c| s.world.solid(c[0], c[1], c[2]))
        }
    }
}

pub(super) fn start(act: &mut Act, m: &mut Mob, _out: &mut Vec<MobEvent>) {
    match act {
        Act::Shoot { timer } => *timer = 32,
        Act::Bounce { timer } => *timer = 0,
        Act::Roost { .. } => {
            m.quirks.air.hanging = true;
            m.dirty = true;
        }
        _ => {}
    }
}

pub(super) fn stop(act: &Act, m: &mut Mob) {
    match act {
        Act::Roost { .. } => {
            m.quirks.air.hanging = false;
            m.dirty = true;
        }
        Act::Freeze => {
            m.goto = None;
            m.nav.stop();
        }
        _ => {}
    }
}

fn liked<'a>(m: &Mob, s: &'a Senses) -> Option<&'a super::Target> {
    let uuid = m.quirks.air.liked?;
    s.players.iter().find(|p| p.uuid == uuid && p.present)
}

pub(super) fn tick(act: &mut Act, m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match act {
        Act::Follow => {
            let Some(p) = liked(m, s) else { return };
            if let Some(at) = m.quirks.air.errand {
                m.goto = Some((at, 1.0));
                return;
            }
            let d = dist_sq(b.pos, p.pos);
            if d > 16.0 {
                m.goto = Some(([p.pos[0], p.pos[1] + 1.0, p.pos[2]], 1.0));
            } else if d < 4.0 {
                m.nav.stop();
                m.goto = None;
            }
        }
        Act::Errand => {
            if let Some(at) = m.quirks.air.errand {
                m.goto = Some((at, 1.0));
            }
        }
        Act::Shoot { timer } => {
            let Some(t) = target(m, s) else { return };
            look_at(m, &t);
            *timer -= 1;
            if *timer <= 0 && line_clear(s.world, eye(m, b.pos), super::goals::eye_of(&t)) {
                *timer = 32;
                out.push(MobEvent::Shoot {
                    target: t.id,
                    aim: super::goals::eye_of(&t),
                    missile: Missile::WindCharge,
                });
            }
        }
        Act::Bounce { timer } => {
            let Some(t) = target(m, s) else { return };
            if !b.on_ground {
                return;
            }
            *timer += 1;
            m.nav.stop();
            m.goto = None;
            if *timer >= 20 {
                *timer = -(40 + m.below(40) as i32);
                let d = [t.pos[0] - b.pos[0], t.pos[2] - b.pos[2]];
                let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
                let h = (len * 0.25).clamp(0.2, 0.9);
                b.vel = [
                    d[0] / len * h,
                    0.7 + ((t.pos[1] - b.pos[1]) * 0.05).clamp(0.0, 0.4),
                    d[1] / len * h,
                ];
                b.on_ground = false;
                out.push(MobEvent::Sound {
                    name: "entity.breeze.jump".into(),
                    volume: 1.0,
                    pitch: 1.0,
                });
            }
        }
        Act::Peek { left } => peek(m, b, s, left, out),
        Act::Freeze => {
            m.goto = None;
            m.nav.stop();
            b.vel[0] = 0.0;
            b.vel[2] = 0.0;
        }
        Act::Roost { .. } => {
            m.goto = None;
            m.nav.stop();
            b.vel = [0.0; 3];
        }
    }
}

/// The shulker's shell: open while a target is within 16 blocks (and
/// sometimes at random), shut otherwise; a bullet every 1 to 5.5 seconds.
fn peek(m: &mut Mob, b: &Body, s: &Senses, left: &mut i32, out: &mut Vec<MobEvent>) {
    m.goto = None;
    m.nav.stop();
    let t = target(m, s).filter(|t| dist_sq(b.pos, t.pos) < 16.0 * 16.0);
    let want = if t.is_some() { 100 } else { 0 };
    let air = &mut m.quirks.air;
    let before = air.peek;
    air.peek += (want - air.peek).clamp(-5, 5);
    if air.peek != before {
        m.dirty = true;
    }
    let Some(t) = t else {
        *left = 20;
        return;
    };
    look_at(m, &t);
    if m.quirks.air.peek < 100 {
        return;
    }
    *left -= 1;
    if *left <= 0 {
        *left = 20 + m.below(100) as i32;
        out.push(MobEvent::Shoot {
            target: t.id,
            aim: super::goals::eye_of(&t),
            missile: Missile::ShulkerBullet,
        });
    }
}

/// Moves a shulker to another spot within 8 blocks of where it is: a cell
/// of air with a full block under it. Whether it found one.
pub fn shulker_teleport(entity: &mut crate::Entity, world: &dyn crate::nav::Terrain) -> bool {
    let crate::Kind::Mob(m) = &mut entity.kind else {
        return false;
    };
    let c = cell_of(entity.pos);
    for _ in 0..5 {
        let x = c[0] + m.below(17) as i32 - 8;
        let y = c[1] + m.below(17) as i32 - 8;
        let z = c[2] + m.below(17) as i32 - 8;
        if world.solid(x, y, z) || world.solid(x, y + 1, z) || !world.solid(x, y - 1, z) {
            continue;
        }
        entity.pos = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
        m.quirks.air.face = 0;
        m.quirks.air.peek = 0;
        m.dirty = true;
        return true;
    }
    false
}

/// A flyer's random spot, for tests and wanderers.
pub fn wander(m: &mut Mob, b: &Body, s: &Senses, h: f64, v: f64) -> Option<[f64; 3]> {
    random_air(m, s, b.pos, h, v)
}
