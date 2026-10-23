//! A mob's body: what happens to it every tick whatever it thinks (growing
//! up, burning, sounds, conversions), turning goals into movement, and the
//! movement itself (walking, swimming, flying).

use super::{Body, Mob, MobEvent, Senses, Sound, Species, cell_of, slot};
use crate::nav::{Liquid, PathParams};
use crate::physics::{Aabb, move_stepping};

pub(super) const GRAVITY: f64 = 0.08;
/// Blocks a tick per point of the speed attribute, for fliers.
const SPEED_SCALE: f64 = 0.9;
/// A walker's ground speed in blocks a tick is this times the square of
/// its speed (the attribute times the goal's modifier): vanilla feeds the
/// speed in both as the input and as the acceleration. A zombie (0.23)
/// walks 0.116 blocks a tick, 2.3 m/s; a cow (0.2) 0.088.
const GROUND: f64 = 2.2;
/// What a walker steps up onto without jumping (slabs, paths, carpets).
const STEP: f64 = 0.6;

pub(super) fn wrap_degrees(a: f32) -> f32 {
    let a = a % 360.0;
    if a > 180.0 {
        a - 360.0
    } else if a < -180.0 {
        a + 360.0
    } else {
        a
    }
}

pub(super) fn approach(from: f32, to: f32, max: f32) -> f32 {
    from + wrap_degrees(to - from).clamp(-max, max)
}

pub(super) fn yaw_to(dx: f64, dz: f64) -> f32 {
    (-dx).atan2(dz).to_degrees() as f32
}

pub(super) fn params(m: &Mob) -> PathParams {
    let mut p = PathParams::walker(m.size().1.ceil() as i32);
    if m.species.fire_immune() {
        p.water_cost = 8.0;
    }
    p
}

fn head_in_water(m: &Mob, b: &Body, s: &Senses) -> bool {
    let eye = b.pos[1] + m.eye_height();
    s.world.liquid(
        b.pos[0].floor() as i32,
        eye.floor() as i32,
        b.pos[2].floor() as i32,
    ) == Liquid::Water
}

pub(super) fn in_water(b: &Body, s: &Senses) -> bool {
    let c = cell_of(b.pos);
    s.world.liquid(c[0], c[1], c[2]) == Liquid::Water
        || s.world.liquid(c[0], (b.pos[1] + 0.4).floor() as i32, c[2]) == Liquid::Water
}

