//! The ender dragon (minecraft.wiki, "Ender Dragon"):
//! - it flies between 24 nodes laid out on three rings over the island
//!   (60, 40 and 20 blocks out, a few blocks above the ground); with no
//!   crystals left it keeps to the inner two;
//! - circling, at the end of each path it lands with a chance of one in
//!   three plus the crystals alive, or goes for a player near the portal;
//!   destroying a crystal sends it at the player who did, to shoot a
//!   fireball;
//! - on the portal it looks for a player within 20 blocks, roars and
//!   breathes for ten seconds (four times at most), charges the nearest
//!   player when there is none near, and takes off after 25% of its health
//!   in hits;
//! - it heals one point every ten ticks from the nearest crystal within 32
//!   blocks; the crystal healing it takes ten points with it when it goes;
//! - only players and explosions hurt it: fully on the head, by
//!   `damage / 4 + min(damage, 1)` elsewhere; arrows bounce off it on the
//!   portal; a fatal blow sends it to the portal to die;
//! - its wings fling away what they touch and deal 5 damage, except while
//!   it is perched (the space under it stays open); head and neck hit for
//!   10, not for half a second after it was hurt;
//! - it flies through blocks, breaking all but the dragon-immune ones
//!   (`mob_griefing`), and slows down in those;
//! - dying takes 200 ticks: it rises, gives 12000 experience the first time
//!   (500 after), and the fight opens the portal.

use mc_entity::boss::{Dragon, Part, Path, Phase};
use mc_entity::mob::Target;
use mc_entity::{Entity, Kind, Mob};
use mc_protocol::Position;
use mc_protocol::packets::play;

use super::{dist_sq, rnd};
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::level::WorldLevel;
use crate::tick::lock;

/// Experience of the first kill and of the later ones.
pub const FIRST_XP: i32 = 12000;
pub const LATER_XP: i32 = 500;
/// Ticks of the death animation.
pub const DEATH_TICKS: i32 = 200;

fn wrap(deg: f64) -> f64 {
    let mut d = deg % 360.0;
    if d >= 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-4 { [0.0; 3] } else { v.map(|c| c / l) }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The part a hit landed on, for [`harm`].
pub fn aim_at(e: &mut Entity, part: Part) {
    if let Kind::Mob(m) = &mut e.kind
        && let Some(d) = m.dragon_mut()
    {
        d.hit_part = Some(part);
    }
}

/// What the dragon knows of the world this tick.
struct World<'a> {
    players: &'a [Target],
    /// Crystals alive on the pillars, `None` outside the End.
    crystals: Option<i32>,
    /// Where it lands: on top of the exit portal.
    podium: [f64; 3],
}

impl World<'_> {
    /// Whether it only uses the inner rings.
    fn inner_only(&self) -> bool {
        self.crystals.is_none_or(|c| c == 0)
    }

    /// The nearest player it may attack within `range` of `from`.
    fn nearest(&self, from: [f64; 3], range: f64, ok: impl Fn(&Target) -> bool) -> Option<&Target> {
        self.players
            .iter()
            .filter(|p| p.targetable && ok(p))
            .map(|p| (dist_sq(p.pos, from), p))
            .filter(|(d, _)| *d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p)| p)
    }
}

/// The 24 flight nodes and which are joined.
fn lay_out_nodes(ctx: &ServerContext) -> Vec<([i32; 3], u32)> {
    use std::f64::consts::PI;
    let mut nodes = Vec::new();
    let mut angles = Vec::new();
    for i in 0..24 {
        let (r, a, lift) = if i < 12 {
            (60.0, 2.0 * (-PI + PI / 12.0 * f64::from(i)), 5)
        } else if i < 20 {
            (40.0, 2.0 * (-PI + PI / 8.0 * f64::from(i - 12)), 15)
        } else {
            (20.0, 2.0 * (-PI + PI / 4.0 * f64::from(i - 20)), 5)
        };
        let x = (r * a.cos()).floor() as i32;
        let z = (r * a.sin()).floor() as i32;
        let y = (super::fight::surface(ctx, x, z) + lift).max(10);
        nodes.push(([x, y, z], 0u32));
        angles.push(a.rem_euclid(2.0 * PI));
    }
    let ring = |i: usize| match i {
        0..12 => 0,
        12..20 => 1,
        _ => 2,
    };
    let gap = |a: f64, b: f64| {
        let d = (a - b).abs() % (2.0 * PI);
        d.min(2.0 * PI - d)
    };
    for i in 0..24 {
        for j in 0..24 {
            if i == j {
                continue;
            }
            let (ri, rj) = (ring(i), ring(j));
            let d = gap(angles[i], angles[j]);
            let joined = match (ri, rj) {
                // Neighbours on a ring.
                (0, 0) => d < PI / 6.0 + 1e-6,
                (1, 1) => d < PI / 4.0 + 1e-6,
                (2, 2) => d < PI / 2.0 + 1e-6,
                // The rings meet where they are closest.
                (0, 1) | (1, 0) => d < PI / 8.0 + 1e-6,
                (1, 2) | (2, 1) => d < PI / 4.0 + 1e-6,
                _ => false,
            };
            if joined {
                nodes[i].1 |= 1 << j;
            }
        }
    }
    nodes
}

fn first_node(w: &World) -> usize {
    if w.inner_only() { 12 } else { 0 }
}

/// The node nearest a point.
fn closest_node(d: &Dragon, w: &World, p: [f64; 3]) -> usize {
    (first_node(w)..d.nodes.len())
        .min_by(|&a, &b| {
            let da = dist_sq(d.nodes[a].0.map(f64::from), p);
            let db = dist_sq(d.nodes[b].0.map(f64::from), p);
            da.total_cmp(&db)
        })
        .unwrap_or(0)
}

