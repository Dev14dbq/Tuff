//! Aquatic mobs (Claude): fish, squids, dolphins, turtles, axolotls, frogs,
//! tadpoles, guardians and nautiluses (minecraft.wiki, the page of each
//! mob). What they share: they swim in three dimensions where there is
//! water, and the ones that live only in water flop, dry out or choke on
//! land.

use super::{Body, Mob, MobEvent, Other, Senses, Sound, Species, cell_of, dist_sq, line_clear};
use crate::nav::{Liquid, Terrain};

/// Ticks of air a fish has out of water before it chokes (and the 20 more
/// the damage takes).
const FISH_AIR: i32 = 300;
/// Ticks a dolphin stays moist out of water.
const DOLPHIN_MOISTURE: i32 = 2400;
/// Ticks of air a dolphin has under water.
const DOLPHIN_AIR: i32 = 4800;
/// Ticks of air an axolotl has out of water.
const AXOLOTL_AIR: i32 = 6000;
/// Ticks a tadpole takes to grow into a frog.
pub const TADPOLE_GROWTH: i32 = 24000;
/// Ticks of a played dead axolotl.
pub const PLAY_DEAD: i32 = 200;
/// An axolotl waits this long after eating something that did not mean
/// harm.
const HUNT_WAIT: i32 = 2400;

impl Species {
    /// Lives in the water only: it swims where there is water and is
    /// stranded without it.
    pub fn aquatic(self) -> bool {
        use Species::*;
        matches!(
            self,
            Cod | Salmon
                | Pufferfish
                | TropicalFish
                | Squid
                | GlowSquid
                | Dolphin
                | Tadpole
                | Guardian
                | ElderGuardian
                | Nautilus
                | ZombieNautilus
        )
    }

    /// Walks on land and swims all the same.
    pub fn amphibious(self) -> bool {
        matches!(self, Species::Turtle | Species::Frog | Species::Axolotl)
    }

    /// Whether it moves through water in three dimensions (the drowned do
    /// too, in their own way: see the body).
    pub fn swims(self) -> bool {
        self.aquatic() || self.amphibious()
    }

    /// Fish that swim in schools.
    pub fn schooling(self) -> bool {
        matches!(self, Species::Cod | Species::Salmon | Species::TropicalFish)
    }

    /// Blocks a tick it swims at a goal speed of 1.
    pub fn swim_pace(self) -> f64 {
        use Species::*;
        match self {
            Cod | Salmon | TropicalFish => 0.12,
            Pufferfish => 0.08,
            Squid | GlowSquid => 0.1,
            Dolphin => 0.22,
            Turtle => 0.1,
            Axolotl | Frog => 0.1,
            Tadpole => 0.06,
            Guardian => 0.14,
            ElderGuardian => 0.1,
            Nautilus => 0.12,
            ZombieNautilus => 0.13,
            _ => 0.1,
        }
    }

    /// The pace (as the attribute) it walks at on land, where that is
    /// not its speed attribute.
    pub fn land_speed(self) -> Option<f64> {
        use Species::*;
        Some(match self {
            Turtle => 0.125,
            Frog | Axolotl => 0.1,
            Nautilus | ZombieNautilus => 0.15,
            _ => return None,
        })
    }
}

/// Everything the aquatic kinds keep on a mob.
#[derive(Clone, Debug, PartialEq)]
pub struct Aqua {
    /// Whether it was in water last tick.
    pub wet: bool,
    /// Air (or, for a dolphin, moisture) left.
    pub air: i32,
    /// Dolphins: the breath held under water.
    pub breath: i32,
    /// Picked up in a bucket once (it never despawns).
    pub from_bucket: bool,
    /// Pufferfish: 0 small, 1 half, 2 full; the counters of both ways.
    pub puff: u8,
    pub inflate: i32,
    pub deflate: i32,
    /// Glow squid: ticks it is dark after a hit.
    pub dark: i32,
    /// Squid: where it swims (blocks a tick) and for how long.
    pub drift: [f64; 3],
    /// Squid and glow squid: squirt ink on the next tick.
    pub ink: bool,
    /// Dolphin: fed a fish, and the treasure it was sent for.
    pub got_fish: bool,
    pub treasure: Option<[i32; 3]>,
    /// Turtle: home beach, carrying an egg, ticks of laying.
    pub home: Option<[i32; 3]>,
    pub has_egg: bool,
    pub laying: i32,
    /// Frog and turtle: pregnant but will lay spawn (frog) or an egg.
    pub pregnant: bool,
    /// Axolotl: ticks left of playing dead, and the wait before hunting
    /// something harmless again.
    pub dead_play: i32,
    pub hunt_wait: i32,
    /// Frog: ticks into the tongue (0: not out), and whom it caught.
    pub tongue: i32,
    pub caught: Option<i32>,
    /// Tadpole: ticks lived.
    pub growth: i32,
    /// Guardian: ticks charging the beam, whom it aims at (entity id, 0
    /// for none), and the spikes pulled in.
    pub charge: i32,
    pub beam: i32,
    pub retract: bool,
    /// Elder guardian: ticks to its next curse.
    pub curse_in: i32,
    /// Nautilus: ticks before it may dash again, and a dash under way.
    pub dash_cd: i32,
    pub dashing: i32,
    /// Ticks since it last moved on land (a stranded fish flops).
    pub still: i32,
}

impl Default for Aqua {
    fn default() -> Self {
        Self {
            wet: true,
            air: FISH_AIR,
            breath: DOLPHIN_AIR,
            from_bucket: false,
            puff: 0,
            inflate: 0,
            deflate: 0,
            dark: 0,
            drift: [0.0; 3],
            ink: false,
            got_fish: false,
            treasure: None,
            home: None,
            has_egg: false,
            laying: 0,
            pregnant: false,
            dead_play: 0,
            hunt_wait: 0,
            tongue: 0,
            caught: None,
            growth: 0,
            charge: 0,
            beam: 0,
            retract: false,
            curse_in: 0,
            dash_cd: 0,
            dashing: 0,
            still: 0,
        }
    }
}