/// What happens every tick whatever the mob wants.
pub(super) fn life(m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    // Growing up, and the wait between babies.
    if m.baby_age < 0 {
        m.baby_age += 1;
        if m.baby_age == 0 {
            m.dirty = true;
        }
    } else if m.baby_age > 0 {
        m.baby_age -= 1;
    }
    if m.love > 0 {
        m.love -= 1;
        if m.love % 10 == 0 {
            out.push(MobEvent::Status(18));
        }
    }
    if m.anger > 0 {
        m.anger -= 1;
        if m.anger == 0 {
            m.dirty = true;
            if m.species != Species::Enderman {
                m.target = None;
            }
        }
    }
    let wet = in_water(b, s);
    // Sun and fire.
    let c = cell_of(b.pos);
    if m.species.burns_in_sun() {
        let head = [c[0], (b.pos[1] + m.size().1).floor() as i32, c[2]];
        if s.sunny && !wet && s.world.sky_light(head[0], head[1], head[2]) >= 15 {
            if m.gear[slot::HEAD].is_some() {
                // A helmet takes the sun instead, and wears.
                if m.below(2) == 0 && m.age.is_multiple_of(20) {
                    out.push(MobEvent::Wear {
                        slot: slot::HEAD,
                        amount: 1,
                    });
                }
            } else {
                m.burning = m.burning.max(160);
            }
        }
    }
    if m.burning > 0 {
        if wet || m.species.fire_immune() {
            if wet {
                m.burning = 0;
            } else {
                m.burning -= 1;
            }
        } else {
            m.burning -= 1;
            if m.age.is_multiple_of(20) {
                out.push(MobEvent::Hurt {
                    amount: 1.0,
                    kind: "minecraft:on_fire",
                });
            }
        }
    }
    if s.world.liquid(c[0], c[1], c[2]) == Liquid::Lava && !m.species.fire_immune() {
        m.burning = m.burning.max(300);
        out.push(MobEvent::Hurt {
            amount: 4.0,
            kind: "minecraft:lava",
        });
    }
    // Endermen hate water.
    if m.species == Species::Enderman
        && (wet || s.raining && sky_above(m, b, s))
        && m.age.is_multiple_of(10)
    {
        out.push(MobEvent::Hurt {
            amount: 1.0,
            kind: "minecraft:drown",
        });
    }
    // Chickens lay eggs.
    if m.species == Species::Chicken && !m.is_baby() {
        m.egg_time -= 1;
        if m.egg_time <= 0 {
            m.egg_time = m.rand_range(6000, 12000);
            let pitch = (m.next_f64() - m.next_f64()) as f32 * 0.2 + 1.0;
            out.push(MobEvent::Sound {
                name: "entity.chicken.egg".into(),
                volume: 1.0,
                pitch,
            });
            out.push(MobEvent::Lay {
                item: "minecraft:egg",
            });
        }
    }
    // Creepers.
    if m.species == Species::Creeper {
        let before = m.swell;
        if m.swell_dir > 0 || m.ignited {
            if m.swell == 0 {
                out.push(MobEvent::Sound {
                    name: "entity.creeper.primed".into(),
                    volume: 1.0,
                    pitch: 0.5,
                });
            }
            m.swell += 1;
        } else {
            m.swell = (m.swell - 1).max(0);
        }
        if m.swell != before && (before == 0 || m.swell == 0) {
            m.dirty = true;
        }
        if m.swell >= 30 {
            m.swell = 30;
            out.push(MobEvent::Explode {
                power: if m.powered { 6.0 } else { 3.0 },
            });
            return;
        }
    }
    conversions(m, b, s, out);
    // Witches drink what they need.
    if m.species == Species::Witch {
        if m.drinking > 0 {
            m.drinking -= 1;
            if m.drinking == 0 {
                m.gear[slot::MAINHAND] = None;
                m.dirty = true;
                if m.health < m.max_health {
                    out.push(MobEvent::Heal(4.0));
                }
                if m.burning > 0 {
                    m.burning = 0;
                }
            }
        } else if m.below(10) == 0
            && (m.health < m.max_health * 0.5 && m.below(5) == 0 || m.burning > 0)
        {
            m.drinking = 32;
            m.dirty = true;
            out.push(MobEvent::Sound {
                name: "entity.witch.drink".into(),
                volume: 1.0,
                pitch: 0.8 + m.next_f64() as f32 * 0.4,
            });
        }
    }
    // Ambient sounds now and then.
    let interval = m.species.ambient_interval();
    m.ambient_timer += 1;
    if (m.below(1000) as i32) < m.ambient_timer {
        m.ambient_timer = -interval;
        if let Some(e) = m.sound(Sound::Ambient) {
            out.push(e);
        }
    }
    // Horses test their rider.
    if m.species.horse() && !m.tame && !m.riders.is_empty() && m.below(50) == 0 {
        if (m.below(100) as i32) < m.temper {
            m.tame = true;
            m.dirty = true;
            out.push(MobEvent::Status(7));
        } else {
            m.temper = (m.temper + 5).min(100);
            out.push(MobEvent::Status(6));
            out.push(MobEvent::Buck);
            out.push(MobEvent::Sound {
                name: format!("entity.{}.angry", m.species.path()),
                volume: 1.0,
                pitch: 1.0,
            });
        }
    }
    if m.boost > 0 {
        m.boost -= 1;
    }
    if in_wall(m, b, s) {
        out.push(MobEvent::Hurt {
            amount: 1.0,
            kind: "minecraft:in_wall",
        });
    }
}