/// The shortest way along the nodes from `from` to `to`, ending at `last`
/// if given.
fn find_path(
    d: &Dragon,
    w: &World,
    from: usize,
    to: usize,
    last: Option<[i32; 3]>,
) -> Option<Path> {
    let n = d.nodes.len();
    if from >= n || to >= n {
        return None;
    }
    let skip = first_node(w);
    let point = |i: usize| d.nodes[i].0.map(f64::from);
    let mut dist = vec![f64::INFINITY; n];
    let mut prev = vec![usize::MAX; n];
    let mut done = vec![false; n];
    dist[from] = 0.0;
    loop {
        let u = (0..n)
            .filter(|&i| !done[i] && dist[i].is_finite())
            .min_by(|&a, &b| {
                (dist[a] + dist_sq(point(a), point(to)).sqrt())
                    .total_cmp(&(dist[b] + dist_sq(point(b), point(to)).sqrt()))
            })?;
        if u == to {
            break;
        }
        done[u] = true;
        for v in 0..n {
            if d.nodes[u].1 & (1 << v) == 0 || done[v] || (v < skip && v != to) {
                continue;
            }
            let alt = dist[u] + dist_sq(point(u), point(v)).sqrt();
            if alt < dist[v] {
                dist[v] = alt;
                prev[v] = u;
            }
        }
    }
    let mut order = vec![to];
    let mut at = to;
    while at != from {
        at = prev[at];
        if at == usize::MAX {
            return None;
        }
        order.push(at);
    }
    order.reverse();
    let mut points: Vec<[i32; 3]> = order.into_iter().map(|i| d.nodes[i].0).collect();
    points.extend(last);
    Some(Path { points, next: 0 })
}

/// Sets the next point of the path as the target, a little above the node.
fn next_on_path(d: &mut Dragon) {
    let Some(path) = d.path.as_mut() else {
        return;
    };
    let Some(p) = path.next_point() else {
        return;
    };
    path.advance();
    let y = f64::from(p[1]) + rnd() * 20.0;
    d.target = Some([f64::from(p[0]), y, f64::from(p[2])]);
}

fn set_phase(m: &mut Mob, phase: Phase) {
    let Some(d) = m.dragon_mut() else {
        return;
    };
    if d.phase == phase {
        return;
    }
    d.phase = phase;
    begin(d);
    m.dirty = true;
}

/// What a phase does when it starts.
fn begin(d: &mut Dragon) {
    match d.phase {
        Phase::StrafePlayer => {
            d.counter = 0;
            d.target = None;
            d.path = None;
            d.attack = None;
        }
        Phase::LandingApproach | Phase::TakingOff => {
            d.path = None;
            d.target = None;
            d.first_tick = true;
        }
        Phase::Landing | Phase::Dying | Phase::ChargingPlayer => {
            d.target = None;
            d.counter = 0;
        }
        Phase::SittingScanning | Phase::SittingAttacking => d.counter = 0,
        Phase::SittingFlaming => {
            d.counter = 0;
            d.flames += 1;
        }
        Phase::Hovering => d.target = None,
        Phase::HoldingPattern => {}
    }
}

fn fly_speed(phase: Phase) -> f64 {
    match phase {
        Phase::Landing => 1.5,
        Phase::ChargingPlayer => 3.0,
        Phase::Hovering => 1.0,
        _ => 0.6,
    }
}

fn turn_speed(phase: Phase, vel: [f64; 3]) -> f64 {
    let f = vel[0].hypot(vel[2]) + 1.0;
    let f1 = f.min(40.0);
    if phase == Phase::Landing {
        f1 / f
    } else {
        0.7 / f1 / f
    }
}

/// Goes after `player` to shoot it.
fn strafe(m: &mut Mob, pos: [f64; 3], w: &World, player: &Target) {
    set_phase(m, Phase::StrafePlayer);
    let Some(d) = m.dragon_mut() else {
        return;
    };
    d.attack = Some(player.id);
    let from = closest_node(d, w, pos);
    let to = closest_node(d, w, player.pos);
    let (px, pz) = (player.pos[0].floor(), player.pos[2].floor());
    let dist = (px - pos[0]).hypot(pz - pos[2]);
    let up = (0.4 + dist / 80.0 - 1.0).min(10.0);
    let last = [px as i32, (player.pos[1] + up).floor() as i32, pz as i32];
    d.path = find_path(d, w, from, to, Some(last));
    if let Some(p) = d.path.as_mut() {
        p.advance();
    }
    next_on_path(d);
}

/// Circling: a new path round the pillars, or landing, or a player.
fn holding_new_target(m: &mut Mob, pos: [f64; 3], w: &World) {
    let done = m
        .dragon()
        .is_some_and(|d| d.path.as_ref().is_some_and(Path::done));
    if done {
        let crystals = w.crystals.unwrap_or(0);
        if super::below(crystals + 3) == 0 {
            set_phase(m, Phase::LandingApproach);
            return;
        }
        let player = w.nearest(w.podium, 128.0, |_| true).cloned();
        let d = player
            .as_ref()
            .map_or(64.0, |p| dist_sq(w.podium, p.pos) / 512.0);
        if let Some(p) = player
            && (super::below(d.abs() as i32 + 2) == 0 || super::below(crystals + 2) == 0)
        {
            strafe(m, pos, w, &p);
            return;
        }
    }
    let Some(d) = m.dragon_mut() else {
        return;
    };
    if d.path.as_ref().is_none_or(Path::done) {
        let from = closest_node(d, w, pos);
        let mut to = from as i32;
        if super::below(8) == 0 {
            d.clockwise = !d.clockwise;
            to += 6;
        }
        to += if d.clockwise { 1 } else { -1 };
        let to = if w.inner_only() {
            ((to - 12) & 7) + 12
        } else {
            to.rem_euclid(12)
        };
        d.path = find_path(d, w, from, to as usize, None);
        if let Some(p) = d.path.as_mut() {
            p.advance();
        }
    }
    next_on_path(d);
}

