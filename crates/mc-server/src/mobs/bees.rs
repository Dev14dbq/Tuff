//! Bees, hives and nests (minecraft.wiki, "Bee", "Beehive", "Bee Nest").
//!
//! A hive or nest holds three bees. A bee in the daytime looks for a flower
//! within 5 blocks and circles it for 400 ticks to take nectar; carrying
//! nectar it flies to its hive, and over a crop 1 to 2 blocks below it
//! grows it (5% a tick, up to 10 times a load). At night, in the rain and
//! with nectar it goes home; inside it stays at least 2400 ticks (600 for
//! the bees a tree grew with), then comes out the front by day, and the
//! nectar it carried lifts the honey level (by 2 one time in a hundred).
//! Breaking the block, or taking honey without a lit campfire within five
//! blocks below, lets the bees out angry.

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use mc_entity::{Entity, Kind, Species};

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::level::WorldLevel;
use crate::tick::lock;

const CAPACITY: usize = 3;
/// How long a bee that went in stays there at least.
const STAY: i32 = 2400;
/// The wait before a bee that was turned away tries again.
const SMOKED: i32 = 400;

pub fn is_hive(name: &str) -> bool {
    matches!(name, "minecraft:beehive" | "minecraft:bee_nest")
}

fn name_at(ctx: &ServerContext, p: Position) -> Option<&str> {
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
}

/// A bee in a hive.
#[derive(Clone, Debug)]
pub struct Inside {
    pub data: Compound,
    pub ticks: i32,
    pub min_ticks: i32,
}

fn int(c: &Compound, key: &str) -> i32 {
    match c.get(key) {
        Some(Tag::Int(v)) => *v,
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Byte(v)) => i32::from(*v),
        _ => 0,
    }
}

/// The bees inside the hive at `p`.
pub fn inside(ctx: &ServerContext, p: Position) -> Vec<Inside> {
    let Some(be) = ctx.world.block_entity(p, &ctx.data.blocks) else {
        return Vec::new();
    };
    let Some(Tag::List(list)) = be.data.get("bees") else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|t| {
            let Tag::Compound(c) = t else { return None };
            let Some(Tag::Compound(data)) = c.get("entity_data") else {
                return None;
            };
            Some(Inside {
                data: data.clone(),
                ticks: int(c, "ticks_in_hive"),
                min_ticks: int(c, "min_ticks_in_hive"),
            })
        })
        .collect()
}

fn store(ctx: &ServerContext, p: Position, bees: &[Inside]) {
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or_else(|| mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: "minecraft:beehive".into(),
            data: Compound::new(),
        });
    let list = bees
        .iter()
        .map(|b| {
            let mut c = Compound::new();
            c.insert("entity_data", Tag::Compound(b.data.clone()));
            c.insert("ticks_in_hive", Tag::Int(b.ticks));
            c.insert("min_ticks_in_hive", Tag::Int(b.min_ticks));
            Tag::Compound(c)
        })
        .collect();
    be.data.insert("bees", Tag::List(list));
    ctx.world.set_block_entity(be, blocks);
}