/// Whether the mob's eyes are inside a block (it suffocates): a box a
/// little narrower than its body at eye height, as vanilla's.
pub(super) fn in_wall(m: &Mob, b: &Body, s: &Senses) -> bool {
    let half = m.size().0 * 0.4;
    let eye = b.pos[1] + m.eye_height();
    let y = eye.floor() as i32;
    let xs = [
        (b.pos[0] - half).floor() as i32,
        (b.pos[0] + half).floor() as i32,
    ];
    let zs = [
        (b.pos[2] - half).floor() as i32,
        (b.pos[2] + half).floor() as i32,
    ];
    xs.iter().any(|&x| {
        zs.iter().any(|&z| {
            [y, y - 1].into_iter().any(|cy| {
                s.world.span(x, cy, z).is_some_and(|(bottom, top)| {
                    let (lo, hi) = (f64::from(cy) + bottom, f64::from(cy) + top);
                    // Fences and the like do not smother.
                    top - bottom >= 1.0 && top <= 1.0 && lo < eye && eye < hi
                })
            })
        })
    })
}

fn sky_above(m: &Mob, b: &Body, s: &Senses) -> bool {
    let c = cell_of(b.pos);
    s.world
        .sky_light(c[0], (b.pos[1] + m.size().1).floor() as i32, c[2])
        >= 15
}

/// Zombies drown into drowned, husks soak into zombies, piglins rot out of
/// the Nether, zombie villagers are cured.
fn conversions(m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match m.species {
        Species::Zombie | Species::Husk => {
            if m.conversion >= 0 {
                m.conversion -= 1;
                if m.conversion < 0 {
                    out.push(MobEvent::Convert(if m.species == Species::Husk {
                        Species::Zombie
                    } else {
                        Species::Drowned
                    }));
                }
            } else if head_in_water(m, b, s) {
                m.soaked += 1;
                if m.soaked >= 600 {
                    m.conversion = 300;
                    m.dirty = true;
                }
            } else {
                m.soaked = -1;
            }
        }
        Species::ZombieVillager if m.conversion >= 0 => {
            m.conversion -= 1;
            if m.conversion < 0 {
                out.push(MobEvent::Cured);
            }
        }
        Species::Piglin if !s.nether && !m.no_ai => {
            m.soaked += 1;
            if m.soaked > 300 {
                out.push(MobEvent::Sound {
                    name: "entity.piglin.converted_to_zombified".into(),
                    volume: 1.0,
                    pitch: 1.0,
                });
                out.push(MobEvent::Convert(Species::ZombifiedPiglin));
            }
        }
        _ => {}
    }
}

/// Turns where the mob wants to go into movement for this tick: a heading
/// and a speed, plus a height for those that swim or fly.
pub(super) struct Input {
    pub dir: [f64; 3],
    pub speed: f64,
}

