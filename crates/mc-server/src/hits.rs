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
    /// The entity type of a mob (`minecraft:cod`), for Impaling.
    pub kind: Option<&'static str>,
}

/// An entity hurt by a projectile or a player; the tick applies it once the
/// entities have moved.
#[derive(Clone, Debug)]
#[allow(dead_code)] // `by`, `with` and `kind` are for whoever reacts to the hit
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

/// Effects for a mob from a potion, cloud or arrow; the tick gives them
/// once the entities have moved.
#[derive(Clone, Debug)]
pub struct Dose {
    pub target: i32,
    pub effects: Vec<mc_entity::extra::EffectSpec>,
    /// How much of their strength they have (1 at the full).
    pub strength: f64,
    /// From a splash: effects with a second or less left are not given.
    pub splash: bool,
    /// The player behind it.
    pub by: Option<u128>,
}

/// What an entity's tick leaves for the tick to do.
#[derive(Default)]
pub struct Out {
    pub hits: Vec<Hit>,
    pub doses: Vec<Dose>,
    pub sync: Vec<Sync>,
    /// Entities to add after this tick.
    pub spawn: Vec<Entity>,
    /// Metadata that changed.
    pub data: Vec<mc_protocol::packets::play::SetEntityData>,
    /// Explosions to set off.
    pub blasts: Vec<crate::explosion::Blast>,
    /// Wind bursts to set off.
    pub gusts: Vec<crate::projectiles::wind::Gust>,
    /// Cells a fireball set alight (fire goes there if they are air).
    pub fires: Vec<[i32; 3]>,
    /// Command block minecarts an activator rail set off (entity ids): they
    /// run once the entities are back in the world.
    pub carts: Vec<i32>,
}

