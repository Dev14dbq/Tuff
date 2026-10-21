//! Mobs: living entities that decide what to do with a set of goals.
//!
//! The goal selector follows the public description of vanilla's
//! (minecraft.wiki, "Mob AI" and the pages of each mob). Every goal has a
//! priority (lower runs first) and claims some of the mob's controls (move,
//! look, jump). A goal that is not running starts when it can be used and every
//! running goal that shares a control with it has a lower priority (a larger
//! number); those are stopped. Target goals work the same way on their own
//! list and only choose whom the mob goes after.

use crate::Entity;
use crate::Kind;
use crate::nav::{Cell, Graze, Liquid, Nav, PathParams, Terrain, standable};
use crate::physics::{Aabb, move_entity};

pub const MOVE: u8 = 1;
pub const LOOK: u8 = 2;
pub const JUMP: u8 = 4;
pub const TARGET: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Species {
    Cow,
    Sheep,
    Pig,
    Chicken,
    Zombie,
    Skeleton,
    Creeper,
    Spider,
}

/// Spawn categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Creature,
    Monster,
}

impl Species {
    pub const ALL: [Species; 8] = [
        Species::Cow,
        Species::Sheep,
        Species::Pig,
        Species::Chicken,
        Species::Zombie,
        Species::Skeleton,
        Species::Creeper,
        Species::Spider,
    ];

    /// The registry name.
    pub fn id(self) -> &'static str {
        match self {
            Species::Cow => "minecraft:cow",
            Species::Sheep => "minecraft:sheep",
            Species::Pig => "minecraft:pig",
            Species::Chicken => "minecraft:chicken",
            Species::Zombie => "minecraft:zombie",
            Species::Skeleton => "minecraft:skeleton",
            Species::Creeper => "minecraft:creeper",
            Species::Spider => "minecraft:spider",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    /// The name death messages show.
    pub fn display_name(self) -> &'static str {
        match self {
            Species::Cow => "Cow",
            Species::Sheep => "Sheep",
            Species::Pig => "Pig",
            Species::Chicken => "Chicken",
            Species::Zombie => "Zombie",
            Species::Skeleton => "Skeleton",
            Species::Creeper => "Creeper",
            Species::Spider => "Spider",
        }
    }

    pub fn category(self) -> Category {
        match self {
            Species::Cow | Species::Sheep | Species::Pig | Species::Chicken => Category::Creature,
            _ => Category::Monster,
        }
    }

    /// Width and height of the collision box.
    pub fn size(self) -> (f64, f64) {
        match self {
            Species::Cow => (0.9, 1.4),
            Species::Sheep => (0.9, 1.3),
            Species::Pig => (0.9, 0.9),
            Species::Chicken => (0.4, 0.7),
            Species::Zombie => (0.6, 1.95),
            Species::Skeleton => (0.6, 1.99),
            Species::Creeper => (0.6, 1.7),
            Species::Spider => (1.4, 0.9),
        }
    }

    pub fn eye_height(self) -> f64 {
        match self {
            Species::Cow => 1.3,
            Species::Sheep => 1.235,
            Species::Pig => 0.765,
            Species::Chicken => 0.644,
            Species::Zombie => 1.74,
            Species::Skeleton => 1.74,
            Species::Creeper => 1.445,
            Species::Spider => 0.65,
        }
    }

    pub fn max_health(self) -> f32 {
        match self {
            Species::Cow | Species::Pig => 10.0,
            Species::Sheep => 8.0,
            Species::Chicken => 4.0,
            Species::Zombie | Species::Skeleton | Species::Creeper => 20.0,
            Species::Spider => 16.0,
        }
    }

    /// The `movement_speed` attribute.
    pub fn speed(self) -> f64 {
        match self {
            Species::Cow => 0.2,
            Species::Sheep => 0.23,
            Species::Pig | Species::Chicken | Species::Skeleton | Species::Creeper => 0.25,
            Species::Zombie => 0.23,
            Species::Spider => 0.3,
        }
    }

    /// The `follow_range` attribute.
    pub fn follow_range(self) -> f64 {
        match self {
            Species::Zombie => 35.0,
            _ => 16.0,
        }
    }

    /// The `attack_damage` attribute at normal difficulty (0 for those that
    /// do not bite).
    pub fn attack_damage(self) -> f32 {
        match self {
            Species::Zombie => 3.0,
            Species::Spider => 2.0,
            _ => 0.0,
        }
    }

    /// Whether sunlight sets it on fire.
    pub fn burns_in_sun(self) -> bool {
        matches!(self, Species::Zombie | Species::Skeleton)
    }

    /// Blocks of headroom a path through the world must leave.
    pub fn body_height(self) -> i32 {
        self.size().1.ceil() as i32
    }

    /// Whether falls hurt it.
    pub fn takes_fall_damage(self) -> bool {
        self != Species::Chicken
    }

