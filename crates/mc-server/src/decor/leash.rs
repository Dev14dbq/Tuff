//! Leads (minecraft.wiki, "Lead"; the 1.21.6 rework). A lead ties an
//! entity to a holder: a player, a knot on a fence, or another entity (two
//! mobs, a mob and a boat). Within six blocks (between the centres of their
//! boxes) the leashed one roams free; beyond that the lead pulls it back and
//! keeps it from moving away, and it turns to face the holder; beyond twelve
//! the lead snaps and drops. Shears snip every lead of an entity.

use std::collections::HashMap;

use mc_entity::extra::{Craft, Decor, Hung};
use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::SetEntityLink;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held;
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Who holds a lead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    Player(u128),
    /// The entity id of a knot (or of another entity holding it).
    Knot(i32),
}

/// How far a leashed entity roams before the lead pulls.
pub const FREE: f64 = 6.0;
/// How far a lead stretches before it snaps.
pub const SNAP: f64 = 12.0;

/// Whether a lead goes on this entity: mobs other than monsters (a wolf
/// only while calm), and boats.
pub fn leashable(e: &Entity) -> bool {
    match &e.kind {
        Kind::Mob(m) => {
            m.species.category() != mc_entity::mob::Category::Monster
                && !(m.species == mc_entity::Species::Wolf && m.anger > 0)
                && m.dying.is_none()
        }
        Kind::Vehicle(v) => matches!(v.craft, Craft::Boat { .. }),
        _ => false,
    }
}

/// The centre of an entity's box.
fn centre(e: &Entity) -> [f64; 3] {
    let b = e.aabb();
    [
        (b.min[0] + b.max[0]) / 2.0,
        (b.min[1] + b.max[1]) / 2.0,
        (b.min[2] + b.max[2]) / 2.0,
    ]
}

