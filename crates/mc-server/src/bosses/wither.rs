//! The wither (minecraft.wiki, "Wither"):
//! - built as a T of four soul sand or soul soil with three wither skeleton
//!   skulls on top, air beside the stem, in any orientation, the last
//!   piece a skull; not on peaceful. The blocks go and the wither appears
//!   at the stem's foot;
//! - for 220 ticks it grows (from a third of its health, 10 points every
//!   ten ticks), cannot be hurt and does nothing; then it explodes with
//!   power 7 (`mob_griefing`) and its sound is heard by all;
//! - it goes after any living thing within 40 blocks but the undead and
//!   ghasts; it hovers 5 blocks above its target (level with it below half
//!   health), closing in to 3 blocks;
//! - its middle head shoots a skull every 40 ticks at its target (one in a
//!   thousand blue); the side heads pick their own targets within 20 blocks
//!   they can see and shoot every 40–59 ticks, and on normal and hard fire a
//!   blue skull somewhere near when idle too long (being hurt counts three);
//! - hurt, it breaks the 3×4×3 blocks around it 20 ticks later (all but the
//!   `wither_immune` ones, `mob_griefing`); it heals a point a second;
//! - below half health arrows and tridents do not hurt it; nothing undead
//!   hurts it, nor its own skulls, nor drowning;
//! - it drops a nether star.

use mc_entity::mob::Target;
use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;

use super::{below, dist_sq, rnd};
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::level::WorldLevel;

/// Ticks of the spawn animation.
pub const SPAWN_TICKS: i32 = 220;
/// Power of the explosion when it is done.
const SPAWN_POWER: f32 = 7.0;

/// Whether a wither may not break this block (the `wither_immune` tag).
pub fn immune_block(ctx: &ServerContext, name: &str) -> bool {
    super::block_tag(ctx, "minecraft:wither_immune").contains(name)
}

fn powered(m: &mc_entity::Mob) -> bool {
    m.health <= m.max_health / 2.0
}

/// A hit on the wither: `Some(false)` when it shrugs it off.
pub fn harm(_ctx: &ServerContext, e: &mut Entity, h: &crate::mobs::hurt::Harm) -> Option<bool> {
    let Kind::Mob(m) = &mut e.kind else {
        return None;
    };
    let bypass = matches!(h.kind, "minecraft:out_of_world" | "minecraft:generic_kill");
    let w = m.wither()?;
    if w.invulnerable > 0 && !bypass {
        return Some(false);
    }
    if h.kind == "minecraft:drown" {
        return Some(false);
    }
    let undead = h
        .killer
        .as_deref()
        .and_then(Species::from_id)
        .is_some_and(Species::undead);
    if undead {
        return Some(false);
    }
    if powered(m) && matches!(h.kind, "minecraft:arrow" | "minecraft:trident") {
        return Some(false);
    }
    if let Some(w) = m.wither_mut() {
        if w.destroy_blocks <= 0 {
            w.destroy_blocks = 20;
        }
        for n in &mut w.idle_head_updates {
            *n += 3;
        }
    }
    None
}

/// The nether star it leaves.
pub fn drop_star(ctx: &ServerContext, pos: [f64; 3]) {
    if let Some(star) = ctx.items.id("minecraft:nether_star") {
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.3, pos[2]], star, 1);
    }
}

/// What it may go after: players it may attack, and living mobs that are
/// not undead (nor ghasts).
fn valid(entities: &[Entity], players: &[Target], id: i32) -> Option<([f64; 3], f64)> {
    if let Some(p) = players.iter().find(|p| p.id == id) {
        return p.targetable.then_some((p.pos, 1.62));
    }
    entities
        .iter()
        .find(|e| e.id == id && !e.removed)
        .and_then(|e| match &e.kind {
            Kind::Mob(m)
                if m.dying.is_none()
                    && !m.is_dead()
                    && !m.species.undead()
                    && m.species != Species::Ghast =>
            {
                Some((e.pos, m.eye_height()))
            }
            Kind::Merchant(_) => Some((e.pos, 1.62)),
            _ => None,
        })
}

