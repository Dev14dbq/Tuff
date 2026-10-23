//! Goals and the goal selector (see the module comment of [`super`]).

use super::body::{params, yaw_to};
use super::{
    Body, Category, Missile, Mob, MobEvent, PLAYER_EYE, Seen, Senses, Species, Target, cell_of,
    dist_sq, line_clear, slot,
};
use crate::nav::{Cell, Graze, Liquid, standable};

#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub priority: u8,
    pub flags: u8,
    pub goal: Goal,
    pub running: bool,
}

/// What a ranged attacker shoots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ranged {
    /// Skeletons and strays, with a bow in hand.
    Bow,
    /// Drowned with a trident.
    Trident,
    /// Witches' splash potions.
    Potion,
    /// Ghast fireballs.
    Fireball,
    /// Blazes: three small fireballs, or a burn up close.
    Blaze,
}

/// Whom a mob keeps away from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Avoidee {
    /// Players it could be hurt by (survival or adventure).
    Players,
    Kinds(&'static [Species]),
}

/// What a goal does, with the state it keeps while running.
#[derive(Clone, Debug, PartialEq)]
pub enum Goal {
    /// Swim up out of water.
    Float,
    /// Run off after being hurt.
    Panic { speed: f64 },
    /// Wander to a random place now and then.
    Stroll {
        speed: f64,
        /// On average one start in this many ticks.
        interval: u32,
        avoid_water: bool,
    },
    /// Watch a player that comes close.
    LookAtPlayer { range: f64, left: i32 },
    /// Turn the head to a random side.
    RandomLook { left: i32, angle: f64 },
    /// Chase the target and hit it.
    Melee {
        speed: f64,
        /// Gives up on its own at times (spiders by day).
        fickle: bool,
    },
    /// Back away from the target when it is too close.
    Retreat { range: f64, speed: f64 },
    /// Keep away from players or some kinds of mob.
    Avoid {
        from: Avoidee,
        range: f64,
        walk: f64,
        sprint: f64,
        threat: Option<i32>,
    },
    /// Creeper: fuse while the target is close.
    Swell,
    /// Pounce at a target a few blocks away.
    Leap { up: f64 },
    /// Shoot or throw from a distance.
    Ranged {
        kind: Ranged,
        /// Ticks between shots.
        interval: i32,
        range: f64,
        /// Ticks until the next shot (or of drawing the bow).
        timer: i32,
        /// Ticks the target has been in sight (negative: out of sight).
        seen: i32,
    },
    /// Sheep: graze on grass.
    EatGrass { left: i32 },
    /// Follow a player holding its food.
    Tempt {
        speed: f64,
        cooldown: i32,
        player: Option<i32>,
    },
    /// Find a partner in love and make a baby.
    Breed {
        speed: f64,
        partner: Option<i32>,
        timer: i32,
    },
    /// A baby keeps near a grown-up of its kind.
    FollowParent {
        speed: f64,
        parent: Option<i32>,
        delay: i32,
    },
    /// A pet told to sit stays put.
    Sit,
    /// A pet follows its owner, and catches up by teleporting.
    FollowOwner { speed: f64, start: f64, stop: f64 },
    /// Undead on fire look for shade.
    FleeSun { speed: f64 },
    /// Slimes and magma cubes hop about.
    Hop { delay: i32 },
    /// Fliers drift to random places.
    Hover { range: f64, rise: f64 },
    /// Phantoms circle and swoop.
    Swoop {
        /// Ticks until the next swoop; negative while swooping.
        timer: i32,
        angle: f64,
    },
    /// Foxes sleep through the day.
    Sleep,
    /// A piglin looks at the gold it was given.
    Admire,
    // Target goals.
    /// Go after whoever hurt the mob; `alert` calls the others of its kind.
    HurtBy { alert: bool },
    /// Go after the nearest player.
    Nearest {
        range: f64,
        /// Spiders lose interest in the light.
        fickle: bool,
        /// Piglins leave those in gold alone.
        spare_gold: bool,
        /// Ticks the target has been out of sight.
        unseen: i32,
    },
    /// Go after the nearest mob of some kinds (or villagers).
    Prey {
        kinds: &'static [Species],
        villagers: bool,
        /// Only while untamed (wolves hunt sheep in the wild).
        wild_only: bool,
        unseen: i32,
    },
    /// A pet goes after whoever hurt its owner...
    OwnerHurtBy,
    /// ...and whatever its owner hits.
    OwnerHurt,
    /// Endermen go after a player who looks at them.
    Stare,
}

// ---- helpers --------------------------------------------------------------

pub(super) fn eye(m: &Mob, pos: [f64; 3]) -> [f64; 3] {
    [pos[0], pos[1] + m.eye_height(), pos[2]]
}

fn eye_of(t: &Seen) -> [f64; 3] {
    [t.pos[0], t.pos[1] + t.eye, t.pos[2]]
}

fn can_see(m: &Mob, b: &Body, s: &Senses, t: &Seen) -> bool {
    line_clear(s.world, eye(m, b.pos), eye_of(t))
}

fn sees_player(m: &Mob, b: &Body, s: &Senses, p: &Target) -> bool {
    line_clear(
        s.world,
        eye(m, b.pos),
        [p.pos[0], p.pos[1] + PLAYER_EYE, p.pos[2]],
    )
}

fn center(c: Cell) -> [f64; 3] {
    [
        f64::from(c[0]) + 0.5,
        f64::from(c[1]),
        f64::from(c[2]) + 0.5,
    ]
}