    fn brain(self) -> Brain {
        use Goal::*;
        let slot = |priority, flags, goal| Slot {
            priority,
            flags,
            goal,
            running: false,
        };
        let float = slot(0, JUMP, Float);
        let look = |p, range| slot(p, LOOK, LookAtPlayer { range, left: 0 });
        let random_look = |p| {
            slot(
                p,
                MOVE | LOOK,
                RandomLook {
                    left: 0,
                    angle: 0.0,
                },
            )
        };
        let stroll = |p, speed, avoid_water| {
            slot(
                p,
                MOVE,
                Stroll {
                    speed,
                    interval: 120,
                    avoid_water,
                },
            )
        };
        let panic = |speed| slot(1, MOVE, Panic { speed });
        let hurt_by = |p| slot(p, TARGET, HurtBy);
        let nearest = |p, fickle| {
            slot(
                p,
                TARGET,
                Nearest {
                    range: self.follow_range(),
                    fickle,
                    unseen: 0,
                },
            )
        };
        let melee = |p, fickle| slot(p, MOVE | LOOK, Melee { speed: 1.0, fickle });
        let (goals, targets) = match self {
            Species::Cow | Species::Pig => (
                vec![
                    float,
                    panic(if self == Species::Cow { 2.0 } else { 1.25 }),
                    stroll(6, 1.0, true),
                    look(7, 6.0),
                    random_look(8),
                ],
                vec![],
            ),
            Species::Sheep => (
                vec![
                    float,
                    panic(1.25),
                    slot(5, MOVE | LOOK | JUMP, EatGrass { left: 0 }),
                    stroll(6, 1.0, true),
                    look(7, 6.0),
                    random_look(8),
                ],
                vec![],
            ),
            Species::Chicken => (
                vec![
                    float,
                    panic(1.4),
                    stroll(6, 1.0, true),
                    look(7, 6.0),
                    random_look(8),
                ],
                vec![],
            ),
            Species::Zombie => (
                vec![
                    float,
                    melee(2, false),
                    stroll(7, 1.0, true),
                    look(8, 8.0),
                    random_look(8),
                ],
                vec![hurt_by(1), nearest(2, false)],
            ),
            Species::Skeleton => (
                vec![
                    float,
                    slot(
                        3,
                        MOVE,
                        Avoid {
                            range: 4.0,
                            speed: 1.2,
                        },
                    ),
                    slot(
                        4,
                        MOVE | LOOK,
                        Bow {
                            cooldown: 20,
                            drawn: 0,
                            range: 15.0,
                        },
                    ),
                    stroll(5, 1.0, true),
                    look(6, 8.0),
                    random_look(6),
                ],
                vec![hurt_by(1), nearest(2, false)],
            ),
            Species::Creeper => (
                vec![
                    Slot {
                        priority: 1,
                        ..float
                    },
                    slot(2, MOVE, Swell),
                    melee(4, false),
                    stroll(5, 0.8, true),
                    look(6, 8.0),
                    random_look(6),
                ],
                vec![nearest(1, false), hurt_by(2)],
            ),
            Species::Spider => (
                vec![
                    Slot {
                        priority: 1,
                        ..float
                    },
                    slot(3, MOVE | JUMP, Leap),
                    melee(4, true),
                    stroll(5, 0.8, true),
                    look(6, 8.0),
                    random_look(6),
                ],
                vec![hurt_by(1), nearest(2, true)],
            ),
        };
        Brain { goals, targets }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    pub priority: u8,
    pub flags: u8,
    pub goal: Goal,
    pub running: bool,
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
    Avoid { range: f64, speed: f64 },
    /// Creeper: fuse while the target is close.
    Swell,
    /// Spider: pounce at a target a few blocks away.
    Leap,
    /// Shoot arrows from a distance.
    Bow {
        cooldown: u32,
        drawn: u32,
        range: f64,
    },
    /// Sheep: graze on grass.
    EatGrass { left: i32 },
    // Target goals.
    /// Go after whoever hurt the mob.
    HurtBy,
    /// Go after the nearest player.
    Nearest {
        range: f64,
        /// Spiders lose interest in the light.
        fickle: bool,
        /// Ticks the target has been out of sight.
        unseen: i32,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Brain {
    pub goals: Vec<Slot>,
    pub targets: Vec<Slot>,
}

/// A player as mobs see them.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    /// Entity id.
    pub id: i32,
    pub uuid: u128,
    /// Feet.
    pub pos: [f64; 3],
    /// Whether mobs may go after them (survival or adventure, alive).
    pub targetable: bool,
}

pub const PLAYER_EYE: f64 = 1.62;

/// What a mob can see and sense this tick.
#[derive(Clone, Copy)]
pub struct Senses<'a> {
    pub world: &'a dyn Terrain,
    pub players: &'a [Target],
    /// Game time.
    pub tick: u64,
    /// 0 peaceful to 3 hard.
    pub difficulty: i32,
    /// Day and not raining: the sun burns.
    pub sunny: bool,
}

impl std::fmt::Debug for Senses<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Senses")
            .field("players", &self.players.len())
            .field("tick", &self.tick)
            .finish_non_exhaustive()
    }
}

impl Senses<'_> {
    fn player(&self, id: i32) -> Option<&Target> {
        self.players.iter().find(|p| p.id == id)
    }

    /// The nearest player passing `ok`, within `range`.
    pub fn nearest(
        &self,
        from: [f64; 3],
        range: f64,
        ok: impl Fn(&Target) -> bool,
    ) -> Option<&Target> {
        self.players
            .iter()
            .filter(|p| ok(p))
            .map(|p| (dist_sq(from, p.pos), p))
            .filter(|(d, _)| *d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p)
    }
}