/// One server tick of the current phase.
fn phase_tick(ctx: &ServerContext, me: &mut Entity, w: &World, out: &mut Vec<Effect>) {
    let pos = me.pos;
    let vel = me.vel;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    let yaw = f64::from(m.yaw);
    let Some(d) = m.dragon() else {
        return;
    };
    let target_dist = d.target.map_or(0.0, |t| dist_sq(t, pos));
    let far = !(100.0..=22500.0).contains(&target_dist);
    match d.phase {
        Phase::HoldingPattern => {
            if d.target.is_none() || far || d.collided {
                holding_new_target(m, pos, w);
            }
        }
        Phase::StrafePlayer => {
            let attack = d
                .attack
                .and_then(|id| w.players.iter().find(|p| p.id == id && p.targetable));
            let Some(player) = attack.cloned() else {
                set_phase(m, Phase::HoldingPattern);
                return;
            };
            let d = m.dragon_mut().expect("dragon");
            if d.path.as_ref().is_some_and(Path::done) {
                let (x, z) = (player.pos[0], player.pos[2]);
                let dist = (x - pos[0]).hypot(z - pos[2]);
                let up = (0.4 + dist / 80.0 - 1.0).min(10.0);
                d.target = Some([x, player.pos[1] + up, z]);
            }
            let td = d.target.map_or(0.0, |t| dist_sq(t, pos));
            if !(100.0..=22500.0).contains(&td) {
                if d.path.as_ref().is_none_or(Path::done) {
                    let from = closest_node(d, w, pos);
                    let mut to = from as i32;
                    if super::below(8) == 0 {
                        d.clockwise = !d.clockwise;
                        to += 6;
                    }
                    to += if d.clockwise { 1 } else { -1 };
                    d.path = find_path(d, w, from, to.rem_euclid(12) as usize, None);
                    if let Some(p) = d.path.as_mut() {
                        p.advance();
                    }
                }
                next_on_path(d);
            }
            if dist_sq(player.pos, pos) < 64.0 * 64.0 {
                d.counter += 1;
                let to = normalize([player.pos[0] - pos[0], 0.0, player.pos[2] - pos[2]]);
                let facing = normalize([yaw.to_radians().sin(), 0.0, -yaw.to_radians().cos()]);
                let angle = dot(facing, to).clamp(-1.0, 1.0).acos().to_degrees() + 0.5;
                if d.counter >= 5 && (0.0..10.0).contains(&angle) {
                    out.push(Effect::Fireball(player.pos));
                    d.counter = 0;
                    if let Some(p) = d.path.as_mut() {
                        p.next = p.points.len();
                    }
                    set_phase(m, Phase::HoldingPattern);
                }
            } else if d.counter > 0 {
                d.counter -= 1;
            }
        }
        Phase::LandingApproach => {
            if d.target.is_none() || far || d.collided {
                let d = m.dragon_mut().expect("dragon");
                if d.path.as_ref().is_none_or(Path::done) {
                    let from = closest_node(d, w, pos);
                    let to = match w.nearest(w.podium, 128.0, |_| true) {
                        Some(p) => {
                            let v = normalize([p.pos[0], 0.0, p.pos[2]]);
                            closest_node(d, w, [-v[0] * 40.0, 105.0, -v[2] * 40.0])
                        }
                        None => closest_node(d, w, [40.0, w.podium[1], 0.0]),
                    };
                    let last = w.podium.map(|c| c.floor() as i32);
                    d.path = find_path(d, w, from, to, Some(last));
                    if let Some(p) = d.path.as_mut() {
                        p.advance();
                    }
                }
                next_on_path(d);
                if d.path.as_ref().is_some_and(Path::done) {
                    set_phase(m, Phase::Landing);
                }
            }
        }
        Phase::Landing => {
            let d = m.dragon_mut().expect("dragon");
            let t = *d
                .target
                .get_or_insert([w.podium[0] + 0.5, w.podium[1], w.podium[2] + 0.5]);
            if dist_sq(t, pos) < 1.0 {
                d.flames = 0;
                set_phase(m, Phase::SittingScanning);
            }
        }
        Phase::SittingScanning => {
            let d = m.dragon_mut().expect("dragon");
            d.counter += 1;
            let time = d.counter;
            let near = w
                .nearest(pos, 20.0, |p| (p.pos[1] - pos[1]).abs() <= 10.0)
                .cloned();
            if let Some(p) = near {
                if time > 25 {
                    set_phase(m, Phase::SittingAttacking);
                } else {
                    let to = normalize([p.pos[0] - pos[0], 0.0, p.pos[2] - pos[2]]);
                    let facing = normalize([yaw.to_radians().sin(), 0.0, -yaw.to_radians().cos()]);
                    let angle = dot(facing, to).clamp(-1.0, 1.0).acos().to_degrees() + 0.5;
                    if !(0.0..=10.0).contains(&angle) {
                        let head = d.parts[Part::Head as usize];
                        let (dx, dz) = (p.pos[0] - head[0], p.pos[2] - head[2]);
                        let turn =
                            wrap(180.0 - dx.atan2(dz).to_degrees() - yaw).clamp(-100.0, 100.0);
                        d.yaw_speed *= 0.8;
                        let f = dx.hypot(dz) + 1.0;
                        d.yaw_speed += (turn * 0.7 / f.min(40.0) / f) as f32;
                        let speed = f64::from(d.yaw_speed) * 0.1;
                        m.yaw += speed as f32;
                    }
                }
            } else if time >= 100 {
                let charge = w.nearest(pos, 150.0, |_| true).cloned();
                set_phase(m, Phase::TakingOff);
                if let Some(p) = charge {
                    set_phase(m, Phase::ChargingPlayer);
                    if let Some(d) = m.dragon_mut() {
                        d.target = Some(p.pos);
                        d.counter = 0;
                    }
                }
            }
        }
        Phase::SittingAttacking => {
            let d = m.dragon_mut().expect("dragon");
            if d.counter == 0 {
                out.push(Effect::Growl);
            }
            d.counter += 1;
            if d.counter > 40 {
                set_phase(m, Phase::SittingFlaming);
            }
        }
        Phase::SittingFlaming => {
            let d = m.dragon_mut().expect("dragon");
            d.counter += 1;
            if d.counter >= 200 {
                if let Some(id) = d.flame.take() {
                    out.push(Effect::EndFlame(id));
                }
                if d.flames >= 4 {
                    set_phase(m, Phase::TakingOff);
                } else {
                    set_phase(m, Phase::SittingScanning);
                }
            } else if d.counter == 10 {
                let head = d.parts[Part::Head as usize];
                let v = normalize([head[0] - pos[0], 0.0, head[2] - pos[2]]);
                let x = head[0] + v[0] * 5.0 / 2.0;
                let z = head[2] + v[2] * 5.0 / 2.0;
                let top = head[1] + 0.5;
                let mut y = top;
                let blocks = &ctx.data.blocks;
                let free = |y: f64| {
                    ctx.world
                        .block(
                            Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32),
                            blocks,
                        )
                        .is_none_or(|s| blocks.is_air(s))
                };
                while free(y) {
                    y -= 1.0;
                    if y < 0.0 {
                        y = top;
                        break;
                    }
                }
                out.push(Effect::Flame([x, y.floor() + 1.0, z]));
            }
        }
        Phase::TakingOff => {
            let d = m.dragon_mut().expect("dragon");
            if !d.first_tick && d.path.is_some() {
                let p = w.podium;
                let close = dist_sq([p[0] + 0.5, p[1] + 0.5, p[2] + 0.5], pos) < 100.0;
                if !close {
                    set_phase(m, Phase::HoldingPattern);
                }
            } else {
                d.first_tick = false;
                let from = closest_node(d, w, pos);
                let look = normalize([yaw.to_radians().sin(), 0.0, -yaw.to_radians().cos()]);
                let to = closest_node(d, w, [-look[0] * 40.0, 105.0, -look[2] * 40.0]) as i32;
                let to = if w.inner_only() {
                    ((to - 12) & 7) + 12
                } else {
                    to.rem_euclid(12)
                };
                d.path = find_path(d, w, from, to as usize, None);
                next_on_path(d);
            }
        }
        Phase::ChargingPlayer => {
            let d = m.dragon_mut().expect("dragon");
            let Some(t) = d.target else {
                set_phase(m, Phase::HoldingPattern);
                return;
            };
            if d.counter > 0 {
                d.counter += 1;
                if d.counter > 10 {
                    set_phase(m, Phase::HoldingPattern);
                }
            } else {
                let td = dist_sq(t, pos);
                if !(100.0..=22500.0).contains(&td) || d.collided {
                    d.counter += 1;
                }
            }
        }
        Phase::Dying => {
            let d = m.dragon_mut().expect("dragon");
            let t = *d
                .target
                .get_or_insert([w.podium[0] + 0.5, w.podium[1], w.podium[2] + 0.5]);
            let td = dist_sq(t, pos);
            if (100.0..=22500.0).contains(&td) && !d.collided {
                m.health = 1.0;
            } else {
                m.health = 0.0;
            }
        }
        Phase::Hovering => {
            let d = m.dragon_mut().expect("dragon");
            d.target.get_or_insert(pos);
        }
    }
    let _ = vel;
}