fn light_at(s: &Senses, pos: [f64; 3]) -> u8 {
    let c = cell_of(pos);
    s.world.light(c[0], c[1], c[2])
}

/// A random standable cell near `from`, at most `h` away sideways and `v`
/// up or down; `away_from` keeps the pick on the far side of a point.
pub(super) fn random_spot(
    m: &mut Mob,
    s: &Senses,
    from: Cell,
    h: i32,
    v: i32,
    avoid_water: bool,
    away_from: Option<[f64; 3]>,
) -> Option<Cell> {
    let p = params(m);
    for _ in 0..10 {
        let dx = m.below(2 * h as u32 + 1) as i32 - h;
        let dz = m.below(2 * h as u32 + 1) as i32 - h;
        if let Some(a) = away_from {
            let (ax, az) = (a[0] - f64::from(from[0]), a[2] - f64::from(from[2]));
            if f64::from(dx) * ax + f64::from(dz) * az > 0.0 {
                continue;
            }
        }
        let (x, z) = (from[0] + dx, from[2] + dz);
        for y in (from[1] - v..=from[1] + v).rev() {
            let c = [x, y, z];
            if standable(s.world, c, &p) {
                if avoid_water && s.world.liquid(x, y, z) == Liquid::Water {
                    break;
                }
                return Some(c);
            }
        }
    }
    None
}

/// A random point in the air near `from` (fliers).
fn random_air(m: &mut Mob, s: &Senses, from: [f64; 3], h: f64, v: f64) -> Option<[f64; 3]> {
    for _ in 0..10 {
        let p = [
            from[0] + (m.next_f64() * 2.0 - 1.0) * h,
            from[1] + (m.next_f64() * 2.0 - 1.0) * v,
            from[2] + (m.next_f64() * 2.0 - 1.0) * h,
        ];
        let c = cell_of(p);
        if !s.world.solid(c[0], c[1], c[2]) && line_clear(s.world, from, p) {
            return Some(p);
        }
    }
    None
}

/// Moves the mob to a standable spot near `around` (within `r`); whether it
/// found one.
pub(super) fn teleport_near(
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    around: [f64; 3],
    r: i32,
) -> bool {
    let p = params(m);
    for _ in 0..16 {
        let dx = m.below(2 * r as u32 + 1) as i32 - r;
        let dz = m.below(2 * r as u32 + 1) as i32 - r;
        let dy = m.below(2 * r as u32 + 1) as i32 - r;
        let base = cell_of(around);
        let (x, z) = (base[0] + dx, base[2] + dz);
        let top = base[1] + dy;
        for y in (top - 8..=top).rev() {
            let c = [x, y, z];
            if standable(s.world, c, &p) && s.world.liquid(x, y, z) == Liquid::None {
                b.pos = center(c);
                b.vel = [0.0; 3];
                m.nav.stop();
                return true;
            }
        }
    }
    false
}

/// The mob's target, if it is still there to go after.
fn target(m: &Mob, s: &Senses) -> Option<Seen> {
    s.seen(m.target?)
}

/// Whether the main hand holds this item.
fn holds(m: &Mob, s: &Senses, name: &str) -> bool {
    m.gear_item(slot::MAINHAND)
        .is_some_and(|i| s.items.name(i) == name)
}

/// Whether a player holds something the mob wants.
fn holds_food(p: &Target, s: &Senses, tag: &str) -> bool {
    p.held.iter().any(|&i| i >= 0 && s.items.tagged(i, tag))
}

/// Melee reach squared: twice the attacker's width, plus the target's.
fn reach(m: &Mob, t: &Seen) -> f64 {
    let w = m.size().0;
    (w * 2.0).powi(2) + t.width
}

fn look_at(m: &mut Mob, t: &Seen) {
    m.look = Some(eye_of(t));
}

/// A partner for breeding: another of its kind (a horse and a donkey make
/// mules) in love and grown up.
fn partner_ok(m: &Mob, o: &super::Other) -> bool {
    let Some(kind) = o.species else {
        return false;
    };
    let pair = kind == m.species
        || matches!(
            (m.species, kind),
            (Species::Horse, Species::Donkey) | (Species::Donkey, Species::Horse)
        );
    pair && o.in_love && !o.baby && kind != Species::Mule
}

// ---- the selector ---------------------------------------------------------