/// Sets what a new mob of the kind starts out with.
pub(super) fn init(m: &mut Mob) {
    match m.species {
        Species::Salmon => {
            // 31.6% small, 52.6% medium, 15.8% large.
            let roll = m.below(190);
            m.variant = if roll < 60 {
                0
            } else if roll < 160 {
                1
            } else {
                2
            };
        }
        Species::TropicalFish => m.variant = random_tropical(m),
        Species::Dolphin => m.quirks.aqua.air = DOLPHIN_MOISTURE,
        Species::Axolotl => {
            m.quirks.aqua.air = AXOLOTL_AIR;
            // Four colours in the wild (blue comes from breeding only).
            m.variant = m.below(4) as i32;
        }
        Species::ElderGuardian => m.quirks.aqua.curse_in = 1200,
        Species::Frog | Species::Turtle => {}
        _ => {}
    }
    if let Some(speed) = m.species.land_speed() {
        m.speed_attr = speed;
    }
}

/// The common tropical fish (minecraft.wiki, "Tropical Fish"), as the
/// variant number.
pub const TROPICAL_KINDS: [i32; 22] = [
    65536, 459008, 917504, 918273, 918529, 16778497, 50660352, 50726144, 67108865, 67110144,
    67371009, 67699456, 67764993, 101253888, 117441025, 117441793, 117506305, 117899265, 118161664,
    185008129, 234882305, 235340288,
];

/// A wild tropical fish: nine in ten are one of the common kinds.
fn random_tropical(m: &mut Mob) -> i32 {
    if m.below(10) < 9 {
        return TROPICAL_KINDS[m.below(22) as usize];
    }
    let shape = m.below(2) as i32;
    let pattern = m.below(6) as i32;
    let base = m.below(16) as i32;
    let color = m.below(16) as i32;
    shape | (pattern << 8) | (base << 16) | (color << 24)
}

/// A pufferfish's size by how puffed it is.
pub fn puff_size(state: u8) -> (f64, f64) {
    match state {
        0 => (0.35, 0.35),
        1 => (0.49, 0.49),
        _ => (0.7, 0.7),
    }
}

/// A salmon's size by kind: small, medium, large.
pub fn salmon_size(kind: i32) -> (f64, f64) {
    match kind {
        0 => (0.35, 0.2),
        2 => (1.05, 0.6),
        _ => (0.7, 0.4),
    }
}

/// How fast it walks on land compared to the attribute it has.
pub(super) fn land_factor(m: &Mob) -> f64 {
    match m.species {
        // The attribute is already set to the walking pace.
        _ => 1.0,
    }
}

/// Blocks a tick it swims at a goal speed of 1, with its effects.
pub(super) fn pace(m: &Mob) -> f64 {
    let base = m.species.swim_pace();
    let attr = if m.speed_attr > 0.0 {
        m.speed_attr
    } else {
        1.0
    };
    let own = m.species.land_speed().unwrap_or_else(|| m.species.speed());
    let scale = if own > 0.0 { attr / own } else { 1.0 };
    // Effects (speed, slowness) scale the attribute: carry them over.
    let effects = m.movement_speed() / attr.max(1e-9);
    base * scale.max(0.0) * effects
}

/// One tick of swimming in water: the velocity eases towards the way it
/// goes.
pub(super) fn swim_physics(m: &mut Mob, b: &mut Body, i: Option<&super::body::Input>) {
    match i {
        Some(i) => {
            let pace = pace(m) * i.speed;
            for axis in 0..3 {
                b.vel[axis] += (i.dir[axis] * pace - b.vel[axis]) * 0.2;
            }
            m.yaw = super::body::approach(m.yaw, super::body::yaw_to(i.dir[0], i.dir[2]), 30.0);
        }
        None => {
            for v in &mut b.vel {
                *v *= 0.9;
            }
        }
    }
    m.fall_distance = 0.0;
}

pub(super) fn water(w: &dyn Terrain, c: [i32; 3]) -> bool {
    w.liquid(c[0], c[1], c[2]) == Liquid::Water
}

/// Whether the whole way from one point to another is water.
pub(super) fn line_water(w: &dyn Terrain, a: [f64; 3], b: [f64; 3]) -> bool {
    let d = dist_sq(a, b).sqrt();
    let steps = (d * 2.0).ceil().max(1.0) as i32;
    (1..=steps).all(|i| {
        let t = f64::from(i) / f64::from(steps);
        let c = cell_of([
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]);
        water(w, c) && !w.solid(c[0], c[1], c[2])
    })
}

/// A spot of water within `h` blocks sideways and `v` up or down.
pub(super) fn water_spot(m: &mut Mob, b: &Body, s: &Senses, h: i32, v: i32) -> Option<[f64; 3]> {
    let here = cell_of(b.pos);
    for _ in 0..10 {
        let d = [
            m.below((2 * h + 1) as u32) as i32 - h,
            m.below((2 * v + 1) as u32) as i32 - v,
            m.below((2 * h + 1) as u32) as i32 - h,
        ];
        let c = [here[0] + d[0], here[1] + d[1], here[2] + d[2]];
        let p = [
            f64::from(c[0]) + 0.5,
            f64::from(c[1]) + 0.5,
            f64::from(c[2]) + 0.5,
        ];
        if water(s.world, c) && line_water(s.world, b.pos, p) {
            return Some(p);
        }
    }
    None
}

/// A spot of water away from `from`, a stride long.
fn water_away(m: &mut Mob, b: &Body, s: &Senses, from: [f64; 3], reach: f64) -> Option<[f64; 3]> {
    let (dx, dz) = (b.pos[0] - from[0], b.pos[2] - from[2]);
    let len = dx.hypot(dz).max(1e-6);
    for _ in 0..10 {
        let jitter = m.next_f64() * 2.0 - 1.0;
        let jitter_y = m.next_f64() * 2.0 - 1.0;
        let p = [
            b.pos[0] + dx / len * reach - dz / len * jitter * 3.0,
            b.pos[1] + jitter_y * 3.0 + 0.2,
            b.pos[2] + dz / len * reach + dx / len * jitter * 3.0,
        ];
        let c = cell_of(p);
        if water(s.world, c) && line_water(s.world, b.pos, p) {
            return Some(p);
        }
    }
    None
}

