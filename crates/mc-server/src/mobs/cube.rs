//! Sulfur cubes in the world (minecraft.wiki, "Sulfur Cube"): a large one
//! swallows a full block that is used on it or lies by it, a small one
//! never does; shears take the block out again (and it does not swallow for
//! five seconds); a held block makes it take no harm from most things but
//! fly off from a blow; TNT in it can be lit and goes off in six seconds;
//! magma burns what touches it; a golden dandelion stops a small one from
//! growing.

use mc_entity::mob::cube;
use mc_entity::mob::slot;
use mc_entity::{Entity, Kind, Species};
use mc_protocol::packets::play::ItemStack;

use super::hurt::Harm;
use crate::context::{Event, ServerContext};
use crate::fx;

/// The archetype (index plus one) a block item gives, if the cube may
/// swallow it.
pub fn archetype_for(ctx: &ServerContext, name: &str) -> u8 {
    for (i, a) in cube::ARCHETYPES.iter().enumerate() {
        let tag = format!("minecraft:sulfur_cube_archetype/{}", a.name);
        if ctx
            .commands
            .env
            .tag("minecraft:item", &tag)
            .iter()
            .any(|n| n == name)
        {
            return i as u8 + 1;
        }
    }
    0
}

/// Harm a held block does not shield a cube from.
fn gets_through(kind: &str) -> bool {
    matches!(
        kind,
        "minecraft:on_fire"
            | "minecraft:in_fire"
            | "minecraft:lava"
            | "minecraft:hot_floor"
            | "minecraft:campfire"
            | "minecraft:in_wall"
            | "minecraft:cramming"
            | "minecraft:magic"
            | "minecraft:wither"
            | "minecraft:indirect_magic"
            | "minecraft:out_of_world"
            | "minecraft:generic_kill"
            | "minecraft:starve"
    )
}

/// A blow at a cube that holds a block: no harm, but it flies off. `Some`
/// when this was one (the answer for [`super::hurt::harm`]).
pub fn hit(ctx: &ServerContext, e: &mut Entity, h: &Harm) -> Option<bool> {
    let Kind::Mob(m) = &mut e.kind else {
        return None;
    };
    cube::of(m)?;
    if m.dying.is_some() || m.is_dead() || gets_through(h.kind) {
        return None;
    }
    let explosive = cube::of(m).is_some_and(|a| a.explosive);
    if explosive && (h.fire > 0 || h.kind.contains("fire")) && m.quirks.air.fuse < 0 {
        m.quirks.air.fuse = cube::FUSE;
        m.dirty = true;
    }
    let from = h.from.unwrap_or([e.pos[0] + 0.01, e.pos[1], e.pos[2]]);
    let mut vel = e.vel;
    cube::launch(m, &mut vel, e.pos, from, h.amount, h.knockback);
    e.vel = vel;
    e.on_ground = false;
    fx::sound(
        ctx,
        "entity.sulfur_cube.regular.hit",
        fx::category::NEUTRAL,
        e.pos,
        1.0,
        1.0,
    );
    Some(true)
}

/// The block a cube holds goes back into the world as an item.
fn release(ctx: &ServerContext, e: &mut Entity) -> bool {
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    let Some(g) = m.gear[slot::BODY].take() else {
        return false;
    };
    m.quirks.air.archetype = 0;
    m.quirks.air.fuse = -1;
    m.quirks.air.pickup_timer = 100;
    m.dirty = true;
    crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.5, pos[2]], g.item, 1);
    super::interact::send_equipment(ctx, e);
    true
}

/// Shears take the block out.
pub fn shear(ctx: &ServerContext, e: &mut Entity) -> bool {
    matches!(&e.kind, Kind::Mob(m) if m.species == Species::SulfurCube) && release(ctx, e)
}