/// Living things within `range` of `at` it may go after.
fn candidates(
    entities: &[Entity],
    players: &[Target],
    me: i32,
    at: [f64; 3],
    range: [f64; 3],
) -> Vec<(i32, [f64; 3], f64)> {
    let within = |p: [f64; 3]| (0..3).all(|k| (p[k] - at[k]).abs() <= range[k]);
    let mut out: Vec<(i32, [f64; 3], f64)> = players
        .iter()
        .filter(|p| p.targetable && within(p.pos))
        .map(|p| (p.id, p.pos, 1.62))
        .collect();
    for e in entities
        .iter()
        .filter(|e| e.id != me && !e.removed && within(e.pos))
    {
        if let Some((pos, eye)) = valid(entities, &[], e.id) {
            out.push((e.id, pos, eye));
        }
    }
    out
}

/// Whether nothing solid is between two points.
fn sees(ctx: &ServerContext, from: [f64; 3], to: [f64; 3]) -> bool {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let steps = (len * 4.0).ceil() as i32;
    for s in 1..steps {
        let f = f64::from(s) / f64::from(steps);
        let p = [from[0] + d[0] * f, from[1] + d[1] * f, from[2] + d[2] * f];
        let Some(state) = ctx.world.block(
            Position::new(
                p[0].floor() as i32,
                p[1].floor() as i32,
                p[2].floor() as i32,
            ),
            &ctx.data.blocks,
        ) else {
            continue;
        };
        if ctx.rules.blocks_motion(state) {
            return false;
        }
    }
    true
}

/// Where head `i` (0 the middle) is.
fn head_pos(pos: [f64; 3], body_yaw: f32, i: usize) -> [f64; 3] {
    if i == 0 {
        return [pos[0], pos[1] + 3.0, pos[2]];
    }
    let a = (f64::from(body_yaw) + 180.0 * (i as f64 - 1.0)).to_radians();
    [pos[0] + a.cos() * 1.3, pos[1] + 2.2, pos[2] + a.sin() * 1.3]
}