fn can_use(g: &mut Goal, m: &mut Mob, b: &mut Body, s: &Senses) -> bool {
    match g {
        Goal::Float => body_wet(m, b, s),
        Goal::Panic { .. } => {
            (m.revenge.is_some() || m.burning > 0)
                && m.species.category() == Category::Creature
                && !(m.is_tame() && m.species == Species::Wolf)
        }
        Goal::Stroll { interval, .. } => m.below(*interval) == 0,
        Goal::LookAtPlayer { range, .. } => {
            m.next_f64() < 0.02 && s.nearest(b.pos, *range, |p| p.present).is_some()
        }
        Goal::RandomLook { .. } => m.next_f64() < 0.02,
        Goal::Melee { .. } => {
            if m.species.skeleton() && holds(m, s, "minecraft:bow")
                || m.species == Species::Drowned && holds(m, s, "minecraft:trident")
            {
                return false;
            }
            target(m, s).is_some()
        }
        Goal::Retreat { range, .. } => {
            target(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) < *range * *range)
        }
        Goal::Avoid {
            from,
            range,
            threat,
            ..
        } => {
            if m.is_tame() {
                return false;
            }
            let found = match from {
                Avoidee::Players => s
                    .nearest(b.pos, *range, |p| p.targetable && sees_player(m, b, s, p))
                    .map(|p| (p.id, p.pos)),
                Avoidee::Kinds(kinds) => s
                    .nearest_mob(b.pos, *range, |o| {
                        o.species.is_some_and(|k| kinds.contains(&k))
                    })
                    .map(|o| (o.id, o.pos)),
            };
            let Some((id, pos)) = found else {
                return false;
            };
            let from_cell = cell_of(b.pos);
            match random_spot(m, s, from_cell, 16, 7, true, Some(pos)) {
                Some(c) => {
                    *threat = Some(id);
                    m.nav.stop();
                    m.goto = Some((center(c), 1.0));
                    true
                }
                None => false,
            }
        }
        Goal::Swell => {
            m.ignited
                || target(m, s).is_some_and(|t| m.swell_dir > 0 || dist_sq(b.pos, t.pos) < 9.0)
        }
        Goal::Leap { .. } => {
            let Some(t) = target(m, s) else {
                return false;
            };
            let d = dist_sq(b.pos, t.pos);
            b.on_ground && (4.0..=16.0).contains(&d) && m.below(5) == 0
        }
        Goal::Ranged { kind, .. } => {
            let armed = match kind {
                Ranged::Bow => holds(m, s, "minecraft:bow"),
                Ranged::Trident => holds(m, s, "minecraft:trident"),
                Ranged::Potion => m.drinking == 0,
                Ranged::Fireball | Ranged::Blaze => true,
            };
            armed && target(m, s).is_some()
        }
        Goal::EatGrass { .. } => {
            let chance = if m.is_baby() { 50 } else { 1000 };
            if m.below(chance) != 0 {
                return false;
            }
            let c = cell_of(b.pos);
            s.world.grazing(c[0], c[1], c[2]) != Graze::None
        }
        Goal::Tempt {
            cooldown, player, ..
        } => {
            if *cooldown > 0 {
                *cooldown -= 1;
                return false;
            }
            let Some(tag) = m.species.tempt_tag() else {
                return false;
            };
            let found = s
                .nearest(b.pos, 10.0, |p| p.present && holds_food(p, s, tag))
                .map(|p| p.id);
            *player = found;
            found.is_some()
        }
        Goal::Breed { partner, timer, .. } => {
            if !m.in_love() {
                return false;
            }
            let me = b.id;
            let found = s
                .nearest_mob(b.pos, 8.0, |o| o.id != me && partner_ok(m, o))
                .map(|o| o.id);
            *partner = found;
            *timer = 0;
            found.is_some()
        }
        Goal::FollowParent { parent, delay, .. } => {
            if !m.is_baby() {
                return false;
            }
            if *delay > 0 {
                *delay -= 1;
                return false;
            }
            *delay = 40;
            let me = b.id;
            let kind = m.species;
            let found = s
                .nearest_mob(b.pos, 8.0, |o| {
                    o.id != me && o.species == Some(kind) && !o.baby
                })
                .filter(|o| dist_sq(b.pos, o.pos) >= 9.0)
                .map(|o| o.id);
            *parent = found;
            found.is_some()
        }
        Goal::Sit => m.sitting && m.is_tame() && !body_wet(m, b, s),
        Goal::FollowOwner { start, .. } => {
            if m.sitting || !m.riders.is_empty() {
                return false;
            }
            owner_of(m, s).is_some_and(|o| dist_sq(b.pos, o.pos) >= *start * *start)
        }
        Goal::FleeSun { .. } => {
            if m.burning == 0 || !s.sunny || m.gear[slot::HEAD].is_some() {
                return false;
            }
            let from = cell_of(b.pos);
            let p = params(m);
            for _ in 0..10 {
                let dx = m.below(21) as i32 - 10;
                let dz = m.below(21) as i32 - 10;
                let dy = m.below(7) as i32 - 3;
                let c = [from[0] + dx, from[1] + dy, from[2] + dz];
                if standable(s.world, c, &p) && s.world.sky_light(c[0], c[1] + 1, c[2]) < 15 {
                    m.nav.stop();
                    m.goto = Some((center(c), 1.0));
                    return true;
                }
            }
            false
        }
        Goal::Hop { .. } => true,
        Goal::Hover { .. } => m.below(20) == 0 || m.nav.goal.is_none() && m.below(5) == 0,
        Goal::Swoop { .. } => true,
        Goal::Sleep => {
            s.sunny
                && m.target.is_none()
                && m.revenge.is_none()
                && b.on_ground
                && m.below(140) == 0
                && s.nearest(b.pos, 6.0, |p| p.targetable).is_none()
        }
        Goal::Admire => m.charge > 0,
        Goal::HurtBy { .. } => {
            let Some((id, _)) = m.revenge else {
                return false;
            };
            if m.owner
                .is_some_and(|o| s.player(id).is_some_and(|p| p.uuid == o))
            {
                return false;
            }
            s.seen(id).is_some()
        }
        Goal::Nearest {
            range,
            fickle,
            spare_gold,
            ..
        } => {
            if m.below(10) != 0 {
                return false;
            }
            if *fickle && light_at(s, b.pos) >= 8 {
                return false;
            }
            let (range, spare) = (*range, *spare_gold);
            let found = s
                .nearest(b.pos, range, |p| {
                    p.targetable && !(spare && p.gold) && sees_player(m, b, s, p)
                })
                .map(|p| p.id);
            found.is_some_and(|id| {
                m.target = Some(id);
                true
            })
        }
        Goal::Prey {
            kinds,
            villagers,
            wild_only,
            ..
        } => {
            if *wild_only && m.is_tame() || m.below(10) != 0 {
                return false;
            }
            let range = m.species.follow_range();
            let (kinds, villagers) = (*kinds, *villagers);
            let me = b.id;
            let eye_pos = eye(m, b.pos);
            let found = s
                .nearest_mob(b.pos, range, |o| {
                    o.id != me
                        && match o.species {
                            Some(k) => kinds.contains(&k),
                            None => villagers,
                        }
                        && line_clear(
                            s.world,
                            eye_pos,
                            [o.pos[0], o.pos[1] + o.height * 0.85, o.pos[2]],
                        )
                })
                .map(|o| o.id);
            found.is_some_and(|id| {
                m.target = Some(id);
                true
            })
        }
        Goal::OwnerHurtBy | Goal::OwnerHurt => {
            if !m.is_tame() || m.sitting {
                return false;
            }
            let Some(o) = owner_of(m, s) else {
                return false;
            };
            let id = if matches!(g, Goal::OwnerHurtBy) {
                o.hurt_by
            } else {
                o.hit
            };
            let Some(id) = id else {
                return false;
            };
            // Not the owner's other pets.
            let own = m.owner;
            if s.mob(id)
                .is_some_and(|x| x.owner.is_some() && x.owner == own)
            {
                return false;
            }
            if s.seen(id).is_none() {
                return false;
            }
            m.target = Some(id);
            true
        }
        Goal::Stare => {
            let e = eye(m, b.pos);
            let found = s
                .nearest(b.pos, 64.0, |p| p.targetable && looks_at(p, e, s))
                .map(|p| p.id);
            found.is_some_and(|id| {
                m.target = Some(id);
                true
            })
        }
    }
}