/// What a mob did that the world must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum MobEvent {
    /// A bite or a blow at the player with this entity id.
    Attack { target: i32, damage: f32 },
    /// An arrow at the player with this entity id (no projectiles yet: a
    /// hit with this damage, if it lands).
    Shoot { target: i32, damage: f32 },
    /// The creeper blows up.
    Explode { power: f32 },
    /// A chicken lays an egg.
    Egg,
    /// A sheep grazed: the plant, or the grass block under it, goes.
    Graze { cell: Cell, block: bool },
    /// Entity event for clients (`EntityEvent` status).
    Status(u8),
    /// The mob took damage not from a player (burning, falling).
    Hurt { amount: f32, kind: &'static str },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mob {
    pub species: Species,
    pub health: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub head_yaw: f32,
    pub brain: Brain,
    pub nav: Nav,
    /// Whom it goes after: a player's entity id.
    pub target: Option<i32>,
    /// Who hurt it last and for how many more ticks that counts.
    pub revenge: Option<(i32, i32)>,
    /// Where goals want it to go this tick: the cell and speed multiplier.
    pub goto: Option<(Cell, f64)>,
    /// Where goals want it to look this tick.
    pub look: Option<[f64; 3]>,
    pub jump: bool,
    /// Ticks until it may be hurt again, and the damage of the hit that did.
    pub invulnerable: i32,
    pub last_damage: f32,
    pub burning: i32,
    pub attack_cooldown: i32,
    pub fall_distance: f64,
    /// Remaining ticks of the death animation; `Some` once health is gone.
    pub dying: Option<i32>,
    /// Never despawns (named, or it picked something up).
    pub persistent: bool,
    // Species state.
    /// Sheep: the wool colour (0..16) and whether it is shorn.
    pub color: u8,
    pub sheared: bool,
    /// Creeper: fuse ticks, their direction, and whether it was lit by flint.
    pub swell: i32,
    pub swell_dir: i32,
    pub ignited: bool,
    pub powered: bool,
    /// Chicken: ticks until an egg.
    pub egg_time: i32,
    pub age: u32,
    /// Spider: clinging to a wall.
    pub climbing: bool,
    /// Whether clients must be told about changed fields.
    pub dirty: bool,
    aggressive: bool,
    rng: u64,
}

impl Mob {
    pub fn new(species: Species, seed: u64) -> Self {
        let mut mob = Self {
            species,
            health: species.max_health(),
            yaw: 0.0,
            pitch: 0.0,
            head_yaw: 0.0,
            brain: species.brain(),
            nav: Nav::default(),
            target: None,
            revenge: None,
            goto: None,
            look: None,
            jump: false,
            invulnerable: 0,
            last_damage: 0.0,
            burning: 0,
            attack_cooldown: 0,
            fall_distance: 0.0,
            dying: None,
            persistent: false,
            color: 0,
            sheared: false,
            swell: 0,
            swell_dir: -1,
            ignited: false,
            powered: false,
            egg_time: 0,
            age: 0,
            climbing: false,
            dirty: false,
            aggressive: false,
            rng: seed | 1,
        };
        mob.yaw = (mob.next_f64() * 360.0) as f32;
        mob.head_yaw = mob.yaw;
        if species == Species::Chicken {
            mob.egg_time = mob.rand_range(6000, 12000);
        }
        mob
    }

    // -- random numbers (xorshift64*)

    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A whole number in `0..n`.
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (self.next_u64() >> 33) as u32 % n
        }
    }

    fn rand_range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1).max(1) as u32) as i32
    }

    pub fn is_dead(&self) -> bool {
        self.health <= 0.0
    }

    /// Hurts the mob. Returns whether the hit counted. Within the
    /// invulnerability window only a stronger hit goes through, for the
    /// difference.
    pub fn hurt(&mut self, amount: f32, attacker: Option<i32>) -> bool {
        if self.is_dead() || amount <= 0.0 {
            return false;
        }
        let amount = if self.invulnerable > 0 {
            if amount <= self.last_damage {
                return false;
            }
            let more = amount - self.last_damage;
            self.last_damage = amount;
            more
        } else {
            self.invulnerable = 20;
            self.last_damage = amount;
            amount
        };
        self.health = (self.health - amount).max(0.0);
        if let Some(by) = attacker {
            self.revenge = Some((by, 100));
        } else if self.species.category() == Category::Creature {
            // Hurt by something else: animals run all the same.
            self.revenge = Some((-1, 100));
        }
        self.dirty = true;
        true
    }

    /// Whether `Sheep::shear` etc. apply: a shorn or baby sheep gives
    /// nothing.
    pub fn can_shear(&self) -> bool {
        self.species == Species::Sheep && !self.sheared && !self.is_dead()
    }

    pub fn shear(&mut self) {
        self.sheared = true;
        self.dirty = true;
    }

    pub fn set_color(&mut self, color: u8) {
        self.color = color & 15;
        self.dirty = true;
    }

    fn set_aggressive(&mut self, on: bool) {
        if self.aggressive != on {
            self.aggressive = on;
            self.dirty = true;
        }
    }

    pub fn aggressive(&self) -> bool {
        self.aggressive
    }
}

// ---- the goal engine ------------------------------------------------------