/// Head `head` shoots a skull at `at`.
fn shoot(ctx: &ServerContext, me: &Entity, head: usize, at: [f64; 3], dangerous: bool) {
    let Kind::Mob(m) = &me.kind else {
        return;
    };
    fx::level_event(ctx, 1024, me.pos, 0);
    let from = head_pos(me.pos, m.yaw, head);
    let d = [at[0] - from[0], at[1] - from[1], at[2] - from[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-9);
    crate::projectiles::spawn_by_mob(
        ctx,
        mc_entity::extra::Shot::WitherSkull { dangerous },
        (me.id, "Wither"),
        from,
        d.map(|c| c / len * 0.1),
    );
}

/// One tick of the wither at `i`.
pub fn tick(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    i: usize,
    players: &[Target],
    now: i64,
    blasts: &mut Vec<crate::explosion::Blast>,
) {
    let mut me = entities[i].clone();
    let id = me.id;
    let Kind::Mob(m) = &mut me.kind else {
        return;
    };
    if m.no_ai {
        return;
    }
    let difficulty = ctx.difficulty();
    let Some(w) = m.wither_mut() else {
        return;
    };
    // Growing: nothing else.
    if w.invulnerable > 0 {
        let left = w.invulnerable - 1;
        if left <= 0 {
            blasts.push(crate::explosion::Blast {
                center: [me.pos[0], me.pos[1] + 3.5 * 0.85, me.pos[2]],
                power: SPAWN_POWER,
                source: id,
                owner: None,
                mob: true,
                dangerous: false,
                by: Some("minecraft:wither"),
                fire: false,
                respawn: None,
            });
            fx::emit(
                ctx,
                fx::Fx::Level(mc_protocol::packets::play::LevelEvent {
                    event: 1023,
                    position: Position::new(
                        me.pos[0].floor() as i32,
                        me.pos[1].floor() as i32,
                        me.pos[2].floor() as i32,
                    ),
                    data: 0,
                    global: true,
                }),
            );
        }
        w.invulnerable = left;
        m.dirty = true;
        if m.age.is_multiple_of(10) {
            m.health = (m.health + 10.0).min(m.max_health);
        }
        me.vel = [0.0; 3];
        entities[i] = me;
        return;
    }
    // Its target: who hurt it, or the nearest it may go after.
    if let Some(t) = m.target
        && valid(entities, players, t).is_none_or(|(p, _)| dist_sq(p, me.pos) > 40.0 * 40.0 * 1.21)
    {
        m.target = None;
    }
    if let Some((by, _)) = m.revenge
        && by >= 0
        && valid(entities, players, by).is_some()
    {
        m.target = Some(by);
    }
    if m.target.is_none() && below(10) == 0 {
        let near = candidates(entities, players, id, me.pos, [40.0, 4.0 * 40.0, 40.0]);
        m.target = near
            .iter()
            .filter(|(_, p, _)| dist_sq(*p, me.pos) <= 40.0 * 40.0)
            .min_by(|a, b| dist_sq(a.1, me.pos).total_cmp(&dist_sq(b.1, me.pos)))
            .map(|c| c.0);
    }
    let target = m
        .target
        .and_then(|t| valid(entities, players, t).map(|v| (t, v)));
    // The middle head.
    let mut shots: Vec<(usize, [f64; 3], bool)> = Vec::new();
    let yaw = m.yaw;
    let w = m.wither_mut().expect("wither");
    if let Some((_, (tp, eye))) = target {
        let aim = [tp[0], tp[1] + eye * 0.5, tp[2]];
        let seen = sees(ctx, head_pos(me.pos, 0.0, 0), [tp[0], tp[1] + eye, tp[2]]);
        w.seen = if seen {
            w.seen.max(0) + 1
        } else {
            w.seen.min(0) - 1
        };
        w.attack_time -= 1;
        if w.attack_time == 0 {
            if seen {
                shots.push((0, aim, rnd() < 0.001));
                w.attack_time = 40;
            }
        } else if w.attack_time < 0 {
            w.attack_time = 40;
        }
    } else {
        w.attack_time = -1;
        w.seen = 0;
    }
    // The side heads.
    for h in 1..3 {
        let k = h - 1;
        if now < w.next_head_update[k] {
            continue;
        }
        w.next_head_update[k] = now + 10 + i64::from(below(10));
        if difficulty >= 2 {
            let idle = w.idle_head_updates[k];
            w.idle_head_updates[k] = idle + 1;
            if idle > 15 {
                let at = [
                    me.pos[0] + (rnd() * 2.0 - 1.0) * 10.0,
                    me.pos[1] + (rnd() * 2.0 - 1.0) * 5.0,
                    me.pos[2] + (rnd() * 2.0 - 1.0) * 10.0,
                ];
                shots.push((h, at, true));
                w.idle_head_updates[k] = 0;
            }
        }
        let current = w.heads[h];
        if current != 0 {
            let ok = valid(entities, players, current).filter(|(p, eye)| {
                dist_sq(*p, me.pos) <= 900.0
                    && sees(ctx, head_pos(me.pos, yaw, h), [p[0], p[1] + eye, p[2]])
            });
            match ok {
                Some((p, eye)) => {
                    shots.push((h, [p[0], p[1] + eye * 0.5, p[2]], false));
                    w.next_head_update[k] = now + 40 + i64::from(below(20));
                    w.idle_head_updates[k] = 0;
                }
                None => w.heads[h] = 0,
            }
        } else {
            let mut near = candidates(entities, players, id, me.pos, [20.0, 8.0, 20.0]);
            for _ in 0..10 {
                if near.is_empty() {
                    break;
                }
                let pick = below(near.len() as i32) as usize;
                let (cid, p, eye) = near[pick];
                if sees(ctx, head_pos(me.pos, yaw, h), [p[0], p[1] + eye, p[2]]) {
                    w.heads[h] = cid;
                    break;
                }
                near.remove(pick);
            }
        }
    }
    w.heads[0] = target.map_or(0, |(t, _)| t);
    let heads = w.heads;
    // Hurt: the blocks around go.
    if w.destroy_blocks > 0 {
        w.destroy_blocks -= 1;
        if w.destroy_blocks == 0 && ctx.game_rule_bool("minecraft:mob_griefing") {
            break_around(ctx, level, me.pos);
        }
    }
    if m.age.is_multiple_of(20) {
        m.health = (m.health + 1.0).min(m.max_health);
    }
    if heads != m.wither().map_or([0; 3], |w| w.heads) {
        m.dirty = true;
    }
    m.dirty = true;
    // Hovering over its target.
    let mut vel = [me.vel[0], me.vel[1] * 0.6, me.vel[2]];
    if let Some((_, (tp, _))) = target {
        let mut dy = vel[1];
        if me.pos[1] < tp[1] || !powered(m) && me.pos[1] < tp[1] + 5.0 {
            dy = dy.max(0.0);
            dy += 0.3 - dy * 0.6;
        }
        vel[1] = dy;
        let flat = [tp[0] - me.pos[0], tp[2] - me.pos[2]];
        let d2 = flat[0] * flat[0] + flat[1] * flat[1];
        if d2 > 9.0 {
            let len = d2.sqrt();
            vel[0] += flat[0] / len * 0.3 - vel[0] * 0.6;
            vel[2] += flat[1] / len * 0.3 - vel[2] * 0.6;
        }
    }
    if vel[0] * vel[0] + vel[2] * vel[2] > 0.05 {
        m.yaw = (vel[2].atan2(vel[0]).to_degrees() - 90.0) as f32;
    }
    if let Some((_, (tp, eye))) = target {
        let d = [tp[0] - me.pos[0], tp[2] - me.pos[2]];
        let _ = eye;
        m.head_yaw = (d[1].atan2(d[0]).to_degrees() - 90.0) as f32;
    } else {
        m.head_yaw = m.yaw;
    }
    let (moved, on_ground) = mc_entity::move_entity(me.aabb(), vel, &crate::tick::Solid(level));
    for k in 0..3 {
        me.pos[k] += moved[k];
        if (moved[k] - vel[k]).abs() > 1e-9 {
            vel[k] = 0.0;
        }
    }
    me.on_ground = on_ground;
    me.vel = [vel[0] * 0.91, vel[1] * 0.91, vel[2] * 0.91];
    for (h, at, dangerous) in shots {
        shoot(ctx, &me, h, at, dangerous);
    }
    entities[i] = me;
}

/// Breaks the 3×4×3 blocks around its feet that a wither may break.
fn break_around(ctx: &ServerContext, level: &mut WorldLevel, pos: [f64; 3]) {
    let (x, y, z) = (
        pos[0].floor() as i32,
        pos[1].floor() as i32,
        pos[2].floor() as i32,
    );
    let blocks = &ctx.data.blocks;
    let mut broke = false;
    let mut drops = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            for dy in 0..=3 {
                let p = Position::new(x + dx, y + dy, z + dz);
                let Some(s) = ctx.world.block(p, blocks) else {
                    continue;
                };
                let name = blocks.block_name(s).unwrap_or("");
                if blocks.is_air(s) || immune_block(ctx, name) {
                    continue;
                }
                if crate::containers::kind_of(name).is_some() {
                    let (items, _) = crate::containers::broken(ctx, p);
                    drops.extend(items.into_iter().map(|i| (p, i)));
                    let _ = ctx.events.send(Event::ContainerGone { position: p });
                }
                if ctx.game_rule_bool("minecraft:block_drops") {
                    let props = |k: &str| blocks.property(s, k).map(str::to_owned);
                    for d in ctx.loot.drops(name, &props) {
                        drops.push((p, d.stack()));
                    }
                }
                fx::level_event(
                    ctx,
                    2001,
                    [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
                    s as i32,
                );
                broke |= level.set_block(p, ctx.shapes.air());
            }
        }
    }
    for (p, stack) in drops {
        crate::tick::spawn_stack_at(
            ctx,
            [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ],
            &stack,
        );
    }
    if broke {
        fx::level_event(ctx, 1022, pos, 0);
    }
}