/// Whether a player looks an enderman in the eyes (no pumpkin on).
fn looks_at(p: &Target, eye_pos: [f64; 3], s: &Senses) -> bool {
    if p.head >= 0 && s.items.name(p.head) == "minecraft:carved_pumpkin" {
        return false;
    }
    let from = [p.pos[0], p.pos[1] + PLAYER_EYE, p.pos[2]];
    let (yaw, pitch) = (
        f64::from(p.yaw).to_radians(),
        f64::from(p.pitch).to_radians(),
    );
    let look = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let to = [
        eye_pos[0] - from[0],
        eye_pos[1] - from[1],
        eye_pos[2] - from[2],
    ];
    let d = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
    if d < 1e-6 {
        return false;
    }
    let dot = (look[0] * to[0] + look[1] * to[1] + look[2] * to[2]) / d;
    dot > 1.0 - 0.025 / d && line_clear(s.world, from, eye_pos)
}

fn owner_of<'a>(m: &Mob, s: &'a Senses) -> Option<&'a Target> {
    let o = m.owner?;
    s.players.iter().find(|p| p.uuid == o && p.present)
}

fn body_wet(m: &Mob, b: &Body, s: &Senses) -> bool {
    let y = b.pos[1] + m.size().1 * 0.4;
    s.world.liquid(
        b.pos[0].floor() as i32,
        y.floor() as i32,
        b.pos[2].floor() as i32,
    ) != Liquid::None
}

fn can_continue(g: &mut Goal, m: &mut Mob, b: &mut Body, s: &Senses) -> bool {
    match g {
        Goal::Float => body_wet(m, b, s),
        Goal::Panic { .. } | Goal::Stroll { .. } | Goal::FleeSun { .. } => !m.nav.is_done(),
        Goal::LookAtPlayer { left, range } => {
            *left > 0 && s.nearest(b.pos, *range, |p| p.present).is_some()
        }
        Goal::RandomLook { left, .. } => *left > 0,
        Goal::Melee { fickle, .. } => {
            if *fickle && m.below(100) == 0 && light_at(s, b.pos) >= 8 {
                m.target = None;
                return false;
            }
            can_use(g, m, b, s)
        }
        Goal::Retreat { range, .. } => {
            target(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) < (*range + 1.0).powi(2))
        }
        Goal::Avoid { .. } => !m.nav.is_done(),
        Goal::Swell => true,
        Goal::Leap { .. } => !b.on_ground,
        Goal::Ranged { .. } => can_use(g, m, b, s),
        Goal::EatGrass { left } => *left > 0,
        Goal::Tempt { player, .. } => {
            let tag = m.species.tempt_tag().unwrap_or("");
            player.and_then(|id| s.player(id)).is_some_and(|p| {
                p.present && holds_food(p, s, tag) && dist_sq(b.pos, p.pos) < 100.0
            })
        }
        Goal::Breed { partner, timer, .. } => {
            let me = &*m;
            *timer < 60
                && me.in_love()
                && partner
                    .and_then(|id| s.mob(id))
                    .is_some_and(|o| o.alive && o.in_love)
        }
        Goal::FollowParent { parent, .. } => {
            m.is_baby()
                && parent.and_then(|id| s.mob(id)).is_some_and(|o| {
                    let d = dist_sq(b.pos, o.pos);
                    o.alive && (9.0..=256.0).contains(&d)
                })
        }
        Goal::Sit => m.sitting,
        Goal::FollowOwner { stop, .. } => {
            !m.sitting && owner_of(m, s).is_some_and(|o| dist_sq(b.pos, o.pos) > *stop * *stop)
        }
        Goal::Hop { .. } | Goal::Swoop { .. } => true,
        Goal::Hover { .. } => !m.nav.is_done() && m.below(200) != 0,
        Goal::Sleep => s.sunny && m.revenge.is_none() && m.target.is_none(),
        Goal::Admire => m.charge > 0,
        Goal::HurtBy { .. } | Goal::OwnerHurtBy | Goal::OwnerHurt => {
            target(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) <= m.species.follow_range().powi(2))
        }
        Goal::Nearest { range, unseen, .. } => {
            let Some(t) = target(m, s) else {
                return false;
            };
            if !t.player || dist_sq(b.pos, t.pos) > *range * *range {
                return false;
            }
            if can_see(m, b, s, &t) {
                *unseen = 0;
            } else {
                *unseen += 1;
            }
            *unseen <= 60
        }
        Goal::Prey { unseen, .. } => {
            let Some(t) = target(m, s) else {
                return false;
            };
            if dist_sq(b.pos, t.pos) > m.species.follow_range().powi(2) {
                return false;
            }
            if can_see(m, b, s, &t) {
                *unseen = 0;
            } else {
                *unseen += 1;
            }
            *unseen <= 60
        }
        Goal::Stare => target(m, s).is_some(),
    }
}

