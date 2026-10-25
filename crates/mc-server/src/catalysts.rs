//! Sculk catalysts (minecraft.wiki, "Sculk Catalyst"): a mob a player kills
//! within 8 blocks of one leaves no experience; the catalyst blooms and the
//! experience runs out from where it died as a charge of sculk. Each tick a
//! charge turns the block it is on to sculk (stone, dirt, sand and the like:
//! `sculk_replaceable`) for one of its points and creeps on to a
//! neighbouring block that lies open to the air; on sculk far enough from the
//! catalyst a big charge sometimes grows a sensor, or a shrieker (one that
//! never summons), and spends ten. A charge that finds nowhere to go, or has
//! nothing left, is done.
//!
//! This is a simpler walk than vanilla's sculk spreader (no veins on the
//! faces of other blocks, no decay delays); the blocks it makes are the
//! same.

use std::collections::HashSet;
use std::sync::OnceLock;

use mc_protocol::Position;

use crate::context::{Event, ServerContext};
use crate::level::WorldLevel;
use crate::tick::lock;
use crate::util::random_u64;

/// How far from a death a catalyst takes the experience.
const RANGE: f64 = 8.0;
const BLOOM_TICKS: i64 = 8;

#[derive(Clone, Copy, Debug)]
pub struct Charge {
    pub catalyst: Position,
    pub pos: Position,
    pub left: i32,
}

#[derive(Debug, Default)]
pub struct Catalysts {
    pub charges: Vec<Charge>,
    /// Catalysts in bloom and when they stop.
    blooms: Vec<(Position, i64)>,
}

fn below(n: u64) -> u64 {
    random_u64() % n.max(1)
}

fn replaceable(ctx: &ServerContext) -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        ctx.commands
            .env
            .tag("minecraft:block", "minecraft:sculk_replaceable")
            .into_iter()
            .collect()
    })
}

fn name_at(ctx: &ServerContext, p: Position) -> Option<&str> {
    let state = ctx.world.block_if_loaded(p)?;
    ctx.data.blocks.block_name(state)
}

/// A mob a player killed at `pos` would leave `xp`: a catalyst within 8
/// blocks takes it (true) and the sculk spreads.
pub fn took(ctx: &ServerContext, pos: [f64; 3], xp: i32) -> bool {
    if xp <= 0 {
        return false;
    }
    let catalyst = lock(&ctx.vibrations).catalysts_near(pos, RANGE);
    let Some(catalyst) = catalyst
        .into_iter()
        .find(|p| name_at(ctx, *p) == Some(crate::vibrations::CATALYST))
    else {
        return false;
    };
    let at = Position::new(
        pos[0].floor() as i32,
        (pos[1] - 0.5).floor() as i32,
        pos[2].floor() as i32,
    );
    let now = lock(&ctx.time).age;
    {
        let mut c = lock(&ctx.catalysts);
        c.charges.push(Charge {
            catalyst,
            pos: at,
            left: xp,
        });
        c.blooms.push((catalyst, now + BLOOM_TICKS));
    }
    set_bloom(ctx, catalyst, true);
    crate::fx::sound(
        ctx,
        "block.sculk_catalyst.bloom",
        crate::fx::category::BLOCKS,
        [
            f64::from(catalyst.x) + 0.5,
            f64::from(catalyst.y) + 0.5,
            f64::from(catalyst.z) + 0.5,
        ],
        2.0,
        0.6 + below(40) as f32 / 100.0,
    );
    true
}

fn set_bloom(ctx: &ServerContext, p: Position, on: bool) {
    let Some(state) = ctx.world.block_if_loaded(p) else {
        return;
    };
    let value = if on { "true" } else { "false" };
    let Some(new) = ctx.data.blocks.with_property(state, "bloom", value) else {
        return;
    };
    if new != state {
        set(ctx, p, new);
    }
}

fn set(ctx: &ServerContext, p: Position, state: u32) {
    let mut level = WorldLevel::new(ctx);
    level.set_block(p, state);
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

/// Whether a block lies open to the air on some side.
fn exposed(ctx: &ServerContext, p: Position) -> bool {
    [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ]
    .iter()
    .any(|(dx, dy, dz)| {
        ctx.world
            .block_if_loaded(Position::new(p.x + dx, p.y + dy, p.z + dz))
            .is_some_and(|s| ctx.data.blocks.is_air(s))
    })
}

/// One tick of the charges and the blooms.
pub fn tick(ctx: &ServerContext) {
    let now = lock(&ctx.time).age;
    let (charges, done) = {
        let mut c = lock(&ctx.catalysts);
        let (done, later): (Vec<_>, Vec<_>) = std::mem::take(&mut c.blooms)
            .into_iter()
            .partition(|b| b.1 <= now);
        c.blooms = later;
        (std::mem::take(&mut c.charges), done)
    };
    for (p, _) in done {
        set_bloom(ctx, p, false);
    }
    let mut kept = Vec::new();
    for ch in charges {
        if let Some(next) = step(ctx, ch) {
            kept.push(next);
        }
    }
    lock(&ctx.catalysts).charges.extend(kept);
}

fn step(ctx: &ServerContext, mut ch: Charge) -> Option<Charge> {
    if ch.left <= 0 {
        return None;
    }
    let blocks = &ctx.data.blocks;
    let name = name_at(ctx, ch.pos)?;
    if replaceable(ctx).contains(name) {
        set(ctx, ch.pos, blocks.default_state("minecraft:sculk")?);
        ch.left -= 1;
    } else if name == "minecraft:sculk" {
        // Growths, away from the catalyst, from a big charge.
        let far = (ch.pos.x - ch.catalyst.x)
            .abs()
            .max((ch.pos.z - ch.catalyst.z).abs())
            >= 4;
        let up = Position::new(ch.pos.x, ch.pos.y + 1, ch.pos.z);
        let open = ctx
            .world
            .block_if_loaded(up)
            .is_some_and(|s| blocks.is_air(s));
        if far && open && ch.left >= 10 && below(10) == 0 {
            let growth = if below(10) == 0 {
                blocks.default_state("minecraft:sculk_shrieker")
            } else {
                blocks.default_state("minecraft:sculk_sensor")
            };
            if let Some(g) = growth {
                set(ctx, up, g);
                ch.left -= 10;
            }
        }
    }
    // On to a neighbour (diagonals up and down too) that is open to the air.
    let mut options = Vec::new();
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if (dx, dy, dz) == (0, 0, 0) {
                    continue;
                }
                let q = Position::new(ch.pos.x + dx, ch.pos.y + dy, ch.pos.z + dz);
                let Some(n) = name_at(ctx, q) else { continue };
                if (n == "minecraft:sculk" || replaceable(ctx).contains(n)) && exposed(ctx, q) {
                    options.push(q);
                }
            }
        }
    }
    // Rather somewhere not yet sculk.
    let fresh: Vec<Position> = options
        .iter()
        .copied()
        .filter(|q| name_at(ctx, *q) != Some("minecraft:sculk"))
        .collect();
    let pool = if fresh.is_empty() { &options } else { &fresh };
    if pool.is_empty() {
        return None;
    }
    ch.pos = pool[below(pool.len() as u64) as usize];
    Some(ch)
}
