//! Leads. A lead ties an entity to a holder: a player, or a knot on a
//! fence. The links live here; entities with minds of their own (mobs) ask
//! for them with [`attach`] and follow the holder themselves. Leashed
//! entities that have no mind are pulled along, and the lead snaps when they
//! get more than 10 blocks from the holder (minecraft.wiki, "Lead").

use std::collections::HashMap;

use mc_entity::extra::{Decor, Hung};
use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::SetEntityLink;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held;
use crate::hits;
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Who holds a lead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    Player(u128),
    /// The entity id of a knot (or of another entity holding it).
    Knot(i32),
}

/// Whether a lead goes on this mob: anything but monsters (minecraft.wiki,
/// "Lead").
pub fn leashable(m: &mc_entity::Mob) -> bool {
    m.species.category() != mc_entity::mob::Category::Monster && m.dying.is_none()
}

/// A player uses `e` (a mob): one leading it lets go (the lead drops but in
/// creative), one with a lead in hand ties it if it is free. Whether the use
/// was the lead's.
pub fn use_on_mob(
    ctx: &ServerContext,
    e: &Entity,
    (player, game_mode): (u128, i32),
    holding_lead: Option<usize>,
) -> bool {
    let Kind::Mob(m) = &e.kind else {
        return false;
    };
    let holder = lock(&ctx.leashes).links.get(&e.id).copied();
    if holder == Some(Holder::Player(player)) {
        detach(ctx, e.id);
        fx::sound(ctx, "item.lead.untied", fx::category::NEUTRAL, e.pos, 1.0, 1.0);
        if game_mode != 1 {
            drop_lead(ctx, Some(e.pos));
        }
        return true;
    }
    let Some(slot) = holding_lead else {
        return false;
    };
    if holder.is_some() || !leashable(m) {
        return false;
    }
    attach(ctx, e.id, Holder::Player(player));
    fx::sound(ctx, "item.lead.tied", fx::category::NEUTRAL, e.pos, 1.0, 1.0);
    if game_mode != 1 {
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

/// A knot goes: what hung on it drops its leads.
pub fn knot_gone(ctx: &ServerContext, knot: i32) {
    for id in held_by(ctx, Holder::Knot(knot)) {
        detach(ctx, id);
    }
}

/// Moves everything tied to a player to a knot.
fn move_to_knot(ctx: &ServerContext, player: u128, knot: i32) -> usize {
    let near = lock(&ctx.players).get(&player).and_then(|p| p.pose);
    let Some(pose) = near else {
        return 0;
    };
    let mut n = 0;
    for id in held_by(ctx, Holder::Player(player)) {
        // Within 7 blocks of the player.
        let close = lock(&ctx.entities)
            .iter()
            .find(|e| e.id == id)
            .is_none_or(|e| {
                (e.pos[0] - pose.x).powi(2)
                    + (e.pos[1] - pose.y).powi(2)
                    + (e.pos[2] - pose.z).powi(2)
                    <= 49.0
            });
        if close {
            attach(ctx, id, Holder::Knot(knot));
            n += 1;
        }
    }
    n
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
    let bound = move_to_knot(ctx, actor.uuid, knot);
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

/// A player uses a knot: whatever hangs on it is theirs to lead.
pub fn untie_knot(ctx: &ServerContext, outcome: &mut Outcome, actor: Actor, knot: i32) {
    let tied = held_by(ctx, Holder::Knot(knot));
    for id in &tied {
        attach(ctx, *id, Holder::Player(actor.uuid));
    }
    if let Some(e) = lock(&ctx.entities).iter_mut().find(|e| e.id == knot) {
        e.removed = true;
    }
    let _ = outcome;
}

/// Once a tick: leads that stretch too far snap (and the lead item drops).
pub fn tick(ctx: &ServerContext) {
    let links: Vec<(i32, Holder)> = lock(&ctx.leashes)
        .links
        .iter()
        .map(|(&a, &b)| (a, b))
        .collect();
    if links.is_empty() {
        return;
    }
    let mut entities = lock(&ctx.entities);
    for (id, holder) in links {
        let Some(me) = entities.iter().find(|e| e.id == id && !e.removed) else {
            // The leashed one is gone (or is a player): the lead falls.
            if !lock(&ctx.players).values().any(|p| p.entity_id == id) {
                drop_lead(ctx, None);
                lock(&ctx.leashes).links.remove(&id);
            }
            continue;
        };
        let from = match holder {
            Holder::Player(u) => lock(&ctx.players)
                .get(&u)
                .and_then(|p| p.pose)
                .map(|p| [p.x, p.y + 1.0, p.z]),
            Holder::Knot(k) => entities
                .iter()
                .find(|e| e.id == k && !e.removed)
                .map(|e| e.pos),
        };
        let snap = match from {
            None => true,
            Some(f) => {
                (me.pos[0] - f[0]).powi(2) + (me.pos[1] - f[1]).powi(2) + (me.pos[2] - f[2]).powi(2)
                    > 100.0
            }
        };
        // Beyond six blocks the lead pulls the mob towards its holder.
        if !snap
            && let Some(f) = from
            && let Some(me) = entities.iter_mut().find(|e| e.id == id)
            && matches!(me.kind, Kind::Mob(_))
        {
            let d = [f[0] - me.pos[0], f[1] - me.pos[1], f[2] - me.pos[2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if len > 6.0 {
                for (v, d) in me.vel.iter_mut().zip(d) {
                    let n = d / len;
                    *v += (n * n * 0.4).copysign(n);
                }
            }
            continue;
        }
        if snap {
            let at = entities.iter().find(|e| e.id == id).map_or([0.0; 3], |e| e.pos);
            fx::sound(ctx, "item.lead.break", fx::category::NEUTRAL, at, 1.0, 1.0);
            lock(&ctx.leashes).links.remove(&id);
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
    let _ = held::enchant;
    let _ = hits::rnd;
}

fn drop_lead(ctx: &ServerContext, at: Option<[f64; 3]>) {
    if let Some(at) = at
        && let Some(e) = super::dropped(ctx, at, "minecraft:lead", 1, 0)
    {
        super::spawn_all(ctx, vec![e]);
    }
}