fn start(g: &mut Goal, m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match g {
        Goal::Panic { speed } => {
            let away = m.revenge.and_then(|(id, _)| s.seen(id)).map(|p| p.pos);
            let from = cell_of(b.pos);
            if let Some(c) = random_spot(m, s, from, 5, 4, false, away) {
                m.nav.stop();
                m.goto = Some((center(c), *speed));
            }
        }
        Goal::Stroll {
            speed, avoid_water, ..
        } => {
            let from = cell_of(b.pos);
            let avoid = *avoid_water;
            if let Some(c) = random_spot(m, s, from, 10, 7, avoid, None) {
                m.nav.stop();
                m.goto = Some((center(c), *speed));
            }
        }
        Goal::LookAtPlayer { left, .. } => *left = 40 + m.below(40) as i32,
        Goal::RandomLook { left, angle } => {
            *angle = m.next_f64() * std::f64::consts::TAU;
            *left = 20 + m.below(20) as i32;
        }
        Goal::EatGrass { left } => {
            *left = 40;
            m.nav.stop();
        }
        Goal::Swell | Goal::Sit | Goal::Admire => m.nav.stop(),
        Goal::Ranged { timer, seen, .. } => {
            *timer = -1;
            *seen = 0;
        }
        Goal::Melee { .. } => {
            if m.species.zombie() {
                m.arms_up = true;
                m.dirty = true;
            }
        }
        Goal::Sleep => {
            m.sitting = true;
            m.dirty = true;
            m.nav.stop();
        }
        Goal::HurtBy { alert } => {
            if let Some((id, _)) = m.revenge {
                m.target = Some(id);
                if neutral(m.species) {
                    m.anger = 400 + m.below(400) as i32;
                    m.dirty = true;
                }
                if *alert {
                    out.push(MobEvent::Alert { target: id });
                }
            }
        }
        Goal::Stare => {
            m.anger = 400 + m.below(400) as i32;
            m.dirty = true;
            out.push(MobEvent::Sound {
                name: "entity.enderman.stare".into(),
                volume: 1.0,
                pitch: 1.0,
            });
        }
        _ => {}
    }
}

/// Neutral mobs stay angry for a while after being hurt.
fn neutral(k: Species) -> bool {
    matches!(
        k,
        Species::Wolf | Species::Enderman | Species::ZombifiedPiglin | Species::Piglin
    )
}

fn stop(g: &mut Goal, m: &mut Mob) {
    match g {
        Goal::Panic { .. }
        | Goal::Stroll { .. }
        | Goal::Avoid { .. }
        | Goal::FleeSun { .. }
        | Goal::FollowOwner { .. }
        | Goal::FollowParent { .. }
        | Goal::Breed { .. } => {
            m.nav.stop();
        }
        Goal::Melee { .. } => {
            m.nav.stop();
            if m.arms_up {
                m.arms_up = false;
                m.dirty = true;
            }
        }
        Goal::Tempt { cooldown, .. } => {
            *cooldown = 100;
            m.nav.stop();
        }
        Goal::Swell => {
            m.swell_dir = -1;
            m.dirty = true;
        }
        Goal::Sleep => {
            m.sitting = false;
            m.dirty = true;
        }
        Goal::Ranged { kind, .. } => {
            if *kind == Ranged::Fireball && m.charge != 0 {
                m.charge = 0;
                m.dirty = true;
            }
        }
        Goal::Nearest { unseen, .. } | Goal::Prey { unseen, .. } => {
            *unseen = 0;
            m.target = None;
        }
        Goal::HurtBy { .. } | Goal::OwnerHurtBy | Goal::OwnerHurt | Goal::Stare
            if m.anger == 0 || !neutral(m.species) =>
        {
            m.target = None;
        }
        _ => {}
    }
}