fn player_centre(ctx: &ServerContext, player: u128) -> Option<[f64; 3]> {
    lock(&ctx.players)
        .get(&player)
        .filter(|p| p.dimension == ctx.world.dimension)
        .and_then(|p| p.pose)
        .map(|p| [p.x, p.y + 0.9, p.z])
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// What a player does to a leashable entity with a lead in mind, and
/// whether that was all the use did. `held` is the item in hand and its
/// slot. In order: shears snip the entity's leads (its own and those it
/// holds); sneaking with leashed entities ties them to this one (if none
/// would snap at once); the player leading it lets go; a lead ties it to
/// the player (taking it from a knot or another entity, never from another
/// player).
pub fn use_on(
    ctx: &ServerContext,
    e: &Entity,
    (player, game_mode, sneaking): (u128, i32, bool),
    held: Option<(&str, usize)>,
    entities: &[Entity],
    fresh: &mut Vec<Entity>,
) -> bool {
    let creative = game_mode == 1;
    let holder = lock(&ctx.leashes).links.get(&e.id).copied();
    let holding = held_by(ctx, Holder::Knot(e.id));
    if let Some(("minecraft:shears", slot)) = held
        && (holder.is_some() || !holding.is_empty())
    {
        let at = e.pos;
        let mut cut = holding;
        if holder.is_some() {
            cut.push(e.id);
        }
        for id in cut {
            detach(ctx, id);
            if !creative {
                fresh.extend(lead_at(ctx, at));
            }
        }
        fx::sound(ctx, "item.shears.snip", fx::category::PLAYERS, at, 1.0, 1.0);
        if !creative {
            held::wear(ctx, player, slot, 1);
        }
        return true;
    }
    let mine = held_by(ctx, Holder::Player(player));
    if sneaking && !mine.is_empty() {
        let here = centre(e);
        let moved: Vec<i32> = mine.into_iter().filter(|&id| id != e.id).collect();
        let fits = moved.iter().all(|id| {
            entities
                .iter()
                .find(|o| o.id == *id)
                .is_none_or(|o| dist(centre(o), here) <= SNAP)
        });
        if moved.is_empty() || !fits {
            return false;
        }
        for id in moved {
            attach(ctx, id, Holder::Knot(e.id));
        }
        fx::sound(
            ctx,
            "item.lead.tied",
            fx::category::NEUTRAL,
            e.pos,
            1.0,
            1.0,
        );
        return true;
    }
    if holder == Some(Holder::Player(player)) {
        detach(ctx, e.id);
        fx::sound(
            ctx,
            "item.lead.untied",
            fx::category::NEUTRAL,
            e.pos,
            1.0,
            1.0,
        );
        if !creative {
            fresh.extend(lead_at(ctx, e.pos));
        }
        return true;
    }
    let Some(("minecraft:lead", slot)) = held else {
        return false;
    };
    if matches!(holder, Some(Holder::Player(_))) || !leashable(e) {
        return false;
    }
    if player_centre(ctx, player).is_some_and(|p| dist(p, centre(e)) > SNAP) {
        return false;
    }
    if holder.is_some() {
        // Taken from a knot or an entity: that lead snaps.
        detach(ctx, e.id);
        fresh.extend(lead_at(ctx, e.pos));
    }
    attach(ctx, e.id, Holder::Player(player));
    fx::sound(
        ctx,
        "item.lead.tied",
        fx::category::NEUTRAL,
        e.pos,
        1.0,
        1.0,
    );
    if !creative {
        held::consume(ctx, player, slot, 1);
    }
    true
}

/// Leashed entity id -> holder.
#[derive(Debug, Default)]
pub struct Leashes {
    pub links: HashMap<i32, Holder>,
}

fn holder_id(ctx: &ServerContext, h: Holder) -> Option<i32> {
    match h {
        Holder::Player(u) => lock(&ctx.players).get(&u).map(|p| p.entity_id),
        Holder::Knot(id) => Some(id),
    }
}

/// Ties `entity` to `holder`; clients get the link.
pub fn attach(ctx: &ServerContext, entity: i32, holder: Holder) {
    lock(&ctx.leashes).links.insert(entity, holder);
    if let Some(h) = holder_id(ctx, holder) {
        fx::emit(
            ctx,
            fx::Fx::Link(SetEntityLink {
                entity_id: entity,
                holder: h,
            }),
        );
    }
}

/// Cuts the lead of `entity`; whether there was one.
pub fn detach(ctx: &ServerContext, entity: i32) -> bool {
    let had = lock(&ctx.leashes).links.remove(&entity).is_some();
    if had {
        fx::emit(
            ctx,
            fx::Fx::Link(SetEntityLink {
                entity_id: entity,
                holder: 0,
            }),
        );
    }
    had
}

/// What `holder` has on leads.
pub fn held_by(ctx: &ServerContext, holder: Holder) -> Vec<i32> {
    lock(&ctx.leashes)
        .links
        .iter()
        .filter(|(_, h)| **h == holder)
        .map(|(&e, _)| e)
        .collect()
}

/// A knot (or another holder) goes: what hung on it is let go, and each
/// lead drops where the holder was (`at`, none in creative).
pub fn knot_gone(ctx: &ServerContext, knot: i32, at: Option<[f64; 3]>, fresh: &mut Vec<Entity>) {
    for id in held_by(ctx, Holder::Knot(knot)) {
        detach(ctx, id);
        if let Some(at) = at {
            fresh.extend(lead_at(ctx, at));
        }
    }
}

/// Moves everything tied to a player to a knot, unless one of them would
/// snap at once there.
fn move_to_knot(
    ctx: &ServerContext,
    player: u128,
    knot: i32,
    at: [f64; 3],
    entities: &[Entity],
) -> usize {
    let mine = held_by(ctx, Holder::Player(player));
    let fits = mine.iter().all(|id| {
        entities
            .iter()
            .find(|e| e.id == *id)
            .is_none_or(|e| dist(centre(e), at) <= SNAP)
    });
    if !fits {
        return 0;
    }
    for &id in &mine {
        attach(ctx, id, Holder::Knot(knot));
    }
    mine.len()
}

/// A lead used on a fence: ties what the player leads to a knot there.
pub fn tie_to_fence(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    _hand: i32,
    fence: Position,
) -> bool {
    let anchor = [fence.x, fence.y, fence.z];
    let existing = lock(&ctx.entities).iter().find_map(|e| match &e.kind {
        Kind::Hung(Hung {
            decor: Decor::LeashKnot,
            anchor: a,
            ..
        }) if *a == anchor && !e.removed => Some(e.id),
        _ => None,
    });
    let (knot, fresh) = match existing {
        Some(id) => (id, false),
        None => {
            let e = Entity {
                id: ctx.next_entity_id(),
                uuid: crate::util::random_uuid(),
                kind: Kind::Hung(Hung {
                    decor: Decor::LeashKnot,
                    facing: 0,
                    yaw: 0.0,
                    anchor,
                }),
                pos: [
                    f64::from(fence.x) + 0.5,
                    f64::from(fence.y) + 0.5,
                    f64::from(fence.z) + 0.5,
                ],
                vel: [0.0; 3],
                on_ground: false,
                age: 0,
                removed: false,
                tags: Vec::new(),
            };
            let id = e.id;
            let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
            lock(&ctx.entities).push(e);
            (id, true)
        }
    };
    let centre_of_knot = [
        f64::from(fence.x) + 0.5,
        f64::from(fence.y) + 0.5,
        f64::from(fence.z) + 0.5,
    ];
    let all = lock(&ctx.entities).clone();
    let bound = move_to_knot(ctx, actor.uuid, knot, centre_of_knot, &all);
    if bound == 0 && fresh {
        // A knot with nothing on it is not kept.
        if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == knot) {
            e.removed = true;
        }
        return false;
    }
    fx::sound(
        ctx,
        "item.lead.tied",
        fx::category::BLOCKS,
        [
            anchor[0] as f64 + 0.5,
            anchor[1] as f64 + 0.5,
            anchor[2] as f64 + 0.5,
        ],
        1.0,
        1.0,
    );
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
    true
}

