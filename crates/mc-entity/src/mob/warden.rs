//! The warden's state and its two goals of its own (minecraft.wiki,
//! "Warden"). The server hears for it, keeps its anger and decides what it
//! does (`mc-server`'s `mobs::warden`); here it stands still while it is
//! busy (emerging, digging, roaring, sniffing, charging a sonic boom) and
//! walks to what it heard last when it has nobody to fight.

use super::{Body, Mob, MobEvent, Senses, dist_sq};

/// What a warden is in the middle of: the pose clients see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Doing {
    #[default]
    Nothing,
    Emerging,
    Digging,
    Roaring,
    Sniffing,
    /// Charging and firing a sonic boom.
    Booming,
}

/// Ticks each takes.
pub const EMERGE_TICKS: i32 = 134;
pub const DIG_TICKS: i32 = 100;
pub const ROAR_TICKS: i32 = 84;
pub const SNIFF_TICKS: i32 = 84;
pub const BOOM_TICKS: i32 = 60;
/// The sonic boom goes off this far into its charge.
pub const BOOM_FIRES: i32 = 34;

/// One entity the warden is angry at, and how angry (0 to 150).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suspect {
    pub id: i32,
    pub anger: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Warden {
    pub doing: Doing,
    /// Ticks left of what it is doing.
    pub left: i32,
    pub suspects: Vec<Suspect>,
    /// Ticks since anything disturbed it; at 1200 it digs back down.
    pub calm: i32,
    /// Ticks before it hears again.
    pub hearing_cd: i32,
    pub sniff_cd: i32,
    pub boom_cd: i32,
    /// Ticks since it last hit its target or a sonic boom went.
    pub since_attack: i32,
    /// Ticks it has had its target.
    pub fighting: i32,
    pub touch_cd: i32,
    pub heartbeat: i32,
    /// Where it heard something last, to go and see.
    pub investigate: Option<[f64; 3]>,
    /// The suspect it roared at (it chases once the roar is over).
    pub roared_at: Option<i32>,
}

impl Warden {
    pub fn busy(&self) -> bool {
        self.doing != Doing::Nothing
    }

    /// The highest anger it holds (what clients see).
    pub fn anger(&self) -> i32 {
        self.suspects.iter().map(|s| s.anger).max().unwrap_or(0)
    }

    /// Angry enough to fight: the most angry suspect at 80 or more.
    pub fn angry_at(&self) -> Option<i32> {
        self.suspects
            .iter()
            .filter(|s| s.anger >= 80)
            .max_by_key(|s| s.anger)
            .map(|s| s.id)
    }

    /// More anger at an entity, at most 150.
    pub fn anger_at(&mut self, id: i32, by: i32) {
        match self.suspects.iter_mut().find(|s| s.id == id) {
            Some(s) => s.anger = (s.anger + by).clamp(0, 150),
            None => self.suspects.push(Suspect {
                id,
                anger: by.clamp(0, 150),
            }),
        }
        self.calm = 0;
    }

    pub fn start(&mut self, doing: Doing, ticks: i32) {
        self.doing = doing;
        self.left = ticks;
    }

    /// The pose clients see.
    pub fn pose(&self) -> i32 {
        match self.doing {
            Doing::Nothing | Doing::Booming => 0,
            Doing::Roaring => 11,
            Doing::Sniffing => 12,
            Doing::Emerging => 13,
            Doing::Digging => 14,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Stands still while it emerges, digs, roars, sniffs or booms.
    Busy,
    /// Walks to where it heard something, with nobody to fight.
    Investigate,
}

pub(super) fn can_use(act: &Act, m: &Mob, b: &Body) -> bool {
    let w = &m.quirks.warden;
    match act {
        Act::Busy => w.busy(),
        Act::Investigate => {
            !w.busy()
                && m.target.is_none()
                && w.investigate.is_some_and(|at| dist_sq(at, b.pos) > 4.0)
        }
    }
}

pub(super) fn can_continue(act: &Act, m: &Mob, b: &Body) -> bool {
    can_use(act, m, b)
}

pub(super) fn stop(act: &Act, m: &mut Mob) {
    if *act == Act::Investigate {
        m.nav.stop();
    }
}

pub(super) fn tick(act: &Act, m: &mut Mob, b: &Body, s: &Senses, _out: &mut Vec<MobEvent>) {
    match act {
        Act::Busy => {
            m.goto = None;
            m.nav.stop();
            if matches!(m.quirks.warden.doing, Doing::Roaring | Doing::Booming)
                && let Some(t) = m.target.and_then(|id| s.seen(id)).map(|t| t.pos)
            {
                m.look = Some([t[0], t[1] + 1.0, t[2]]);
            }
        }
        Act::Investigate => {
            if let Some(at) = m.quirks.warden.investigate {
                if dist_sq(at, b.pos) <= 4.0 {
                    m.quirks.warden.investigate = None;
                } else {
                    m.goto = Some((at, 0.7));
                }
            }
        }
    }
}
