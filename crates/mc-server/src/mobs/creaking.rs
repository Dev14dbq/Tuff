//! Creaking hearts (minecraft.wiki, "Creaking Heart", "Creaking").
//!
//! A heart between two pale oak logs of its own axis wakes at night in the
//! overworld (ticks 12600 to 23400 of the day) and, 20 ticks after it finds
//! a spot within 16 blocks sideways and 8 up or down (solid floor, three
//! blocks of air), sends out a creaking bound to it. The creaking cannot be
//! hurt but by the void and `/kill`; a creaking hit at night by a player
//! grows a resin clump on a log by the heart, once every 5 seconds. By day
//! the heart goes dormant and the creaking goes; one farther than 34 blocks
//! from it goes too. Breaking the heart kills the creaking, and a natural
//! heart gives 20 to 24 experience.

use mc_nbt::Tag;
use mc_protocol::Position;

use mc_entity::{Entity, Kind, Species};

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::level::WorldLevel;
use crate::tick::lock;

const ID: &str = "minecraft:creaking_heart";

fn uuid_of(c: &mc_nbt::Compound) -> Option<u128> {
    match c.get("creaking")? {
        Tag::IntArray(v) if v.len() == 4 => Some(
            v.iter()
                .fold(0u128, |acc, &i| (acc << 32) | u128::from(i as u32)),
        ),
        _ => None,
    }
}

fn uuid_tag(u: u128) -> Tag {
    Tag::IntArray((0..4).map(|i| (u >> (96 - 32 * i)) as u32 as i32).collect())
}

fn is_log(name: &str) -> bool {
    matches!(
        name,
        "minecraft:pale_oak_log"
            | "minecraft:stripped_pale_oak_log"
            | "minecraft:pale_oak_wood"
            | "minecraft:stripped_pale_oak_wood"
    )
}

fn name_at(ctx: &ServerContext, p: Position) -> Option<&str> {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
}

/// The cells either side of a heart along its axis.
fn ends(ctx: &ServerContext, p: Position, state: u32) -> [Position; 2] {
    let (dx, dy, dz) = match ctx.data.blocks.property(state, "axis") {
        Some("x") => (1, 0, 0),
        Some("z") => (0, 0, 1),
        _ => (0, 1, 0),
    };
    [
        Position::new(p.x + dx, p.y + dy, p.z + dz),
        Position::new(p.x - dx, p.y - dy, p.z - dz),
    ]
}

/// Whether pale oak logs of the heart's axis sit on both its ends.
pub fn aligned(ctx: &ServerContext, p: Position) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(state) = ctx.world.block(p, blocks) else {
        return false;
    };
    let axis = blocks.property(state, "axis");
    ends(ctx, p, state).iter().all(|&e| {
        let Some(s) = ctx.world.block(e, blocks) else {
            return false;
        };
        blocks.block_name(s).is_some_and(is_log) && blocks.property(s, "axis") == axis
    })
}

/// Whether it is the time of day a heart is awake: ticks 12600 to 23400.
pub fn awake_time(day_time: i64) -> bool {
    (12600..23400).contains(&day_time.rem_euclid(24000))
}

/// The creaking bound to the heart at `p`, if it lives.
fn linked(ctx: &ServerContext, entities: &[Entity], p: Position) -> Option<usize> {
    let be = ctx.world.block_entity(p, &ctx.data.blocks)?;
    let u = uuid_of(&be.data)?;
    entities.iter().position(|e| {
        e.uuid == u && !e.removed && matches!(&e.kind, Kind::Mob(m) if m.dying.is_none())
    })
}

/// A heart was placed: it gets its block entity, so the tick finds it.
pub fn placed(ctx: &ServerContext, p: Position) {
    if ctx.world.block_entity(p, &ctx.data.blocks).is_none() {
        bind(ctx, p, None);
    }
}

