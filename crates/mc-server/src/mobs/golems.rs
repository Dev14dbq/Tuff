//! Iron golems and snow golems (minecraft.wiki, "Iron Golem", "Snow
//! Golem"): building them from blocks, the snow a snow golem leaves, and
//! how the villagers' defenders find out who hurt a villager.
//!
//! - an iron golem is four iron blocks in a T (three in a row and one under
//!   the middle) with a carved pumpkin or jack o'lantern on top of the
//!   middle, in any of the ways it can lie, the four corners of the 3×3
//!   air; a snow golem is two snow blocks under a pumpkin, in any
//!   direction. Whichever block is placed last, by a player, a dispenser or
//!   an enderman, completes it;
//! - a snow golem leaves a layer of snow on each of the four corners under
//!   it where the air is free and snow can lie.

use mc_entity::{Entity, Kind, Species};
use mc_protocol::Position;

use super::new_mob;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::level::WorldLevel;

const DIRS: [[i32; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

fn add(p: Position, d: [i32; 3], k: i32) -> Position {
    Position::new(p.x + d[0] * k, p.y + d[1] * k, p.z + d[2] * k)
}

fn name_at(ctx: &ServerContext, p: Position) -> Option<&str> {
    let state = ctx.world.block(p, &ctx.data.blocks)?;
    ctx.data.blocks.block_name(state)
}

fn is(ctx: &ServerContext, p: Position, name: &str) -> bool {
    name_at(ctx, p) == Some(name)
}

fn air(ctx: &ServerContext, p: Position) -> bool {
    ctx.world
        .block(p, &ctx.data.blocks)
        .is_some_and(|s| ctx.data.blocks.is_air(s))
}

/// Whether a block is a head a golem takes.
pub fn is_head(name: &str) -> bool {
    matches!(
        name,
        "minecraft:carved_pumpkin" | "minecraft:jack_o_lantern"
    )
}

/// A golem the blocks around a head make.
#[derive(Debug, PartialEq)]
pub struct Build {
    pub species: Species,
    /// Every block of it, the head too.
    pub cells: Vec<Position>,
    /// The block the golem stands at.
    pub foot: Position,
}

/// The golem a head at `head` completes with the blocks around it, if any
/// (the head itself is not looked at, so a dispenser can ask first).
pub fn find(ctx: &ServerContext, head: Position) -> Option<Build> {
    for d in DIRS {
        let (body, foot) = (add(head, d, 1), add(head, d, 2));
        // Two snow blocks under it.
        if is(ctx, body, "minecraft:snow_block") && is(ctx, foot, "minecraft:snow_block") {
            return Some(Build {
                species: Species::SnowGolem,
                cells: vec![head, body, foot],
                foot,
            });
        }
        // Or a T: arms across the body, the corners free.
        if !is(ctx, body, "minecraft:iron_block") || !is(ctx, foot, "minecraft:iron_block") {
            continue;
        }
        for a in DIRS {
            // One of each pair of opposite arm directions is enough.
            if a[0] * d[0] + a[1] * d[1] + a[2] * d[2] != 0 || a.iter().sum::<i32>() < 0 {
                continue;
            }
            let (left, right) = (add(body, a, 1), add(body, a, -1));
            let corners = [
                add(head, a, 1),
                add(head, a, -1),
                add(foot, a, 1),
                add(foot, a, -1),
            ];
            if is(ctx, left, "minecraft:iron_block")
                && is(ctx, right, "minecraft:iron_block")
                && corners.iter().all(|c| air(ctx, *c))
            {
                return Some(Build {
                    species: Species::IronGolem,
                    cells: vec![head, body, foot, left, right],
                    foot,
                });
            }
        }
    }
    None
}

/// The blocks of `build` go and the golem takes their place (made by a
/// player, so an iron one leaves players alone). Returns the block changes
/// for clients.
fn raise(ctx: &ServerContext, build: &Build) -> Event {
    let mut level = WorldLevel::new(ctx);
    for &c in &build.cells {
        if let Some(s) = ctx.world.block(c, &ctx.data.blocks)
            && !ctx.data.blocks.is_air(s)
        {
            fx::level_event(
                ctx,
                2001,
                [f64::from(c.x), f64::from(c.y), f64::from(c.z)],
                s as i32,
            );
        }
        level.set_block(c, ctx.shapes.air());
    }
    let at = [
        f64::from(build.foot.x) + 0.5,
        f64::from(build.foot.y) + 0.05,
        f64::from(build.foot.z) + 0.5,
    ];
    let mut e = new_mob(ctx, build.species, at);
    if let Kind::Mob(m) = &mut e.kind {
        m.yaw = 0.0;
        m.head_yaw = 0.0;
        m.quirks.player_made = true;
    }
    crate::tick::spawn_entity(ctx, e);
    Event::BlocksChanged {
        by: 0,
        changes: level.into_changes(),
    }
}

/// A block was placed at `p`: if it is a head that finishes a golem, the
/// golem comes. Returns the events for clients.
pub fn placed(ctx: &ServerContext, p: Position, state: u32) -> Vec<Event> {
    if !ctx.data.blocks.block_name(state).is_some_and(is_head) {
        return Vec::new();
    }
    match find(ctx, p) {
        Some(build) => vec![raise(ctx, &build)],
        None => Vec::new(),
    }
}

/// Whether a carved pumpkin placed in the empty cell `front` would finish
/// a golem (a dispenser asks).
pub fn would_build(ctx: &ServerContext, front: Position) -> bool {
    air(ctx, front) && find(ctx, front).is_some()
}

/// A snow golem's snow: a layer on each corner of its feet where there is
/// air and snow can lie.
pub fn snow_trail(ctx: &ServerContext, level: &mut WorldLevel, e: &Entity) {
    let Some(snow) = ctx.data.blocks.default_state("minecraft:snow") else {
        return;
    };
    for i in 0..4 {
        let at = Position::new(
            (e.pos[0] + f64::from(i % 2 * 2 - 1) * 0.25).floor() as i32,
            e.pos[1].floor() as i32,
            (e.pos[2] + f64::from(i / 2 % 2 * 2 - 1) * 0.25).floor() as i32,
        );
        if !air(ctx, at) {
            continue;
        }
        if mc_tick::support::can_survive(level, level.logic(), crate::level::to_pos(at), snow) {
            level.set_block(at, snow);
        }
    }
}

/// A villager was hurt by the mob `attacker`: the iron golems within 16
/// blocks of it go for the attacker.
pub fn defend(entities: &mut [Entity], villager: usize, attacker: i32) {
    let at = entities[villager].pos;
    for e in entities.iter_mut() {
        let near = super::dist_sq(e.pos, at) <= 256.0;
        if let Kind::Mob(m) = &mut e.kind
            && near
            && m.species == Species::IronGolem
            && e.id != attacker
        {
            m.revenge = Some((attacker, 100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick::lock;

    fn set(ctx: &ServerContext, p: [i32; 3], name: &str) {
        let state = ctx.data.blocks.default_state(name).unwrap();
        ctx.world
            .set_block(
                Position::new(p[0], p[1], p[2]),
                state,
                &ctx.data.blocks,
                &ctx.rules,
            )
            .unwrap();
    }

    fn golems(ctx: &ServerContext, species: Species) -> Vec<Entity> {
        lock(&ctx.entities)
            .iter()
            .filter(|e| matches!(&e.kind, Kind::Mob(m) if m.species == species))
            .cloned()
            .collect()
    }

    /// Four iron blocks of an upright T with the head to be put at (0,3,0)
    /// from a base at (x, -60, 0).
    fn upright(ctx: &ServerContext, x: i32) {
        for (dx, dy) in [(0, 0), (0, 1), (-1, 1), (1, 1)] {
            set(ctx, [x + dx, -60 + dy, 0], "minecraft:iron_block");
        }
    }

    fn place_head(ctx: &ServerContext, at: [i32; 3], name: &str) -> Vec<Event> {
        set(ctx, at, name);
        let state = ctx
            .world
            .block(Position::new(at[0], at[1], at[2]), &ctx.data.blocks)
            .unwrap();
        placed(ctx, Position::new(at[0], at[1], at[2]), state)
    }

    #[test]
    fn a_t_of_iron_with_a_pumpkin_makes_an_iron_golem_that_a_player_made() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        crate::testkit::join(&ctx, 1, 11, [0.5, -60.0, 5.5], 0);
        upright(&ctx, 20);
        let events = place_head(&ctx, [20, -58, 0], "minecraft:carved_pumpkin");
        assert!(matches!(events[0], Event::BlocksChanged { .. }));
        for (dx, dy) in [(0, 0), (0, 1), (-1, 1), (1, 1), (0, 2)] {
            assert_eq!(
                crate::testkit::block(&ctx, [20 + dx, -60 + dy, 0]),
                "minecraft:air"
            );
        }
        let g = golems(&ctx, Species::IronGolem);
        assert_eq!(g.len(), 1);
        let Kind::Mob(m) = &g[0].kind else { panic!() };
        assert!(m.quirks.player_made);
        assert_eq!((g[0].pos[0], g[0].pos[2]), (20.5, 0.5));
    }

    #[test]
    fn the_iron_golem_t_may_lie_or_hang_and_wants_free_corners_and_a_head() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        crate::testkit::join(&ctx, 1, 11, [0.5, -60.0, 5.5], 0);
        // Lying along x: the body at x = 30..31, arms up and down the y axis.
        for (x, y) in [(30, -58), (31, -58), (31, -59), (31, -57)] {
            set(&ctx, [x, y, 0], "minecraft:iron_block");
        }
        // A block in a corner spoils it.
        set(&ctx, [32, -59, 0], "minecraft:stone");
        place_head(&ctx, [32, -58, 0], "minecraft:jack_o_lantern");
        assert!(golems(&ctx, Species::IronGolem).is_empty());
        set(&ctx, [32, -59, 0], "minecraft:air");
        let state = ctx
            .world
            .block(Position::new(32, -58, 0), &ctx.data.blocks)
            .unwrap();
        let events = placed(&ctx, Position::new(32, -58, 0), state);
        assert!(!events.is_empty());
        assert_eq!(golems(&ctx, Species::IronGolem).len(), 1);
        // Without the T there is nothing, and a plain pumpkin is no head.
        assert!(place_head(&ctx, [40, -60, 0], "minecraft:carved_pumpkin").is_empty());
        upright(&ctx, 50);
        assert!(place_head(&ctx, [50, -58, 0], "minecraft:pumpkin").is_empty());
        assert_eq!(golems(&ctx, Species::IronGolem).len(), 1);
    }

    #[test]
    fn two_snow_blocks_and_a_pumpkin_make_a_snow_golem_wearing_it() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        crate::testkit::join(&ctx, 1, 11, [0.5, -60.0, 5.5], 0);
        set(&ctx, [20, -60, 0], "minecraft:snow_block");
        set(&ctx, [20, -59, 0], "minecraft:snow_block");
        place_head(&ctx, [20, -58, 0], "minecraft:carved_pumpkin");
        let g = golems(&ctx, Species::SnowGolem);
        assert_eq!(g.len(), 1);
        let Kind::Mob(m) = &g[0].kind else { panic!() };
        assert!(m.quirks.pumpkin);
        assert_eq!(crate::testkit::block(&ctx, [20, -59, 0]), "minecraft:air");
        // Sideways works too.
        set(&ctx, [30, -60, 0], "minecraft:snow_block");
        set(&ctx, [31, -60, 0], "minecraft:snow_block");
        place_head(&ctx, [32, -60, 0], "minecraft:jack_o_lantern");
        assert_eq!(golems(&ctx, Species::SnowGolem).len(), 2);
    }

    #[test]
    fn a_dispenser_can_ask_whether_a_pumpkin_would_finish_a_golem() {
        let Some(ctx) = crate::testkit::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        set(&ctx, [20, -60, 0], "minecraft:snow_block");
        set(&ctx, [20, -59, 0], "minecraft:snow_block");
        assert!(would_build(&ctx, Position::new(20, -58, 0)));
        // Not into a cell that is taken, nor where nothing is built.
        assert!(!would_build(&ctx, Position::new(20, -59, 0)));
        assert!(!would_build(&ctx, Position::new(40, -58, 0)));
    }
}