pub struct Tick<'a> {
    pub ctx: &'a ServerContext,
    pub players: &'a [Who],
    pub targets: &'a [Target],
    pub out: &'a mut Out,
    /// The mob that shot the projectile ticking, if a mob did.
    pub shooter: Option<(i32, &'static str)>,
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
        // A segment ending right on a face does not enter the block.
        if t_max[axis] >= 1.0 {
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
#[derive(Clone, Copy, Debug)]
pub enum Strike {
    /// A block: its position and the face hit (0 down .. 5 east).
    Block {
        cell: [i32; 3],
        face: usize,
    },
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
    let mut best =
        first_solid(t.ctx, from, to).map(|(f, cell, face)| (f, Strike::Block { cell, face }));
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

/// The signal a projectile that stopped at `point` on the `face` of the
/// block at `cell` gives a target block there: 15 from the middle of the
/// face down to 1 at its edge (minecraft.wiki, "Target").
pub fn target_strength(cell: [i32; 3], face: usize, point: [f64; 3]) -> u8 {
    let off = |i: usize| {
        let v = point[i] - f64::from(cell[i]);
        (v - v.floor() - 0.5).abs()
    };
    let far = match face {
        0 | 1 => off(0).max(off(2)),
        2 | 3 => off(0).max(off(1)),
        _ => off(1).max(off(2)),
    };
    let share = ((0.5 - far) / 0.5).clamp(0.0, 1.0);
    ((15.0 * share).ceil() as u8).max(1)
}

/// A projectile stopped at `point` against a block: a target block gives a
/// signal for 8 ticks (20 for an arrow or a trident, `long`).
///
/// Whether a target block was hit.
pub fn hit_block(ctx: &ServerContext, strike: &Strike, point: [f64; 3], long: bool) -> bool {
    let Strike::Block { cell, face } = *strike else {
        return false;
    };
    if block_at(ctx, cell).and_then(|s| ctx.data.blocks.block_name(s)) != Some("minecraft:target") {
        return false;
    }
    let mut level = crate::level::WorldLevel::new(ctx);
    let logic = level.logic();
    let strength = target_strength(cell, face, point);
    let hold = if long { 20 } else { 8 };
    mc_tick::redstone::target_hit(
        &mut level,
        logic,
        crate::level::to_pos(Position::new(cell[0], cell[1], cell[2])),
        strength,
        hold,
    );
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx
            .events
            .send(crate::context::Event::BlocksChanged { by: 0, changes });
    }
    true
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
    // Where the projectile came from, for the push.
    let from = hit
        .with
        .and_then(|w| entities.iter().find(|e| e.id == w))
        .map(|p| [p.pos[0] - p.vel[0], p.pos[1], p.pos[2] - p.vel[2]]);
    let part = crate::bosses::part_owner(entities, hit.target);
    let target = part.map_or(hit.target, |(id, _)| id);
    let Some(e) = entities.iter_mut().find(|e| e.id == target && !e.removed) else {
        return;
    };
    if let Kind::Crystal(c) = &mut e.kind {
        crate::bosses::crystal::strike(c, hit.by, hit.kind);
        return;
    }
    if let Some((_, part)) = part {
        crate::bosses::dragon::aim_at(e, part);
    }
    if matches!(e.kind, Kind::Mob(_) | Kind::Merchant(_)) {
        let mut h = crate::mobs::hurt::Harm::new(hit.amount, hit.kind);
        h.player = hit.by;
        h.by = hit
            .by
            .and_then(|o| crate::tick::lock(&ctx.players).get(&o).map(|p| p.entity_id));
        if h.player.is_some() {
            h.killer = Some("minecraft:player".into());
        }
        h.direct = hit.with;
        h.from = from;
        crate::mobs::hurt::harm(ctx, e, &h);
        return;
    }
    let mut fresh = Vec::new();
    let events = match &e.kind {
        Kind::Hung(_) => crate::decor::hurt(ctx, e, &hit, &mut fresh),
        Kind::Vehicle(_) => crate::vehicles::hurt(ctx, e, &hit, &mut fresh),
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
        let part = crate::bosses::part_owner(&entities, entity_id);
        let owner = part.map_or(entity_id, |(id, _)| id);
        // Those on leads, for how far they are from a new holder.
        let leashed: Vec<Entity> = {
            let links = crate::tick::lock(&ctx.leashes);
            entities
                .iter()
                .filter(|e| links.links.contains_key(&e.id))
                .cloned()
                .collect()
        };
        let Some(e) = entities.iter_mut().find(|e| e.id == owner && !e.removed) else {
            return outcome;
        };
        // Within reach of the eyes.
        let b = match (part, &e.kind) {
            (Some((_, part)), Kind::Mob(m)) => m.dragon().map_or(e.aabb(), |d| d.part_box(part)),
            _ => e.aabb(),
        };
        let eye = [actor.pose.x, actor.pose.y + 1.62, actor.pose.z];
        let d2: f64 = (0..3)
            .map(|i| (b.min[i] - eye[i]).max(eye[i] - b.max[i]).max(0.0).powi(2))
            .sum();
        if d2 > 36.0 {
            return outcome;
        }
        let handled = matches!(e.kind, Kind::Mob(_))
            && crate::mobs::chest::use_on(ctx, &actor, e, hand, attack, &mut outcome);
        match &e.kind {
            Kind::Hung(_) => {
                crate::decor::interact(
                    ctx,
                    actor,
                    e,
                    (hand, attack, at),
                    &leashed,
                    &mut outcome,
                    &mut fresh,
                );
            }
            Kind::Vehicle(_) => {
                crate::vehicles::interact(
                    ctx,
                    actor,
                    e,
                    (hand, attack),
                    &leashed,
                    &mut outcome,
                    &mut fresh,
                );
            }
            Kind::Mob(_) if !handled => crate::mobs::clicked(ctx, &actor, entity_id, hand, attack),
            // Villagers and wandering traders trade on a click; a blow hurts.
            Kind::Merchant(_) if attack => {
                crate::mobs::clicked(ctx, &actor, entity_id, hand, attack)
            }
            _ => {}
        }
        if let Kind::Crystal(c) = &mut e.kind
            && attack
            && actor.game_mode != 3
        {
            crate::bosses::crystal::strike(c, Some(actor.uuid), "minecraft:player_attack");
        }
    }
    crate::decor::spawn_all(ctx, fresh);
    outcome
}
