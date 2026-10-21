//! What the thrown, hung and ridden entities share while they tick: the
//! players and other entities they can hit, rays through blocks and boxes,
//! and what they ask the tick to do afterwards.

use mc_entity::{Aabb, Entity, Kind};
use mc_protocol::Position;

use crate::context::{Pose, ServerContext};
use crate::fx::Sync;

/// A player in the world.
#[derive(Clone, Copy, Debug)]
pub struct Who {
    pub uuid: u128,
    pub entity_id: i32,
    pub mode: i32,
    pub pose: Pose,
}

impl Who {
    pub fn aabb(&self) -> Aabb {
        let p = self.pose;
        Aabb {
            min: [p.x - 0.3, p.y, p.z - 0.3],
            max: [p.x + 0.3, p.y + 1.8, p.z + 0.3],
        }
    }

    pub fn pose_vec(&self) -> [f64; 3] {
        [self.pose.x, self.pose.y, self.pose.z]
    }

    /// Spectators are not hit by anything.
    pub fn solid(&self) -> bool {
        self.mode != 3
    }
}

/// Another entity a projectile can hit (frames, stands, boats...).
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub id: i32,
    pub aabb: Aabb,
}

/// An entity hurt by a projectile or a player; the tick applies it once the
/// entities have moved.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Hit {
    pub target: i32,
    pub amount: f32,
    /// The player behind it.
    pub by: Option<u128>,
    /// The projectile or other entity that did it.
    pub with: Option<i32>,
    /// Damage type (`minecraft:arrow`).
    pub kind: &'static str,
}

/// What an entity's tick leaves for the tick to do.
#[derive(Default)]
pub struct Out {
    pub hits: Vec<Hit>,
    pub sync: Vec<Sync>,
    /// Entities to add after this tick.
    pub spawn: Vec<Entity>,
    /// Metadata that changed.
    pub data: Vec<mc_protocol::packets::play::SetEntityData>,
}

pub struct Tick<'a> {
    pub ctx: &'a ServerContext,
    pub players: &'a [Who],
    pub targets: &'a [Target],
    pub out: &'a mut Out,
}