pub(super) fn drive(m: &mut Mob, b: &mut Body, s: &Senses) -> Option<Input> {
    m.nav.tick(b.pos, s.tick as u32);
    let (point, speed) = m.goto.take()?;
    // Fliers and swimmers head straight for the point.
    let swims = m.species == Species::Drowned && in_water(b, s);
    if m.species.flies() || swims {
        let d = [
            point[0] - b.pos[0],
            point[1] - b.pos[1],
            point[2] - b.pos[2],
        ];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if len < 0.1 {
            return None;
        }
        return Some(Input {
            dir: [d[0] / len, d[1] / len, d[2] / len],
            speed,
        });
    }
    let cell = cell_of(point);
    // Standing on a slab or a path is standing in the cell above the block.
    let from = if b.on_ground {
        cell_of([b.pos[0], b.pos[1] + 0.5, b.pos[2]])
    } else {
        cell_of(b.pos)
    };
    // Stuck against something for two seconds: give the walk up.
    if m.nav.stuck >= 2 {
        m.nav.stop();
        m.nav.stuck = 0;
        return None;
    }
    let p = params(m);
    m.nav.go_to(s.world, from, cell, &p);
    let reached = (m.size().0 / 2.0 + 0.2).max(0.4);
    let (tx, ty, tz) = match m.nav.waypoint(b.pos, reached) {
        Some(c) => (
            f64::from(c[0]) + 0.5,
            crate::nav::floor(s.world, c),
            f64::from(c[2]) + 0.5,
        ),
        None => {
            // No path (or at its end): walk straight when the spot is near.
            if (point[0] - b.pos[0]).abs() + (point[2] - b.pos[2]).abs() > 3.0 {
                return None;
            }
            (point[0], point[1], point[2])
        }
    };
    let (dx, dz) = (tx - b.pos[0], tz - b.pos[2]);
    let len = (dx * dx + dz * dz).sqrt();
    if len < 0.05 {
        return None;
    }
    // Jump only for what is too high to step onto, once close to it.
    if ty > b.pos[1] + STEP && b.on_ground && len < 1.0_f64.max(m.size().0) {
        m.jump = true;
    }
    Some(Input {
        dir: [dx / len, 0.0, dz / len],
        speed,
    })
}

