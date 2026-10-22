//! Experience orbs (minecraft.wiki, "Experience"): sizes, merging, being
//! drawn to players, pickup and Mending.

use mc_entity::{Entity, Kind};

use crate::context::{Direct, Event, Pose, ServerContext};
use crate::tick::lock;

/// Orbs live five minutes.
pub const LIFETIME: u32 = 6000;
/// Orbs notice players this far away and are drawn to them.
const ATTRACT_RANGE: f64 = 8.0;

/// The orb sizes, largest first: a total is split greedily into these.
const SIZES: [i32; 11] = [2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1];

/// The biggest orb value that fits into `amount`.
pub fn orb_value(amount: i32) -> i32 {
    SIZES.iter().copied().find(|&s| amount >= s).unwrap_or(1)
}

/// A fresh orb of one size at `pos`.
fn orb(ctx: &ServerContext, pos: [f64; 3], value: i32, vel: [f64; 3]) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::ExperienceOrb { value, count: 1 },
        pos,
        vel,
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Drops `amount` points as orbs around `pos`.
pub fn spawn(ctx: &ServerContext, pos: [f64; 3], amount: i32) {
    let r = || (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64 * 0.2 - 0.1;
    let mut left = amount;
    while left > 0 {
        let value = orb_value(left);
        left -= value;
        let entity = orb(ctx, pos, value, [r(), 0.2, r()]);
        let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
        lock(&ctx.entities).push(entity);
    }
}

/// A whole number of points from a fractional average: the remainder is a
/// chance of one more (smelting yields 0.7 per item, say).
pub fn roll_fraction(points: f32) -> i32 {
    let whole = points.floor();
    let chance = f64::from(points - whole);
    let roll = (crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64;
    whole as i32 + i32::from(roll < chance)
}

/// What mining `block` drops in experience points (a random amount in the
/// range the wiki gives), when not mined with Silk Touch.
pub fn block_xp(block: &str) -> i32 {
    let range = |lo: i32, hi: i32| lo + (crate::util::random_u64() % (hi - lo + 1) as u64) as i32;
    match block.trim_start_matches("minecraft:") {
        "coal_ore" | "deepslate_coal_ore" => range(0, 2),
        "nether_gold_ore" => range(0, 1),
        "diamond_ore" | "deepslate_diamond_ore" | "emerald_ore" | "deepslate_emerald_ore" => {
            range(3, 7)
        }
        "lapis_ore" | "deepslate_lapis_ore" | "nether_quartz_ore" => range(2, 5),
        "redstone_ore" | "deepslate_redstone_ore" => range(1, 5),
        "spawner" | "trial_spawner" => range(15, 43),
        "sculk" => 1,
        "sculk_sensor" | "calibrated_sculk_sensor" | "sculk_catalyst" | "sculk_shrieker" => 5,
        _ => 0,
    }
}

/// Whether a stack's components (as kept for the slot) carry Mending.
fn has_mending(components: Option<&mc_nbt::Tag>) -> bool {
    let Some(mc_nbt::Tag::Compound(c)) = components else {
        return false;
    };
    let Some(mc_nbt::Tag::Compound(e)) = c.get("minecraft:enchantments") else {
        return false;
    };
    // Either `{"minecraft:mending": 1}` or `{levels: {...}}`.
    e.get("minecraft:mending").is_some()
        || matches!(e.get("levels"), Some(mc_nbt::Tag::Compound(l)) if l.get("minecraft:mending").is_some())
}

/// Mending: one random damaged, mended item held or worn is repaired two
/// points per experience point. Returns the points left over.
fn mend(ctx: &ServerContext, uuid: u128, points: i32) -> i32 {
    let mut inventories = lock(&ctx.inventories);
    let Some(hands) = inventories.get_mut(&uuid) else {
        return points;
    };
    // Armour 5..9, the main hand and the off hand.
    let candidates: Vec<usize> = [5, 6, 7, 8, 36 + hands.selected, 45]
        .into_iter()
        .filter(|&s| {
            hands.slots[s].as_ref().is_some_and(|st| st.damage > 0)
                && has_mending(hands.components.get(&s))
        })
        .collect();
    if candidates.is_empty() {
        return points;
    }
    let slot = candidates[(crate::util::random_u64() % candidates.len() as u64) as usize];
    let Some(stack) = hands.slots[slot].as_mut() else {
        return points;
    };
    let repaired = (points * 2).min(stack.damage);
    stack.damage -= repaired;
    let item = hands.slots[slot].clone();
    let _ = ctx.events.send(Event::InventorySlot {
        uuid,
        slot: slot as i16,
        item,
    });
    points - repaired / 2
}

/// Gives a player experience points, mending first.
pub fn award(ctx: &ServerContext, uuid: u128, points: i32, mending: bool) {
    let left = if mending {
        mend(ctx, uuid, points)
    } else {
        points
    };
    let packet = {
        let mut all = lock(&ctx.vitals);
        all.get_mut(&uuid).map(|v| {
            v.add_xp_points(left);
            v.experience_packet()
        })
    };
    if let Some(packet) = packet {
        let _ = ctx.events.send(Event::Direct {
            uuid,
            packet: Direct::Experience(packet),
        });
    }
}

/// One tick of an orb after it fell: drawn toward the nearest player in
/// range, taken by one touching it, gone after its lifetime.
pub fn tick(ctx: &ServerContext, entity: &mut Entity, players: &[(u128, i32, i32, Pose)]) {
    let Kind::ExperienceOrb { value, count } = entity.kind else {
        return;
    };
    if entity.age >= LIFETIME {
        entity.removed = true;
        return;
    }
    let near = players
        .iter()
        .filter(|(_, _, mode, _)| *mode != 3)
        .map(|p| {
            let d = [
                p.3.x - entity.pos[0],
                p.3.y + 0.9 - entity.pos[1],
                p.3.z - entity.pos[2],
            ];
            (d, d[0] * d[0] + d[1] * d[1] + d[2] * d[2], p)
        })
        .filter(|(_, sq, _)| *sq < ATTRACT_RANGE * ATTRACT_RANGE)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let Some((d, sq, &(uuid, collector, _, pose))) = near else {
        return;
    };
    let dist = sq.sqrt().max(1e-6);
    let pull = (1.0 - dist / ATTRACT_RANGE).powi(2) * 0.1;
    for (v, d) in entity.vel.iter_mut().zip(d) {
        *v += d / dist * pull;
    }
    if crate::tick::picks_up(&pose, entity) {
        for _ in 0..count {
            award(ctx, uuid, value, true);
        }
        let _ = ctx.events.send(Event::ItemTaken {
            item_entity: entity.id,
            collector,
            count,
            slots: Vec::new(),
        });
        entity.removed = true;
    }
}

/// Merges orbs of one value that touch (their boxes grown by half a block).
pub fn merge(entities: &mut [Entity]) {
    let orbs: Vec<usize> = (0..entities.len())
        .filter(|&i| !entities[i].removed && matches!(entities[i].kind, Kind::ExperienceOrb { .. }))
        .collect();
    for (n, &a) in orbs.iter().enumerate() {
        for &b in &orbs[n + 1..] {
            if entities[a].removed || entities[b].removed {
                continue;
            }
            let (
                Kind::ExperienceOrb {
                    value: va,
                    count: ca,
                },
                Kind::ExperienceOrb {
                    value: vb,
                    count: cb,
                },
            ) = (entities[a].kind.clone(), entities[b].kind.clone())
            else {
                continue;
            };
            let near = (0..3).all(|k| (entities[a].pos[k] - entities[b].pos[k]).abs() < 0.5 + 0.5);
            if va == vb && near {
                entities[a].kind = Kind::ExperienceOrb {
                    value: va,
                    count: ca + cb,
                };
                entities[a].age = entities[a].age.min(entities[b].age);
                entities[b].removed = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_split_greedily() {
        assert_eq!(orb_value(1), 1);
        assert_eq!(orb_value(2), 1);
        assert_eq!(orb_value(3), 3);
        assert_eq!(orb_value(16), 7);
        assert_eq!(orb_value(5000), 2477);
    }

    #[test]
    fn orbs_of_one_value_merge() {
        let mk = |x: f64, value| Entity {
            id: 1,
            uuid: 1,
            kind: Kind::ExperienceOrb { value, count: 1 },
            pos: [x, 0.0, 0.0],
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        };
        let mut v = vec![mk(0.0, 3), mk(0.4, 3), mk(0.5, 7)];
        merge(&mut v);
        assert!(matches!(
            v[0].kind,
            Kind::ExperienceOrb { value: 3, count: 2 }
        ));
        assert!(v[1].removed && !v[2].removed);
    }

    #[test]
    fn awarded_points_raise_the_level() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Some(ctx) = mc_data::McData::load(&dir)
            .ok()
            .and_then(|d| ServerContext::new(crate::Config::new(dir), d).ok())
        else {
            eprintln!("skipped: no game data");
            return;
        };
        lock(&ctx.vitals).insert(5, crate::vitals::Vitals::default());
        // 7 points make level 1, 9 more make level 2 (it takes 9 there).
        award(&ctx, 5, 16, true);
        let v = lock(&ctx.vitals)[&5].clone();
        assert_eq!((v.xp_level, v.xp_total), (2, 16));
    }
}