/// A uniform random number in [0, 1).
pub fn rnd() -> f64 {
    (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// Roughly normal with mean 0 and deviation 1.
pub fn gauss() -> f64 {
    (0..12).map(|_| rnd()).sum::<f64>() - 6.0
}

/// The block state at `p`; `None` where no chunk is loaded.
pub fn block_at(ctx: &ServerContext, p: [i32; 3]) -> Option<u32> {
    ctx.world
        .block(Position::new(p[0], p[1], p[2]), &ctx.data.blocks)
}

/// Whether a block stops projectiles. Unloaded chunks count as solid.
pub fn stops(ctx: &ServerContext, p: [i32; 3]) -> bool {
    block_at(ctx, p).is_none_or(|s| ctx.rules.blocks_motion(s))
}

pub fn in_water(ctx: &ServerContext, p: [f64; 3]) -> bool {
    block_at(
        ctx,
        [
            p[0].floor() as i32,
            p[1].floor() as i32,
            p[2].floor() as i32,
        ],
    )
    .is_some_and(|s| ctx.data.blocks.block_name(s) == Some("minecraft:water"))
}

/// Where a segment first enters a box (as a fraction of the segment).
pub fn ray_box(from: [f64; 3], to: [f64; 3], b: &Aabb) -> Option<f64> {
    let (mut near, mut far) = (0.0_f64, 1.0_f64);
    for i in 0..3 {
        let d = to[i] - from[i];
        if d.abs() < 1e-12 {
            if from[i] < b.min[i] || from[i] > b.max[i] {
                return None;
            }
        } else {
            let (a, c) = ((b.min[i] - from[i]) / d, (b.max[i] - from[i]) / d);
            near = near.max(a.min(c));
            far = far.min(a.max(c));
            if near > far {
                return None;
            }
        }
    }
    Some(near)
}

/// `b` grown by `m` on every side.
pub fn grow(b: &Aabb, m: f64) -> Aabb {
    Aabb {
        min: b.min.map(|v| v - m),
        max: b.max.map(|v| v + m),
    }
}

/// The first solid block along a segment: where (fraction), which block and
/// the face of it that was hit (0 down .. 5 east).
pub fn first_solid(
    ctx: &ServerContext,
    from: [f64; 3],
    to: [f64; 3],
) -> Option<(f64, [i32; 3], usize)> {
    let mut cell = from.map(|v| v.floor() as i32);
    if stops(ctx, cell) {
        return Some((0.0, cell, 1));
    }
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let step = d.map(|v| if v > 0.0 { 1 } else { -1 });
    let mut t_max = [f64::INFINITY; 3];
    let mut t_delta = [f64::INFINITY; 3];
    for i in 0..3 {
        if d[i] != 0.0 {
            t_delta[i] = 1.0 / d[i].abs();
            let edge = if d[i] > 0.0 {
                f64::from(cell[i] + 1) - from[i]
            } else {
                from[i] - f64::from(cell[i])
            };
            t_max[i] = edge / d[i].abs();
        }
    }
    // A segment crosses at most a few dozen cells.
    for _ in 0..256 {
        let axis = (0..3).min_by(|&a, &b| t_max[a].total_cmp(&t_max[b]))?;
        if t_max[axis] > 1.0 {
            return None;
        }
        let t = t_max[axis];
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
        if stops(ctx, cell) {
            let face = match (axis, step[axis] > 0) {
                (0, true) => 4,
                (0, false) => 5,
                (1, true) => 0,
                (1, false) => 1,
                (_, true) => 2,
                (_, false) => 3,
            };
            return Some((t, cell, face));
        }
    }
    None
}

/// What a flying thing runs into first.
pub enum Strike {
    Block,
    Player(usize),
    Target(usize),
}

/// The first thing along `from -> to` the entity (half-width `half`) hits,
/// as a fraction of the way; entities with an id in `skip` are not hit, nor
/// is `ignore` (the owner, for a few ticks).
pub fn trace(
    t: &Tick,
    from: [f64; 3],
    to: [f64; 3],
    half: f64,
    ignore: Option<u128>,
    skip: &[i32],
) -> Option<(f64, Strike)> {
    let mut best = first_solid(t.ctx, from, to).map(|(f, _, _)| (f, Strike::Block));
    let mut consider = |frac: f64, strike: Strike| {
        if best.as_ref().is_none_or(|(b, _)| frac < *b) {
            best = Some((frac, strike));
        }
    };
    for (i, p) in t.players.iter().enumerate() {
        if !p.solid() || Some(p.uuid) == ignore || skip.contains(&p.entity_id) {
            continue;
        }
        if let Some(f) = ray_box(from, to, &grow(&p.aabb(), half)) {
            consider(f, Strike::Player(i));
        }
    }
    for (i, g) in t.targets.iter().enumerate() {
        if skip.contains(&g.id) {
            continue;
        }
        if let Some(f) = ray_box(from, to, &grow(&g.aabb, half)) {
            consider(f, Strike::Target(i));
        }
    }
    best
}

/// The name of a player, for death messages.
pub fn name_of(ctx: &ServerContext, uuid: u128) -> Option<String> {
    crate::tick::lock(&ctx.players)
        .get(&uuid)
        .map(|p| p.profile.name.clone())
}

/// Carries out what an entity's tick asked for on other entities. `removed`
/// and `gone` collect entities that went (ids, uuids).
pub fn apply(
    ctx: &ServerContext,
    entities: &mut [Entity],
    removed: &mut Vec<i32>,
    gone: &mut Vec<u128>,
    hit: Hit,
) {
    let Some(e) = entities
        .iter_mut()
        .find(|e| e.id == hit.target && !e.removed)
    else {
        return;
    };
    let mut fresh = Vec::new();
    let events = match &e.kind {
        Kind::Hung(_) => crate::decor::hurt(ctx, e, &hit, &mut fresh),
        _ => Vec::new(),
    };
    for event in events {
        let _ = ctx.events.send(event);
    }
    if e.removed {
        removed.push(e.id);
        gone.push(e.uuid);
    }
    crate::decor::spawn_all(ctx, fresh);
}

/// A player clicks an entity.
pub fn use_entity(
    ctx: &ServerContext,
    actor: crate::interact::Actor,
    entity_id: i32,
    hand: i32,
    attack: bool,
    at: Option<[f64; 3]>,
) -> crate::interact::Outcome {
    let mut outcome = crate::interact::Outcome::default();
    let mut fresh = Vec::new();
    {
        let mut entities = crate::tick::lock(&ctx.entities);
        let Some(e) = entities
            .iter_mut()
            .find(|e| e.id == entity_id && !e.removed)
        else {
            return outcome;
        };
        // Within reach of the eyes.
        let b = e.aabb();
        let eye = [actor.pose.x, actor.pose.y + 1.62, actor.pose.z];
        let d2: f64 = (0..3)
            .map(|i| (b.min[i] - eye[i]).max(eye[i] - b.max[i]).max(0.0).powi(2))
            .sum();
        if d2 > 36.0 {
            return outcome;
        }
        if let Kind::Hung(_) = &e.kind {
            crate::decor::interact(ctx, actor, e, hand, attack, at, &mut outcome, &mut fresh);
        }
    }
    crate::decor::spawn_all(ctx, fresh);
    outcome
}
