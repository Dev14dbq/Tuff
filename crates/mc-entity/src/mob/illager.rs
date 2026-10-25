//! Illagers and their kin (minecraft.wiki, the page of each mob): the
//! pillager's crossbow, the evoker's and the illusioner's spells, the vexes
//! an evoker calls up, and the ravager's stun and roar.

use super::{Body, Mob, MobEvent, Senses, Species, dist_sq, line_clear};

/// A spell, as the byte clients show (`SpellcasterIllager` spell id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spell {
    Vexes = 1,
    Fangs = 2,
    Wololo = 3,
    Mirror = 4,
    Blindness = 5,
}

impl Spell {
    /// Ticks of standing still and casting, ticks before it takes effect
    /// (the warmup is counted down from the start), and the ticks until
    /// this spell may be cast again.
    fn timing(self) -> (i32, i32, i32) {
        match self {
            Spell::Fangs => (40, 20, 100),
            Spell::Vexes => (100, 20, 340),
            Spell::Wololo => (60, 40, 140),
            Spell::Mirror => (20, 20, 340),
            Spell::Blindness => (20, 20, 180),
        }
    }

    fn index(self) -> usize {
        self as usize - 1
    }

    fn sound(self) -> &'static str {
        match self {
            Spell::Fangs => "entity.evoker.prepare_attack",
            Spell::Vexes => "entity.evoker.prepare_summon",
            Spell::Wololo => "entity.evoker.prepare_wololo",
            Spell::Mirror => "entity.illusioner.prepare_mirror",
            Spell::Blindness => "entity.illusioner.prepare_blindness",
        }
    }
}

/// What [`super::goals::Goal::Illager`] is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// A pillager loads its crossbow (`stage` 1), holds it loaded (2) and
    /// shoots; `timer` counts the stage, `seen` the ticks in sight.
    Crossbow { stage: u8, timer: i32, seen: i32 },
    /// Casts one spell; `left` is the casting time left.
    Cast { spell: Spell, left: i32 },
    /// A vex lunges at its target.
    Charge,
    /// A vex drifts about near where it was called up.
    Drift,
    /// A raider with nothing to fight walks where its patrol or its raid
    /// goes (the patrol target, or the raid's centre); `done` once there.
    March { done: bool },
}

/// How near a raid's centre a raider has to come before it roams there.
const RAID_NEAR: f64 = 8.0;

fn march_to(m: &Mob, b: &Body) -> Option<[f64; 3]> {
    let q = &m.quirks.illager;
    if m.target.is_some() || q.celebrating || !(q.patrolling || q.raid.is_some()) {
        return None;
    }
    let to = q.patrol_target?;
    let near = if q.raid.is_some() { RAID_NEAR } else { 2.0 };
    (dist_sq(b.pos, to) > near * near).then_some(to)
}

/// A patrol's leader picks somewhere new to go when it arrives (up to 500
/// blocks off on each side).
fn new_patrol_target(m: &mut Mob, b: &Body) {
    let q = &m.quirks.illager;
    if !q.patrol_leader || q.raid.is_some() || !q.patrolling {
        return;
    }
    let dx = f64::from(m.below(1000)) - 500.0;
    let dz = f64::from(m.below(1000)) - 500.0;
    m.quirks.illager.patrol_target = Some([b.pos[0] + dx, b.pos[1], b.pos[2] + dz]);
}

fn march_speed(m: &Mob) -> f64 {
    let q = &m.quirks.illager;
    if q.raid.is_some() {
        1.0
    } else if q.patrol_leader {
        0.7
    } else {
        0.595
    }
}

