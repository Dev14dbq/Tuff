//! What the bosses remember between ticks: the ender dragon's phase, path
//! and body parts, the wither's heads, and end crystals. They think in the
//! server's own tick (they need the world, crystals and the dragon fight);
//! this is only their state, as minecraft.wiki ("Ender Dragon", "Wither",
//! "End Crystal") describes it.

/// The dragon's phases, numbered as `DragonPhase` and its metadata are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Circles the pillars.
    HoldingPattern,
    /// Flies at a player to shoot a fireball.
    StrafePlayer,
    /// Flies to the exit portal to land.
    LandingApproach,
    Landing,
    TakingOff,
    /// Breathes on the portal.
    SittingFlaming,
    /// Looks around for a player to breathe on.
    SittingScanning,
    /// Roars before breathing.
    SittingAttacking,
    ChargingPlayer,
    /// Flies to the portal to die.
    Dying,
    /// Stays where it is (summoned without a fight).
    #[default]
    Hovering,
}

impl Phase {
    pub const ALL: [Phase; 11] = [
        Phase::HoldingPattern,
        Phase::StrafePlayer,
        Phase::LandingApproach,
        Phase::Landing,
        Phase::TakingOff,
        Phase::SittingFlaming,
        Phase::SittingScanning,
        Phase::SittingAttacking,
        Phase::ChargingPlayer,
        Phase::Dying,
        Phase::Hovering,
    ];

    pub fn id(self) -> i32 {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(10) as i32
    }

    pub fn from_id(id: i32) -> Self {
        usize::try_from(id)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
            .unwrap_or_default()
    }

    /// On the portal (or hovering): arrows burn on it, wings do not hurt.
    pub fn sitting(self) -> bool {
        matches!(
            self,
            Phase::SittingFlaming
                | Phase::SittingScanning
                | Phase::SittingAttacking
                | Phase::Hovering
        )
    }
}

/// The dragon's eight parts, in the order clients number them (the
/// dragon's entity id plus one, two...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Head,
    Neck,
    Body,
    Tail1,
    Tail2,
    Tail3,
    Wing1,
    Wing2,
}

impl Part {
    pub const ALL: [Part; 8] = [
        Part::Head,
        Part::Neck,
        Part::Body,
        Part::Tail1,
        Part::Tail2,
        Part::Tail3,
        Part::Wing1,
        Part::Wing2,
    ];

    /// Width and height.
    pub fn size(self) -> (f64, f64) {
        match self {
            Part::Head => (1.0, 1.0),
            Part::Neck => (3.0, 3.0),
            Part::Body => (5.0, 3.0),
            Part::Tail1 | Part::Tail2 | Part::Tail3 => (2.0, 2.0),
            Part::Wing1 | Part::Wing2 => (4.0, 2.0),
        }
    }
}

/// A path along the dragon's flight nodes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    /// Node points, the last perhaps one of its own (the portal).
    pub points: Vec<[i32; 3]>,
    pub next: usize,
}

impl Path {
    pub fn done(&self) -> bool {
        self.next >= self.points.len()
    }

    pub fn advance(&mut self) {
        self.next += 1;
    }

