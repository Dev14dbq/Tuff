//! A village's life (minecraft.wiki, "Village mechanics", "Villager",
//! "Bell", "Iron Golem"): the tickets villagers hold at the points of
//! interest around them, and what they do together.
//!
//! - a villager looks for a bed (a "home") and a bell (the "meeting point")
//!   near it and takes a ticket at each; a bed has one ticket, so one
//!   villager, and a bell 32;
//! - a ticket goes back when the villager dies or is removed, and the claim
//!   is dropped when the block is broken;
//! - villagers read from a file check their claims against the points
//!   (the points are rebuilt from the chunk's blocks, so a ticket the file
//!   lost is taken again);
//! - gossip: what a villager heard about a player (see
//!   [`mc_entity::gossip`]) is passed on when two villagers meet, fades
//!   every 20 minutes, sets the prices, and makes the golems the village
//!   did not build turn on a player whose reputation with a villager near
//!   them is -100 or less;
//! - breeding: two villagers with food for 12 nutrition points (bread 4,
//!   carrots, potatoes and beetroots 1) and a free bed within 48 blocks
//!   have a baby, which grows up in 20 minutes and takes the bed;
//! - a rung bell sends the villagers within 32 blocks to their beds, and
//!   makes the raiders within 48 blocks glow when one is within 32.

use mc_entity::gossip::{self, Kind as Gossip};
use mc_entity::trade::{Cost, Merchant};
use mc_entity::{Collide as _, Entity, Kind, Species};
use mc_protocol::Position;

use super::Ground;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::poi::{self, Poi};
use crate::tick::lock;

type Cell = (i32, i32, i32);

/// Blocks (sideways, up and down) within which a villager looks for a bed.
const HOME_RADIUS: i32 = 32;
const HOME_HEIGHT: i32 = 8;
/// And for a bell.
const MEETING_RADIUS: i32 = 48;
const MEETING_HEIGHT: i32 = 16;
/// A villager looks again for what it lacks once in this many ticks.
const LOOK_EVERY: i64 = 40;

fn arr(c: Cell) -> [i32; 3] {
    [c.0, c.1, c.2]
}

fn cell(p: [i32; 3]) -> Cell {
    (p[0], p[1], p[2])
}

/// Gives every ticket the villager holds back.
pub fn let_go(ctx: &ServerContext, m: &mut Merchant) {
    for c in [m.home.take(), m.job_site.take(), m.meeting.take()]
        .into_iter()
        .flatten()
    {
        poi::release(ctx, arr(c));
    }
}

/// Villagers that were removed (killed, `/kill`ed) let go of their tickets.
pub fn release_removed(ctx: &ServerContext, entities: &mut [Entity]) {
    for e in entities.iter_mut().filter(|e| e.removed) {
        if let Kind::Merchant(m) = &mut e.kind
            && !m.wandering
        {
            let_go(ctx, m);
        }
    }
}

/// Checks the claims of a villager that has just come into the world.
/// Returns false while a claim's chunk has not been read.
fn verify(poi: &mut Poi, m: &mut Merchant) -> bool {
    let mut done = true;
    for slot in [&mut m.home, &mut m.job_site, &mut m.meeting] {
        let Some(c) = *slot else { continue };
        match poi.get(arr(c)) {
            Some(r) if r.free > 0 => {
                poi.claim(arr(c));
            }
            Some(_) => {}
            None if poi.knows(arr(c)) => *slot = None,
            None => done = false,
        }
    }
    done
}

/// A held claim whose point is gone (the block was broken) is dropped.
fn drop_broken(poi: &Poi, slot: &mut Option<Cell>, kind: &str) {
    if let Some(c) = *slot
        && poi.knows(arr(c))
        && poi.get(arr(c)).is_none_or(|r| r.kind != kind)
    {
        *slot = None;
    }
}