/// What a tick asks of the world.
enum Effect {
    Fireball([f64; 3]),
    Flame([f64; 3]),
    EndFlame(i32),
    Growl,
}

/// One tick of the dragon at `i`.
pub fn tick(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    i: usize,
    players: &[Target],
    now: i64,
) {
    let _ = now;
    let mut me = entities[i].clone();
    let id = me.id;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    if m.no_ai {
        return;
    }
    if m.dragon().is_some_and(|d| d.nodes.is_empty()) {
        let nodes = lay_out_nodes(ctx);
        if let Some(d) = m.dragon_mut() {
            d.nodes = nodes;
        }
    }
    if let Some(d) = m.dragon_mut() {
        d.part_base = id + 1;
        d.since_hurt += 1;
    }
    if m.is_dead() {
        tick_death(ctx, level, &mut me);
        entities[i] = me;
        return;
    }
    let crystals = super::fight::crystals_alive(ctx);
    let podium_y = super::fight::surface(ctx, 0, 0);
    let w = World {
        players,
        crystals,
        podium: [0.0, f64::from(podium_y), 0.0],
    };
    heal_from_crystals(ctx, &mut me, entities, i);
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    // The yaw and height go into the history the tail follows.
    let y = me.pos[1];
    let yaw = wrap(f64::from(m.yaw));
    m.yaw = yaw as f32;
    if let Some(d) = m.dragon_mut() {
        d.remember(yaw, y);
    }
    let mut effects = Vec::new();
    let before = m.dragon().map(|d| d.phase);
    phase_tick(ctx, &mut me, &w, &mut effects);
    // A new phase runs at once.
    if let Kind::Mob(m) = &me.kind
        && m.dragon().map(|d| d.phase) != before
    {
        phase_tick(ctx, &mut me, &w, &mut effects);
    }
    fly(&mut me);
    place_parts(&mut me);
    let flinging =
        matches!(&me.kind, Kind::Mob(m) if m.dragon().is_some_and(|d| d.since_hurt >= 10));
    if flinging {
        touch(ctx, &me, entities, i, players);
    }
    let in_wall = break_blocks(ctx, level, &me);
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    if let Some(d) = m.dragon_mut() {
        d.in_wall = in_wall;
        d.collided = false;
    }
    m.head_yaw = m.yaw;
    // The roar, now and then.
    m.ambient_timer += 1;
    if (super::below(1000)) < m.ambient_timer {
        m.ambient_timer = -80;
        fx::sound(
            ctx,
            "entity.ender_dragon.ambient",
            5,
            me.pos,
            5.0,
            0.8 + rnd() as f32 * 0.4,
        );
    }
    for effect in effects {
        match effect {
            Effect::Fireball(at) => shoot(ctx, &me, at),
            Effect::Flame(at) => {
                let cloud = super::projectile::breath(ctx, id, at, 5.0, 0.0, 200, 0);
                if let Kind::Mob(m) = &mut me.kind
                    && let Some(d) = m.dragon_mut()
                {
                    d.flame = Some(cloud.id);
                }
                crate::tick::spawn_entity(ctx, cloud);
            }
            Effect::EndFlame(cloud) => {
                for e in lock(&ctx.entities).iter_mut() {
                    if e.id == cloud {
                        e.removed = true;
                    }
                }
                let _ = ctx.events.send(Event::EntitiesRemoved(vec![cloud]));
            }
            Effect::Growl => fx::sound(ctx, "entity.ender_dragon.growl", 5, me.pos, 2.5, 1.0),
        }
    }
    entities[i] = me;
}