/// One tick of movement.
pub(super) fn physics(
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    input: Option<Input>,
    out: &mut Vec<MobEvent>,
) {
    let (w, h) = m.size();
    let cell = cell_of(b.pos);
    let wet = in_water(b, s);
    let in_lava = s.world.liquid(cell[0], cell[1], cell[2]) == Liquid::Lava;
    let speed_attr = m.speed_attr * SPEED_SCALE;
    let flies = m.species.flies() && !m.sitting;
    if flies {
        // Fliers: no gravity, a slow drift towards where they go.
        match &input {
            Some(i) => {
                let want = speed_attr
                    * i.speed
                    * if m.species == Species::Ghast {
                        0.15
                    } else {
                        0.5
                    };
                for (axis, d) in [0, 1, 2].into_iter().zip(i.dir) {
                    b.vel[axis] += (d * want - b.vel[axis]) * 0.1;
                }
                m.yaw = approach(m.yaw, yaw_to(i.dir[0], i.dir[2]), 20.0);
            }
            None => {
                for v in &mut b.vel {
                    *v *= 0.91;
                }
            }
        }
        // A parrot without anywhere to go settles down.
        if m.species == Species::Parrot && input.is_none() && !b.on_ground {
            b.vel[1] -= 0.02;
        }
    } else {
        if in_lava {
            b.vel[1] = b.vel[1] * 0.5 + 0.02;
        }
        // Horizontal velocity.
        let friction = if b.on_ground { 0.546 } else { 0.91 };
        match &input {
            Some(i) => {
                let speed = m.speed_attr * i.speed;
                let want = speed * speed * GROUND;
                let control = if b.on_ground || wet { 0.5 } else { 0.1 };
                b.vel[0] += (i.dir[0] * want - b.vel[0]) * control;
                b.vel[2] += (i.dir[2] * want - b.vel[2]) * control;
                m.yaw = approach(m.yaw, yaw_to(i.dir[0], i.dir[2]), 30.0);
            }
            None => {
                b.vel[0] *= friction;
                b.vel[2] *= friction;
            }
        }
        // Vertical.
        if wet {
            b.vel[0] *= 0.8;
            b.vel[2] *= 0.8;
            let swims = m.species == Species::Drowned;
            match (&input, swims) {
                (Some(i), true) => {
                    b.vel[1] += (i.dir[1] * speed_attr * i.speed - b.vel[1]) * 0.2;
                }
                _ => {
                    b.vel[1] = b.vel[1] * 0.8 - 0.02;
                    if m.jump {
                        b.vel[1] += 0.04;
                    }
                }
            }
            m.fall_distance = 0.0;
        } else {
            if m.jump && b.on_ground {
                b.vel[1] = 0.42;
            }
        }
    }
    // Gravity pulls after the move, as in vanilla (a jump rises 1.25).
    let falls = !flies && !wet;
    m.jump = false;
    let before = b.pos;
    let aabb = Aabb {
        min: [b.pos[0] - w / 2.0, b.pos[1], b.pos[2] - w / 2.0],
        max: [b.pos[0] + w / 2.0, b.pos[1] + h, b.pos[2] + w / 2.0],
    };
    let step = if flies { 0.0 } else { STEP };
    let (moved, ground) = move_stepping(aabb, b.vel, s.world, step, b.on_ground);
    let blocked_h = (moved[0] - b.vel[0]).abs() > 1e-9 || (moved[2] - b.vel[2]).abs() > 1e-9;
    for (axis, &d) in moved.iter().enumerate() {
        b.pos[axis] += d;
        if (d - b.vel[axis]).abs() > 1e-9 {
            b.vel[axis] = 0.0;
        }
    }
    if falls {
        b.vel[1] = (b.vel[1] - GRAVITY) * 0.98;
        // A chicken flaps: it falls slowly.
        if m.species == Species::Chicken && b.vel[1] < -0.1 {
            b.vel[1] = -0.1;
        }
    }
    let climbs = matches!(m.species, Species::Spider | Species::CaveSpider);
    let was_climbing = m.climbing;
    m.climbing = climbs && blocked_h;
    if m.climbing != was_climbing {
        m.dirty = true;
    }
    if m.climbing {
        b.vel[1] = b.vel[1].max(0.2);
    } else if blocked_h && s.world.climbable(cell[0], cell[1], cell[2]) {
        b.vel[1] = 0.2;
    }
    b.on_ground = ground;
    // Steps.
    if ground && !wet {
        let walked = (b.pos[0] - before[0]).hypot(b.pos[2] - before[2]);
        m.walked += walked;
        if m.walked > 1.6 {
            m.walked = 0.0;
            if let Some(e) = m.sound(Sound::Step) {
                out.push(e);
            }
        }
    }
    if flies {
        m.fall_distance = 0.0;
    } else if !ground && b.pos[1] < before[1] {
        m.fall_distance += before[1] - b.pos[1];
    } else if ground {
        let fell = m.fall_distance;
        m.fall_distance = 0.0;
        let safe = m.species.safe_fall();
        if fell > safe && m.species.takes_fall_damage() {
            out.push(MobEvent::Hurt {
                amount: (fell - safe).ceil() as f32,
                kind: "minecraft:fall",
            });
        }
    }
    if m.climbing || wet {
        m.fall_distance = 0.0;
    }
}

/// Where it looks: at the goal's point, else the way it goes.
pub(super) fn turn_head(m: &mut Mob, b: &Body) {
    let eye_pos = [b.pos[0], b.pos[1] + m.eye_height(), b.pos[2]];
    if let Some(p) = m.look {
        let (dx, dy, dz) = (p[0] - eye_pos[0], p[1] - eye_pos[1], p[2] - eye_pos[2]);
        let flat = (dx * dx + dz * dz).sqrt();
        m.head_yaw = approach(m.head_yaw, yaw_to(dx, dz), 30.0);
        m.pitch = approach(m.pitch, (-dy).atan2(flat).to_degrees() as f32, 20.0);
        // The body follows the head when it turns far.
        let off = wrap_degrees(m.head_yaw - m.yaw);
        if off.abs() > 50.0 || m.species.flies() {
            m.yaw = approach(m.yaw, m.head_yaw, 10.0);
        }
    } else {
        m.head_yaw = approach(m.head_yaw, m.yaw, 10.0);
        m.pitch = approach(m.pitch, 0.0, 10.0);
    }
}