/// What a mob moves with.
#[derive(Debug)]
pub struct Body {
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn cell_of(pos: [f64; 3]) -> Cell {
    [
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    ]
}

fn eye(m: &Mob, pos: [f64; 3]) -> [f64; 3] {
    [pos[0], pos[1] + m.species.eye_height(), pos[2]]
}

/// Whether nothing solid lies between two points (stepped along the line).
pub fn line_clear(w: &dyn Terrain, a: [f64; 3], b: [f64; 3]) -> bool {
    let d = dist_sq(a, b).sqrt();
    let steps = (d * 2.0).ceil() as i32;
    (1..steps).all(|i| {
        let t = f64::from(i) / f64::from(steps);
        let p = [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ];
        !w.solid(
            p[0].floor() as i32,
            p[1].floor() as i32,
            p[2].floor() as i32,
        )
    })
}

fn can_see(m: &Mob, b: &Body, s: &Senses, t: &Target) -> bool {
    line_clear(
        s.world,
        eye(m, b.pos),
        [t.pos[0], t.pos[1] + PLAYER_EYE, t.pos[2]],
    )
}

fn params(m: &Mob) -> PathParams {
    PathParams::walker(m.species.body_height())
}

/// A random standable cell near `from`, at most `h` away sideways and `v`
/// up or down; `towards` pulls the pick away from or to a point.
fn random_spot(
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
            // Keep to the side away from the threat.
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

fn target_of<'a>(m: &Mob, s: &'a Senses) -> Option<&'a Target> {
    s.player(m.target?).filter(|t| t.targetable)
}

fn can_use(g: &mut Goal, m: &mut Mob, b: &Body, s: &Senses) -> bool {
    match g {
        Goal::Float => {
            s.world.liquid(
                b.pos[0].floor() as i32,
                (b.pos[1] + 0.3).floor() as i32,
                b.pos[2].floor() as i32,
            ) != Liquid::None
        }
        Goal::Panic { .. } => {
            m.revenge.is_some() || m.burning > 0 && m.species.category() == Category::Creature
        }
        Goal::Stroll { interval, .. } => m.below(*interval) == 0,
        Goal::LookAtPlayer { range, .. } => {
            m.next_f64() < 0.02 && s.nearest(b.pos, *range, |p| p.targetable).is_some()
        }
        Goal::RandomLook { .. } => m.next_f64() < 0.02,
        Goal::Melee { .. } => target_of(m, s).is_some(),
        Goal::Avoid { range, .. } => {
            target_of(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) < *range * *range)
        }
        Goal::Swell => {
            m.ignited
                || target_of(m, s).is_some_and(|t| m.swell_dir > 0 || dist_sq(b.pos, t.pos) < 9.0)
        }
        Goal::Leap => {
            let Some(t) = target_of(m, s) else {
                return false;
            };
            let d = dist_sq(b.pos, t.pos);
            b.on_ground && (4.0..=16.0).contains(&d) && m.below(5) == 0
        }
        Goal::Bow { .. } => target_of(m, s).is_some(),
        Goal::EatGrass { .. } => {
            if m.below(1000) != 0 {
                return false;
            }
            let c = cell_of(b.pos);
            s.world.grazing(c[0], c[1], c[2]) != Graze::None
        }
        Goal::HurtBy => m
            .revenge
            .is_some_and(|(id, _)| s.player(id).is_some_and(|p| p.targetable)),
        Goal::Nearest { range, fickle, .. } => {
            if m.below(10) != 0 {
                return false;
            }
            if *fickle
                && s.world.light(
                    b.pos[0].floor() as i32,
                    b.pos[1].floor() as i32,
                    b.pos[2].floor() as i32,
                ) >= 8
            {
                return false;
            }
            let range = *range;
            let found = s
                .nearest(b.pos, range, |p| p.targetable && can_see(m, b, s, p))
                .map(|p| p.id);
            found.is_some_and(|id| {
                m.target = Some(id);
                true
            })
        }
    }
}

fn can_continue(g: &mut Goal, m: &mut Mob, b: &Body, s: &Senses) -> bool {
    match g {
        Goal::Float => can_use(g, m, b, s),
        Goal::Panic { .. } => !m.nav.is_done(),
        Goal::Stroll { .. } => !m.nav.is_done(),
        Goal::LookAtPlayer { left, range } => {
            *left > 0 && s.nearest(b.pos, *range, |p| p.targetable).is_some()
        }
        Goal::RandomLook { left, .. } => *left > 0,
        Goal::Melee { fickle, .. } => {
            if *fickle
                && m.below(100) == 0
                && s.world.light(
                    b.pos[0].floor() as i32,
                    b.pos[1].floor() as i32,
                    b.pos[2].floor() as i32,
                ) >= 8
            {
                m.target = None;
                return false;
            }
            target_of(m, s).is_some()
        }
        Goal::Avoid { range, .. } => {
            target_of(m, s).is_some_and(|t| dist_sq(b.pos, t.pos) < (*range + 1.0).powi(2))
        }
        Goal::Swell => true,
        Goal::Leap => !b.on_ground,
        Goal::Bow { .. } => target_of(m, s).is_some(),
        Goal::EatGrass { left } => *left > 0,
        Goal::HurtBy => target_of(m, s).is_some(),
        Goal::Nearest { range, unseen, .. } => {
            let Some(t) = target_of(m, s) else {
                return false;
            };
            if dist_sq(b.pos, t.pos) > *range * *range {
                return false;
            }
            if can_see(m, b, s, t) {
                *unseen = 0;
            } else {
                *unseen += 1;
            }
            *unseen <= 60
        }
    }
}