/// Heals from the nearest crystal, which it looks for now and then.
fn heal_from_crystals(ctx: &ServerContext, me: &mut Entity, entities: &[Entity], i: usize) {
    let pos = me.pos;
    let age = me.age;
    me.age += 1;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    let Some(d) = m.dragon_mut() else {
        return;
    };
    if let Some(c) = d.crystal {
        let alive = entities
            .iter()
            .any(|e| e.id == c && !e.removed && matches!(e.kind, Kind::Crystal(_)));
        if !alive {
            d.crystal = None;
        } else if age.is_multiple_of(10) && m.health < m.max_health {
            m.health = (m.health + 1.0).min(m.max_health);
            fx::emit(
                ctx,
                fx::Fx::Data(play::SetEntityData {
                    entity_id: me.id,
                    entries: vec![(play::metadata::HEALTH, play::MetadataValue::Float(m.health))],
                }),
            );
        }
    }
    if super::below(10) == 0 {
        let b = me.aabb();
        let nearest = entities
            .iter()
            .enumerate()
            .filter(|(j, e)| *j != i && !e.removed && matches!(e.kind, Kind::Crystal(_)))
            .filter(|(_, e)| {
                (0..3).all(|k| e.pos[k] >= b.min[k] - 32.0 && e.pos[k] <= b.max[k] + 32.0)
            })
            .min_by(|a, b| dist_sq(a.1.pos, pos).total_cmp(&dist_sq(b.1.pos, pos)))
            .map(|(_, e)| e.id);
        if let Kind::Mob(m) = &mut me.kind
            && let Some(d) = m.dragon_mut()
        {
            d.crystal = nearest;
        }
    }
}

/// Steers towards the phase's target and moves (through blocks).
fn fly(me: &mut Entity) {
    let pos = me.pos;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    let Some(d) = m.dragon_mut() else {
        return;
    };
    let phase = d.phase;
    let Some(t) = d
        .target
        .filter(|_| !matches!(phase, Phase::Hovering) && !phase.sitting())
    else {
        return;
    };
    let (dx, mut dy, dz) = (t[0] - pos[0], t[1] - pos[1], t[2] - pos[2]);
    let d3 = dx * dx + dy * dy + dz * dz;
    let rise = fly_speed(phase);
    let flat = dx.hypot(dz);
    // A target straight above would otherwise add the whole height to the
    // vertical speed. The client flaps with `pow(2, vel.y)`, so that speed
    // has to stay within the phase's climb rate.
    let slope = if flat > 1.0e-4 {
        dy / flat
    } else {
        dy.signum()
    };
    dy = slope.clamp(-rise, rise);
    me.vel[1] += dy * 0.01;
    let yaw = wrap(f64::from(m.yaw));
    let to = normalize([dx, t[1] - pos[1], dz]);
    let facing = normalize([yaw.to_radians().sin(), me.vel[1], -yaw.to_radians().cos()]);
    let f6 = ((dot(facing, to) + 0.5) / 1.5).max(0.0);
    let d = m.dragon_mut().expect("dragon");
    let mut yaw = yaw;
    if dx.abs() > 1e-5 || dz.abs() > 1e-5 {
        let turn = wrap(180.0 - dx.atan2(dz).to_degrees() - yaw).clamp(-50.0, 50.0);
        d.yaw_speed *= 0.8;
        d.yaw_speed += (turn * turn_speed(phase, me.vel)) as f32;
        yaw += f64::from(d.yaw_speed) * 0.1;
    }
    let f19 = 2.0 / (d3 + 1.0);
    let speed = 0.06 * (f6 * f19 + (1.0 - f19));
    let r = yaw.to_radians();
    me.vel[0] += speed * r.sin();
    me.vel[2] -= speed * r.cos();
    let scale = if d.in_wall { 0.8 } else { 1.0 };
    for k in 0..3 {
        me.pos[k] += me.vel[k] * scale;
    }
    let along = normalize(me.vel);
    let k = 0.8 + 0.15 * (dot(along, facing) + 1.0) / 2.0;
    me.vel = [me.vel[0] * k, me.vel[1] * 0.91, me.vel[2] * k];
    m.yaw = yaw as f32;
}