/// Extra state on [`super::Quirks`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Illager {
    /// The spell being cast (shown to clients), or `None`.
    pub spell: Option<Spell>,
    /// Ticks until any spell may be cast again.
    pub spell_cd: i32,
    /// Ticks until each spell may be cast again (by [`Spell::index`]).
    pub next: [i32; 5],
    /// A pillager loading its crossbow (shown to clients).
    pub charging: bool,
    /// A raider whose side won cheers (shown to clients).
    pub celebrating: bool,
    /// An evoker: a blue sheep within 16 blocks the server found for it.
    pub blue_sheep: Option<i32>,
    /// A vex: the evoker that called it up, the spot it keeps to, and the
    /// ticks it has left before it starts to waste away (0: forever).
    pub owner: Option<i32>,
    pub bound: Option<[f64; 3]>,
    pub life: i32,
    /// A vex lunging (glows red).
    pub lunging: bool,
    /// A ravager: ticks of its bite, of being stunned, of its roar.
    pub attack_tick: i32,
    pub stun_tick: i32,
    pub roar_tick: i32,
    /// A patrol: its leader carries the banner; where it heads.
    pub patrol_leader: bool,
    pub patrolling: bool,
    pub patrol_target: Option<[f64; 3]>,
    /// A raid this raider fights in, and its wave.
    pub raid: Option<i32>,
    pub wave: i32,
    /// May join a raid it comes near (vanilla's `CanJoinRaid`).
    pub can_join_raid: bool,
}

/// Every tick, before the goals: cooldowns, a vex's life, a ravager's stun.
pub(super) fn pulse(m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let q = &mut m.quirks.illager;
    q.spell_cd = (q.spell_cd - 1).max(0);
    for n in &mut q.next {
        *n = (*n - 1).max(0);
    }
    match m.species {
        Species::Vex => {
            if q.life > 0 {
                q.life -= 1;
                if q.life == 0 {
                    // Out of time: it wastes away, a point a second.
                    q.life = 20;
                    out.push(MobEvent::Hurt {
                        amount: 1.0,
                        kind: "minecraft:starve",
                    });
                }
            }
        }
        Species::Ravager => ravager(m, b, s, out),
        _ => {}
    }
}

fn ravager(m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    let q = &mut m.quirks.illager;
    q.attack_tick = (q.attack_tick - 1).max(0);
    if q.roar_tick > 0 {
        q.roar_tick -= 1;
        if q.roar_tick == 10 {
            out.push(MobEvent::Roar);
        }
    }
    if q.stun_tick > 0 {
        q.stun_tick -= 1;
        b.vel[0] = 0.0;
        b.vel[2] = 0.0;
        m.nav.stop();
        if q.stun_tick == 0 {
            q.roar_tick = 20;
            out.push(MobEvent::Sound {
                name: "entity.ravager.roar".into(),
                volume: 1.0,
                pitch: 1.0,
            });
        }
    }
    // Walking into leaves or ripe crops tramples them.
    if m.bumped && s.griefing && !m.is_dead() {
        out.push(MobEvent::Crush);
    }
}

/// Whether the mob stands still this tick (a stunned or roaring ravager,
/// a caster while it casts).
pub(super) fn held(m: &Mob) -> bool {
    let q = &m.quirks.illager;
    q.stun_tick > 0 || q.roar_tick > 0
}

/// A caster stands still while it casts (whatever else it wants).
pub(super) fn freeze(m: &mut Mob) {
    if m.quirks.illager.spell.is_some() {
        m.goto = None;
        m.nav.stop();
    }
}

/// The ravager shrugs off a shield: stunned for two seconds, half the
/// time.
pub fn stun(m: &mut Mob) {
    if m.species == Species::Ravager && m.next_f64() < 0.5 {
        m.quirks.illager.stun_tick = 40;
        m.nav.stop();
        m.dirty = true;
    }
}