    pub fn next_point(&self) -> Option<[i32; 3]> {
        self.points.get(self.next).copied()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Dragon {
    pub phase: Phase,
    /// The point it flies to.
    pub target: Option<[f64; 3]>,
    pub path: Option<Path>,
    /// Holding pattern: which way round.
    pub clockwise: bool,
    /// Strafing or charging: whom it goes after (entity id).
    pub attack: Option<i32>,
    /// Phase counters: strafe fireball charge, scanning, roar and flame
    /// ticks, ticks since a charge ended, takeoff's first tick.
    pub counter: i32,
    /// Breath attacks in a row while perched.
    pub flames: i32,
    /// The breath cloud while flaming (entity id).
    pub flame: Option<i32>,
    pub first_tick: bool,
    /// Damage taken while perched since it last took off.
    pub sitting_damage: f32,
    /// The crystal healing it (entity id).
    pub crystal: Option<i32>,
    /// Turn rate.
    pub yaw_speed: f32,
    /// The yaw and height of the last 64 ticks (the tail follows them).
    pub history: Vec<(f64, f64)>,
    pub history_at: usize,
    /// Part centres (feet of each box), by [`Part`].
    pub parts: [[f64; 3]; 8],
    /// Id of the head part: the dragon's id plus one.
    pub part_base: i32,
    /// Ticks into the death animation.
    pub death_time: i32,
    /// Inside blocks it may not break (it slows down).
    pub in_wall: bool,
    /// Bumped into something this tick.
    pub collided: bool,
    /// The part the hit being handled landed on.
    pub hit_part: Option<Part>,
    /// The flight nodes, once laid out: point and neighbours.
    pub nodes: Vec<([i32; 3], u32)>,
    /// Ticks since it was last hurt, for its wings and head.
    pub since_hurt: i32,
    /// It belongs to the End's dragon fight.
    pub in_fight: bool,
}

impl Default for Dragon {
    fn default() -> Self {
        Self {
            phase: Phase::Hovering,
            target: None,
            path: None,
            clockwise: false,
            attack: None,
            counter: 0,
            flames: 0,
            flame: None,
            first_tick: true,
            sitting_damage: 0.0,
            crystal: None,
            yaw_speed: 0.0,
            history: vec![(0.0, 0.0); 64],
            history_at: 0,
            parts: [[0.0; 3]; 8],
            part_base: 0,
            death_time: 0,
            in_wall: false,
            collided: false,
            hit_part: None,
            nodes: Vec::new(),
            since_hurt: 100,
            in_fight: false,
        }
    }
}

impl Dragon {
    /// The yaw and height `back` ticks ago.
    pub fn latency(&self, back: usize) -> (f64, f64) {
        let n = self.history.len();
        self.history[(self.history_at + n - back.min(n - 1)) % n]
    }

    pub fn remember(&mut self, yaw: f64, y: f64) {
        self.history_at = (self.history_at + 1) % self.history.len();
        self.history[self.history_at] = (yaw, y);
    }

    /// The part with this entity id.
    pub fn part_of(&self, id: i32) -> Option<Part> {
        let i = usize::try_from(id - self.part_base).ok()?;
        Part::ALL.get(i).copied()
    }

    pub fn part_box(&self, part: Part) -> crate::Aabb {
        let (w, h) = part.size();
        let p = self.parts[part as usize];
        crate::Aabb {
            min: [p[0] - w / 2.0, p[1], p[2] - w / 2.0],
            max: [p[0] + w / 2.0, p[1] + h, p[2] + w / 2.0],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wither {
    /// Ticks of the spawn animation left (`Invul`).
    pub invulnerable: i32,
    /// Whom each head looks at (entity ids, 0 for none): the middle head
    /// is the wither's own target.
    pub heads: [i32; 3],
    /// Game tick each side head looks for a target again.
    pub next_head_update: [i64; 2],
    /// Head updates without a shot (and hurt counts three).
    pub idle_head_updates: [i32; 2],
    /// Ticks until it breaks the blocks around it.
    pub destroy_blocks: i32,
    /// Ticks until the middle head shoots again, and ticks it has seen its
    /// target.
    pub attack_time: i32,
    pub seen: i32,
}

/// A boss's own state.
#[derive(Clone, Debug, PartialEq)]
pub enum Boss {
    Dragon(Box<Dragon>),
    Wither(Wither),
}

/// An end crystal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crystal {
    /// Where its beam points (`beam_target`).
    pub beam: Option<[i32; 3]>,
    /// Shows its bedrock base (`ShowBottom`).
    pub show_bottom: bool,
    /// `Invulnerable` (the respawn sequence's pillar crystals).
    pub invulnerable: bool,
    /// Hit this tick: the player behind it, and whether by an explosion
    /// (it then goes without exploding).
    pub struck: Option<(Option<u128>, bool)>,
}