/// Villagers find homes and meeting points.
pub fn claims(ctx: &ServerContext, entities: &mut [Entity], now: i64) {
    poi::sync(ctx);
    let mut poi = lock(&ctx.poi);
    for e in entities.iter_mut().filter(|e| !e.removed) {
        let at = arr(e.block_pos());
        let id = i64::from(e.id);
        let Kind::Merchant(m) = &mut e.kind else {
            continue;
        };
        if m.wandering {
            continue;
        }
        if !m.verified {
            m.verified = verify(&mut poi, m);
        }
        if (now + id) % LOOK_EVERY != 0 {
            continue;
        }
        drop_broken(&poi, &mut m.home, poi::HOME);
        drop_broken(&poi, &mut m.meeting, poi::MEETING);
        if m.home.is_none() {
            m.home = poi
                .take_nearest(at, HOME_RADIUS, HOME_HEIGHT, |k| k == poi::HOME)
                .map(|(p, _)| cell(p));
        }
        if m.meeting.is_none() {
            m.meeting = poi
                .take_nearest(at, MEETING_RADIUS, MEETING_HEIGHT, |k| k == poi::MEETING)
                .map(|(p, _)| cell(p));
        }
    }
}

// ---- gossip ----

/// Blocks (each way) from a killed villager within which the villagers
/// that see it remember who did it.
const WITNESS: f64 = 16.0;
/// Where a villager's eyes are above its feet.
const EYE: f64 = 1.62;
/// Around an iron golem (blocks sideways, up and down) the villagers whose
/// opinion of a player counts.
const OPINION_SIDE: f64 = 10.0;
const OPINION_HEIGHT: f64 = 8.0;
/// Villagers talk when they are this close (blocks), each at most once in
/// this many ticks.
const TALK_RANGE: f64 = 4.0;
const CHAT_WAIT: i64 = 600;
/// Gossip fades once in 20 minutes.
const FADE_EVERY: i64 = 24_000;

fn within(a: [f64; 3], b: [f64; 3], side: f64, height: f64) -> bool {
    (a[0] - b[0]).abs() <= side && (a[2] - b[2]).abs() <= side && (a[1] - b[1]).abs() <= height
}

/// Whether nothing solid is in the way between two points.
fn sees(ground: &Ground, from: [f64; 3], to: [f64; 3]) -> bool {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let steps = ((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() / 0.25).ceil() as i32;
    (1..steps).all(|i| {
        let t = f64::from(i) / f64::from(steps);
        let at = (0..3).map(|k| (from[k] + d[k] * t).floor() as i32);
        let at: Vec<i32> = at.collect();
        !ground.solid(at[0], at[1], at[2])
    })
}

/// A player killed a villager (see `MobState::offend`): the villagers
/// within 16 blocks with a clear view of it hold it against them for good
/// measure (major negative gossip).
pub fn witnesses(ctx: &ServerContext, entities: &mut [Entity]) {
    let murders: Vec<(u128, [f64; 3])> = {
        let mut all = lock(&ctx.mobs.murders);
        let mine = all
            .iter()
            .filter(|m| m.0 == ctx.dim)
            .map(|m| (m.1, m.2))
            .collect();
        all.retain(|m| m.0 != ctx.dim);
        mine
    };
    if murders.is_empty() {
        return;
    }
    let ground = Ground::new(ctx);
    for (player, at) in murders {
        let victim = [at[0], at[1] + EYE, at[2]];
        for e in entities.iter_mut().filter(|e| !e.removed) {
            let eyes = [e.pos[0], e.pos[1] + EYE, e.pos[2]];
            let Kind::Merchant(m) = &mut e.kind else {
                continue;
            };
            if !m.wandering && within(eyes, victim, WITNESS, WITNESS) && sees(&ground, eyes, victim)
            {
                m.gossip.happen(player, Gossip::MajorNegative);
            }
        }
    }
}

/// The players some villager near the iron golem thinks the worst of: it
/// goes for them unless a player made it.
pub fn scorn(entities: &mut [Entity], now: i64) {
    if now % 10 != 0 {
        return;
    }
    let bad: Vec<([f64; 3], Vec<u128>)> = entities
        .iter()
        .filter(|e| !e.removed)
        .filter_map(|e| match &e.kind {
            Kind::Merchant(m) if !m.wandering => {
                let who: Vec<u128> = m
                    .gossip
                    .people()
                    .into_iter()
                    .filter(|&p| m.gossip.reputation(p) <= gossip::HOSTILE)
                    .collect();
                (!who.is_empty()).then_some((e.pos, who))
            }
            _ => None,
        })
        .collect();
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if m.species != Species::IronGolem || m.quirks.player_made {
            continue;
        }
        let mut who: Vec<u128> = bad
            .iter()
            .filter(|(at, _)| within(*at, pos, OPINION_SIDE, OPINION_HEIGHT))
            .flat_map(|(_, who)| who.iter().copied())
            .collect();
        who.sort_unstable();
        who.dedup();
        if m.quirks.scorned != who {
            m.quirks.scorned = who;
        }
    }
}