pub(super) fn can_use(act: &mut Act, m: &mut Mob, b: &Body, s: &Senses) -> bool {
    match act {
        Act::Crossbow { .. } => {
            m.target.and_then(|t| s.seen(t)).is_some()
                && m.gear_item(super::slot::MAINHAND)
                    .is_some_and(|i| s.items.name(i) == "minecraft:crossbow")
        }
        Act::Cast { spell, .. } => {
            let spell = *spell;
            let q = &m.quirks.illager;
            if q.spell.is_some() || q.spell_cd > 0 || q.next[spell.index()] > 0 {
                return false;
            }
            let target = m.target.and_then(|t| s.seen(t));
            match spell {
                Spell::Fangs | Spell::Blindness => target.is_some(),
                Spell::Mirror => target.is_some() && !m.effects.has("invisibility"),
                Spell::Vexes => {
                    target.is_some()
                        && s.mobs
                            .iter()
                            .filter(|o| {
                                o.alive
                                    && o.species == Some(Species::Vex)
                                    && dist_sq(o.pos, b.pos) <= 16.0 * 16.0
                            })
                            .count()
                            < 8
                }
                Spell::Wololo => {
                    target.is_none()
                        && s.griefing
                        && q.blue_sheep
                            .and_then(|id| s.mob(id))
                            .is_some_and(|o| o.alive && dist_sq(o.pos, b.pos) <= 16.0 * 16.0)
                }
            }
        }
        Act::Charge => {
            let Some(t) = m.target.and_then(|t| s.seen(t)) else {
                return false;
            };
            !m.quirks.illager.lunging && m.below(7) == 0 && dist_sq(b.pos, t.pos) > 4.0
        }
        Act::Drift => m.below(7) == 0 && m.nav.goal.is_none(),
        Act::March { .. } => {
            if march_to(m, b).is_none() {
                if m.target.is_none()
                    && m.quirks
                        .illager
                        .patrol_target
                        .is_some_and(|t| dist_sq(b.pos, t) <= 4.0)
                {
                    new_patrol_target(m, b);
                }
                return false;
            }
            true
        }
    }
}

pub(super) fn can_continue(act: &Act, m: &Mob, s: &Senses) -> bool {
    match act {
        Act::Crossbow { .. } => m.target.and_then(|t| s.seen(t)).is_some(),
        Act::Cast { left, .. } => *left > 0,
        Act::Charge => m.quirks.illager.lunging && m.target.and_then(|t| s.seen(t)).is_some(),
        Act::Drift => false,
        Act::March { done } => {
            !*done && m.target.is_none() && m.quirks.illager.patrol_target.is_some()
        }
    }
}

pub(super) fn start(act: &mut Act, m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match act {
        Act::Crossbow { stage, timer, seen } => {
            *stage = 0;
            *timer = 0;
            *seen = 0;
        }
        Act::Cast { spell, left } => {
            let (cast, _, interval) = spell.timing();
            *left = cast;
            let q = &mut m.quirks.illager;
            q.spell = Some(*spell);
            q.spell_cd = cast;
            q.next[spell.index()] = interval;
            m.nav.stop();
            m.dirty = true;
            out.push(MobEvent::Sound {
                name: spell.sound().into(),
                volume: 1.0,
                pitch: 1.0,
            });
        }
        Act::Charge => {
            if let Some(t) = m.target.and_then(|t| s.seen(t)) {
                m.goto = Some(([t.pos[0], t.pos[1] + t.eye * 0.5, t.pos[2]], 1.0));
                m.quirks.illager.lunging = true;
                m.dirty = true;
                out.push(MobEvent::Sound {
                    name: "entity.vex.charge".into(),
                    volume: 1.0,
                    pitch: 1.0,
                });
            }
        }
        Act::March { done } => *done = false,
        Act::Drift => {
            let home = m.quirks.illager.bound.unwrap_or(b.pos);
            for _ in 0..3 {
                let p = [
                    home[0] + m.next_f64() * 15.0 - 7.5,
                    home[1] + m.next_f64() * 11.0 - 5.5,
                    home[2] + m.next_f64() * 15.0 - 7.5,
                ];
                if !s.world.solid(
                    p[0].floor() as i32,
                    p[1].floor() as i32,
                    p[2].floor() as i32,
                ) {
                    m.goto = Some((p, 0.25));
                    break;
                }
            }
        }
    }
}