fn bind(ctx: &ServerContext, p: Position, uuid: Option<u128>) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or_else(|| mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: ID.into(),
            data: mc_nbt::Compound::new(),
        });
    match uuid {
        Some(u) => {
            be.data.insert("creaking", uuid_tag(u));
        }
        None => {
            be.data.remove("creaking");
        }
    }
    ctx.world.set_block_entity(be, blocks);
}

/// A spot a creaking may come to: a floor, three blocks of air above it,
/// nothing standing there.
fn spot(ctx: &ServerContext, entities: &[Entity], p: Position) -> Option<[f64; 3]> {
    let blocks = &ctx.data.blocks;
    for _ in 0..16 {
        let x = p.x + (crate::hits::rnd() * 33.0).floor() as i32 - 16;
        let z = p.z + (crate::hits::rnd() * 33.0).floor() as i32 - 16;
        for y in (p.y - 8..=p.y + 8).rev() {
            let Some(floor) = ctx.world.block(Position::new(x, y - 1, z), blocks) else {
                break;
            };
            if !ctx.rules.blocks_motion(floor)
                || blocks
                    .block_name(floor)
                    .is_some_and(|n| n.ends_with("_leaves"))
            {
                continue;
            }
            let free = (0..3).all(|dy| {
                ctx.world
                    .block(Position::new(x, y + dy, z), blocks)
                    .is_some_and(|s| ctx.shapes.is_air(s))
            });
            if !free {
                continue;
            }
            let at = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
            let crowded = entities.iter().any(|e| {
                (e.pos[0] - at[0]).abs() < 1.0
                    && (e.pos[2] - at[2]).abs() < 1.0
                    && (e.pos[1] - at[1]).abs() < 3.0
            });
            if !crowded {
                return Some(at);
            }
        }
    }
    None
}

/// One tick of every creaking heart in the loaded chunks (the day decides,
/// and only the overworld has hearts that work).
pub fn tick(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    now: i64,
    day_time: i64,
) {
    let overworld = ctx.world.dimension == "minecraft:overworld";
    let night = overworld && awake_time(day_time);
    for be in ctx.world.block_entities_of(ID) {
        let p = Position::new(be.x, be.y, be.z);
        let blocks = &ctx.data.blocks;
        let Some(state) = ctx.world.block(p, blocks) else {
            continue;
        };
        if blocks.block_name(state) != Some(ID) {
            continue;
        }
        let link = linked(ctx, entities, p);
        let ok = aligned(ctx, p);
        let want = if !ok && link.is_none() {
            "uprooted"
        } else if night {
            "awake"
        } else {
            "dormant"
        };
        if blocks.property(state, "creaking_heart_state") != Some(want) {
            level.set_block(p, ctx.shapes.with(state, "creaking_heart_state", want));
        }
        // A creaking far from home, or the day, takes it away.
        if let Some(i) = link {
            let far = (entities[i].pos[0] - f64::from(p.x)).powi(2)
                + (entities[i].pos[1] - f64::from(p.y)).powi(2)
                + (entities[i].pos[2] - f64::from(p.z)).powi(2)
                > 34.0 * 34.0;
            if far || !night {
                let id = entities[i].id;
                entities[i].removed = true;
                let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
                bind(ctx, p, None);
            } else if let Kind::Mob(m) = &mut entities[i].kind {
                m.quirks.air.home = Some([p.x, p.y, p.z]);
                if !m.quirks.air.active {
                    m.quirks.air.active = true;
                    m.dirty = true;
                }
            }
            continue;
        }
        if !night || !ok || be.data.get("creaking").is_some() && link.is_none() && now % 20 != 0 {
            continue;
        }
        if link.is_none() && be.data.get("creaking").is_some() {
            bind(ctx, p, None);
        }
        // Every so often it looks for a place for one.
        if now % 20 != i64::from(p.x.rem_euclid(20)) {
            continue;
        }
        let Some(at) = spot(ctx, entities, p) else {
            continue;
        };
        let mut e = super::new_mob(ctx, Species::Creaking, at);
        if let Kind::Mob(m) = &mut e.kind {
            m.quirks.air.home = Some([p.x, p.y, p.z]);
            m.quirks.air.active = true;
            m.persistent = true;
        }
        bind(ctx, p, Some(e.uuid));
        let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
        lock(&ctx.entities).push(e);
    }
}