/// What the villagers do among themselves: gossip fades and is passed on,
/// they pick up food, and breed.
pub fn tend(
    ctx: &ServerContext,
    ground: &Ground,
    entities: &mut [Entity],
    now: i64,
    fresh: &mut Vec<Entity>,
) {
    if now > 0 && now % FADE_EVERY == 0 {
        for e in entities.iter_mut() {
            if let Kind::Merchant(m) = &mut e.kind {
                m.gossip.fade();
            }
        }
    }
    chat(entities, now);
    feed(ctx, ground, entities, now);
    breed(ctx, ground, entities, now, fresh);
}

fn villager_awake(e: &Entity) -> Option<&Merchant> {
    match &e.kind {
        Kind::Merchant(m) if !m.wandering && !e.removed && !m.walk.asleep => Some(m),
        _ => None,
    }
}

/// Two villagers close to each other tell each other what they have heard
/// (less the cost of passing it on).
fn chat(entities: &mut [Entity], now: i64) {
    let ready = |e: &Entity| {
        villager_awake(e).is_some_and(|m| m.walk.chatted.is_none_or(|t| now - t >= CHAT_WAIT))
    };
    for i in 0..entities.len() {
        if (now + i64::from(entities[i].id)) % 20 != 0 || !ready(&entities[i]) {
            continue;
        }
        let at = entities[i].pos;
        let Some(j) = (0..entities.len()).find(|&j| {
            j != i && ready(&entities[j]) && within(entities[j].pos, at, TALK_RANGE, 2.0)
        }) else {
            continue;
        };
        let (Kind::Merchant(a), Kind::Merchant(b)) = (&entities[i].kind, &entities[j].kind) else {
            continue;
        };
        let (from_a, from_b) = (a.gossip.clone(), b.gossip.clone());
        for (k, heard) in [(i, from_b), (j, from_a)] {
            if let Kind::Merchant(m) = &mut entities[k].kind {
                heard.tell(&mut m.gossip);
                m.walk.chatted = Some(now);
            }
        }
    }
}

// ---- food and children ----

/// Nutrition points a villager has before it is willing to breed.
const NEEDED: i32 = 12;
/// How far a villager goes for food on the ground (blocks).
const FOOD_RANGE: f64 = 4.0;
/// Ticks before parents breed again, and the age a child is born with
/// (20 minutes before it is grown).
const BREED_WAIT: i32 = 6000;
const CHILD_AGE: i32 = -24_000;
/// How near the two must be, and how far they look for a bed.
const BREED_RANGE: f64 = 4.0;
const BED_RADIUS: i32 = 48;
const BED_HEIGHT: i32 = 16;

/// Nutrition points of an item (`villager_food`).
pub fn nutrition(name: &str) -> i32 {
    match name {
        "minecraft:bread" => 4,
        "minecraft:carrot" | "minecraft:potato" | "minecraft:beetroot" => 1,
        _ => 0,
    }
}

/// All the nutrition a villager has: food it carries and points left from
/// what it ate.
pub fn points(ctx: &ServerContext, m: &Merchant) -> i32 {
    m.food
        + m.pantry
            .iter()
            .map(|c| nutrition(ctx.items.name(c.item).unwrap_or("")) * c.count)
            .sum::<i32>()
}

/// Whether it would breed: grown, not waiting out the last time, and with
/// food enough.
fn willing(ctx: &ServerContext, m: &Merchant) -> bool {
    !m.wandering && m.age == 0 && points(ctx, m) >= NEEDED
}

/// It eats from its first slot on until it has the points, and spends
/// them.
fn eat(ctx: &ServerContext, m: &mut Merchant) {
    let mut i = 0;
    while m.food < NEEDED && i < m.pantry.len() {
        let n = nutrition(ctx.items.name(m.pantry[i].item).unwrap_or(""));
        if n == 0 || m.pantry[i].count <= 0 {
            i += 1;
            continue;
        }
        m.pantry[i].count -= 1;
        m.food += n;
        if m.pantry[i].count == 0 {
            m.pantry.remove(i);
        }
    }
    m.food = (m.food - NEEDED).max(0);
}