pub(super) fn stop(act: &Act, m: &mut Mob) {
    match act {
        Act::Crossbow { .. } => {
            if m.quirks.illager.charging {
                m.quirks.illager.charging = false;
                m.dirty = true;
            }
        }
        Act::Cast { .. } => {
            m.quirks.illager.spell = None;
            m.dirty = true;
        }
        Act::Charge => {
            m.quirks.illager.lunging = false;
            m.dirty = true;
        }
        Act::March { .. } => m.nav.stop(),
        Act::Drift => {}
    }
}

pub(super) fn tick(act: &mut Act, m: &mut Mob, b: &mut Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match act {
        Act::Crossbow { stage, timer, seen } => crossbow(m, b, s, stage, timer, seen, out),
        Act::Cast { spell, left } => {
            let spell = *spell;
            *left -= 1;
            m.nav.stop();
            let (cast, warmup, _) = spell.timing();
            if let Some(t) = m.target.and_then(|t| s.seen(t)) {
                m.look = Some([t.pos[0], t.pos[1] + t.eye, t.pos[2]]);
            } else if let Some(o) = m.quirks.illager.blue_sheep.and_then(|id| s.mob(id)) {
                m.look = Some([o.pos[0], o.pos[1] + o.height * 0.85, o.pos[2]]);
            }
            if *left == cast - warmup {
                cast_spell(spell, m, b, s, out);
            }
        }
        Act::Charge => {
            let Some(t) = m.target.and_then(|t| s.seen(t)) else {
                return;
            };
            let aim = [t.pos[0], t.pos[1] + t.eye * 0.5, t.pos[2]];
            m.look = Some(aim);
            let (w, _) = m.size();
            let reach = (w * 2.0).powi(2) + t.width;
            if dist_sq(b.pos, t.pos) <= reach.max(1.0) {
                if m.attack_cooldown <= 0 {
                    m.attack_cooldown = 20;
                    out.push(MobEvent::Swing);
                    out.push(MobEvent::Attack {
                        target: t.id,
                        damage: m.species.attack_damage(),
                    });
                }
                m.quirks.illager.lunging = false;
                m.dirty = true;
            } else if dist_sq(b.pos, t.pos) < 9.0 {
                m.goto = Some((aim, 1.0));
            }
        }
        Act::March { done } => match march_to(m, b) {
            Some(to) => m.goto = Some((to, march_speed(m))),
            None => {
                *done = true;
                new_patrol_target(m, b);
            }
        },
        Act::Drift => {}
    }
}

/// Load (as long as a crossbow takes, 25 ticks), wait a second or two,
/// shoot; a little closer than eight blocks and in sight.
fn crossbow(
    m: &mut Mob,
    b: &mut Body,
    s: &Senses,
    stage: &mut u8,
    timer: &mut i32,
    seen: &mut i32,
    out: &mut Vec<MobEvent>,
) {
    let Some(t) = m.target.and_then(|t| s.seen(t)) else {
        return;
    };
    let eye = [b.pos[0], b.pos[1] + m.eye_height(), b.pos[2]];
    let aim = [t.pos[0], t.pos[1] + t.eye, t.pos[2]];
    let visible = line_clear(s.world, eye, aim);
    *seen = if visible {
        (*seen).max(0) + 1
    } else {
        (*seen).min(0) - 1
    };
    m.look = Some(aim);
    let d = dist_sq(b.pos, t.pos);
    let close = d <= 8.0 * 8.0 && *seen >= 5;
    if close {
        m.nav.stop();
    } else {
        m.goto = Some((t.pos, if *stage == 0 { 1.0 } else { 0.5 }));
    }
    match *stage {
        0 => {
            if *seen <= 0 {
                return;
            }
            *stage = 1;
            *timer = 0;
            m.quirks.illager.charging = true;
            m.dirty = true;
            out.push(MobEvent::Sound {
                name: "item.crossbow.loading_start".into(),
                volume: 1.0,
                pitch: 1.0,
            });
        }
        1 => {
            *timer += 1;
            if *timer >= 25 {
                *stage = 2;
                *timer = 20 + m.below(20) as i32;
                m.quirks.illager.charging = false;
                m.dirty = true;
                out.push(MobEvent::Sound {
                    name: "item.crossbow.loading_end".into(),
                    volume: 1.0,
                    pitch: 1.0,
                });
            }
        }
        _ => {
            *timer -= 1;
            if *timer <= 0 && visible {
                out.push(MobEvent::Shoot {
                    target: t.id,
                    aim,
                    missile: super::Missile::Bolt,
                });
                out.push(MobEvent::Sound {
                    name: "item.crossbow.shoot".into(),
                    volume: 1.0,
                    pitch: 1.0 / (m.next_f64() as f32 * 0.5 + 1.0) + 0.2,
                });
                *stage = 0;
            }
        }
    }
}