fn center(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

/// The cell in front of the hive (where bees come out).
fn front(ctx: &ServerContext, p: Position) -> Position {
    let blocks = &ctx.data.blocks;
    let (dx, dz) = match ctx
        .world
        .block(p, blocks)
        .and_then(|s| blocks.property(s, "facing"))
    {
        Some("north") => (0, -1),
        Some("east") => (1, 0),
        Some("west") => (-1, 0),
        _ => (0, 1),
    };
    Position::new(p.x + dx, p.y, p.z + dz)
}

/// Puts the bee at `i` into the hive at `hive`; whether there was room.
pub fn enter(ctx: &ServerContext, entities: &mut [Entity], i: usize, hive: Position) -> bool {
    if !name_at(ctx, hive).is_some_and(is_hive) {
        return false;
    }
    let mut bees = inside(ctx, hive);
    if bees.len() >= CAPACITY {
        return false;
    }
    let Kind::Mob(m) = &entities[i].kind else {
        return false;
    };
    let mut data = Compound::new();
    data.insert("id", Tag::String("minecraft:bee".into()));
    super::nbt::write(ctx, m, &mut data);
    bees.push(Inside {
        data,
        ticks: 0,
        min_ticks: STAY,
    });
    store(ctx, hive, &bees);
    let id = entities[i].id;
    entities[i].removed = true;
    let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
    fx::sound(
        ctx,
        "block.beehive.enter",
        fx::category::BLOCKS,
        center(hive),
        1.0,
        1.0,
    );
    true
}

/// How a bee comes out.
#[derive(Clone, Copy, PartialEq)]
pub enum Release {
    /// Out for the day.
    Calm,
    /// The hive was broken or robbed: angry at this player.
    Angry(Option<u128>),
}

/// Sets the bee `n` of the hive free in front of it. Returns its entity.
fn release(ctx: &ServerContext, hive: Position, bee: &Inside, how: Release) -> Entity {
    let at = front(ctx, hive);
    let pos = [
        f64::from(at.x) + 0.5,
        f64::from(at.y) + 0.1,
        f64::from(at.z) + 0.5,
    ];
    let mut e = super::new_mob(ctx, Species::Bee, pos);
    if let Kind::Mob(m) = &mut e.kind {
        super::nbt::apply(ctx, m, &bee.data);
        m.quirks.air.hive = Some([hive.x, hive.y, hive.z]);
        m.quirks.air.cannot_enter = SMOKED;
        if m.quirks.air.nectar && how == Release::Calm {
            m.quirks.air.since_pollination = 0;
        }
        if let Release::Angry(by) = how {
            m.anger = 400 + m.below(380) as i32;
            // At the player who did it.
            if let Some(id) = by.and_then(|u| lock(&ctx.players).get(&u).map(|p| p.entity_id)) {
                m.revenge = Some((id, m.anger));
            }
        }
        m.dirty = true;
    }
    e
}

fn honey_level(ctx: &ServerContext, hive: Position) -> Option<i32> {
    let blocks = &ctx.data.blocks;
    let s = ctx.world.block(hive, blocks)?;
    blocks.property(s, "honey_level")?.parse().ok()
}

fn add_honey(ctx: &ServerContext, level: &mut WorldLevel, hive: Position, by: i32) {
    let Some(have) = honey_level(ctx, hive) else {
        return;
    };
    let blocks = &ctx.data.blocks;
    let Some(s) = ctx.world.block(hive, blocks) else {
        return;
    };
    let new = (have + by).min(5);
    if new != have {
        level.set_block(hive, ctx.shapes.with(s, "honey_level", &new.to_string()));
    }
}

/// Lets the bees out of a hive that was broken or robbed. `by` is the
/// player they will go for. A hive that is robbed with a lit campfire
/// within five blocks below keeps its bees.
pub fn empty(ctx: &ServerContext, hive: Position, by: Option<u128>) {
    let bees = inside(ctx, hive);
    if bees.is_empty() {
        return;
    }
    store(ctx, hive, &[]);
    for b in &bees {
        let e = release(ctx, hive, b, Release::Angry(by));
        let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
        lock(&ctx.entities).push(e);
    }
}

/// Whether a lit campfire sits within five blocks below the hive.
pub fn smoked(ctx: &ServerContext, hive: Position) -> bool {
    let blocks = &ctx.data.blocks;
    for dy in 1..=5 {
        for dx in -5..=5 {
            for dz in -5..=5 {
                let p = Position::new(hive.x + dx, hive.y - dy, hive.z + dz);
                let Some(s) = ctx.world.block(p, blocks) else {
                    continue;
                };
                if blocks
                    .block_name(s)
                    .is_some_and(crate::campfires::is_campfire)
                    && blocks.property(s, "lit") == Some("true")
                {
                    return true;
                }
            }
        }
    }
    false
}

/// A hive was broken (no silk touch) or its honey taken: the bees come out
/// angry unless a campfire smokes them.
pub fn disturbed(ctx: &ServerContext, hive: Position, by: Option<u128>, robbed: bool) {
    if robbed && smoked(ctx, hive) {
        return;
    }
    empty(ctx, hive, by);
}

/// One tick of the bees inside every hive in memory: they wait, and come
/// out by day.
pub fn hives(ctx: &ServerContext, level: &mut WorldLevel, home: bool) {
    let all: Vec<[i32; 3]> = {
        let poi = lock(&ctx.poi);
        let mut v = poi.of_kind([0, 0, 0], crate::poi::BEEHIVE);
        v.extend(poi.of_kind([0, 0, 0], crate::poi::BEE_NEST));
        v
    };
    for c in all {
        let p = Position::new(c[0], c[1], c[2]);
        let mut bees = inside(ctx, p);
        if bees.is_empty() {
            continue;
        }
        for b in &mut bees {
            b.ticks += 1;
        }
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for b in bees {
            if !home && b.ticks >= b.min_ticks {
                out.push(b);
            } else {
                keep.push(b);
            }
        }
        store(ctx, p, &keep);
        for b in out {
            let nectar = matches!(b.data.get("HasNectar"), Some(Tag::Byte(1)));
            let mut b = b;
            if nectar {
                let extra = if crate::hits::rnd() < 0.01 { 2 } else { 1 };
                add_honey(ctx, level, p, extra);
                b.data.insert("HasNectar", Tag::Byte(0));
            }
            let e = release(ctx, p, &b, Release::Calm);
            fx::sound(
                ctx,
                "block.beehive.exit",
                fx::category::BLOCKS,
                center(p),
                1.0,
                1.0,
            );
            let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
            lock(&ctx.entities).push(e);
        }
    }
}

fn flower_at(ctx: &ServerContext, p: Position, flowers: &[String]) -> bool {
    name_at(ctx, p).is_some_and(|n| flowers.iter().any(|f| f == n))
}

/// The age a crop reaches when grown, and the property that holds it.
fn crop_stage(name: &str) -> Option<(&'static str, i32)> {
    Some(match name {
        "minecraft:wheat" | "minecraft:carrots" | "minecraft:potatoes" => ("age", 7),
        "minecraft:beetroots" | "minecraft:sweet_berry_bush" => ("age", 3),
        "minecraft:melon_stem" | "minecraft:pumpkin_stem" => ("age", 7),
        "minecraft:torchflower_crop" => ("age", 1),
        _ => return None,
    })
}

/// Grows a crop by one stage; whether it did.
fn grow(ctx: &ServerContext, level: &mut WorldLevel, p: Position) -> bool {
    let blocks = &ctx.data.blocks;
    let Some(s) = ctx.world.block(p, blocks) else {
        return false;
    };
    let Some(name) = blocks.block_name(s) else {
        return false;
    };
    if name == "minecraft:cave_vines" || name == "minecraft:cave_vines_plant" {
        if blocks.property(s, "berries") == Some("false") {
            level.set_block(p, ctx.shapes.with(s, "berries", "true"));
            return true;
        }
        return false;
    }
    let Some((prop, max)) = crop_stage(name) else {
        return false;
    };
    let age: i32 = blocks
        .property(s, prop)
        .and_then(|a| a.parse().ok())
        .unwrap_or(max);
    if age >= max {
        return false;
    }
    level.set_block(p, ctx.shapes.with(s, prop, &(age + 1).to_string()));
    true
}

/// What the bees do among the blocks each tick: look for a flower and
/// circle it, take nectar home, grow crops under them, go in.
pub fn bees(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut [Entity],
    now: i64,
    home: bool,
) {
    let flowers = ctx
        .commands
        .env
        .tag("minecraft:block", "minecraft:flowers")
        .to_vec();
    for i in 0..entities.len() {
        let pos = entities[i].pos;
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        if m.species != Species::Bee || m.dying.is_some() || entities[i].removed {
            continue;
        }
        let busy = m.target.is_some() || m.anger > 0 || m.leash.is_some();
        let air = &mut m.quirks.air;
        air.cannot_enter = (air.cannot_enter - 1).max(0);
        air.since_pollination += 1;
        air.errand = None;
        if busy || air.stung {
            continue;
        }
        let cell = [
            pos[0].floor() as i32,
            pos[1].floor() as i32,
            pos[2].floor() as i32,
        ];
        // A hive that is gone is forgotten.
        if let Some(h) = air.hive
            && !name_at(ctx, Position::new(h[0], h[1], h[2])).is_some_and(is_hive)
        {
            air.hive = None;
        }
        let wants_home =
            air.cannot_enter == 0 && (home || air.nectar || air.since_pollination > 3600);
        if air.hive.is_none() && (wants_home || m.below(200) == 0) {
            let found = lock(&ctx.poi)
                .near(cell, 20, 20, |r| crate::poi::is_hive_kind(r.kind))
                .into_iter()
                .map(|(p, _)| p)
                .find(|p| inside(ctx, Position::new(p[0], p[1], p[2])).len() < CAPACITY);
            let Kind::Mob(m) = &mut entities[i].kind else {
                continue;
            };
            m.quirks.air.hive = found;
        }
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        let air = &mut m.quirks.air;
        if wants_home && let Some(h) = air.hive {
            let hp = Position::new(h[0], h[1], h[2]);
            let target = center(hp);
            if (pos[0] - target[0]).powi(2)
                + (pos[1] - target[1]).powi(2)
                + (pos[2] - target[2]).powi(2)
                <= 4.0
            {
                enter(ctx, entities, i, hp);
                continue;
            }
            // Out the front, so it does not push through the walls.
            let f = front(ctx, hp);
            air.errand = Some(if (f.x - cell[0]).abs() + (f.z - cell[2]).abs() > 3 {
                [
                    f64::from(f.x) + 0.5,
                    f64::from(f.y) + 0.5,
                    f64::from(f.z) + 0.5,
                ]
            } else {
                target
            });
            continue;
        }
        // Crops under a bee that carries nectar.
        if air.nectar && air.crops < 10 && crate::hits::rnd() < 0.05 {
            let depth = 1 + (m.below(2) as i32);
            let p = Position::new(cell[0], cell[1] - depth, cell[2]);
            if grow(ctx, level, p) {
                let Kind::Mob(m) = &mut entities[i].kind else {
                    continue;
                };
                m.quirks.air.crops += 1;
                fx::level_event(ctx, 2005, center(p), 0);
            }
            let Kind::Mob(m) = &mut entities[i].kind else {
                continue;
            };
            m.quirks.air.errand = None;
            continue;
        }
        if air.nectar {
            continue;
        }
        // The flower it works.
        if let Some(f) = air.flower
            && !flower_at(ctx, Position::new(f[0], f[1], f[2]), &flowers)
        {
            air.flower = None;
            air.circling = 0;
        }
        if air.flower.is_none() && now % 20 == i as i64 % 20 && !home {
            let mut best = None;
            'search: for dx in -5..=5 {
                for dz in -5..=5 {
                    for dy in -3..=3 {
                        let p = Position::new(cell[0] + dx, cell[1] + dy, cell[2] + dz);
                        if flower_at(ctx, p, &flowers) {
                            best = Some([p.x, p.y, p.z]);
                            break 'search;
                        }
                    }
                }
            }
            let Kind::Mob(m) = &mut entities[i].kind else {
                continue;
            };
            m.quirks.air.flower = best;
            continue;
        }
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        let air = &mut m.quirks.air;
        let Some(f) = air.flower else { continue };
        let target = [
            f64::from(f[0]) + 0.5,
            f64::from(f[1]) + 0.9,
            f64::from(f[2]) + 0.5,
        ];
        let near = (pos[0] - target[0]).powi(2)
            + (pos[1] - target[1]).powi(2)
            + (pos[2] - target[2]).powi(2)
            <= 2.25;
        if near {
            air.circling += 1;
            if air.circling >= 400 {
                air.nectar = true;
                air.since_pollination = 0;
                air.flower = None;
                air.circling = 0;
                m.dirty = true;
                fx::level_event(ctx, 2005, pos, 0);
            }
        }
        // Drift around the flower rather than sit on it.
        let t = f64::from(air.circling) / 8.0 + i as f64;
        air.errand = Some([
            target[0] + t.cos() * 0.7,
            target[1],
            target[2] + t.sin() * 0.7,
        ]);
    }
}