fn tick_goal(g: &mut Goal, m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match g {
        Goal::Float => {
            if m.next_f64() < 0.8 {
                m.jump = true;
            }
        }
        Goal::Panic { speed } | Goal::Stroll { speed, .. } | Goal::FleeSun { speed } => {
            if let Some(c) = m.nav.goal {
                m.goto = Some((center(c), *speed));
            }
        }
        Goal::LookAtPlayer { left, range } => {
            *left -= 1;
            if let Some(p) = s.nearest(b.pos, *range, |p| p.present) {
                m.look = Some([p.pos[0], p.pos[1] + PLAYER_EYE, p.pos[2]]);
            }
        }
        Goal::RandomLook { left, angle } => {
            *left -= 1;
            let e = eye(m, b.pos);
            m.look = Some([e[0] + angle.cos() * 8.0, e[1], e[2] + angle.sin() * 8.0]);
        }
        Goal::Melee { speed, .. } => {
            let Some(t) = target(m, s) else {
                return;
            };
            look_at(m, &t);
            m.goto = Some((t.pos, *speed));
            if m.attack_cooldown <= 0
                && dist_sq(b.pos, t.pos) <= reach(m, &t)
                && (b.pos[1] - t.pos[1]).abs() < 2.5
            {
                m.attack_cooldown = 20;
                out.push(MobEvent::Swing);
                let damage = m.species.attack_damage();
                out.push(MobEvent::Attack {
                    target: t.id,
                    damage,
                });
            }
        }
        Goal::Retreat { speed, .. } => {
            let Some(t) = target(m, s) else {
                return;
            };
            if m.nav.is_done() || m.nav.goal.is_none() {
                let from = cell_of(b.pos);
                if let Some(c) = random_spot(m, s, from, 6, 3, true, Some(t.pos)) {
                    m.nav.stop();
                    m.goto = Some((center(c), *speed));
                }
            } else if let Some(c) = m.nav.goal {
                m.goto = Some((center(c), *speed));
            }
        }
        Goal::Avoid {
            walk,
            sprint,
            threat,
            ..
        } => {
            let close = threat
                .and_then(|id| {
                    s.seen(id).or_else(|| {
                        s.mob(id).map(|o| Seen {
                            id,
                            pos: o.pos,
                            width: o.width,
                            eye: o.height,
                            player: false,
                        })
                    })
                })
                .is_some_and(|t| dist_sq(b.pos, t.pos) < 49.0);
            if let Some(c) = m.nav.goal {
                m.goto = Some((center(c), if close { *sprint } else { *walk }));
            }
        }
        Goal::Swell => {
            m.nav.stop();
            let near =
                target(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) <= 49.0 && can_see(m, b, s, &t));
            let dir = if m.ignited || near { 1 } else { -1 };
            if dir != m.swell_dir {
                m.swell_dir = dir;
                m.dirty = true;
            }
            if let Some(t) = target(m, s) {
                look_at(m, &t);
            }
        }
        Goal::Leap { up } => {
            let Some(t) = target(m, s) else {
                return;
            };
            look_at(m, &t);
            if b.on_ground {
                let (dx, dz) = (t.pos[0] - b.pos[0], t.pos[2] - b.pos[2]);
                let len = (dx * dx + dz * dz).sqrt();
                if len > 1e-7 {
                    b.vel[0] += dx / len * 0.4 + b.vel[0] * 0.2;
                    b.vel[2] += dz / len * 0.4 + b.vel[2] * 0.2;
                }
                b.vel[1] = *up;
                b.on_ground = false;
            }
        }
        Goal::Ranged {
            kind,
            interval,
            range,
            timer,
            seen,
        } => ranged(m, b, s, *kind, *interval, *range, timer, seen, out),
        Goal::EatGrass { left } => {
            *left -= 1;
            m.nav.stop();
            if *left == 36 {
                out.push(MobEvent::Status(10));
            }
            if *left == 4 {
                let c = cell_of(b.pos);
                match s.world.grazing(c[0], c[1], c[2]) {
                    Graze::Plant => out.push(MobEvent::Graze {
                        cell: c,
                        block: false,
                    }),
                    Graze::Block => out.push(MobEvent::Graze {
                        cell: [c[0], c[1] - 1, c[2]],
                        block: true,
                    }),
                    Graze::None => {}
                }
                if m.sheared {
                    m.sheared = false;
                    m.dirty = true;
                }
                // A grazing lamb grows up faster.
                if m.is_baby() {
                    m.baby_age = (m.baby_age + 1200).min(0);
                }
            }
            let e = eye(m, b.pos);
            let yaw = f64::from(m.yaw).to_radians();
            m.look = Some([e[0] - yaw.sin(), e[1] - 0.8, e[2] + yaw.cos()]);
        }
        Goal::Tempt { speed, player, .. } => {
            let Some(p) = player.and_then(|id| s.player(id)) else {
                return;
            };
            m.look = Some([p.pos[0], p.pos[1] + PLAYER_EYE, p.pos[2]]);
            if dist_sq(b.pos, p.pos) < 6.25 {
                m.nav.stop();
            } else {
                m.goto = Some((p.pos, *speed));
            }
        }
        Goal::Breed {
            speed,
            partner,
            timer,
        } => {
            let Some(o) = partner.and_then(|id| s.mob(id)) else {
                return;
            };
            m.look = Some([o.pos[0], o.pos[1] + o.height * 0.85, o.pos[2]]);
            m.goto = Some((o.pos, *speed));
            *timer += 1;
            if *timer >= 60 && dist_sq(b.pos, o.pos) < 9.0 {
                out.push(MobEvent::Breed { partner: o.id });
            }
        }
        Goal::FollowParent { speed, parent, .. } => {
            if let Some(o) = parent.and_then(|id| s.mob(id)) {
                m.goto = Some((o.pos, *speed));
            }
        }
        Goal::Sit => {
            m.nav.stop();
        }
        Goal::FollowOwner { speed, .. } => {
            let Some(o) = owner_of(m, s) else {
                return;
            };
            let opos = o.pos;
            m.look = Some([opos[0], opos[1] + PLAYER_EYE, opos[2]]);
            if dist_sq(b.pos, opos) >= 144.0 {
                if teleport_near(m, b, s, opos, 2) {
                    out.push(MobEvent::Teleported);
                }
            } else {
                m.goto = Some((opos, *speed));
            }
        }
        Goal::Hop { delay } => hop(m, b, s, delay, out),
        Goal::Hover { range, rise } => {
            if m.nav.goal.is_none() || m.below(40) == 0 {
                let center_at =
                    target(m, s).map_or(b.pos, |t| [t.pos[0], t.pos[1] + *rise, t.pos[2]]);
                if let Some(p) = random_air(m, s, center_at, *range, (*range / 2.0).max(2.0)) {
                    m.nav.goal = Some(cell_of(p));
                }
            }
            if let Some(c) = m.nav.goal {
                let p = center(c);
                if dist_sq(b.pos, p) < 2.0 {
                    m.nav.goal = None;
                } else {
                    m.goto = Some(([p[0], p[1] + 0.5, p[2]], 1.0));
                }
            }
            if let Some(t) = target(m, s) {
                look_at(m, &t);
            }
        }
        Goal::Swoop { timer, angle } => swoop(m, b, s, timer, angle, out),
        Goal::Sleep | Goal::Admire => m.nav.stop(),
        Goal::HurtBy { .. } | Goal::Nearest { .. } | Goal::Prey { .. } => {}
        Goal::OwnerHurtBy | Goal::OwnerHurt | Goal::Stare => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn ranged(
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    kind: Ranged,
    interval: i32,
    range: f64,
    timer: &mut i32,
    seen: &mut i32,
    out: &mut Vec<MobEvent>,
) {
    let Some(t) = target(m, s) else {
        return;
    };
    let visible = can_see(m, b, s, &t);
    look_at(m, &t);
    if visible {
        *seen = (*seen).max(0) + 1;
    } else {
        *seen = (*seen).min(0) - 1;
    }
    let d = dist_sq(b.pos, t.pos);
    let aim = eye_of(&t);
    match kind {
        Ranged::Bow => {
            if d <= range * range && *seen >= 20 {
                m.nav.stop();
            } else {
                m.goto = Some((t.pos, 1.0));
            }
            // Draw for a second, then loose; wait the interval between.
            if *timer > 0 {
                *timer -= 1;
            } else if visible {
                *timer -= 1;
                if *timer <= -20 {
                    let interval = if s.difficulty >= 3 {
                        interval
                    } else {
                        interval * 2
                    };
                    *timer = interval;
                    out.push(MobEvent::Shoot {
                        target: t.id,
                        aim,
                        missile: Missile::Arrow {
                            damage: 2.0,
                            slowness: m.species == Species::Stray,
                        },
                    });
                    out.push(MobEvent::Sound {
                        name: "entity.skeleton.shoot".into(),
                        volume: 1.0,
                        pitch: 1.0 / (m.next_f64() as f32 * 0.4 + 0.8),
                    });
                }
            } else if *seen < -60 {
                *timer = -1;
            }
        }
        Ranged::Trident | Ranged::Potion => {
            if d <= range * range && *seen >= 5 {
                m.nav.stop();
            } else {
                m.goto = Some((t.pos, 1.0));
            }
            *timer -= 1;
            if *timer == 0 && visible && d <= range * range {
                let missile = if kind == Ranged::Trident {
                    Missile::Trident
                } else if d > 64.0 {
                    Missile::Potion("minecraft:slowness")
                } else if m.below(4) == 0 && d < 9.0 {
                    Missile::Potion("minecraft:weakness")
                } else if m.below(2) == 0 {
                    Missile::Potion("minecraft:poison")
                } else {
                    Missile::Potion("minecraft:harming")
                };
                out.push(MobEvent::Shoot {
                    target: t.id,
                    aim,
                    missile,
                });
                out.push(MobEvent::Swing);
                *timer = interval;
            } else if *timer < 0 {
                *timer = interval / 2 + m.below(interval as u32 / 2 + 1) as i32;
            }
        }
        Ranged::Fireball => {
            let before = m.charge;
            if d < 64.0 * 64.0 && visible {
                m.charge += 1;
                if m.charge == 10 {
                    out.push(MobEvent::Sound {
                        name: "entity.ghast.warn".into(),
                        volume: 10.0,
                        pitch: (m.next_f64() - m.next_f64()) as f32 * 0.2 + 1.0,
                    });
                }
                if m.charge == 20 {
                    out.push(MobEvent::Shoot {
                        target: t.id,
                        aim,
                        missile: Missile::Fireball { power: 1 },
                    });
                    out.push(MobEvent::Sound {
                        name: "entity.ghast.shoot".into(),
                        volume: 10.0,
                        pitch: (m.next_f64() - m.next_f64()) as f32 * 0.2 + 1.0,
                    });
                    m.charge = -40;
                }
            } else if m.charge > 0 {
                m.charge -= 1;
            }
            if (before > 10) != (m.charge > 10) {
                m.dirty = true;
            }
        }
        Ranged::Blaze => {
            *timer -= 1;
            if d < 4.0 {
                if *timer <= 0 {
                    *timer = 20;
                    out.push(MobEvent::Attack {
                        target: t.id,
                        damage: m.species.attack_damage(),
                    });
                }
                m.goto = Some((t.pos, 1.0));
            } else if d < range * range && visible {
                // A burst: catch fire for three seconds, then three shots.
                if *timer <= 0 {
                    m.charge += 1;
                    match m.charge {
                        1 => {
                            *timer = 60;
                            m.dirty = true;
                        }
                        2..=4 => {
                            *timer = 6;
                            out.push(MobEvent::Shoot {
                                target: t.id,
                                aim,
                                missile: Missile::SmallFireball,
                            });
                        }
                        _ => {
                            *timer = 100;
                            m.charge = 0;
                            m.dirty = true;
                        }
                    }
                }
            } else {
                m.goto = Some((t.pos, 1.0));
            }
        }
    }
}

/// Slimes and magma cubes move only in hops.
fn hop(m: &mut Mob, b: &mut Body, s: &Senses, delay: &mut i32, out: &mut Vec<MobEvent>) {
    m.nav.stop();
    let t = target(m, s);
    if let Some(t) = &t {
        look_at(m, t);
        let (dx, dz) = (t.pos[0] - b.pos[0], t.pos[2] - b.pos[2]);
        m.yaw = yaw_to(dx, dz);
        // Big ones hurt what they touch.
        let damage = match m.species {
            Species::MagmaCube => m.size as f32 + 2.0,
            _ if m.size > 1 => m.size as f32,
            _ => 0.0,
        };
        let touch = (m.size().0 + t.width) / 2.0 + 0.6;
        if damage > 0.0 && m.attack_cooldown <= 0 && dist_sq(b.pos, t.pos) < touch * touch {
            m.attack_cooldown = 10;
            out.push(MobEvent::Attack {
                target: t.id,
                damage,
            });
            out.push(MobEvent::Sound {
                name: "entity.slime.attack".into(),
                volume: 1.0,
                pitch: (m.next_f64() - m.next_f64()) as f32 * 0.2 + 1.0,
            });
        }
    } else if m.below(60) == 0 {
        m.yaw += (m.next_f64() as f32 - 0.5) * 180.0;
    }
    m.head_yaw = m.yaw;
    if b.on_ground {
        b.vel[0] *= 0.5;
        b.vel[2] *= 0.5;
        *delay -= 1;
        if *delay <= 0 {
            *delay = m.below(20) as i32 + 10;
            if t.is_some() {
                *delay /= 3;
            }
            let yaw = f64::from(m.yaw).to_radians();
            let speed = m.speed_attr * 0.5;
            b.vel[0] = -yaw.sin() * speed;
            b.vel[2] = yaw.cos() * speed;
            b.vel[1] = 0.42
                + if m.species == Species::MagmaCube {
                    0.1 * f64::from(m.size)
                } else {
                    0.0
                };
            b.on_ground = false;
            let small = if m.size <= 1 { "_small" } else { "" };
            out.push(MobEvent::Sound {
                name: format!("entity.{}.jump{small}", m.species.path()),
                volume: m.species.volume() * m.size as f32,
                pitch: ((m.next_f64() - m.next_f64()) as f32 * 0.2 + 1.0) * 0.8,
            });
        }
    }
}

/// Phantoms circle above their target and swoop down on it.
fn swoop(
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    timer: &mut i32,
    angle: &mut f64,
    out: &mut Vec<MobEvent>,
) {
    let t = target(m, s);
    let anchor = t.map_or([b.pos[0], b.pos[1], b.pos[2]], |t| {
        [t.pos[0], t.pos[1] + 15.0, t.pos[2]]
    });
    if *timer < 0 {
        // Swooping.
        let Some(t) = t else {
            *timer = 100;
            return;
        };
        m.goto = Some((t.pos, 2.0));
        look_at(m, &t);
        let touch = (m.size().0 + t.width) / 2.0 + 0.5;
        if dist_sq(b.pos, t.pos) < touch * touch && m.attack_cooldown <= 0 {
            m.attack_cooldown = 20;
            out.push(MobEvent::Attack {
                target: t.id,
                damage: m.species.attack_damage() + m.size as f32,
            });
            *timer = 100 + m.below(100) as i32;
        }
        *timer += 1;
        if *timer >= 0 {
            *timer = 100 + m.below(100) as i32;
        }
        return;
    }
    *timer -= 1;
    if *timer == 0 && t.is_some() {
        *timer = -100;
        out.push(MobEvent::Sound {
            name: "entity.phantom.swoop".into(),
            volume: 10.0,
            pitch: 0.95 + m.next_f64() as f32 * 0.1,
        });
        return;
    }
    *angle += 0.05;
    let p = [
        anchor[0] + angle.cos() * 10.0,
        anchor[1],
        anchor[2] + angle.sin() * 10.0,
    ];
    m.goto = Some((p, 1.0));
}

/// Runs one list of goals (see the module comment of [`super`]).
pub fn run(list: &mut [Slot], m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    // Stop those that cannot go on.
    for slot in list.iter_mut() {
        if slot.running && !can_continue(&mut slot.goal, m, b, s) {
            slot.running = false;
            stop(&mut slot.goal, m);
        }
    }
    // Start those that can, by priority: every other tick, as vanilla's
    // selector looks for new goals (a stroll starts once in 240 ticks).
    let mut order: Vec<usize> = (0..list.len()).collect();
    if m.age % 2 == 1 && m.age > 1 {
        order.clear();
    }
    order.sort_by_key(|&i| list[i].priority);
    for i in order {
        if list[i].running {
            continue;
        }
        let (flags, prio) = (list[i].flags, list[i].priority);
        let blocked = list
            .iter()
            .any(|o| o.running && o.flags & flags != 0 && o.priority <= prio);
        if blocked || !can_use(&mut list[i].goal, m, b, s) {
            continue;
        }
        for o in list.iter_mut() {
            if o.running && o.flags & flags != 0 {
                o.running = false;
                stop(&mut o.goal, m);
            }
        }
        start(&mut list[i].goal, m, b, s, out);
        list[i].running = true;
    }
    for slot in list.iter_mut() {
        if slot.running {
            tick_goal(&mut slot.goal, m, b, s, out);
        }
    }
}