/// A player uses a knot: what they lead is tied to it; with nothing on
/// their leads (and not sneaking) they take what hangs on it. A knot left
/// with nothing goes. Shears cut all its leads. `entities` are the others
/// (the knot is borrowed apart).
pub fn untie_knot(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    knot: &mut Entity,
    entities: &[Entity],
    fresh: &mut Vec<Entity>,
) {
    let (id, at) = (knot.id, knot.pos);
    let shears = held::held(ctx, actor.uuid, 0).filter(|h| h.name == "minecraft:shears");
    if let Some(shears) = shears {
        knot_gone(ctx, id, (actor.game_mode != 1).then_some(at), fresh);
        fx::sound(ctx, "item.shears.snip", fx::category::PLAYERS, at, 1.0, 1.0);
        if actor.game_mode != 1 {
            held::wear(ctx, actor.uuid, shears.slot, 1);
        }
    } else if !held_by(ctx, Holder::Player(actor.uuid)).is_empty() {
        if move_to_knot(ctx, actor.uuid, id, at, entities) > 0 {
            fx::sound(ctx, "item.lead.tied", fx::category::BLOCKS, at, 1.0, 1.0);
        }
    } else if !actor.sneaking {
        for tied in held_by(ctx, Holder::Knot(id)) {
            attach(ctx, tied, Holder::Player(actor.uuid));
        }
    }
    if held_by(ctx, Holder::Knot(id)).is_empty() {
        knot.removed = true;
        fx::sound(ctx, "item.lead.untied", fx::category::BLOCKS, at, 1.0, 1.0);
    }
    outcome.events.push(Event::Swing {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        times: 1,
    });
}

/// Where `holder` holds its leads, for mobs to walk to: a player's feet,
/// or the holding entity.
fn holder_pos(ctx: &ServerContext, entities: &[Entity], holder: Holder) -> Option<[f64; 3]> {
    match holder {
        Holder::Player(u) => lock(&ctx.players)
            .get(&u)
            .filter(|p| p.dimension == ctx.world.dimension)
            .and_then(|p| p.pose)
            .map(|p| [p.x, p.y, p.z]),
        Holder::Knot(k) => entities
            .iter()
            .find(|e| e.id == k && !e.removed)
            .map(|e| e.pos),
    }
}

/// Tells each mob on a lead where its holder is (`None` for the others).
/// Mobs follow and are pulled by their own goals; [`tick`] pulls the rest.
pub fn tell_mobs(ctx: &ServerContext, entities: &mut [Entity]) {
    let links: Vec<(i32, Holder)> = lock(&ctx.leashes)
        .links
        .iter()
        .map(|(&a, &b)| (a, b))
        .collect();
    let at: Vec<(i32, Option<[f64; 3]>)> = links
        .iter()
        .map(|&(id, h)| (id, holder_pos(ctx, entities, h)))
        .collect();
    for e in entities.iter_mut() {
        if let Kind::Mob(m) = &mut e.kind {
            m.leash = at.iter().find(|(id, _)| *id == e.id).and_then(|(_, p)| *p);
        }
    }
}

/// Unties `entity` and drops the lead at `at` (not when a creative player
/// does it); whether it was on one.
pub fn unleash(ctx: &ServerContext, entity: i32, at: [f64; 3], drop: bool) -> bool {
    if !detach(ctx, entity) {
        return false;
    }
    if drop {
        drop_lead(ctx, Some([at[0], at[1] + 0.5, at[2]]));
    }
    true
}