/// Puts the eight parts where the body, the history of its turns and its
/// height put them.
fn place_parts(me: &mut Entity) {
    let pos = me.pos;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    let yaw = f64::from(m.yaw);
    let Some(d) = m.dragon_mut() else {
        return;
    };
    let latency = |d: &Dragon, back: usize| {
        let (y0, _) = d.latency(back);
        let (y1, h1) = d.latency(back + 1);
        (y0 + wrap(y1 - y0), h1)
    };
    let tilt = ((latency(d, 5).1 - latency(d, 10).1) * 10.0).to_radians();
    let (f13, f1) = (tilt.cos(), tilt.sin());
    let r = yaw.to_radians();
    let (f2, f15) = (r.sin(), r.cos());
    let mut set = |part: Part, dx: f64, dy: f64, dz: f64| {
        d.parts[part as usize] = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
    };
    set(Part::Body, f2 * 0.5, 0.0, -f15 * 0.5);
    set(Part::Wing1, f15 * 4.5, 2.0, f2 * 4.5);
    set(Part::Wing2, f15 * -4.5, 2.0, f2 * -4.5);
    let head_turn = r - f64::from(d.yaw_speed) * 0.01;
    let (f3, f16) = (head_turn.sin(), head_turn.cos());
    let head_y = if d.phase.sitting() {
        -1.0
    } else {
        latency(d, 5).1 - latency(d, 0).1
    };
    d.parts[Part::Head as usize] = [
        pos[0] + f3 * 6.5 * f13,
        pos[1] + head_y + f1 * 6.5,
        pos[2] - f16 * 6.5 * f13,
    ];
    d.parts[Part::Neck as usize] = [
        pos[0] + f3 * 5.5 * f13,
        pos[1] + head_y + f1 * 5.5,
        pos[2] - f16 * 5.5 * f13,
    ];
    let base = latency(d, 5);
    for k in 0..3 {
        let back = latency(d, 12 + k * 2);
        let turn = r + wrap(back.0 - base.0).to_radians();
        let (f18, f20) = (turn.sin(), turn.cos());
        let f22 = (k + 1) as f64 * 2.0;
        let part = [Part::Tail1, Part::Tail2, Part::Tail3][k];
        d.parts[part as usize] = [
            pos[0] - (f2 * 1.5 + f18 * f22) * f13,
            pos[1] + back.1 - base.1 - (f22 + 1.5) * f1 + 1.5,
            pos[2] + (f15 * 1.5 + f20 * f22) * f13,
        ];
    }
}

fn grow(b: mc_entity::Aabb, x: f64, y: f64, z: f64) -> mc_entity::Aabb {
    mc_entity::Aabb {
        min: [b.min[0] - x, b.min[1] - y, b.min[2] - z],
        max: [b.max[0] + x, b.max[1] + y, b.max[2] + z],
    }
}

fn overlaps(a: &mc_entity::Aabb, b: &mc_entity::Aabb) -> bool {
    (0..3).all(|k| a.min[k] < b.max[k] && a.max[k] > b.min[k])
}

/// Wings fling away whoever they touch, unless it is perched. Head and
/// neck hit either way.
fn touch(ctx: &ServerContext, me: &Entity, entities: &mut [Entity], i: usize, players: &[Target]) {
    let Kind::Mob(m) = &me.kind else {
        return;
    };
    let Some(d) = m.dragon() else {
        return;
    };
    let difficulty = ctx.difficulty();
    let sitting = d.phase.sitting();
    let body = d.part_box(Part::Body);
    let (cx, cz) = (
        (body.min[0] + body.max[0]) / 2.0,
        (body.min[2] + body.max[2]) / 2.0,
    );
    let wing = |p: Part| {
        let b = grow(d.part_box(p), 4.0, 2.0, 4.0);
        mc_entity::Aabb {
            min: [b.min[0], b.min[1] - 2.0, b.min[2]],
            max: [b.max[0], b.max[1] - 2.0, b.max[2]],
        }
    };
    let wings = [wing(Part::Wing1), wing(Part::Wing2)];
    let biting = [
        grow(d.part_box(Part::Head), 1.0, 1.0, 1.0),
        grow(d.part_box(Part::Neck), 1.0, 1.0, 1.0),
    ];
    let player_box = |p: &Target| mc_entity::Aabb {
        min: [p.pos[0] - 0.3, p.pos[1], p.pos[2] - 0.3],
        max: [p.pos[0] + 0.3, p.pos[1] + 1.8, p.pos[2] + 0.3],
    };
    let hit = |amount: f32| crate::combat::Hit {
        amount: crate::damage::scale_for_difficulty(amount, difficulty),
        kind: "minecraft:mob_attack",
        death_key: "death.attack.mob",
        attacker: Some("Ender Dragon"),
        by: Some(me.id),
        from: None,
        knockback: 0.0,
        disables_shield: 0.0,
    };
    for p in players.iter().filter(|p| p.present) {
        let b = player_box(p);
        // Perched, the grown wing boxes cover the fountain. Vanilla leaves
        // that space alone: only a flying dragon flings.
        if !sitting {
            for w in &wings {
                if overlaps(w, &b) {
                    let (dx, dz) = (p.pos[0] - cx, p.pos[2] - cz);
                    let d2 = (dx * dx + dz * dz).max(0.1);
                    crate::combat::push_player(
                        ctx,
                        p.uuid,
                        p.id,
                        [dx / d2 * 4.0, 0.2, dz / d2 * 4.0],
                    );
                    if p.targetable {
                        crate::combat::hurt_player(ctx, p.uuid, &hit(5.0));
                    }
                }
            }
        }
        for h in &biting {
            if overlaps(h, &b) && p.targetable {
                crate::combat::hurt_player(ctx, p.uuid, &hit(10.0));
            }
        }
    }
    for (j, o) in entities.iter_mut().enumerate() {
        if j == i || o.removed || !matches!(o.kind, Kind::Mob(_)) {
            continue;
        }
        let b = o.aabb();
        if !sitting {
            for w in &wings {
                if overlaps(w, &b) {
                    let (dx, dz) = (o.pos[0] - cx, o.pos[2] - cz);
                    let d2 = (dx * dx + dz * dz).max(0.1);
                    o.vel[0] += dx / d2 * 4.0;
                    o.vel[1] += 0.2;
                    o.vel[2] += dz / d2 * 4.0;
                    let h = crate::mobs::hurt::Harm {
                        by: Some(me.id),
                        killer: Some("minecraft:ender_dragon".into()),
                        ..crate::mobs::hurt::Harm::new(5.0, "minecraft:mob_attack")
                    };
                    crate::mobs::hurt::harm(ctx, o, &h);
                }
            }
        }
        for h in &biting {
            if overlaps(h, &b) {
                let harm = crate::mobs::hurt::Harm {
                    by: Some(me.id),
                    killer: Some("minecraft:ender_dragon".into()),
                    ..crate::mobs::hurt::Harm::new(10.0, "minecraft:mob_attack")
                };
                crate::mobs::hurt::harm(ctx, o, &harm);
            }
        }
    }
}

