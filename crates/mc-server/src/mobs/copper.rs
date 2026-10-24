//! Copper golems (minecraft.wiki, "Copper Golem"): a copper block with a
//! carved pumpkin on top makes one (and the block a copper chest of the
//! same weathering). It empties the nearest copper chest 16 items at a time
//! and carries them to the nearest wooden chest that is empty or holds that
//! item, looking at each chest for three seconds. It remembers nine chests
//! that would not do; after ten failures it wanders for seven seconds and
//! forgets them. It weathers one stage every 420 to 460 minutes (honeycomb
//! stops it, an axe scrapes it back), and a golem that has fully weathered
//! turns into a statue.

use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use mc_entity::{Entity, Kind, Species};

use crate::containers;
use crate::context::ServerContext;
use crate::fx;

/// How far a golem looks for chests: 32 sideways, 8 up and down.
const RANGE: (f64, f64) = (32.0, 8.0);
/// Ticks it looks at a chest.
const LOOK: i32 = 60;
/// What it takes from a copper chest at most.
const TAKES: i32 = 16;

fn center(c: [i32; 3]) -> [f64; 3] {
    [
        f64::from(c[0]) + 0.5,
        f64::from(c[1]) + 0.5,
        f64::from(c[2]) + 0.5,
    ]
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Chests of a block entity kind near `pos`, nearest first.
fn chests(ctx: &ServerContext, id: &str, pos: [f64; 3]) -> Vec<[i32; 3]> {
    let mut found: Vec<[i32; 3]> = ctx
        .world
        .block_entities_of(id)
        .into_iter()
        .map(|be| [be.x, be.y, be.z])
        .filter(|c| {
            let m = center(*c);
            (m[0] - pos[0]).abs() <= RANGE.0
                && (m[2] - pos[2]).abs() <= RANGE.0
                && (m[1] - pos[1]).abs() <= RANGE.1
        })
        .collect();
    found.sort_by(|a, b| dist_sq(center(*a), pos).total_cmp(&dist_sq(center(*b), pos)));
    found
}

fn kind_at(ctx: &ServerContext, c: [i32; 3]) -> Option<containers::Kind> {
    let p = Position::new(c[0], c[1], c[2]);
    ctx.world
        .block(p, &ctx.data.blocks)
        .and_then(|s| ctx.data.blocks.block_name(s))
        .and_then(containers::kind_of)
}

/// Takes up to 16 of the first item in the copper chest; what it took.
fn take(ctx: &ServerContext, c: [i32; 3]) -> Option<(i32, i32)> {
    let kind = kind_at(ctx, c)?;
    let p = Position::new(c[0], c[1], c[2]);
    let slots = containers::slots(ctx, p, kind);
    let (i, s) = slots
        .iter()
        .enumerate()
        .find_map(|(i, s)| s.as_ref().filter(|s| s.count > 0).map(|s| (i, s.clone())))?;
    let n = s.count.min(TAKES);
    let left = s.count - n;
    let rest = (left > 0).then(|| ItemStack {
        count: left,
        ..s.clone()
    });
    containers::put(ctx, p, kind, &[(i, rest)]);
    Some((s.item, n))
}

/// Whether the chest is empty or holds the item; and puts as much of the
/// stack in as there is room for. Returns what is left over, or `None`
/// when the chest would not do.
fn put(ctx: &ServerContext, c: [i32; 3], item: i32, count: i32) -> Option<i32> {
    let kind = kind_at(ctx, c)?;
    let p = Position::new(c[0], c[1], c[2]);
    let slots = containers::slots(ctx, p, kind);
    let has = slots
        .iter()
        .flatten()
        .any(|s| s.item == item && s.count > 0);
    let empty = slots.iter().flatten().all(|s| s.count <= 0);
    if !has && !empty {
        return None;
    }
    let max = ctx.items.max_stack(item);
    let mut left = count;
    let mut changes = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        if left <= 0 {
            break;
        }
        match s {
            Some(s) if s.item == item && s.count < max => {
                let add = left.min(max - s.count);
                changes.push((
                    i,
                    Some(ItemStack {
                        count: s.count + add,
                        ..s.clone()
                    }),
                ));
                left -= add;
            }
            None => {
                let add = left.min(max);
                changes.push((i, Some(ItemStack::new(item, add))));
                left -= add;
            }
            _ => {}
        }
    }
    containers::put(ctx, p, kind, &changes);
    Some(left)
}

