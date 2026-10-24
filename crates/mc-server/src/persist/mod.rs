//! Entities kept with their chunks (minecraft.wiki, "Entity format" and
//! "Entity chunk format"): every chunk's entities go to `entities/r.x.z.mca`
//! when the world is saved and when the chunk unloads (they leave the world
//! then), and come back when the chunk loads again, in every dimension.
//!
//! An entity is kept with the chunk its root vehicle stands in; whoever rides
//! it is nested under `Passengers`. Players are never kept here, and neither
//! are fishing bobbers (vanilla does not save them). Keys and entities that
//! nothing here understands are written back as they were read, so a vanilla
//! world loses nothing by being opened.

mod kinds;

use std::collections::{HashMap, HashSet};

use mc_entity::extra::{Decor, Hung};
use mc_entity::{Entity, Kind};
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::SetPassengers;

use crate::context::{Event, ServerContext};
use crate::decor::leash::Holder;
use crate::fx;
use crate::tick::lock;

pub use kinds::{read, write};

/// What of the stored entities is kept as it was read.
#[derive(Debug, Default)]
pub struct Kept {
    /// Keys of loaded entities that are not understood, by uuid.
    extra: HashMap<u128, Compound>,
    /// Entities of types that are not understood, by chunk.
    raw: HashMap<(i32, i32), Vec<Tag>>,
}

/// Who holds an entity's lead, as stored (`leash`).
#[derive(Clone, Debug, PartialEq)]
pub enum Leash {
    /// An entity (a player): its uuid.
    Entity(u128),
    /// The knot on the fence at this block.
    Knot([i32; 3]),
}

/// One entity read back, with what lives beside it rather than in it.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub entity: Entity,
    pub leash: Option<Leash>,
    /// Container contents (`Items`, a loot table).
    pub contents: Option<Compound>,
    /// Health of a villager or wandering trader.
    pub merchant_health: Option<f32>,
    /// A hopper minecart's `TransferCooldown`.
    pub cooldown: Option<i32>,
    /// Keys nothing here understands.
    pub extra: Compound,
}

pub(crate) fn uuid_tag(u: u128) -> Tag {
    Tag::IntArray((0..4).map(|i| (u >> (96 - 32 * i)) as u32 as i32).collect())
}

pub(crate) fn uuid_of(tag: Option<&Tag>) -> Option<u128> {
    match tag? {
        Tag::IntArray(v) if v.len() == 4 => Some(
            v.iter()
                .fold(0u128, |u, &part| (u << 32) | u128::from(part as u32)),
        ),
        Tag::String(s) => crate::util::parse_uuid(s),
        _ => None,
    }
}

/// A block state as NBT keeps it: `Name` and `Properties`.
pub(crate) fn state_tag(ctx: &ServerContext, state: u32) -> Option<Compound> {
    let key = ctx.data.blocks.describe(state)?;
    let (name, props) = match key.split_once('[') {
        Some((n, rest)) => (n, rest.strip_suffix(']').unwrap_or(rest)),
        None => (key, ""),
    };
    let mut c = Compound::new();
    c.insert("Name", Tag::String(name.to_owned()));
    let mut p = Compound::new();
    for (k, v) in props.split(',').filter_map(|kv| kv.split_once('=')) {
        p.insert(k, Tag::String(v.to_owned()));
    }
    if !p.is_empty() {
        c.insert("Properties", Tag::Compound(p));
    }
    Some(c)
}