fn start(g: &mut Goal, m: &mut Mob, b: &Body, s: &Senses) {
    match g {
        Goal::Panic { speed } => {
            let away = m.revenge.and_then(|(id, _)| s.player(id)).map(|p| p.pos);
            let from = cell_of(b.pos);
            if let Some(c) = random_spot(m, s, from, 5, 4, false, away) {
                m.nav.stop();
                m.goto = Some((c, *speed));
            }
        }
        Goal::Stroll {
            speed, avoid_water, ..
        } => {
            let from = cell_of(b.pos);
            let avoid = *avoid_water;
            if let Some(c) = random_spot(m, s, from, 10, 7, avoid, None) {
                m.nav.stop();
                m.goto = Some((c, *speed));
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
        Goal::Swell => m.nav.stop(),
        Goal::Leap => {}
        Goal::Bow { drawn, .. } => *drawn = 0,
        _ => {}
    }
}

fn stop(g: &mut Goal, m: &mut Mob) {
    match g {
        Goal::Panic { .. } | Goal::Stroll { .. } | Goal::Melee { .. } | Goal::Avoid { .. } => {
            m.nav.stop();
        }
        Goal::Swell => {
            m.swell_dir = -1;
            m.dirty = true;
        }
        Goal::Nearest { unseen, .. } => {
            *unseen = 0;
            m.target = None;
        }
        Goal::HurtBy => {
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
        Goal::Panic { speed } => {
            if let Some(c) = m.nav.goal {
                m.goto = Some((c, *speed));
            }
        }
        Goal::Stroll { speed, .. } => {
            if let Some(c) = m.nav.goal {
                m.goto = Some((c, *speed));
            }
        }
        Goal::LookAtPlayer { left, range } => {
            *left -= 1;
            if let Some(p) = s.nearest(b.pos, *range, |p| p.targetable) {
                m.look = Some([p.pos[0], p.pos[1] + PLAYER_EYE, p.pos[2]]);
            }
        }
        Goal::RandomLook { left, angle } => {
            *left -= 1;
            let e = eye(m, b.pos);
            m.look = Some([e[0] + angle.cos() * 8.0, e[1], e[2] + angle.sin() * 8.0]);
        }
        Goal::Melee { speed, .. } => {
            let Some(t) = target_of(m, s).cloned() else {
                return;
            };
            m.look = Some([t.pos[0], t.pos[1] + PLAYER_EYE, t.pos[2]]);
            m.goto = Some((cell_of(t.pos), *speed));
            let (w, _) = m.species.size();
            let reach = (w * 2.0).powi(2) + 0.6;
            if m.attack_cooldown <= 0
                && dist_sq(b.pos, t.pos) <= reach
                && (b.pos[1] - t.pos[1]).abs() < 2.0
            {
                m.attack_cooldown = 20;
                let damage = m.species.attack_damage();
                if damage > 0.0 {
                    out.push(MobEvent::Attack {
                        target: t.id,
                        damage,
                    });
                }
            }
        }
        Goal::Avoid { speed, .. } => {
            let Some(t) = target_of(m, s).cloned() else {
                return;
            };
            if m.nav.is_done() || m.nav.goal.is_none() {
                let from = cell_of(b.pos);
                if let Some(c) = random_spot(m, s, from, 6, 3, true, Some(t.pos)) {
                    m.nav.stop();
                    m.goto = Some((c, *speed));
                }
            } else if let Some(c) = m.nav.goal {
                m.goto = Some((c, *speed));
            }
        }
        Goal::Swell => {
            m.nav.stop();
            let near = target_of(m, s)
                .is_some_and(|t| dist_sq(b.pos, t.pos) <= 49.0 && can_see(m, b, s, t));
            let dir = if m.ignited || near { 1 } else { -1 };
            if dir != m.swell_dir {
                m.swell_dir = dir;
                m.dirty = true;
            }
            if let Some(t) = target_of(m, s) {
                m.look = Some([t.pos[0], t.pos[1] + PLAYER_EYE, t.pos[2]]);
            }
        }
        Goal::Leap => {
            let Some(t) = target_of(m, s).cloned() else {
                return;
            };
            m.look = Some([t.pos[0], t.pos[1] + PLAYER_EYE, t.pos[2]]);
            if b.on_ground {
                let (dx, dz) = (t.pos[0] - b.pos[0], t.pos[2] - b.pos[2]);
                let len = (dx * dx + dz * dz).sqrt();
                if len > 1e-7 {
                    b.vel[0] += dx / len * 0.4 + b.vel[0] * 0.2;
                    b.vel[2] += dz / len * 0.4 + b.vel[2] * 0.2;
                }
                b.vel[1] = 0.4;
                b.on_ground = false;
            }
        }
        Goal::Bow {
            cooldown,
            drawn,
            range,
        } => {
            let Some(t) = target_of(m, s).cloned() else {
                return;
            };
            let see = can_see(m, b, s, &t);
            m.look = Some([t.pos[0], t.pos[1] + PLAYER_EYE, t.pos[2]]);
            let d = dist_sq(b.pos, t.pos);
            if d > *range * *range || !see {
                m.goto = Some((cell_of(t.pos), 1.0));
                *drawn = 0;
            } else {
                m.nav.stop();
                *drawn += 1;
                if *drawn >= *cooldown {
                    *drawn = 0;
                    // A skeleton's arrows get less sure with distance; harder
                    // difficulty aims better.
                    let sure =
                        (0.9 - d.sqrt() / 30.0 + 0.05 * f64::from(s.difficulty)).clamp(0.2, 0.95);
                    if m.next_f64() < sure {
                        out.push(MobEvent::Shoot {
                            target: t.id,
                            damage: 2.0 + 2.0 * m.next_f64() as f32,
                        });
                    }
                }
            }
        }
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
            }
            if let Some(a) = m.look {
                m.look = Some(a);
            } else {
                let e = eye(m, b.pos);
                let yaw = f64::from(m.yaw).to_radians();
                m.look = Some([e[0] - yaw.sin(), e[1] - 0.8, e[2] + yaw.cos()]);
            }
        }
        Goal::HurtBy => {
            // The attacker is the target while the grudge lasts.
            if let Some((id, _)) = m.revenge
                && s.player(id).is_some_and(|p| p.targetable)
            {
                m.target = Some(id);
            }
        }
        Goal::Nearest { .. } => {}
    }
}

/// Runs one list of goals (see the module comment).
fn run_goals(list: &mut [Slot], m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    // Stop those that cannot go on.
    for slot in list.iter_mut() {
        if slot.running && !can_continue(&mut slot.goal, m, b, s) {
            slot.running = false;
            stop(&mut slot.goal, m);
        }
    }
    // Start those that can, by priority.
    let mut order: Vec<usize> = (0..list.len()).collect();
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
        start(&mut list[i].goal, m, b, s);
        list[i].running = true;
    }
    for slot in list.iter_mut() {
        if slot.running {
            tick_goal(&mut slot.goal, m, b, s, out);
        }
    }
}