fn is_skull(name: &str) -> bool {
    matches!(
        name,
        "minecraft:wither_skeleton_skull" | "minecraft:wither_skeleton_wall_skull"
    )
}

/// The six directions as unit steps.
const DIRS: [[i32; 3]; 6] = [
    [0, 1, 0],
    [0, -1, 0],
    [1, 0, 0],
    [-1, 0, 0],
    [0, 0, 1],
    [0, 0, -1],
];

/// The summoning T with the skull just placed at `p`: the nine cells
/// (row 0 the skulls, row 2 the stem's foot), from top-left.
fn find_pattern(ctx: &ServerContext, p: Position) -> Option<([[Position; 3]; 3], [i32; 3])> {
    let blocks = &ctx.data.blocks;
    let base = super::block_tag(ctx, "minecraft:wither_summon_base_blocks");
    let name = |q: Position| {
        ctx.world
            .block(q, blocks)
            .and_then(|s| blocks.block_name(s))
            .unwrap_or("minecraft:air")
    };
    let air = |q: Position| ctx.world.block(q, blocks).is_none_or(|s| blocks.is_air(s));
    for up in DIRS {
        for right in DIRS {
            let cross = up[0] * right[0] + up[1] * right[1] + up[2] * right[2];
            if cross != 0 {
                continue;
            }
            for c in 0..3 {
                let origin = [p.x - right[0] * c, p.y - right[1] * c, p.z - right[2] * c];
                let cell = |r: i32, c: i32| {
                    Position::new(
                        origin[0] + right[0] * c - up[0] * r,
                        origin[1] + right[1] * c - up[1] * r,
                        origin[2] + right[2] * c - up[2] * r,
                    )
                };
                let ok = (0..3).all(|c| is_skull(name(cell(0, c))))
                    && (0..3).all(|c| base.contains(name(cell(1, c))))
                    && base.contains(name(cell(2, 1)))
                    && air(cell(2, 0))
                    && air(cell(2, 2));
                if ok {
                    let cells =
                        std::array::from_fn(|r| std::array::from_fn(|c| cell(r as i32, c as i32)));
                    return Some((cells, right));
                }
            }
        }
    }
    None
}