/// A player used the held item on a cube: a block goes in (the old one
/// out), a flint and steel lights TNT, a golden dandelion keeps a small
/// one small. Whether the use was a cube's.
pub fn used(
    ctx: &ServerContext,
    e: &mut Entity,
    by: &super::Actor,
    name: &str,
    hslot: usize,
    stack: Option<&ItemStack>,
) -> bool {
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.species != Species::SulfurCube || m.dying.is_some() {
        return false;
    }
    let consume = || {
        if by.game_mode != 1 {
            crate::held::consume(ctx, by.uuid, hslot, 1);
        }
    };
    if name == "minecraft:golden_dandelion" && m.is_baby() {
        m.quirks.air.locked = !m.quirks.air.locked;
        m.dirty = true;
        consume();
        return true;
    }
    if name == "minecraft:flint_and_steel"
        && cube::of(m).is_some_and(|a| a.explosive)
        && m.quirks.air.fuse < 0
    {
        m.quirks.air.fuse = cube::FUSE;
        m.dirty = true;
        fx::sound(
            ctx,
            "item.flintandsteel.use",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        if by.game_mode != 1 {
            crate::held::wear(ctx, by.uuid, hslot, 1);
        }
        return true;
    }
    let kind = archetype_for(ctx, name);
    if kind == 0 || m.is_baby() {
        return false;
    }
    let Some(s) = stack else { return false };
    let old = m.gear[slot::BODY].replace(mc_entity::mob::Gear {
        item: s.item,
        count: 1,
        damage: 0,
        components: Default::default(),
    });
    m.drop_chance[slot::BODY] = 2.0;
    m.quirks.air.archetype = kind;
    m.quirks.air.fuse = -1;
    m.persistent = true;
    m.dirty = true;
    consume();
    if let Some(old) = old {
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.5, pos[2]], old.item, 1);
    }
    fx::sound(
        ctx,
        "entity.sulfur_cube.absorb",
        fx::category::NEUTRAL,
        pos,
        1.0,
        1.0,
    );
    super::interact::send_equipment(ctx, e);
    true
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Each tick: a cube whose block came from a file is told what it is, a
/// large one swallows a block that lies beside it, magma burns.
pub fn tick(ctx: &ServerContext, entities: &mut [Entity], targets: &[super::Target], now: i64) {
    let griefing = ctx.game_rule_bool("minecraft:mob_griefing");
    for i in 0..entities.len() {
        let pos = entities[i].pos;
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        if m.species != Species::SulfurCube || m.dying.is_some() || entities[i].removed {
            continue;
        }
        // A block loaded from disk: work its archetype out.
        if m.quirks.air.archetype == 0
            && let Some(g) = &m.gear[slot::BODY]
        {
            let name = ctx.items.name(g.item).unwrap_or("").to_owned();
            m.quirks.air.archetype = archetype_for(ctx, &name);
        }
        m.quirks.air.pickup_timer = (m.quirks.air.pickup_timer - 1).max(0);
        let hot = cube::of(m).is_some_and(|a| a.hot);
        let free =
            cube::of(m).is_none() && !m.is_baby() && m.quirks.air.pickup_timer == 0 && griefing;
        if hot && now % 10 == 0 {
            let id = entities[i].id;
            for t in targets {
                if (t.pos[0] - pos[0]).abs() < 1.2
                    && (t.pos[2] - pos[2]).abs() < 1.2
                    && t.pos[1] > pos[1] - 1.8
                    && t.pos[1] < pos[1] + 1.1
                {
                    let hit = crate::combat::Hit {
                        amount: 1.0,
                        kind: "minecraft:sulfur_cube_hot",
                        death_key: "death.attack.mob",
                        attacker: None,
                        by: Some(id),
                        from: Some(pos),
                        knockback: 0.0,
                        disables_shield: 0.0,
                    };
                    crate::combat::hurt_player(ctx, t.uuid, &hit);
                }
            }
            for (j, o) in entities.iter_mut().enumerate() {
                if j == i || o.removed {
                    continue;
                }
                let near = (o.pos[0] - pos[0]).abs() < 1.2
                    && (o.pos[2] - pos[2]).abs() < 1.2
                    && (o.pos[1] - pos[1]).abs() < 1.5;
                if near && matches!(&o.kind, Kind::Mob(t) if t.species != Species::SulfurCube) {
                    let harm = Harm {
                        by: Some(id),
                        ..Harm::new(1.0, "minecraft:sulfur_cube_hot")
                    };
                    super::hurt::harm(ctx, o, &harm);
                }
            }
        }
        if !free {
            continue;
        }
        // A block on the ground within a block of it.
        let found = entities.iter().enumerate().find(|(_, o)| {
            !o.removed
                && matches!(&o.kind, Kind::Item { item, pickup_delay: 0, .. }
                    if archetype_for(ctx, ctx.items.name(*item).unwrap_or("")) > 0)
                && dist_sq(o.pos, pos) < 1.3 * 1.3
        });
        let Some((j, _)) = found else { continue };
        let Kind::Item {
            item,
            count,
            thrower,
            ..
        } = &mut entities[j].kind
        else {
            continue;
        };
        let (item, thrower) = (*item, *thrower);
        *count -= 1;
        let gone = *count <= 0;
        let id = entities[j].id;
        if gone {
            entities[j].removed = true;
            let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
        }
        let name = ctx.items.name(item).unwrap_or("").to_owned();
        let kind = archetype_for(ctx, &name);
        if let Kind::Mob(m) = &mut entities[i].kind {
            m.gear[slot::BODY] = Some(mc_entity::mob::Gear {
                item,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
            m.drop_chance[slot::BODY] = 2.0;
            m.quirks.air.archetype = kind;
            m.persistent = true;
            m.dirty = true;
        }
        super::interact::send_equipment(ctx, &entities[i]);
        if let Some(by) = thrower {
            let picker = crate::criteria::entity(ctx, &entities[i]);
            let taken = mc_protocol::packets::play::ItemStack::new(item, 1);
            crate::criteria::entity_and_item(
                ctx,
                by,
                "minecraft:thrown_item_picked_up_by_entity",
                &picker,
                Some(&taken),
            );
        }
    }
}