/// The state a `Name`/`Properties` compound names.
pub(crate) fn state_from(ctx: &ServerContext, c: &Compound) -> Option<u32> {
    let Some(Tag::String(name)) = c.get("Name") else {
        return None;
    };
    let props: Vec<(String, String)> = match c.get("Properties") {
        Some(Tag::Compound(p)) => p
            .iter()
            .filter_map(|(k, v)| match v {
                Tag::String(v) => Some((k.to_string(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let refs: Vec<(&str, &str)> = props
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    ctx.data
        .blocks
        .state_id(&mc_command::args::full_id(name), &refs)
}

/// Who rides the entity (entity ids, players among them).
pub(crate) fn riders(e: &Entity) -> &[i32] {
    match &e.kind {
        Kind::Mob(m) => &m.riders,
        Kind::Vehicle(v) => &v.riders,
        _ => &[],
    }
}

fn riders_mut(e: &mut Entity) -> Option<&mut Vec<i32>> {
    match &mut e.kind {
        Kind::Mob(m) => Some(&mut m.riders),
        Kind::Vehicle(v) => Some(&mut v.riders),
        _ => None,
    }
}

/// The lead of `e` as it is stored.
fn leash_of(ctx: &ServerContext, e: &Entity, all: &[Entity]) -> Option<Leash> {
    let holder = *lock(&ctx.leashes).links.get(&e.id)?;
    match holder {
        Holder::Player(u) => Some(Leash::Entity(u)),
        Holder::Knot(k) => all.iter().find(|o| o.id == k).map(|o| match &o.kind {
            Kind::Hung(Hung {
                decor: Decor::LeashKnot,
                anchor,
                ..
            }) => Leash::Knot(*anchor),
            // Another entity holds it.
            _ => Leash::Entity(o.uuid),
        }),
    }
}

/// The chunk an entity is kept with.
fn chunk_of(e: &Entity) -> (i32, i32) {
    let (x, _, z) = e.block_pos();
    (x >> 4, z >> 4)
}

/// Ids of the entities riding others (they are kept inside their vehicle).
fn passengers(entities: &[Entity]) -> HashSet<i32> {
    entities
        .iter()
        .filter(|e| !e.removed)
        .flat_map(|e| riders(e).iter().copied())
        .collect()
}

/// The entities of `entities` by chunk, as NBT, with the ones that were
/// not understood.
fn by_chunk(
    ctx: &ServerContext,
    entities: &[Entity],
    within: Option<&[(i32, i32)]>,
) -> HashMap<(i32, i32), Vec<Tag>> {
    let mut out: HashMap<(i32, i32), Vec<Tag>> = HashMap::new();
    let riding = passengers(entities);
    for e in entities
        .iter()
        .filter(|e| !e.removed && !riding.contains(&e.id))
    {
        let key = chunk_of(e);
        if within.is_some_and(|w| !w.contains(&key)) {
            continue;
        }
        if let Some(c) = write(ctx, e, entities) {
            out.entry(key).or_default().push(Tag::Compound(c));
        }
    }
    let kept = lock(&ctx.entity_kept);
    for (key, raw) in &kept.raw {
        if within.is_none_or(|w| w.contains(key)) {
            out.entry(*key).or_default().extend(raw.iter().cloned());
        }
    }
    out
}

/// Brings in the entities stored with chunks that were read since the last
/// call. One whose uuid is already in the world is left out, as vanilla
/// does.
pub fn load_stored(ctx: &ServerContext) {
    let tags = ctx.world.take_loaded_entities();
    if tags.is_empty() {
        return;
    }
    let mut fresh: Vec<Loaded> = Vec::new();
    for tag in tags {
        let Tag::Compound(c) = &tag else { continue };
        let loaded = read(ctx, c);
        if loaded.is_empty() {
            if let Some(key) = raw_chunk(c) {
                lock(&ctx.entity_kept)
                    .raw
                    .entry(key)
                    .or_default()
                    .push(tag.clone());
            }
            continue;
        }
        fresh.extend(loaded);
    }
    spawn_loaded(ctx, fresh);
}

/// Puts entities read back into the world, with what lives beside them.
fn spawn_loaded(ctx: &ServerContext, mut fresh: Vec<Loaded>) {
    let mut known: HashSet<u128> = lock(&ctx.entities).iter().map(|e| e.uuid).collect();
    // Knots by the fence they are on; a second knot on a fence is left out.
    let mut knots: HashMap<[i32; 3], i32> = lock(&ctx.entities)
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            Kind::Hung(Hung {
                decor: Decor::LeashKnot,
                anchor,
                ..
            }) => Some((*anchor, e.id)),
            _ => None,
        })
        .collect();
    // A rider left out takes nothing with it; its vehicle forgets it.
    let mut dropped_ids: HashSet<i32> = HashSet::new();
    fresh.retain(|l| {
        let knot = match &l.entity.kind {
            Kind::Hung(Hung {
                decor: Decor::LeashKnot,
                anchor,
                ..
            }) => Some(*anchor),
            _ => None,
        };
        let keep = known.insert(l.entity.uuid)
            && knot.is_none_or(|a| knots.insert(a, l.entity.id).is_none());
        if !keep {
            dropped_ids.insert(l.entity.id);
        }
        keep
    });
    let mut leashes = Vec::new();
    for mut l in fresh {
        if let Some(r) = riders_mut(&mut l.entity) {
            r.retain(|id| !dropped_ids.contains(id));
        }
        let id = l.entity.id;
        if let Some(contents) = l.contents {
            lock(&ctx.entity_items).insert(id, contents);
        }
        if let Some(h) = l.merchant_health {
            ctx.mobs.set_villager_health(id, Some(h));
        }
        if let Some(c) = l.cooldown.filter(|c| *c > 0) {
            lock(&ctx.cart_rests).insert(id, c);
        }
        if !l.extra.is_empty() {
            lock(&ctx.entity_kept).extra.insert(l.entity.uuid, l.extra);
        }
        if let Some(leash) = l.leash {
            leashes.push((id, leash));
        }
        let ridden = (!riders(&l.entity).is_empty()).then(|| riders(&l.entity).to_vec());
        crate::tick::spawn_entity(ctx, l.entity);
        if let Some(passengers) = ridden {
            fx::emit(
                ctx,
                fx::Fx::Passengers(SetPassengers {
                    vehicle: id,
                    passengers,
                }),
            );
        }
    }
    for (id, leash) in leashes {
        let holder = match leash {
            Leash::Entity(u) => match lock(&ctx.entities).iter().find(|e| e.uuid == u) {
                Some(holder) => Holder::Knot(holder.id),
                None => Holder::Player(u),
            },
            Leash::Knot(at) => match knots.get(&at) {
                Some(&k) => Holder::Knot(k),
                None => Holder::Knot(new_knot(ctx, at, &mut knots)),
            },
        };
        crate::decor::leash::attach(ctx, id, holder);
    }
}

/// A knot on the fence at `at` for a lead whose knot was not stored.
fn new_knot(ctx: &ServerContext, at: [i32; 3], knots: &mut HashMap<[i32; 3], i32>) -> i32 {
    let e = Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Hung(Hung {
            decor: Decor::LeashKnot,
            facing: 0,
            yaw: 0.0,
            anchor: at,
        }),
        pos: at.map(|v| f64::from(v) + 0.5),
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    let id = e.id;
    knots.insert(at, id);
    crate::tick::spawn_entity(ctx, e);
    id
}

/// The chunk an entity that is not understood is kept with (its `Pos`).
fn raw_chunk(c: &Compound) -> Option<(i32, i32)> {
    let Some(Tag::List(pos)) = c.get("Pos") else {
        return None;
    };
    let at = |i: usize| match pos.get(i) {
        Some(Tag::Double(v)) => Some(v.floor() as i32 >> 4),
        _ => None,
    };
    Some((at(0)?, at(2)?))
}

/// Writes the entities of every loaded chunk (world folders only).
pub fn save(ctx: &ServerContext) {
    load_stored(ctx);
    let entities = lock(&ctx.entities);
    let alive: HashSet<u128> = entities.iter().map(|e| e.uuid).collect();
    lock(&ctx.entity_kept)
        .extra
        .retain(|u, _| alive.contains(u));
    let stored = by_chunk(ctx, &entities, None);
    drop(entities);
    ctx.world.save_entities(stored, None);
}

/// Chunks were unloaded: their entities are written and leave the world,
/// passengers with their vehicles.
pub fn unloaded(ctx: &ServerContext, chunks: &[(i32, i32)]) {
    if !ctx.world.is_saved() {
        return;
    }
    load_stored(ctx);
    let mut entities = lock(&ctx.entities);
    // Entities that wandered out of the loaded chunks go too, to the file of
    // the chunk they are in.
    let mut chunks = chunks.to_vec();
    for e in entities.iter() {
        let key = chunk_of(e);
        if !chunks.contains(&key) && !ctx.world.chunk_loaded(key.0, key.1) {
            chunks.push(key);
        }
    }
    if chunks.is_empty() {
        return;
    }
    let chunks = &chunks[..];
    let stored = by_chunk(ctx, &entities, Some(chunks));
    ctx.world.save_entities(stored, Some(chunks));
    let riding = passengers(&entities);
    let mut leaving: Vec<i32> = entities
        .iter()
        .filter(|e| !riding.contains(&e.id) && chunks.contains(&chunk_of(e)))
        .map(|e| e.id)
        .collect();
    let mut i = 0;
    while i < leaving.len() {
        let id = leaving[i];
        if let Some(e) = entities.iter().find(|e| e.id == id) {
            let more: Vec<i32> = riders(e)
                .iter()
                .copied()
                .filter(|r| !leaving.contains(r) && entities.iter().any(|o| o.id == *r))
                .collect();
            leaving.extend(more);
        }
        i += 1;
    }
    lock(&ctx.entity_kept)
        .raw
        .retain(|key, _| !chunks.contains(key));
    take_out(ctx, &mut entities, leaving);
}

/// Takes the entities `leaving` out of the world (they are kept elsewhere
/// now), with what lives beside them.
fn take_out(ctx: &ServerContext, entities: &mut Vec<Entity>, leaving: Vec<i32>) {
    let gone: HashSet<i32> = leaving.iter().copied().collect();
    let mut uuids = Vec::new();
    entities.retain(|e| {
        let leaves = gone.contains(&e.id);
        if leaves {
            uuids.push(e.uuid);
        }
        !leaves
    });
    {
        let mut kept = lock(&ctx.entity_kept);
        for u in &uuids {
            kept.extra.remove(u);
        }
    }
    lock(&ctx.entity_items).retain(|id, _| !gone.contains(id));
    lock(&ctx.cart_rests).retain(|id, _| !gone.contains(id));
    lock(&ctx.leashes).links.retain(|id, _| !gone.contains(id));
    for id in &leaving {
        ctx.mobs.set_villager_health(*id, None);
    }
    if !leaving.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(leaving));
    }
}

/// The vehicle `player` (an entity id) rides and those above it: the
/// direct one and the root one.
fn vehicles_of(all: &[Entity], player: i32) -> Option<(&Entity, &Entity)> {
    let direct = all
        .iter()
        .find(|e| !e.removed && riders(e).contains(&player))?;
    let mut root = direct;
    while let Some(up) = all
        .iter()
        .find(|e| !e.removed && riders(e).contains(&root.id))
    {
        root = up;
    }
    Some((direct, root))
}

/// What a player rides, as their file keeps it (`RootVehicle`: the uuid of
/// the vehicle they sit on, and the bottom one with all its riders).
pub fn root_vehicle(ctx: &ServerContext, player: i32) -> Option<Compound> {
    let all = lock(&ctx.entities).clone();
    let (direct, root) = vehicles_of(&all, player)?;
    let mut c = Compound::new();
    c.insert("Attach", uuid_tag(direct.uuid));
    c.insert("Entity", Tag::Compound(write(ctx, root, &all)?));
    Some(c)
}

/// A player left: what they rode leaves with them, unless another player
/// rides it too.
pub fn left_with(ctx: &ServerContext, player: i32) {
    let mut entities = lock(&ctx.entities);
    let Some((_, root)) = vehicles_of(&entities, player) else {
        return;
    };
    let mut tree = vec![root.id];
    let mut players = 0;
    let mut i = 0;
    while i < tree.len() {
        if let Some(e) = entities.iter().find(|e| e.id == tree[i]) {
            for &r in riders(e) {
                if entities.iter().any(|o| o.id == r) {
                    tree.push(r);
                } else {
                    players += 1;
                }
            }
        }
        i += 1;
    }
    if players == 1 {
        take_out(ctx, &mut entities, tree);
    }
}

/// A player who rode something when they left gets back on: it comes back
/// with them unless it is in the world already.
pub fn ride_again(ctx: &ServerContext, player: i32, saved: &Compound) {
    let Some(attach) = uuid_of(saved.get("Attach")) else {
        return;
    };
    let present = lock(&ctx.entities).iter().any(|e| e.uuid == attach);
    if !present && let Some(Tag::Compound(c)) = saved.get("Entity") {
        spawn_loaded(ctx, read(ctx, c));
    }
    let mut entities = lock(&ctx.entities);
    let Some(v) = entities.iter_mut().find(|e| e.uuid == attach && !e.removed) else {
        return;
    };
    let id = v.id;
    let Some(r) = riders_mut(v) else {
        return;
    };
    if !r.contains(&player) {
        r.push(player);
    }
    let passengers = r.clone();
    drop(entities);
    fx::emit(
        ctx,
        fx::Fx::Passengers(SetPassengers {
            vehicle: id,
            passengers,
        }),
    );
}

/// Keys of an entity that were read but not understood.
pub(crate) fn extra_of(ctx: &ServerContext, uuid: u128) -> Option<Compound> {
    lock(&ctx.entity_kept).extra.get(&uuid).cloned()
}

#[cfg(test)]
mod tests;