/// Once a tick: leads beyond [`FREE`] pull, leads beyond [`SNAP`] snap
/// (and the lead drops).
pub fn tick(ctx: &ServerContext) {
    let links: Vec<(i32, Holder)> = lock(&ctx.leashes)
        .links
        .iter()
        .map(|(&a, &b)| (a, b))
        .collect();
    if links.is_empty() {
        return;
    }
    let players: Vec<i32> = lock(&ctx.players).values().map(|p| p.entity_id).collect();
    let mut snapped = Vec::new();
    let mut gone = Vec::new();
    let mut departed = Vec::new();
    {
        let mut entities = lock(&ctx.entities);
        for (id, holder) in links {
            let Some(i) = entities.iter().position(|e| e.id == id && !e.removed) else {
                // The leashed one is gone (or is a player).
                if !players.contains(&id) {
                    gone.push(id);
                }
                continue;
            };
            let from = match holder {
                Holder::Player(u) => player_centre(ctx, u),
                Holder::Knot(k) => entities
                    .iter()
                    .find(|e| e.id == k && !e.removed)
                    .map(centre),
            };
            // A trader's llamas go when the trader does.
            if from.is_none()
                && matches!(holder, Holder::Knot(_))
                && matches!(&entities[i].kind, Kind::Mob(m) if m.species == mc_entity::Species::TraderLlama)
            {
                departed.push(id);
                continue;
            }
            let me = centre(&entities[i]);
            let Some(from) = from.filter(|f| dist(*f, me) <= SNAP) else {
                snapped.push((id, entities[i].pos));
                continue;
            };
            // Mobs pull themselves towards [`tell_mobs`]'s holder; they only
            // turn to face it here.
            if matches!(entities[i].kind, Kind::Mob(_)) {
                if dist(from, me) > FREE {
                    face(&mut entities[i], me, from);
                }
            } else {
                pull(&mut entities[i], me, from);
            }
        }
    }
    let mut leashes = lock(&ctx.leashes);
    for id in &gone {
        leashes.links.remove(id);
    }
    for (id, _) in &snapped {
        leashes.links.remove(id);
    }
    drop(leashes);
    for id in departed {
        detach(ctx, id);
        if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == id) {
            e.removed = true;
        }
        let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
    }
    for (id, at) in snapped {
        fx::sound(ctx, "item.lead.break", fx::category::NEUTRAL, at, 1.0, 1.0);
        fx::emit(
            ctx,
            fx::Fx::Link(SetEntityLink {
                entity_id: id,
                holder: 0,
            }),
        );
        drop_lead(ctx, Some(at));
    }
}

/// The lead's pull on an entity whose centre is at `me`, held at `from`:
/// past [`FREE`] it may no longer move away, is drawn back the harder the
/// further it is, and turns to face the holder.
fn pull(e: &mut Entity, me: [f64; 3], from: [f64; 3]) {
    let d = [from[0] - me[0], from[1] - me[1], from[2] - me[2]];
    let len = dist(from, me);
    if len <= FREE || len < 1e-6 {
        return;
    }
    let n = d.map(|c| c / len);
    let towards: f64 = (0..3).map(|k| e.vel[k] * n[k]).sum();
    let stretch = ((len - FREE) * 0.1).min(0.4);
    // Moving away is undone; then the lead draws it in.
    for (v, n) in e.vel.iter_mut().zip(n) {
        *v += n * (stretch - towards.min(0.0));
    }
    face(e, me, from);
}

/// Turns an entity whose centre is at `me` towards `from`.
fn face(e: &mut Entity, me: [f64; 3], from: [f64; 3]) {
    let yaw = (-(from[0] - me[0]).atan2(from[2] - me[2])).to_degrees() as f32;
    match &mut e.kind {
        Kind::Mob(m) => {
            m.yaw = yaw;
            m.head_yaw = yaw;
            m.dirty = true;
        }
        Kind::Vehicle(v) => v.yaw = yaw,
        _ => {}
    }
}

/// A dispenser's shears facing `cell`: every lead of the entities there is
/// snipped. Whether there was one.
pub fn snip_at(ctx: &ServerContext, cell: Position) -> bool {
    let (lo, hi) = (
        [cell.x, cell.y, cell.z].map(f64::from),
        [cell.x + 1, cell.y + 1, cell.z + 1].map(f64::from),
    );
    let links = lock(&ctx.leashes).links.clone();
    let there: Vec<(i32, [f64; 3])> = lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter(|e| {
            let b = e.aabb();
            (0..3).all(|k| b.max[k] > lo[k] && b.min[k] < hi[k])
        })
        .map(|e| (e.id, e.pos))
        .collect();
    let mut cut = false;
    for (id, at) in there {
        let mut leads: Vec<i32> = links
            .iter()
            .filter(|(_, h)| **h == Holder::Knot(id))
            .map(|(&e, _)| e)
            .collect();
        if links.contains_key(&id) {
            leads.push(id);
        }
        for l in leads {
            detach(ctx, l);
            drop_lead(ctx, Some(at));
            cut = true;
        }
        if cut {
            fx::sound(ctx, "item.shears.snip", fx::category::BLOCKS, at, 1.0, 1.0);
        }
    }
    cut
}

/// A lead item lying at `at`, to be added.
fn lead_at(ctx: &ServerContext, at: [f64; 3]) -> Option<Entity> {
    super::dropped(ctx, at, "minecraft:lead", 1, 0)
}

fn drop_lead(ctx: &ServerContext, at: Option<[f64; 3]>) {
    if let Some(at) = at
        && let Some(e) = super::dropped(ctx, at, "minecraft:lead", 1, 0)
    {
        super::spawn_all(ctx, vec![e]);
    }
}