/// A heart was broken: its creaking dies (the breaker gets the credit), and
/// a natural heart gives experience.
pub fn broken(ctx: &ServerContext, p: Position, state: u32, by: Option<u128>) {
    let blocks = &ctx.data.blocks;
    if let Some(be) = ctx.world.block_entity(p, blocks)
        && let Some(u) = uuid_of(&be.data)
    {
        let mut all = lock(&ctx.entities);
        if let Some(e) = all.iter_mut().find(|e| e.uuid == u)
            && let Kind::Mob(m) = &mut e.kind
        {
            m.hurt_by_player = by.map(|u| (u, 100));
            super::hurt::die(ctx, e, None, 0);
        }
    }
    if blocks.property(state, "natural") == Some("true") && by.is_some() {
        let xp = 20 + (crate::hits::rnd() * 5.0) as i32;
        crate::xp::spawn(
            ctx,
            [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ],
            xp,
        );
    }
}

/// A creaking its heart keeps was hit by a player at night: a resin clump
/// grows on a pale oak log near the heart (not twice in five seconds).
pub fn hit(ctx: &ServerContext, home: [i32; 3], now: i64) {
    let blocks = &ctx.data.blocks;
    let p = Position::new(home[0], home[1], home[2]);
    let Some(mut be) = ctx.world.block_entity(p, blocks) else {
        return;
    };
    let ready = match be.data.get("resin_ready") {
        Some(Tag::Long(t)) => *t,
        _ => 0,
    };
    if now < ready {
        return;
    }
    be.data.insert("resin_ready", Tag::Long(now + 100));
    ctx.world.set_block_entity(be, blocks);
    let faces = [
        ("up", (0, 1, 0)),
        ("down", (0, -1, 0)),
        ("north", (0, 0, -1)),
        ("south", (0, 0, 1)),
        ("west", (-1, 0, 0)),
        ("east", (1, 0, 0)),
    ];
    for _ in 0..30 {
        let q = Position::new(
            p.x + (crate::hits::rnd() * 7.0) as i32 - 3,
            p.y + (crate::hits::rnd() * 7.0) as i32 - 3,
            p.z + (crate::hits::rnd() * 7.0) as i32 - 3,
        );
        if !name_at(ctx, q).is_some_and(is_log) {
            continue;
        }
        let (face, (dx, dy, dz)) = faces[(crate::hits::rnd() * 6.0) as usize % 6];
        let at = Position::new(q.x + dx, q.y + dy, q.z + dz);
        if !ctx
            .world
            .block(at, blocks)
            .is_some_and(|s| ctx.shapes.is_air(s))
        {
            continue;
        }
        let Some(clump) = blocks.default_state("minecraft:resin_clump") else {
            return;
        };
        // The clump clings to the log from the side the log is on.
        let opposite = match face {
            "up" => "down",
            "down" => "up",
            "north" => "south",
            "south" => "north",
            "west" => "east",
            _ => "west",
        };
        let state = ctx.shapes.with(clump, opposite, "true");
        let _ = ctx.world.set_block(at, state, blocks, &ctx.rules);
        let _ = ctx.events.send(Event::BlocksChanged {
            by: 0,
            changes: vec![(at, state)],
        });
        fx::sound(
            ctx,
            "block.resin_clump.place",
            fx::category::BLOCKS,
            [
                f64::from(at.x) + 0.5,
                f64::from(at.y) + 0.5,
                f64::from(at.z) + 0.5,
            ],
            1.0,
            1.0,
        );
        return;
    }
}