/// A wither skeleton skull was placed at `p`: if it finishes the T, the
/// wither comes. Events go into `events`.
pub fn placed(ctx: &ServerContext, events: &mut Vec<Event>, p: Position, state: u32) {
    if !ctx.data.blocks.block_name(state).is_some_and(is_skull) || ctx.difficulty() == 0 {
        return;
    }
    let Some((cells, right)) = find_pattern(ctx, p) else {
        return;
    };
    let mut level = WorldLevel::new(ctx);
    for row in &cells {
        for &q in row {
            if let Some(s) = ctx.world.block(q, &ctx.data.blocks)
                && !ctx.data.blocks.is_air(s)
            {
                fx::level_event(
                    ctx,
                    2001,
                    [f64::from(q.x), f64::from(q.y), f64::from(q.z)],
                    s as i32,
                );
                level.set_block(q, ctx.shapes.air());
            }
        }
    }
    let foot = cells[2][1];
    let mut e = crate::mobs::new_mob(
        ctx,
        Species::Wither,
        [
            f64::from(foot.x) + 0.5,
            f64::from(foot.y) + 0.55,
            f64::from(foot.z) + 0.5,
        ],
    );
    if let Kind::Mob(m) = &mut e.kind {
        m.yaw = if right[0] != 0 { 0.0 } else { 90.0 };
        m.head_yaw = m.yaw;
        m.health = m.max_health / 3.0;
        if let Some(w) = m.wither_mut() {
            w.invulnerable = SPAWN_TICKS;
        }
    }
    events.push(Event::BlocksChanged {
        by: 0,
        changes: level.into_changes(),
    });
    crate::criteria::summoned(ctx, &e);
    crate::tick::spawn_entity(ctx, e);
}