/// One tick of every copper golem: its errands between the chests.
pub fn tick(
    ctx: &ServerContext,
    level: &mut crate::level::WorldLevel,
    entities: &mut [Entity],
    _now: i64,
) {
    for e in entities.iter_mut() {
        let pos = e.pos;
        let Kind::Mob(m) = &mut e.kind else { continue };
        if m.species != Species::CopperGolem || m.dying.is_some() || e.removed {
            continue;
        }
        // A weathered golem that stands still sets into a statue.
        if m.quirks.air.weather >= 3
            && m.quirks.air.next_weather != -2
            && m.quirks.air.wait == 0
            && crate::hits::rnd() < 1.0 / 1200.0
        {
            let facing = match ((m.yaw.rem_euclid(360.0) + 45.0) / 90.0) as i32 % 4 {
                0 => "south",
                1 => "west",
                2 => "north",
                _ => "east",
            };
            let carried = m.quirks.air.carried.take();
            let cell = Position::new(
                pos[0].floor() as i32,
                pos[1].floor() as i32,
                pos[2].floor() as i32,
            );
            if let Some(statue) = ctx
                .data
                .blocks
                .default_state("minecraft:copper_golem_statue")
            {
                let statue = ctx.shapes.with(statue, "facing", facing);
                level.set_block(cell, statue);
                if let Some((item, count)) = carried {
                    crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.6, pos[2]], item, count);
                }
                e.removed = true;
                let _ = ctx
                    .events
                    .send(crate::context::Event::EntitiesRemoved(vec![e.id]));
                continue;
            }
        }
        let air = &mut m.quirks.air;
        air.errand = None;
        // Looking at a chest, or wandering after too many failures.
        if air.wait > 0 {
            air.wait -= 1;
            if air.wait > 0 {
                continue;
            }
            let Some(c) = air.at_chest.take() else {
                continue;
            };
            match air.carried {
                None => match take(ctx, c) {
                    Some(got) => {
                        air.carried = Some(got);
                        air.state = 1;
                    }
                    None => air.state = 2,
                },
                Some((item, count)) => match put(ctx, c, item, count) {
                    Some(0) => {
                        air.carried = None;
                        air.checks = 0;
                        air.memory.clear();
                        air.state = 3;
                    }
                    Some(left) => {
                        air.carried = Some((item, left));
                        air.state = 4;
                        remember(air, c);
                    }
                    None => {
                        air.state = 4;
                        remember(air, c);
                    }
                },
            }
            m.dirty = true;
            if m.quirks.air.checks >= 10 {
                m.quirks.air.wait = 140;
                m.quirks.air.memory.clear();
                m.quirks.air.checks = 0;
            }
            continue;
        }
        if air.state != 0 {
            air.state = 0;
            m.dirty = true;
        }
        let air = &mut m.quirks.air;
        let target = match air.carried {
            None => chests(ctx, "minecraft:copper_chest", pos)
                .into_iter()
                .next(),
            Some(_) => {
                let mut all = chests(ctx, "minecraft:chest", pos);
                all.extend(chests(ctx, "minecraft:trapped_chest", pos));
                all.sort_by(|a, b| dist_sq(center(*a), pos).total_cmp(&dist_sq(center(*b), pos)));
                all.into_iter().find(|c| !air.memory.contains(c))
            }
        };
        let Some(c) = target else { continue };
        let at = center(c);
        let near = (at[0] - pos[0]).abs() <= 2.0
            && (at[2] - pos[2]).abs() <= 2.0
            && (at[1] - pos[1]).abs() <= 2.0;
        if near {
            air.at_chest = Some(c);
            air.wait = LOOK;
            fx::sound(ctx, "block.chest.open", fx::category::BLOCKS, at, 0.5, 1.0);
        } else {
            air.errand = Some(at);
        }
    }
}

fn remember(air: &mut mc_entity::mob::air::Air, c: [i32; 3]) {
    if !air.memory.contains(&c) {
        air.memory.push(c);
        if air.memory.len() > 9 {
            air.memory.remove(0);
        }
    }
    air.checks += 1;
}