fn cast_spell(spell: Spell, m: &mut Mob, b: &Body, s: &Senses, out: &mut Vec<MobEvent>) {
    match spell {
        Spell::Fangs => {
            let Some(t) = m.target.and_then(|t| s.seen(t)) else {
                return;
            };
            out.push(MobEvent::Fangs(fangs(b, s, t.pos)));
        }
        Spell::Vexes => out.push(MobEvent::SummonVexes),
        Spell::Wololo => {
            if let Some(sheep) = m.quirks.illager.blue_sheep.take() {
                out.push(MobEvent::Wololo { sheep });
                out.push(MobEvent::Sound {
                    name: "entity.evoker.cast_spell".into(),
                    volume: 1.0,
                    pitch: 1.0,
                });
            }
        }
        Spell::Blindness => {
            if let Some(t) = m.target {
                out.push(MobEvent::Blind { target: t });
            }
        }
        Spell::Mirror => {
            m.effects
                .active
                .insert("invisibility", crate::effect::Instance::new(0, 1200, true));
            m.dirty = true;
        }
    }
}

/// Where the fangs come up: two rings round the evoker when the target is
/// within three blocks, else a line of sixteen towards it. Each is a spot,
/// its facing (degrees) and its delay in ticks.
pub fn fangs(b: &Body, s: &Senses, target: [f64; 3]) -> Vec<([f64; 3], f32, i32)> {
    let low = target[1].min(b.pos[1]);
    let high = target[1].max(b.pos[1]) + 1.0;
    let angle = (target[2] - b.pos[2]).atan2(target[0] - b.pos[0]);
    let mut out = Vec::new();
    let mut put = |x: f64, z: f64, yaw: f64, delay: i32| {
        if let Some(y) = floor_between(s, x, z, low, high) {
            out.push(([x, y, z], yaw.to_degrees() as f32, delay));
        }
    };
    if dist_sq(b.pos, target) < 9.0 {
        for i in 0..5 {
            let a = angle + f64::from(i) * std::f64::consts::PI * 0.4;
            put(b.pos[0] + a.cos() * 1.5, b.pos[2] + a.sin() * 1.5, a, 0);
        }
        for i in 0..8 {
            let a = angle
                + f64::from(i) * std::f64::consts::PI * 2.0 / 8.0
                + std::f64::consts::PI * 2.0 / 5.0;
            put(b.pos[0] + a.cos() * 2.5, b.pos[2] + a.sin() * 2.5, a, 3);
        }
    } else {
        for i in 0..16 {
            let r = 1.25 * f64::from(i + 1);
            put(
                b.pos[0] + angle.cos() * r,
                b.pos[2] + angle.sin() * r,
                angle,
                i,
            );
        }
    }
    out
}

/// The top of the highest solid block in the column between `low` and
/// `high`, if there is room above it.
fn floor_between(s: &Senses, x: f64, z: f64, low: f64, high: f64) -> Option<f64> {
    let (cx, cz) = (x.floor() as i32, z.floor() as i32);
    let mut y = high.floor() as i32;
    while f64::from(y) >= low.floor() - 1.0 {
        if s.world.solid(cx, y - 1, cz) && !s.world.solid(cx, y, cz) {
            return Some(f64::from(y));
        }
        y -= 1;
    }
    None
}
