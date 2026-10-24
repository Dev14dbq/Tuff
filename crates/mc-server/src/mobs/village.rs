//! A village's life (minecraft.wiki, "Villager", "Village mechanics"): the
//! tickets villagers hold at the points of interest around them, and what
//! they do together.
//!
//! - a villager looks for a bed (a "home") and a bell (the "meeting point")
//!   near it and takes a ticket at each; a bed has one ticket, so one
//!   villager, and a bell 32;
//! - a ticket goes back when the villager dies or is removed, and the claim
//!   is dropped when the block is broken;
//! - villagers read from a file check their claims against the points
//!   (the points are rebuilt from the chunk's blocks, so a ticket the file
//!   lost is taken again).

use mc_entity::trade::Merchant;
use mc_entity::{Entity, Kind};

use crate::context::ServerContext;
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