// ---- body: steering and physics -----------------------------------------

fn wrap_degrees(a: f32) -> f32 {
    let a = a % 360.0;
    if a > 180.0 {
        a - 360.0
    } else if a < -180.0 {
        a + 360.0
    } else {
        a
    }
}

fn approach(from: f32, to: f32, max: f32) -> f32 {
    from + wrap_degrees(to - from).clamp(-max, max)
}

fn yaw_to(dx: f64, dz: f64) -> f32 {
    (-dx).atan2(dz).to_degrees() as f32
}

const GRAVITY: f64 = 0.08;
/// Blocks a tick per point of the speed attribute.
const SPEED_SCALE: f64 = 0.9;

/// Turns where the mob wants to go into movement for this tick.
fn drive(m: &mut Mob, b: &mut Body, s: &Senses) -> Option<([f64; 2], f64)> {
    m.nav.tick(b.pos, s.tick as u32);
    let (cell, speed) = m.goto.take()?;
    let from = cell_of(b.pos);
    let p = params(m);
    m.nav.go_to(s.world, from, cell, &p);
    let reached = (m.species.size().0 / 2.0 + 0.2).max(0.4);
    let (tx, ty, tz) = match m.nav.waypoint(b.pos, reached) {
        Some(c) => (
            f64::from(c[0]) + 0.5,
            f64::from(c[1]),
            f64::from(c[2]) + 0.5,
        ),
        None => {
            // No path (or at its end): walk straight when the spot is near.
            let (cx, cz) = (f64::from(cell[0]) + 0.5, f64::from(cell[2]) + 0.5);
            if (cx - b.pos[0]).abs() + (cz - b.pos[2]).abs() > 3.0 {
                return None;
            }
            (cx, f64::from(cell[1]), cz)
        }
    };
    let (dx, dz) = (tx - b.pos[0], tz - b.pos[2]);
    let len = (dx * dx + dz * dz).sqrt();
    if len < 0.05 {
        return None;
    }
    if ty > b.pos[1] + 0.1 && b.on_ground {
        m.jump = true;
    }
    Some(([dx / len, dz / len], speed))
}

/// One tick of movement.
fn physics(
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    input: Option<([f64; 2], f64)>,
    out: &mut Vec<MobEvent>,
) {
    let (w, h) = m.species.size();
    let cell = cell_of(b.pos);
    let in_water = s.world.liquid(cell[0], cell[1], cell[2]) == Liquid::Water
        || s.world
            .liquid(cell[0], (b.pos[1] + 0.4).floor() as i32, cell[2])
            == Liquid::Water;
    let in_lava = s.world.liquid(cell[0], cell[1], cell[2]) == Liquid::Lava;
    if in_lava {
        b.vel[1] = b.vel[1] * 0.5 + 0.02;
        m.burning = m.burning.max(300);
    }
    let speed_attr = m.species.speed() * SPEED_SCALE;
    // Horizontal velocity.
    let friction = if b.on_ground { 0.546 } else { 0.91 };
    match input {
        Some((dir, speed)) => {
            let want = speed_attr * speed;
            let control = if b.on_ground || in_water { 0.5 } else { 0.1 };
            let target = [dir[0] * want, dir[1] * want];
            b.vel[0] += (target[0] - b.vel[0]) * control;
            b.vel[2] += (target[1] - b.vel[2]) * control;
            m.yaw = approach(m.yaw, yaw_to(dir[0], dir[1]), 30.0);
        }
        None => {
            b.vel[0] *= friction;
            b.vel[2] *= friction;
        }
    }
    // Vertical.
    if in_water {
        b.vel[0] *= 0.8;
        b.vel[2] *= 0.8;
        b.vel[1] = b.vel[1] * 0.8 - 0.02;
        if m.jump {
            b.vel[1] += 0.04;
        }
        m.fall_distance = 0.0;
    } else {
        if m.jump && b.on_ground {
            b.vel[1] = 0.42;
        }
        b.vel[1] = (b.vel[1] - GRAVITY) * 0.98;
        // A chicken flaps: it falls slowly.
        if m.species == Species::Chicken && b.vel[1] < -0.1 {
            b.vel[1] = -0.1;
        }
    }
    m.jump = false;
    let before_y = b.pos[1];
    let aabb = Aabb {
        min: [b.pos[0] - w / 2.0, b.pos[1], b.pos[2] - w / 2.0],
        max: [b.pos[0] + w / 2.0, b.pos[1] + h, b.pos[2] + w / 2.0],
    };
    let (moved, ground) = move_entity(aabb, b.vel, s.world);
    let blocked_h = (moved[0] - b.vel[0]).abs() > 1e-9 || (moved[2] - b.vel[2]).abs() > 1e-9;
    for (axis, &d) in moved.iter().enumerate() {
        b.pos[axis] += d;
        if (d - b.vel[axis]).abs() > 1e-9 {
            b.vel[axis] = 0.0;
        }
    }
    // Walking into a block with somewhere to go: hop over it.
    if blocked_h && ground_or(b, ground) && input.is_some() && !m.species_climbs() {
        m.jump = true;
    }
    m.climbing = m.species_climbs() && blocked_h;
    if m.climbing {
        b.vel[1] = b.vel[1].max(0.2);
    } else if blocked_h && s.world.climbable(cell[0], cell[1], cell[2]) {
        b.vel[1] = 0.2;
    }
    b.on_ground = ground;
    if !ground && b.pos[1] < before_y {
        m.fall_distance += before_y - b.pos[1];
    } else if ground {
        let fell = m.fall_distance;
        m.fall_distance = 0.0;
        if fell > 3.0 && m.species.takes_fall_damage() {
            out.push(MobEvent::Hurt {
                amount: (fell - 3.0).ceil() as f32,
                kind: "minecraft:fall",
            });
        }
    }
    if m.climbing {
        m.fall_distance = 0.0;
    }
}