/// Villagers pick up the food lying within 4 blocks of them (a player's
/// thrown bread and carrots) and keep it for when they breed.
fn feed(ctx: &ServerContext, ground: &Ground, entities: &mut [Entity], now: i64) {
    if !ctx.game_rule_bool("minecraft:mob_griefing") {
        return;
    }
    for i in 0..entities.len() {
        if (now + i64::from(entities[i].id)) % 10 != 0
            || villager_awake(&entities[i]).is_none_or(|m| m.is_baby())
        {
            continue;
        }
        let at = entities[i].pos;
        let food = entities
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                !o.removed
                    && matches!(&o.kind, Kind::Item { item, pickup_delay: 0, .. }
                        if nutrition(ctx.items.name(*item).unwrap_or("")) > 0)
                    && within(o.pos, at, FOOD_RANGE, 2.0)
            })
            .min_by(|(_, a), (_, b)| {
                super::dist_sq(a.pos, at).total_cmp(&super::dist_sq(b.pos, at))
            })
            .map(|(j, _)| j);
        let Some(j) = food else {
            continue;
        };
        if within(entities[j].pos, at, 1.0, 1.5) {
            let Kind::Item { item, count, .. } = entities[j].kind else {
                continue;
            };
            let (collector, item_entity) = (entities[i].id, entities[j].id);
            if let Kind::Merchant(m) = &mut entities[i].kind {
                match m.pantry.iter_mut().find(|c| c.item == item) {
                    Some(c) => c.count += count,
                    None => m.pantry.push(Cost { item, count }),
                }
            }
            entities[j].removed = true;
            let _ = ctx.events.send(Event::ItemTaken {
                item_entity,
                collector,
                count,
                slots: Vec::new(),
            });
            let _ = ctx.events.send(Event::EntitiesRemoved(vec![item_entity]));
        } else {
            let p = entities[j].pos;
            let to = (
                p[0].floor() as i32,
                p[1].floor() as i32,
                p[2].floor() as i32,
            );
            mc_entity::villager::head_for(&mut entities[i], ground, [to.0, to.1, to.2]);
        }
    }
}

/// A bed whose two blocks above the pillow are free (a child needs room).
fn roomy(ground: &Ground, p: [i32; 3]) -> bool {
    !ground.solid(p[0], p[1] + 1, p[2]) && !ground.solid(p[0], p[1] + 2, p[2])
}

/// Two willing villagers next to each other breed: both eat and spend 12
/// points, and a child is born into a bed nobody has, or (no such bed
/// within 48 blocks) the villagers show their anger and no child comes.
fn breed(
    ctx: &ServerContext,
    ground: &Ground,
    entities: &mut [Entity],
    now: i64,
    fresh: &mut Vec<Entity>,
) {
    let ready = |e: &Entity| villager_awake(e).is_some_and(|m| willing(ctx, m));
    for i in 0..entities.len() {
        if (now + i64::from(entities[i].id)) % 40 != 0 || !ready(&entities[i]) {
            continue;
        }
        let at = entities[i].pos;
        let Some(j) = (0..entities.len()).find(|&j| {
            j != i && ready(&entities[j]) && within(entities[j].pos, at, BREED_RANGE, 2.0)
        }) else {
            continue;
        };
        for k in [i, j] {
            if let Kind::Merchant(m) = &mut entities[k].kind {
                eat(ctx, m);
            }
        }
        let (ids, pos) = ([entities[i].id, entities[j].id], entities[i].pos);
        let bed = {
            let origin = [
                pos[0].floor() as i32,
                pos[1].floor() as i32,
                pos[2].floor() as i32,
            ];
            let mut poi = lock(&ctx.poi);
            let found = poi
                .near(origin, BED_RADIUS, BED_HEIGHT, |r| {
                    r.kind == poi::HOME && r.free > 0
                })
                .into_iter()
                .map(|(p, _)| p)
                .find(|&p| roomy(ground, p));
            found.filter(|&p| poi.claim(p))
        };
        let Some(bed) = bed else {
            for id in ids {
                fx::status(ctx, id, 13);
            }
            continue;
        };
        let mut variants = Vec::new();
        for k in [i, j] {
            if let Kind::Merchant(m) = &mut entities[k].kind {
                m.age = BREED_WAIT;
                variants.push(m.variant.clone());
            }
        }
        let Some(mut child) = crate::trading::new_merchant(ctx, "minecraft:villager", pos) else {
            continue;
        };
        if let Kind::Merchant(c) = &mut child.kind {
            c.age = CHILD_AGE;
            c.home = Some(cell(bed));
            c.verified = true;
            if let Some(v) = variants.get((crate::util::random_u64() >> 40) as usize % 2) {
                c.variant.clone_from(v);
            }
        }
        for id in ids {
            fx::status(ctx, id, 12);
        }
        fresh.push(child);
    }
}