/// What the aquatic goals are doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Swims to a random spot of water; when `scared`, only after being
    /// hurt, and then away.
    Swim {
        speed: f64,
        interval: i32,
        h: i32,
        v: i32,
        scared: bool,
        to: Option<[f64; 3]>,
        left: i32,
    },
    /// Fish keep away from players.
    Flee {
        range: f64,
        walk: f64,
        sprint: f64,
        to: Option<[f64; 3]>,
        from: Option<i32>,
    },
    /// Swims after the school's leader (the lowest id near).
    School { leader: Option<i32> },
    /// A squid drifts the way it has set, and bolts when hurt.
    Squid { time: i32 },
    /// A dolphin goes after a swimming player or one in a boat.
    Escort { player: Option<i32>, grace: i32 },
    /// A dolphin leads on to the treasure it was sent for.
    Treasure { left: i32 },
    /// Goes up for air.
    Breathe { to: Option<[f64; 3]> },
    /// A turtle goes back to the beach it was born on.
    Home { to: Option<[i32; 3]>, left: i32 },
    /// A turtle digs a nest in the sand where it stands.
    LayEgg { left: i32 },
    /// A turtle or frog on land makes for water.
    ToWater { to: Option<[f64; 3]>, left: i32 },
    /// A frog lays frogspawn on water.
    LaySpawn { to: Option<[i32; 3]>, left: i32 },
    /// Chooses whom to hunt among some kinds.
    Hunt {
        kinds: &'static [Species],
        hostile: &'static [Species],
        small: bool,
        wait: bool,
    },
    /// A frog's tongue.
    Tongue { range: f64 },
    /// A guardian's beam.
    Beam { duration: i32, hold: f64 },
    /// A nautilus bolts at its target.
    Dash { speed: f64 },
}

pub(super) fn can_use(act: &mut Act, m: &mut Mob, b: &mut Body, s: &Senses) -> bool {
    if m.is_dead() {
        return false;
    }
    let wet = in_water(b, s);
    match act {
        Act::Swim {
            interval,
            h,
            v,
            scared,
            to,
            ..
        } => {
            if !wet {
                return false;
            }
            if *scared {
                let Some((by, _)) = m.revenge else {
                    return false;
                };
                let from = s
                    .player(by)
                    .map(|p| p.pos)
                    .or_else(|| s.mob(by).map(|o| o.pos))
                    .unwrap_or([b.pos[0] + m.next_f64() - 0.5, b.pos[1], b.pos[2]]);
                *to = water_away(m, b, s, from, 8.0);
                return to.is_some();
            }
            if m.below(*interval as u32) != 0 {
                return false;
            }
            *to = water_spot(m, b, s, *h, *v);
            to.is_some()
        }
        Act::Flee {
            range, to, from, ..
        } => {
            if !wet {
                return false;
            }
            let Some(p) = s.nearest(b.pos, *range, |p| p.present) else {
                return false;
            };
            *from = Some(p.id);
            *to = water_away(m, b, s, p.pos, 8.0);
            to.is_some()
        }
        Act::School { leader } => {
            if !wet {
                return false;
            }
            let found = school_leader(m, b, s);
            match found {
                Some(o) if dist_sq(b.pos, o.pos) > 9.0 => {
                    *leader = Some(o.id);
                    true
                }
                _ => false,
            }
        }
        Act::Squid { .. } => wet,
        Act::Escort { player, .. } => {
            if !wet || m.is_baby() {
                return false;
            }
            let found = s
                .nearest(b.pos, 15.0, |p| p.present && (p.swimming || p.boating))
                .map(|p| p.id);
            *player = found;
            found.is_some()
        }
        Act::Treasure { .. } => m.quirks.aqua.treasure.is_some() && wet,
        Act::Breathe { to } => {
            if !wet || m.quirks.aqua.breath >= 300 {
                return false;
            }
            *to = Some([b.pos[0], b.pos[1] + 8.0, b.pos[2]]);
            true
        }
        Act::Home { to, .. } => {
            let a = &m.quirks.aqua;
            let Some(home) = a.home else {
                return false;
            };
            // It carries an egg home, or wanders back now and then.
            let far = dist_sq(
                b.pos,
                [
                    f64::from(home[0]) + 0.5,
                    f64::from(home[1]),
                    f64::from(home[2]) + 0.5,
                ],
            ) > 81.0;
            if m.is_baby() || !(a.has_egg && (a.laying == 0) || m.below(700) == 0 && far) {
                return false;
            }
            *to = Some(home);
            far
        }
        Act::LayEgg { .. } => {
            let a = &m.quirks.aqua;
            let Some(home) = a.home else {
                return false;
            };
            let near = dist_sq(
                b.pos,
                [
                    f64::from(home[0]) + 0.5,
                    f64::from(home[1]),
                    f64::from(home[2]) + 0.5,
                ],
            ) < 81.0;
            a.has_egg && near && b.on_ground && sand_under(s.world, cell_of(b.pos))
        }
        Act::ToWater { to, .. } => {
            if wet || !b.on_ground || m.is_baby() && m.below(5) != 0 {
                return false;
            }
            if m.species == Species::Turtle && m.quirks.aqua.has_egg {
                return false;
            }
            *to = nearest_water(s, b.pos, 24);
            to.is_some()
        }
        Act::LaySpawn { to, .. } => {
            if !m.quirks.aqua.pregnant || m.species != Species::Frog {
                return false;
            }
            *to = frogspawn_spot(s, b.pos, 8);
            to.is_some()
        }
        Act::Hunt {
            kinds,
            hostile,
            small,
            wait,
        } => hunt(m, b, s, kinds, hostile, *small, *wait),
        Act::Tongue { .. } => m.target.is_some() && m.quirks.aqua.tongue == 0,
        Act::Beam { .. } => m.target.is_some() && wet_or_guardian(m, wet),
        Act::Dash { .. } => m.target.is_some() && m.quirks.aqua.dash_cd == 0 && wet,
    }
}

/// Guardians shoot on land too.
fn wet_or_guardian(m: &Mob, wet: bool) -> bool {
    wet || matches!(m.species, Species::Guardian | Species::ElderGuardian)
}