fn ground_or(b: &Body, ground: bool) -> bool {
    b.on_ground || ground
}

impl Mob {
    fn species_climbs(&self) -> bool {
        self.species == Species::Spider
    }
}

/// Advances a mob one tick. Anything the world must act on goes to `out`.
pub fn tick(entity: &mut Entity, s: &Senses, out: &mut Vec<MobEvent>) {
    let Kind::Mob(m) = &mut entity.kind else {
        return;
    };
    let mut body = Body {
        pos: entity.pos,
        vel: entity.vel,
        on_ground: entity.on_ground,
    };
    m.age += 1;
    m.invulnerable = (m.invulnerable - 1).max(0);
    m.attack_cooldown = (m.attack_cooldown - 1).max(0);
    if let Some((_, ticks)) = &mut m.revenge {
        *ticks -= 1;
        if *ticks <= 0 {
            m.revenge = None;
        }
    }
    if let Some(left) = m.dying {
        // Dead: the body lies there for a second.
        m.dying = Some(left - 1);
        body.vel[0] *= 0.5;
        body.vel[2] *= 0.5;
        physics(m, &mut body, s, None, &mut Vec::new());
        entity.pos = body.pos;
        entity.vel = body.vel;
        entity.on_ground = body.on_ground;
        if left <= 1 {
            entity.removed = true;
        }
        return;
    }
    if m.is_dead() {
        return;
    }
    if body.pos[1] < -64.0 {
        m.health = 0.0;
        m.dirty = true;
        return;
    }
    // Sun and fire.
    if m.species.burns_in_sun() {
        let c = cell_of(body.pos);
        let head = [
            c[0],
            (body.pos[1] + m.species.size().1).floor() as i32,
            c[2],
        ];
        if s.sunny
            && s.world.sky_light(head[0], head[1], head[2]) >= 15
            && s.world.liquid(c[0], c[1], c[2]) == Liquid::None
        {
            m.burning = m.burning.max(160);
        }
    }
    if m.burning > 0 {
        let c = cell_of(body.pos);
        if s.world.liquid(c[0], c[1], c[2]) == Liquid::Water {
            m.burning = 0;
        } else {
            m.burning -= 1;
            if m.age % 20 == 0 {
                out.push(MobEvent::Hurt {
                    amount: 1.0,
                    kind: "minecraft:on_fire",
                });
            }
        }
    }
    // Chickens lay eggs.
    if m.species == Species::Chicken {
        m.egg_time -= 1;
        if m.egg_time <= 0 {
            m.egg_time = m.rand_range(6000, 12000);
            out.push(MobEvent::Egg);
        }
    }
    // Creepers.
    if m.species == Species::Creeper {
        let before = m.swell;
        if m.swell_dir > 0 || m.ignited {
            m.swell += 1;
        } else {
            m.swell = (m.swell - 1).max(0);
        }
        if m.swell != before && (before == 0 || m.swell == 0) {
            m.dirty = true;
        }
        if m.swell >= 30 {
            out.push(MobEvent::Explode {
                power: if m.powered { 6.0 } else { 3.0 },
            });
            return;
        }
    }
    // Goals.
    m.look = None;
    let mut brain = std::mem::take(&mut m.brain);
    run_goals(&mut brain.targets, m, &mut body, s, out);
    run_goals(&mut brain.goals, m, &mut body, s, out);
    m.brain = brain;
    let aggressive = m.target.is_some();
    m.set_aggressive(aggressive);
    let input = drive(m, &mut body, s);
    physics(m, &mut body, s, input, out);
    // Where it looks: at the goal's point, else the way it goes.
    let eye_pos = eye(m, body.pos);
    if let Some(p) = m.look {
        let (dx, dy, dz) = (p[0] - eye_pos[0], p[1] - eye_pos[1], p[2] - eye_pos[2]);
        let flat = (dx * dx + dz * dz).sqrt();
        m.head_yaw = approach(m.head_yaw, yaw_to(dx, dz), 30.0);
        m.pitch = approach(m.pitch, (-dy).atan2(flat).to_degrees() as f32, 20.0);
        // The body follows the head when it turns far.
        let off = wrap_degrees(m.head_yaw - m.yaw);
        if off.abs() > 50.0 {
            m.yaw = approach(m.yaw, m.head_yaw, 10.0);
        }
    } else {
        m.head_yaw = approach(m.head_yaw, m.yaw, 10.0);
        m.pitch = approach(m.pitch, 0.0, 10.0);
    }
    entity.pos = body.pos;
    entity.vel = body.vel;
    entity.on_ground = body.on_ground;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::Collide;

    struct Flat;
    impl Collide for Flat {
        fn solid(&self, _: i32, y: i32, _: i32) -> bool {
            y < 0
        }
    }
    impl Terrain for Flat {
        fn liquid(&self, _: i32, _: i32, _: i32) -> Liquid {
            Liquid::None
        }
        fn hazard(&self, _: i32, _: i32, _: i32) -> bool {
            false
        }
        fn sky_light(&self, _: i32, _: i32, _: i32) -> u8 {
            15
        }
        fn light(&self, _: i32, _: i32, _: i32) -> u8 {
            15
        }
    }

    fn mob(species: Species, pos: [f64; 3]) -> Entity {
        Entity {
            id: 1,
            uuid: 1,
            kind: Kind::Mob(Box::new(Mob::new(species, 0x1234_5678_9abc))),
            pos,
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }
    }

    fn run(e: &mut Entity, players: &[Target], ticks: u64, sunny: bool) -> Vec<MobEvent> {
        let mut out = Vec::new();
        for t in 0..ticks {
            let s = Senses {
                world: &Flat,
                players,
                tick: t,
                difficulty: 2,
                sunny,
            };
            tick(e, &s, &mut out);
        }
        out
    }

    fn player(pos: [f64; 3]) -> Target {
        Target {
            id: 100,
            uuid: 100,
            pos,
            targetable: true,
        }
    }

    #[test]
    fn mobs_land_and_stay_on_the_ground() {
        let mut e = mob(Species::Cow, [0.5, 5.0, 0.5]);
        run(&mut e, &[], 40, false);
        assert!(e.on_ground);
        assert!((e.pos[1]).abs() < 1e-6, "{:?}", e.pos);
    }

    #[test]
    fn a_zombie_walks_to_a_player_and_bites() {
        let mut e = mob(Species::Zombie, [0.5, 0.0, 0.5]);
        let p = player([8.5, 0.0, 0.5]);
        let out = run(&mut e, std::slice::from_ref(&p), 200, false);
        assert!(e.pos[0] > 5.0, "{:?}", e.pos);
        assert!(
            out.iter()
                .any(|m| matches!(m, MobEvent::Attack { target: 100, .. })),
            "{out:?}"
        );
    }

    #[test]
    fn zombies_burn_in_the_sun_and_creeper_fuse_blows() {
        let mut z = mob(Species::Zombie, [0.5, 0.0, 0.5]);
        let out = run(&mut z, &[], 100, true);
        assert!(out.iter().any(|m| matches!(
            m,
            MobEvent::Hurt {
                kind: "minecraft:on_fire",
                ..
            }
        )));
        let mut c = mob(Species::Creeper, [0.5, 0.0, 0.5]);
        let p = player([2.5, 0.0, 0.5]);
        let out = run(&mut c, &[p], 120, false);
        assert!(
            out.iter()
                .any(|m| matches!(m, MobEvent::Explode { power } if *power == 3.0)),
            "{out:?}"
        );
    }

    #[test]
    fn a_hurt_animal_panics_and_runs() {
        let mut e = mob(Species::Cow, [0.5, 0.0, 0.5]);
        run(&mut e, &[], 5, false);
        if let Kind::Mob(m) = &mut e.kind {
            assert!(m.hurt(2.0, Some(100)));
            assert!(!m.hurt(1.0, Some(100)));
        }
        let start = e.pos;
        run(&mut e, &[player([0.5, 0.0, -3.5])], 60, false);
        let moved = (e.pos[0] - start[0]).hypot(e.pos[2] - start[2]);
        assert!(moved > 2.0, "{moved}");
    }

    #[test]
    fn hurt_has_an_invulnerability_window() {
        let mut m = Mob::new(Species::Sheep, 7);
        assert!(m.hurt(4.0, None));
        assert!(!m.hurt(3.0, None));
        assert!(m.hurt(6.0, None));
        assert_eq!(m.health, 2.0);
        assert!(m.hurt(0.1, None) || m.invulnerable > 0);
    }

    #[test]
    fn higher_priority_goals_preempt_lower_ones() {
        let mut m = Mob::new(Species::Cow, 99);
        let mut out = Vec::new();
        let mut b = Body {
            pos: [0.5, 0.0, 0.5],
            vel: [0.0; 3],
            on_ground: true,
        };
        let s = Senses {
            world: &Flat,
            players: &[],
            tick: 0,
            difficulty: 2,
            sunny: false,
        };
        let mut list = vec![
            Slot {
                priority: 6,
                flags: MOVE,
                goal: Goal::Stroll {
                    speed: 1.0,
                    interval: 1,
                    avoid_water: false,
                },
                running: false,
            },
            Slot {
                priority: 1,
                flags: MOVE,
                goal: Goal::Panic { speed: 2.0 },
                running: false,
            },
        ];
        run_goals(&mut list, &mut m, &mut b, &s, &mut out);
        assert!(list[0].running && !list[1].running);
        m.revenge = Some((-1, 100));
        run_goals(&mut list, &mut m, &mut b, &s, &mut out);
        assert!(list[1].running && !list[0].running);
    }

    #[test]
    fn species_round_trip() {
        for s in Species::ALL {
            assert_eq!(Species::from_id(s.id()), Some(s));
        }
    }
}