// ---- bells ----

/// Villagers hear a bell this far (blocks), run to a bed this far from
/// them, and stay there for 15 seconds.
const BELL_RANGE: f64 = 32.0;
const HIDE_FOR: i64 = 300;
/// Raiders glow when one is within 32 blocks of a bell, all of them within
/// 48, for 3 seconds.
const RAIDER_RANGE: f64 = 32.0;
const GLOW_RANGE: f64 = 48.0;
const GLOW_TICKS: i32 = 60;

/// The species of raid mobs and illagers.
fn raider(species: Species) -> bool {
    matches!(
        species.id(),
        "minecraft:pillager"
            | "minecraft:vindicator"
            | "minecraft:evoker"
            | "minecraft:illusioner"
            | "minecraft:ravager"
            | "minecraft:vex"
            | "minecraft:witch"
    )
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}

/// The bell at `at` rang: villagers within 32 blocks run to their beds
/// (their own, or the nearest one within 32 blocks), and wake if they
/// slept; raiders near it glow.
pub fn ring(ctx: &ServerContext, entities: &mut [Entity], at: Position, now: i64) {
    let bell = [
        f64::from(at.x) + 0.5,
        f64::from(at.y) + 0.5,
        f64::from(at.z) + 0.5,
    ];
    let until = (now + HIDE_FOR) as u32;
    {
        let poi = lock(&ctx.poi);
        for e in entities.iter_mut().filter(|e| !e.removed) {
            let (pos, from) = (e.pos, arr(e.block_pos()));
            let Kind::Merchant(m) = &mut e.kind else {
                continue;
            };
            if m.wandering || dist(pos, bell) > BELL_RANGE {
                continue;
            }
            let bed = m.home.or_else(|| {
                poi.near(from, 32, 8, |r| r.kind == poi::HOME)
                    .first()
                    .map(|(p, _)| cell(*p))
            });
            if let Some(bed) = bed {
                m.walk.hiding = Some((until, [bed.0, bed.1, bed.2]));
            }
        }
    }
    let near = entities.iter().any(|e| {
        !e.removed
            && matches!(&e.kind, Kind::Mob(m) if raider(m.species) && m.dying.is_none())
            && dist(e.pos, bell) <= RAIDER_RANGE
    });
    if !near {
        return;
    }
    let Some(glowing) = crate::effects::kind_of("glowing") else {
        return;
    };
    for e in entities.iter_mut().filter(|e| !e.removed) {
        if matches!(&e.kind, Kind::Mob(m) if raider(m.species)) && dist(e.pos, bell) <= GLOW_RANGE {
            let on = crate::effects::Instance::new(0, GLOW_TICKS, false);
            crate::mob_effects::give(ctx, e, glowing, on, None);
        }
    }
}