/// Breaks the blocks in its head, neck and body (but the dragon-immune
/// ones); whether any it could not break is there.
fn break_blocks(ctx: &ServerContext, level: &mut WorldLevel, me: &Entity) -> bool {
    let Kind::Mob(m) = &me.kind else {
        return false;
    };
    let Some(d) = m.dragon() else {
        return false;
    };
    let griefing = ctx.game_rule_bool("minecraft:mob_griefing");
    let immune = super::block_tag(ctx, "minecraft:dragon_immune");
    let transparent = super::block_tag(ctx, "minecraft:dragon_transparent");
    let blocks = &ctx.data.blocks;
    let mut blocked = false;
    for part in [Part::Head, Part::Neck, Part::Body] {
        let b = d.part_box(part);
        let lo = b.min.map(|c| c.floor() as i32);
        let hi = b.max.map(|c| c.floor() as i32);
        let mut broke = false;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let p = Position::new(x, y, z);
                    let Some(s) = ctx.world.block(p, blocks) else {
                        continue;
                    };
                    if blocks.is_air(s) {
                        continue;
                    }
                    let name = blocks.block_name(s).unwrap_or("");
                    if transparent.contains(name)
                        || name == "minecraft:water"
                        || name == "minecraft:lava"
                    {
                        continue;
                    }
                    if griefing && !immune.contains(name) {
                        // Containers spill what they hold; the block drops
                        // nothing.
                        if crate::containers::kind_of(name).is_some()
                            && !name.ends_with("shulker_box")
                            && name != "minecraft:ender_chest"
                        {
                            let (items, _) = crate::containers::broken(ctx, p);
                            for stack in items {
                                crate::tick::spawn_stack_at(
                                    ctx,
                                    [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5],
                                    &stack,
                                );
                            }
                            let _ = ctx.events.send(Event::ContainerGone { position: p });
                        }
                        let to = if blocks.property(s, "waterlogged") == Some("true") {
                            blocks
                                .default_state("minecraft:water")
                                .unwrap_or(ctx.shapes.air())
                        } else {
                            ctx.shapes.air()
                        };
                        broke |= level.set_block(p, to);
                    } else {
                        blocked = true;
                    }
                }
            }
        }
        if broke {
            let at = [
                f64::from(lo[0] + super::below(hi[0] - lo[0] + 1)),
                f64::from(lo[1] + super::below(hi[1] - lo[1] + 1)),
                f64::from(lo[2] + super::below(hi[2] - lo[2] + 1)),
            ];
            fx::level_event(ctx, 2008, at, 0);
        }
    }
    blocked
}

/// A fireball from the head at `at`.
fn shoot(ctx: &ServerContext, me: &Entity, at: [f64; 3]) {
    let Kind::Mob(m) = &me.kind else {
        return;
    };
    let Some(d) = m.dragon() else {
        return;
    };
    let head = d.parts[Part::Head as usize];
    let r = f64::from(m.yaw).to_radians();
    let view = [-r.sin(), 0.0, r.cos()];
    let from = [head[0] - view[0], head[1] + 0.5 + 0.5, head[2] - view[2]];
    let dir = normalize([at[0] - from[0], at[1] - from[1], at[2] - from[2]]);
    fx::level_event(ctx, 1017, me.pos, 0);
    crate::projectiles::spawn_by_mob(
        ctx,
        mc_entity::extra::Shot::DragonFireball,
        (me.id, "Ender Dragon"),
        from,
        dir.map(|c| c * 0.1),
    );
}