pub(super) fn in_water(b: &Body, s: &Senses) -> bool {
    super::body::in_water(b, s)
}

/// The school's leader: the nearest of the kind with a lower id.
fn school_leader<'a>(m: &Mob, b: &Body, s: &'a Senses) -> Option<&'a Other> {
    let me = b.id;
    s.nearest_mob(b.pos, 12.0, |o| {
        o.id < me && o.species == Some(m.species) && !o.baby
    })
}

/// The nearest cell of water on foot, within `range` blocks sideways
/// (looking at ground level and one block either way).
fn nearest_water(s: &Senses, pos: [f64; 3], range: i32) -> Option<[f64; 3]> {
    let c = cell_of(pos);
    let mut best: Option<(i32, [f64; 3])> = None;
    for dx in -range..=range {
        for dz in -range..=range {
            let d = dx * dx + dz * dz;
            if d > range * range || best.is_some_and(|(bd, _)| bd <= d) {
                continue;
            }
            for dy in -1..=1 {
                let cell = [c[0] + dx, c[1] + dy, c[2] + dz];
                if water(s.world, cell) && !s.world.solid(cell[0], cell[1], cell[2]) {
                    best = Some((
                        d,
                        [
                            f64::from(cell[0]) + 0.5,
                            f64::from(cell[1]) + 0.5,
                            f64::from(cell[2]) + 0.5,
                        ],
                    ));
                    break;
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// A water cell with air above it near `pos`: where frogspawn floats.
fn frogspawn_spot(s: &Senses, pos: [f64; 3], range: i32) -> Option<[i32; 3]> {
    let c = cell_of(pos);
    let mut best: Option<(i32, [i32; 3])> = None;
    for dx in -range..=range {
        for dz in -range..=range {
            for dy in -2..=2 {
                let below = [c[0] + dx, c[1] + dy, c[2] + dz];
                let above = [below[0], below[1] + 1, below[2]];
                if water(s.world, below)
                    && !water(s.world, above)
                    && !s.world.solid(above[0], above[1], above[2])
                {
                    let d = dx * dx + dz * dz + dy * dy;
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, above));
                    }
                }
            }
        }
    }
    best.map(|(_, c)| c)
}

/// Picks something to hunt. Whatever means harm comes first; the rest only
/// when it has not hunted for a while.
fn hunt(
    m: &mut Mob,
    b: &Body,
    s: &Senses,
    kinds: &'static [Species],
    hostile: &'static [Species],
    small: bool,
    wait: bool,
) -> bool {
    if m.is_baby() || m.below(10) != 0 {
        return false;
    }
    let range = m.species.follow_range();
    let me = b.id;
    let eye = [b.pos[0], b.pos[1] + m.eye_height(), b.pos[2]];
    let pick = |hostile_only: bool| {
        s.nearest_mob(b.pos, range, |o| {
            o.id != me
                && o.species.is_some_and(|k| {
                    if hostile_only {
                        hostile.contains(&k)
                    } else {
                        kinds.contains(&k)
                    }
                })
                && (!small || o.width <= 0.53)
                && line_clear(
                    s.world,
                    eye,
                    [o.pos[0], o.pos[1] + o.height * 0.5, o.pos[2]],
                )
        })
        .map(|o| o.id)
    };
    let found = pick(true).or_else(|| {
        if wait && m.quirks.aqua.hunt_wait > 0 {
            None
        } else {
            pick(false)
        }
    });
    found.is_some_and(|id| {
        m.target = Some(id);
        true
    })
}

pub(super) fn can_continue(act: &mut Act, m: &mut Mob, b: &mut Body, s: &Senses) -> bool {
    if m.is_dead() {
        return false;
    }
    match act {
        Act::Swim { to, left, .. } => {
            *left -= 1;
            *left > 0 && to.is_some_and(|p| dist_sq(b.pos, p) > 1.5) && in_water(b, s)
        }
        Act::Flee { to, from, .. } => {
            let near = from
                .and_then(|id| s.player(id))
                .is_some_and(|p| p.present && dist_sq(b.pos, p.pos) < 100.0);
            near && to.is_some_and(|p| dist_sq(b.pos, p) > 1.5) && in_water(b, s)
        }
        Act::School { leader } => {
            leader
                .and_then(|id| s.mob(id))
                .is_some_and(|o| o.alive && dist_sq(b.pos, o.pos) > 4.0)
                && in_water(b, s)
        }
        Act::Squid { .. } => in_water(b, s),
        Act::Escort { player, .. } => player.and_then(|id| s.player(id)).is_some_and(|p| {
            p.present && (p.swimming || p.boating) && dist_sq(b.pos, p.pos) < 400.0
        }),
        Act::Treasure { left } => *left > 0 && m.quirks.aqua.treasure.is_some(),
        Act::Breathe { .. } => in_water(b, s) && m.quirks.aqua.breath < 4000,
        Act::Home { to, left } => {
            *left -= 1;
            *left > 0
                && to.is_some_and(|h| {
                    dist_sq(
                        b.pos,
                        [
                            f64::from(h[0]) + 0.5,
                            f64::from(h[1]),
                            f64::from(h[2]) + 0.5,
                        ],
                    ) > 9.0
                })
        }
        Act::LayEgg { left } => *left > 0,
        Act::ToWater { to, left } => {
            *left -= 1;
            *left > 0 && !in_water(b, s) && to.is_some()
        }
        Act::LaySpawn { to, left } => {
            *left -= 1;
            *left > 0 && to.is_some() && m.quirks.aqua.pregnant
        }
        Act::Hunt { .. } => {
            let Some(id) = m.target else {
                return false;
            };
            s.seen(id).is_some() && dist_sq(b.pos, s.seen(id).map_or(b.pos, |t| t.pos)) < 400.0
        }
        Act::Tongue { .. } | Act::Beam { .. } | Act::Dash { .. } => {
            m.target.and_then(|id| s.seen(id)).is_some()
        }
    }
}

pub(super) fn start(act: &mut Act, m: &mut Mob, _b: &Body, out: &mut Vec<MobEvent>) {
    match act {
        Act::Swim { left, .. } => *left = 200,
        Act::Home { left, .. } | Act::ToWater { left, .. } | Act::LaySpawn { left, .. } => {
            *left = 600
        }
        Act::LayEgg { left } => {
            *left = 200;
            m.quirks.aqua.laying = 200;
            m.dirty = true;
        }
        Act::Treasure { left } => *left = 600,
        Act::Beam { .. } => {
            m.quirks.aqua.charge = -10;
            m.quirks.aqua.beam = 0;
        }
        Act::Dash { .. } => {
            let name = format!("entity.{}.dash", m.species.path());
            out.push(MobEvent::Sound {
                name,
                volume: 1.0,
                pitch: 1.0,
            });
        }
        _ => {}
    }
}

pub(super) fn stop(act: &mut Act, m: &mut Mob) {
    match act {
        Act::Beam { .. } => {
            m.quirks.aqua.charge = 0;
            if m.quirks.aqua.beam != 0 {
                m.quirks.aqua.beam = 0;
                m.dirty = true;
            }
            m.target = None;
        }
        Act::LayEgg { .. } => {
            m.quirks.aqua.laying = 0;
            m.dirty = true;
        }
        Act::Hunt { wait, .. } if *wait => {
            m.target = None;
        }
        Act::Tongue { .. } => {
            m.quirks.aqua.tongue = 0;
            m.quirks.aqua.caught = None;
            m.dirty = true;
        }
        _ => {}
    }
}

pub(super) fn tick(act: &mut Act, m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match act {
        Act::Swim { speed, to, .. } => {
            if let Some(p) = *to {
                m.goto = Some((p, *speed));
            }
        }
        Act::Flee {
            walk,
            sprint,
            to,
            from,
            ..
        } => {
            let close = from
                .and_then(|id| s.player(id))
                .is_some_and(|p| dist_sq(b.pos, p.pos) < 49.0);
            if let Some(p) = *to {
                m.goto = Some((p, if close { *sprint } else { *walk }));
            }
        }
        Act::School { leader } => {
            if let Some(o) = leader.and_then(|id| s.mob(id)) {
                m.goto = Some((o.pos, 1.0));
            }
        }
        Act::Squid { time } => squid_tick(time, m, b, s, out),
        Act::Escort { player, grace } => {
            let Some(p) = player.and_then(|id| s.player(id)) else {
                return;
            };
            m.look = Some([p.pos[0], p.pos[1] + 1.0, p.pos[2]]);
            if dist_sq(b.pos, p.pos) > 6.0 {
                m.goto = Some((p.pos, 1.2));
            }
            *grace -= 1;
            // The sprint of a swimmer within nine blocks is a blessing.
            if *grace <= 0 && p.swimming && dist_sq(b.pos, p.pos) < 81.0 {
                *grace = 20;
                out.push(MobEvent::Grace { player: p.id });
            }
        }
        Act::Treasure { .. } => {
            if let Some(t) = m.quirks.aqua.treasure {
                let p = [
                    f64::from(t[0]) + 0.5,
                    f64::from(t[1]) + 0.5,
                    f64::from(t[2]) + 0.5,
                ];
                if dist_sq(b.pos, p) < 16.0 {
                    m.quirks.aqua.treasure = None;
                    m.quirks.aqua.got_fish = false;
                    m.dirty = true;
                } else {
                    m.goto = Some((p, 1.2));
                }
            }
        }
        Act::Breathe { to } => {
            if let Some(p) = *to {
                m.goto = Some((p, 1.0));
            }
        }
        Act::Home { to, .. } => {
            if let Some(h) = *to {
                m.goto = Some((
                    [
                        f64::from(h[0]) + 0.5,
                        f64::from(h[1]),
                        f64::from(h[2]) + 0.5,
                    ],
                    1.0,
                ));
            }
        }
        Act::LayEgg { left } => {
            *left -= 1;
            m.nav.stop();
            if *left == 100 {
                out.push(MobEvent::Sound {
                    name: "entity.turtle.shamble".into(),
                    volume: 0.3,
                    pitch: 1.0,
                });
            }
            if *left <= 1 {
                let count = 1 + m.below(4) as i32;
                out.push(MobEvent::LayEgg {
                    cell: cell_of(b.pos),
                    count,
                });
                out.push(MobEvent::Sound {
                    name: "entity.turtle.lay_egg".into(),
                    volume: 0.3,
                    pitch: 0.9 + m.next_f64() as f32 * 0.2,
                });
                m.quirks.aqua.has_egg = false;
                m.quirks.aqua.laying = 0;
                m.baby_age = m.baby_age.max(6000);
                m.dirty = true;
                *left = 0;
            }
        }
        Act::ToWater { to, .. } => {
            if let Some(p) = *to {
                m.goto = Some((p, 1.0));
            }
        }
        Act::LaySpawn { to, .. } => {
            let Some(c) = *to else {
                return;
            };
            let p = [
                f64::from(c[0]) + 0.5,
                f64::from(c[1]),
                f64::from(c[2]) + 0.5,
            ];
            if dist_sq(b.pos, p) < 6.0 {
                out.push(MobEvent::LaySpawn { cell: c });
                out.push(MobEvent::Sound {
                    name: "entity.frog.lay_spawn".into(),
                    volume: 1.0,
                    pitch: 1.0,
                });
                m.quirks.aqua.pregnant = false;
                m.baby_age = m.baby_age.max(6000);
                m.dirty = true;
                *to = None;
            } else {
                m.goto = Some((p, 1.0));
            }
        }
        Act::Hunt { .. } => {}
        Act::Tongue { range } => tongue_tick(*range, m, b, s, out),
        Act::Beam { duration, hold } => beam_tick(*duration, *hold, m, b, s, out),
        Act::Dash { speed } => {
            let Some(t) = m.target.and_then(|id| s.seen(id)) else {
                return;
            };
            m.look = Some([t.pos[0], t.pos[1] + 0.5, t.pos[2]]);
            m.goto = Some((t.pos, *speed));
            let reach = m.size().0 + t.width + 0.5;
            if m.attack_cooldown <= 0 && dist_sq(b.pos, t.pos) <= reach * reach {
                m.attack_cooldown = 20;
                m.quirks.aqua.dash_cd = 40;
                out.push(MobEvent::Attack {
                    target: t.id,
                    damage: m.species.attack_damage(),
                });
            }
        }
    }
}

/// A squid keeps to the way it set until a notion takes it elsewhere.
fn squid_tick(time: &mut i32, m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    *time += 1;
    let fleeing = m.revenge.is_some();
    if fleeing && m.quirks.aqua.ink {
        m.quirks.aqua.ink = false;
        out.push(MobEvent::Ink);
        // Away from whoever did it.
        let from = m
            .revenge
            .and_then(|(id, _)| {
                s.player(id)
                    .map(|p| p.pos)
                    .or_else(|| s.mob(id).map(|o| o.pos))
            })
            .unwrap_or(b.pos);
        let d = [b.pos[0] - from[0], b.pos[1] - from[1], b.pos[2] - from[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        m.quirks.aqua.drift = if len < 1e-3 {
            // Hurt by something it cannot place: any way will do.
            let angle = m.next_f64() * std::f64::consts::TAU;
            [angle.cos(), 0.0, angle.sin()]
        } else {
            [d[0] / len, d[1] / len, d[2] / len]
        };
        *time = 0;
    }
    let drift = m.quirks.aqua.drift;
    let bored = *time > 100 && !fleeing;
    let stuck = !water(
        s.world,
        cell_of([
            b.pos[0] + drift[0],
            b.pos[1] + drift[1],
            b.pos[2] + drift[2],
        ]),
    );
    if !fleeing && (m.below(50) == 0 || stuck || drift == [0.0; 3] || bored) {
        let angle = m.next_f64() * std::f64::consts::TAU;
        let y = m.next_f64() * 0.2 - 0.1;
        m.quirks.aqua.drift = [angle.cos() * 0.2, y, angle.sin() * 0.2];
        if bored {
            *time = 0;
        }
    }
    let d = m.quirks.aqua.drift;
    let go = [
        b.pos[0] + d[0] * 10.0,
        b.pos[1] + d[1] * 10.0,
        b.pos[2] + d[2] * 10.0,
    ];
    let speed = if fleeing { 2.5 } else { 0.6 };
    m.goto = Some((go, speed));
}

/// A frog shoots its tongue at the prey.
fn tongue_tick(range: f64, m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let Some(t) = m.target.and_then(|id| s.seen(id)) else {
        return;
    };
    m.look = Some([t.pos[0], t.pos[1] + 0.2, t.pos[2]]);
    let d = dist_sq(b.pos, t.pos);
    if m.quirks.aqua.tongue > 0 {
        m.quirks.aqua.tongue += 1;
        m.nav.stop();
        // Six ticks out and back; it swallows at the turn.
        if m.quirks.aqua.tongue == 6 {
            out.push(MobEvent::Eat { target: t.id });
            out.push(MobEvent::Sound {
                name: "entity.frog.eat".into(),
                volume: 1.0,
                pitch: 1.0,
            });
        }
        if m.quirks.aqua.tongue >= 12 {
            m.quirks.aqua.tongue = 0;
            m.quirks.aqua.caught = None;
            m.target = None;
            m.dirty = true;
        }
        return;
    }
    if d <= range * range && line_clear(s.world, b.pos, t.pos) {
        m.quirks.aqua.tongue = 1;
        m.quirks.aqua.caught = Some(t.id);
        m.dirty = true;
        out.push(MobEvent::Sound {
            name: "entity.frog.tongue".into(),
            volume: 1.0,
            pitch: 1.0,
        });
    } else {
        m.goto = Some((t.pos, 1.0));
    }
}

/// A guardian charges its beam for `duration` ticks while the target stays
/// in sight, and hits when the charge is full.
fn beam_tick(duration: i32, hold: f64, m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let Some(t) = m.target.and_then(|id| s.seen(id)) else {
        return;
    };
    let eye = [b.pos[0], b.pos[1] + m.eye_height(), b.pos[2]];
    let aim = [t.pos[0], t.pos[1] + t.eye * 0.5, t.pos[2]];
    m.look = Some(aim);
    let d = dist_sq(b.pos, t.pos);
    // Out of reach or out of sight: close in, and charge nothing.
    if d > 15.0 * 15.0 || !line_clear(s.world, eye, aim) {
        m.goto = Some((t.pos, 0.8));
        m.quirks.aqua.charge = m.quirks.aqua.charge.min(0);
        return;
    }
    if d > hold * hold {
        m.goto = Some((t.pos, 0.6));
    }
    let a = &mut m.quirks.aqua;
    a.charge += 1;
    if a.charge == 0 {
        a.beam = t.id;
        m.dirty = true;
        out.push(MobEvent::Sound {
            name: "entity.guardian.attack".into(),
            volume: 1.0,
            pitch: 1.0,
        });
    } else if a.charge >= duration {
        let base = m.species.attack_damage();
        out.push(MobEvent::Attack {
            target: t.id,
            damage: base,
        });
        // The beam is magic as well.
        let mut magic = 2.0;
        if s.difficulty >= 3 {
            magic += 3.0;
        }
        if m.species == Species::ElderGuardian {
            magic += 2.0;
        }
        out.push(MobEvent::MagicAttack {
            target: t.id,
            damage: magic,
        });
        let a = &mut m.quirks.aqua;
        a.charge = -10;
        a.beam = 0;
        m.target = None;
        m.dirty = true;
    }
}

/// Each tick, whatever the mob wants.
pub(super) fn pulse(m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let sp = m.species;
    if !(sp.swims() || sp == Species::Frog) {
        return;
    }
    let wet = in_water(b, s);
    m.quirks.aqua.wet = wet;
    if m.quirks.aqua.hunt_wait > 0 {
        m.quirks.aqua.hunt_wait -= 1;
    }
    if m.quirks.aqua.dash_cd > 0 {
        m.quirks.aqua.dash_cd -= 1;
    }
    breathing(m, b, s, wet, out);
    stranded(m, b, wet, out);
    match sp {
        Species::Pufferfish => puff(m, b, s, out),
        Species::GlowSquid => {
            if m.quirks.aqua.dark > 0 {
                m.quirks.aqua.dark -= 1;
                m.dirty = true;
            }
        }
        Species::Tadpole => {
            let a = &mut m.quirks.aqua;
            a.growth += 1;
            if a.growth >= TADPOLE_GROWTH {
                out.push(MobEvent::Sound {
                    name: "entity.tadpole.grow_up".into(),
                    volume: 0.15,
                    pitch: 1.0,
                });
                out.push(MobEvent::Convert(Species::Frog));
            }
        }
        Species::Axolotl => {
            if m.quirks.aqua.dead_play > 0 {
                m.quirks.aqua.dead_play -= 1;
                m.nav.stop();
                if m.quirks.aqua.dead_play == 0 {
                    m.dirty = true;
                }
            }
        }
        Species::ElderGuardian => {
            let a = &mut m.quirks.aqua;
            a.curse_in -= 1;
            if a.curse_in <= 0 {
                a.curse_in = 1200;
                out.push(MobEvent::Curse);
            }
        }
        Species::Guardian => {}
        _ => {}
    }
    if matches!(sp, Species::Guardian | Species::ElderGuardian) {
        let moving = b.vel[0] * b.vel[0] + b.vel[1] * b.vel[1] + b.vel[2] * b.vel[2] > 0.0004;
        let retract = wet && moving;
        if retract != m.quirks.aqua.retract {
            m.quirks.aqua.retract = retract;
            m.dirty = true;
        }
    }
}

/// Air, moisture and the damage of being without.
fn breathing(m: &mut Mob, b: &Body, s: &Senses, wet: bool, out: &mut Vec<MobEvent>) {
    let sp = m.species;
    let breathes_water = matches!(
        sp,
        Species::Cod
            | Species::Salmon
            | Species::Pufferfish
            | Species::TropicalFish
            | Species::Squid
            | Species::GlowSquid
            | Species::Tadpole
            | Species::Nautilus
            | Species::ZombieNautilus
    );
    if breathes_water {
        let a = &mut m.quirks.aqua;
        if wet {
            a.air = FISH_AIR;
        } else {
            a.air -= 1;
            if a.air <= -20 {
                a.air = 0;
                out.push(MobEvent::Hurt {
                    amount: 2.0,
                    kind: "minecraft:drown",
                });
            }
        }
    }
    if sp == Species::Axolotl {
        let a = &mut m.quirks.aqua;
        if wet {
            a.air = AXOLOTL_AIR;
        } else {
            a.air -= 1;
            if a.air <= -20 {
                a.air = 0;
                out.push(MobEvent::Hurt {
                    amount: 2.0,
                    kind: "minecraft:dry_out",
                });
            }
        }
    }
    if sp == Species::Dolphin {
        let wet_or_rain = wet
            || s.raining
                && s.world.sky_light(
                    cell_of(b.pos)[0],
                    (b.pos[1] + m.size().1).floor() as i32,
                    cell_of(b.pos)[2],
                ) >= 15;
        let head = {
            let eye = b.pos[1] + m.eye_height();
            let c = cell_of(b.pos);
            water(s.world, [c[0], eye.floor() as i32, c[2]])
        };
        let a = &mut m.quirks.aqua;
        if wet_or_rain {
            a.air = DOLPHIN_MOISTURE;
        } else {
            a.air -= 1;
            if a.air <= 0 {
                out.push(MobEvent::Hurt {
                    amount: 1.0,
                    kind: "minecraft:dry_out",
                });
            }
        }
        if head {
            a.breath -= 1;
            if a.breath <= -20 {
                a.breath = 0;
                out.push(MobEvent::Hurt {
                    amount: 2.0,
                    kind: "minecraft:drown",
                });
            }
        } else {
            a.breath = (a.breath + 4).min(DOLPHIN_AIR);
        }
    }
}

/// Fish stranded on land flop; guardians flop too and squeak.
fn stranded(m: &mut Mob, b: &mut Body, wet: bool, out: &mut Vec<MobEvent>) {
    let sp = m.species;
    let flops = matches!(
        sp,
        Species::Cod
            | Species::Salmon
            | Species::Pufferfish
            | Species::TropicalFish
            | Species::Tadpole
            | Species::Guardian
            | Species::ElderGuardian
    );
    if !flops || wet || !b.on_ground {
        return;
    }
    let (kick, lift) = if matches!(sp, Species::Guardian | Species::ElderGuardian) {
        (0.4, 0.5)
    } else {
        (0.05, 0.4)
    };
    b.vel[0] += (m.next_f64() * 2.0 - 1.0) * kick;
    b.vel[1] = lift;
    b.vel[2] += (m.next_f64() * 2.0 - 1.0) * kick;
    b.on_ground = false;
    if matches!(sp, Species::Guardian | Species::ElderGuardian) {
        m.yaw = (m.next_f64() * 360.0) as f32;
    }
    let path = match sp {
        Species::Pufferfish => "puffer_fish",
        s => s.path(),
    };
    let pitch = 0.8 + m.next_f64() as f32 * 0.4;
    out.push(MobEvent::Sound {
        name: format!("entity.{path}.flop"),
        volume: 0.3,
        pitch,
    });
}

/// Whether a creature scares a pufferfish: players that can be hurt and
/// every mob but the ones in `#not_scary_for_pufferfish`.
fn scary(o: &Other) -> bool {
    use Species::*;
    match o.species {
        Some(k) => {
            o.alive
                && !matches!(
                    k,
                    Turtle
                        | Guardian
                        | ElderGuardian
                        | Cod
                        | Pufferfish
                        | Salmon
                        | TropicalFish
                        | Dolphin
                        | Squid
                        | GlowSquid
                        | Tadpole
                        | Nautilus
                        | ZombieNautilus
                )
        }
        None => o.alive,
    }
}

/// The pufferfish swells when something it fears is near and eases off
/// when it goes; swollen, it stings what touches it.
fn puff(m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let width = m.size().0;
    let reach = |other_width: f64| 2.0 + width / 2.0 + other_width / 2.0;
    let threatened = s.players.iter().any(|p| {
        p.targetable
            && dist_sq(b.pos, p.pos) <= reach(0.6).powi(2)
            && (b.pos[1] - p.pos[1]).abs() < 2.5
    }) || s.mobs.iter().any(|o| {
        scary(o)
            && o.id != b.id
            && dist_sq(b.pos, o.pos) <= reach(o.width).powi(2)
            && (b.pos[1] - o.pos[1]).abs() < 2.5
    });
    let a = &mut m.quirks.aqua;
    let before = a.puff;
    if threatened {
        a.deflate = 0;
        a.inflate += 1;
        if a.puff == 0 {
            a.puff = 1;
            push_puff(out, "blow_up");
        } else if a.inflate > 40 && a.puff == 1 {
            a.puff = 2;
            push_puff(out, "blow_up");
        }
    } else {
        a.inflate = 0;
        if a.puff > 0 {
            a.deflate += 1;
            if a.deflate > 60 && a.puff == 2 {
                a.puff = 1;
                a.deflate = 0;
                push_puff(out, "blow_out");
            } else if a.deflate > 100 && a.puff == 1 {
                a.puff = 0;
                a.deflate = 0;
                push_puff(out, "blow_out");
            }
        }
    }
    if a.puff != before {
        m.dirty = true;
    }
    // Touching it hurts and poisons.
    let state = m.quirks.aqua.puff;
    if state > 0 && m.age.is_multiple_of(10) {
        let touch = |w: f64| 0.3 + width / 2.0 + w / 2.0;
        for p in s.players.iter().filter(|p| p.targetable) {
            if dist_sq(b.pos, p.pos) <= touch(0.6).powi(2) && (b.pos[1] - p.pos[1]).abs() < 1.8 {
                sting(p.id, state, out);
            }
        }
        for o in s.mobs.iter().filter(|o| scary(o) && o.id != b.id) {
            if o.species
                .is_some_and(|k| matches!(k, Species::Creeper | Species::Ghast))
            {
                continue;
            }
            if dist_sq(b.pos, o.pos) <= touch(o.width).powi(2) && (b.pos[1] - o.pos[1]).abs() < 1.8
            {
                sting(o.id, state, out);
            }
        }
    }
}

fn sting(target: i32, state: u8, out: &mut Vec<MobEvent>) {
    out.push(MobEvent::Sting {
        target,
        damage: 1.0 + f32::from(state),
        poison: 60 * i32::from(state),
    });
    push_puff(out, "sting");
}

fn push_puff(out: &mut Vec<MobEvent>, what: &str) {
    out.push(MobEvent::Sound {
        name: format!("entity.puffer_fish.{what}"),
        volume: 1.0,
        pitch: 1.0,
    });
}

/// A hit on the mob: squids ink and bolt, axolotls play dead, glow squids
/// darken. Called with the damage that went through.
pub(super) fn hurt_hook(m: &mut Mob, amount: f32, attacker: Option<i32>) {
    match m.species {
        Species::Squid | Species::GlowSquid => {
            if attacker.is_some() || m.revenge.is_some() {
                m.quirks.aqua.ink = true;
            }
            if m.species == Species::GlowSquid {
                m.quirks.aqua.dark = 100;
            }
        }
        Species::Axolotl => {
            let a = &m.quirks.aqua;
            if a.dead_play > 0 || !a.wet || m.is_dead() {
                return;
            }
            let low = m.health < m.max_health * 0.5;
            let roll = (m.below(3) as f32) < amount;
            if low || roll || m.below(3) == 0 {
                m.quirks.aqua.dead_play = PLAY_DEAD;
                m.target = None;
                m.nav.stop();
                m.dirty = true;
            }
        }
        _ => {}
    }
}

/// Tells that it ate or won something harmless: it does not hunt again
/// for a while.
pub fn ate(m: &mut Mob) {
    m.quirks.aqua.hunt_wait = HUNT_WAIT;
}

/// A mob that comes out of a bucket or a spawn egg set up as one of a
/// kind keeping its home.
pub fn home_at(m: &mut Mob, cell: [i32; 3]) {
    m.quirks.aqua.home = Some(cell);
}

/// Whether the block under the cell is sand (a turtle nests there).
pub(super) fn sand_under(w: &dyn Terrain, cell: [i32; 3]) -> bool {
    w.feature(cell[0], cell[1] - 1, cell[2], crate::nav::Feature::Sand)
}

/// What the aquatic mobs use to tell sounds apart.
pub fn voice(species: Species, which: Sound, baby: bool) -> Option<Option<String>> {
    use Species::*;
    let (base, kinds): (&str, &[&str]) = match species {
        Cod => ("cod", &["ambient", "hurt", "death"]),
        Salmon => ("salmon", &["ambient", "hurt", "death"]),
        Pufferfish => ("puffer_fish", &["ambient", "hurt", "death"]),
        TropicalFish => ("tropical_fish", &["ambient", "hurt", "death"]),
        Squid => ("squid", &["ambient", "hurt", "death"]),
        GlowSquid => ("glow_squid", &["ambient", "hurt", "death"]),
        Dolphin => ("dolphin", &["ambient_water", "hurt", "death"]),
        Turtle => ("turtle", &["ambient_land", "hurt", "death", "shamble"]),
        Axolotl => ("axolotl", &["idle_water", "hurt", "death"]),
        Frog => ("frog", &["ambient", "hurt", "death", "step"]),
        Tadpole => ("tadpole", &["", "hurt", "death"]),
        Guardian => ("guardian", &["ambient", "hurt", "death"]),
        ElderGuardian => ("elder_guardian", &["ambient", "hurt", "death"]),
        Nautilus => ("nautilus", &["ambient", "hurt", "death"]),
        ZombieNautilus => ("zombie_nautilus", &["ambient", "hurt", "death"]),
        _ => return None,
    };
    let name = match which {
        Sound::Ambient => kinds[0],
        Sound::Hurt => kinds[1],
        Sound::Death => kinds[2],
        Sound::Step => match kinds.get(3) {
            Some(s) => s,
            None => return Some(None),
        },
    };
    if name.is_empty() {
        return Some(None);
    }
    let suffix = if baby && species == Turtle && matches!(which, Sound::Hurt | Sound::Death) {
        "_baby"
    } else if baby && species == Turtle && which == Sound::Step {
        "_baby"
    } else {
        ""
    };
    Some(Some(format!("entity.{base}.{name}{suffix}")))
}