/// A baby villager took a poppy from an iron golem.
pub fn flower_taken(ctx: &ServerContext, child: i32) {
    fx::status(ctx, child, 14);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    fn villager(ctx: &ServerContext, pos: [f64; 3]) -> i32 {
        let e = crate::trading::new_merchant(ctx, "minecraft:villager", pos).unwrap();
        let id = e.id;
        lock(&ctx.entities).push(e);
        id
    }

    fn merchant(ctx: &ServerContext, id: i32) -> Box<Merchant> {
        let all = lock(&ctx.entities);
        match all.iter().find(|e| e.id == id).map(|e| e.kind.clone()) {
            Some(Kind::Merchant(m)) => m,
            _ => panic!("no villager {id}"),
        }
    }

    fn bed(ctx: &ServerContext, head: [i32; 3]) {
        let b = &ctx.data.blocks;
        let s = b.default_state("minecraft:red_bed").unwrap();
        let s = b.with_property(s, "part", "head").unwrap();
        ctx.world
            .set_block(
                mc_protocol::Position::new(head[0], head[1], head[2]),
                s,
                b,
                &ctx.rules,
            )
            .unwrap();
    }

    fn run_claims(ctx: &ServerContext, now: i64) {
        let mut all = std::mem::take(&mut *lock(&ctx.entities));
        claims(ctx, &mut all, now);
        *lock(&ctx.entities) = all;
    }

    #[test]
    fn two_villagers_one_bed_the_nearer_gets_it_and_death_frees_it() {
        let Some(ctx) = testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        testkit::join(&ctx, 1, 11, [0.5, -60.0, 0.5], 0);
        bed(&ctx, [6, -60, 3]);
        testkit::set(&ctx, [9, -60, 3], "minecraft:bell");
        let near = villager(&ctx, [5.5, -60.0, 3.5]);
        for now in 0..40 {
            run_claims(&ctx, now);
        }
        // The second one comes when the bed is taken.
        let far = villager(&ctx, [20.5, -60.0, 3.5]);
        for now in 40..120 {
            run_claims(&ctx, now);
        }
        let (a, b) = (merchant(&ctx, near), merchant(&ctx, far));
        assert_eq!(a.home, Some((6, -60, 3)));
        assert_eq!(b.home, None);
        // Both meet at the bell: it has tickets for many.
        assert_eq!(a.meeting, Some((9, -60, 3)));
        assert_eq!(b.meeting, Some((9, -60, 3)));
        // The one with the bed dies; the other takes it.
        for e in lock(&ctx.entities).iter_mut().filter(|e| e.id == near) {
            e.removed = true;
        }
        let mut all = std::mem::take(&mut *lock(&ctx.entities));
        release_removed(&ctx, &mut all);
        all.retain(|e| !e.removed);
        *lock(&ctx.entities) = all;
        assert_eq!(lock(&ctx.poi).get([6, -60, 3]).unwrap().free, 1);
        assert_eq!(lock(&ctx.poi).get([9, -60, 3]).unwrap().free, 31);
        for now in 120..200 {
            run_claims(&ctx, now);
        }
        assert_eq!(merchant(&ctx, far).home, Some((6, -60, 3)));
    }

    #[test]
    fn a_broken_bed_leaves_its_villager_homeless() {
        let Some(ctx) = testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        testkit::join(&ctx, 1, 11, [0.5, -60.0, 0.5], 0);
        bed(&ctx, [6, -60, 3]);
        let id = villager(&ctx, [5.5, -60.0, 3.5]);
        for now in 0..80 {
            run_claims(&ctx, now);
        }
        assert_eq!(merchant(&ctx, id).home, Some((6, -60, 3)));
        testkit::set(&ctx, [6, -60, 3], "minecraft:air");
        for now in 80..160 {
            run_claims(&ctx, now);
        }
        assert_eq!(merchant(&ctx, id).home, None);
    }

    #[test]
    fn a_villager_that_comes_in_with_a_claim_the_points_lost_takes_it_again() {
        let Some(ctx) = testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        testkit::join(&ctx, 1, 11, [0.5, -60.0, 0.5], 0);
        bed(&ctx, [6, -60, 3]);
        let id = villager(&ctx, [5.5, -60.0, 3.5]);
        for e in lock(&ctx.entities).iter_mut().filter(|e| e.id == id) {
            if let Kind::Merchant(m) = &mut e.kind {
                m.home = Some((6, -60, 3));
            }
        }
        // The bed has its ticket (rebuilt, say), the villager says it is
        // its own: the ticket is taken so nobody else gets the bed.
        run_claims(&ctx, 1);
        assert_eq!(lock(&ctx.poi).get([6, -60, 3]).unwrap().free, 0);
        assert_eq!(merchant(&ctx, id).home, Some((6, -60, 3)));
    }
}