/// The death animation: it rises for ten seconds, giving its experience
/// from the eighth on; at the end the fight hears of it.
fn tick_death(ctx: &ServerContext, level: &mut WorldLevel, me: &mut Entity) {
    let uuid = me.uuid;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    let Some(d) = m.dragon_mut() else {
        return;
    };
    d.death_time += 1;
    let t = d.death_time;
    let drops = ctx.game_rule_bool("minecraft:mob_drops");
    let first = super::fight::fight_dragon(ctx).is_some_and(|(_, won)| !won);
    let total = if first { FIRST_XP } else { LATER_XP };
    if t > 150 && t % 5 == 0 && drops {
        crate::xp::spawn(ctx, me.pos, (total as f32 * 0.08).floor() as i32);
    }
    if t == 1 {
        fx::heard(ctx, 1028, me.pos);
    }
    me.pos[1] += 0.1;
    if t >= DEATH_TICKS {
        if drops {
            crate::xp::spawn(ctx, me.pos, (total as f32 * 0.2).floor() as i32);
        }
        super::fight::dragon_killed(ctx, level, uuid);
        me.removed = true;
    }
}

/// A hit on the dragon (in the part [`aim_at`] named, the body if none).
pub fn harm(ctx: &ServerContext, e: &mut Entity, h: &crate::mobs::hurt::Harm) -> bool {
    let id = e.id;
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    let dead = m.is_dead();
    let Some(d) = m.dragon_mut() else {
        return false;
    };
    let part = d.hit_part.take().unwrap_or(Part::Body);
    if d.phase == Phase::Dying || d.death_time > 0 || dead {
        return false;
    }
    let phase = d.phase;
    let mut amount = h.amount;
    // Arrows only bounce off it on the portal.
    if phase.sitting() && matches!(h.kind, "minecraft:arrow" | "minecraft:trident") {
        return false;
    }
    if part != Part::Head {
        amount = amount / 4.0 + amount.min(1.0);
    }
    if amount < 0.01 {
        return false;
    }
    let explosion = matches!(
        h.kind,
        "minecraft:explosion"
            | "minecraft:player_explosion"
            | "minecraft:fireworks"
            | "minecraft:bad_respawn_point"
    );
    if h.player.is_none() && !explosion {
        return false;
    }
    let before = m.health;
    if !m.hurt(amount, h.by.or(h.player.map(|_| -1))) {
        return false;
    }
    if let Some(p) = h.player {
        m.hurt_by_player = Some((p, 100));
    }
    if let Some(d) = m.dragon_mut() {
        d.since_hurt = 0;
    }
    if m.is_dead() && !phase.sitting() {
        m.health = 1.0;
        set_phase(m, Phase::Dying);
    }
    let (health, max) = (m.health.max(0.0), m.max_health);
    if phase.sitting()
        && let Some(d) = m.dragon_mut()
    {
        d.sitting_damage += before - health;
        if d.sitting_damage > 0.25 * max {
            d.sitting_damage = 0.0;
            set_phase(m, Phase::TakingOff);
        }
    }
    fx::emit(
        ctx,
        fx::Fx::Damage(play::DamageEvent {
            entity_id: id,
            damage_type: ctx.damage_type(h.kind),
            direct: h.direct.or(h.by),
            cause: h.by,
        }),
    );
    fx::sound(
        ctx,
        "entity.ender_dragon.hurt",
        5,
        pos,
        5.0,
        0.8 + rnd() as f32 * 0.4,
    );
    fx::emit(
        ctx,
        fx::Fx::Data(play::SetEntityData {
            entity_id: id,
            entries: vec![(
                play::metadata::HEALTH,
                play::MetadataValue::Float(m.health.max(0.0)),
            )],
        }),
    );
    true
}

/// A crystal went: the one healing it takes ten points with it (an
/// explosion by the player behind it), and circling it goes for that
/// player (or the nearest within 64 blocks).
pub fn crystal_destroyed(
    ctx: &ServerContext,
    e: &mut Entity,
    crystal: i32,
    at: [f64; 3],
    by: Option<u128>,
) {
    let players: Vec<Target> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            let q = p.pose?;
            let mut t = Target::at(p.entity_id, p.profile.uuid, [q.x, q.y, q.z]);
            t.targetable = matches!(p.game_mode, 0 | 2);
            Some(t)
        })
        .collect();
    let player = by
        .and_then(|u| players.iter().find(|p| p.uuid == u))
        .or_else(|| {
            players
                .iter()
                .filter(|p| p.targetable && dist_sq(p.pos, at) <= 64.0 * 64.0)
                .min_by(|a, b| dist_sq(a.pos, at).total_cmp(&dist_sq(b.pos, at)))
        })
        .cloned();
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let healing = m.dragon().is_some_and(|d| d.crystal == Some(crystal));
    if healing {
        aim_at(e, Part::Head);
        let mut h = crate::mobs::hurt::Harm::new(
            10.0,
            if player.is_some() {
                "minecraft:player_explosion"
            } else {
                "minecraft:explosion"
            },
        );
        h.player = player.as_ref().map(|p| p.uuid);
        h.by = player.as_ref().map(|p| p.id);
        harm(ctx, e, &h);
    }
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.dragon().is_some_and(|d| d.phase == Phase::HoldingPattern)
        && let Some(p) = player.filter(|p| p.targetable)
    {
        let nodes_ready = m.dragon().is_some_and(|d| !d.nodes.is_empty());
        if nodes_ready {
            let w = World {
                players: std::slice::from_ref(&p),
                crystals: super::fight::crystals_alive(ctx),
                podium: [0.0, f64::from(super::fight::surface(ctx, 0, 0)), 0.0],
            };
            strafe(m, pos, &w, &p);
        } else {
            set_phase(m, Phase::StrafePlayer);
            if let Some(d) = m.dragon_mut() {
                d.attack = Some(p.id);
            }
        }
    }
}
