//! Wolf armor (minecraft.wiki, "Wolf Armor"): the owner puts it on a grown
//! tamed wolf, and it takes the damage the wolf would, a point of
//! durability for each point, until it breaks. It cracks as its durability
//! falls to 60, 44 and 20; an armadillo scute mends 8; the owner's shears
//! take it off (not a Curse of Binding one, but in creative).

use mc_entity::mob::{Gear, Species, slot};
use mc_entity::{Entity, Kind, Mob};
use mc_protocol::packets::play::ItemStack;

use super::Actor;
use crate::context::ServerContext;
use crate::fx;

pub const ARMOR: &str = "minecraft:wolf_armor";

/// Damage that goes past the armor to the wolf.
const BYPASSES: [&str; 11] = [
    "minecraft:drown",
    "minecraft:freeze",
    "minecraft:in_wall",
    "minecraft:magic",
    "minecraft:indirect_magic",
    "minecraft:thorns",
    "minecraft:wither",
    "minecraft:out_of_world",
    "minecraft:outside_border",
    "minecraft:cramming",
    "minecraft:generic_kill",
];

/// Durability left at which the armor cracks further.
const CRACKS: [i32; 3] = [60, 44, 20];

/// Mends per scute.
const SCUTE_MENDS: i32 = 8;

fn stack_of(g: &Gear) -> ItemStack {
    ItemStack {
        count: g.count,
        item: g.item,
        damage: g.damage,
        components: g.components.clone(),
    }
}

fn worn<'a>(ctx: &ServerContext, m: &'a Mob) -> Option<&'a Gear> {
    m.gear[slot::BODY]
        .as_ref()
        .filter(|g| ctx.items.name(g.item) == Some(ARMOR))
}

/// A player used `name` (in `slot`) on a wolf: armor on, a scute in, or
/// shears to take it off. Whether something happened; `taken` gets the
/// armor taken off.
pub fn used(
    ctx: &ServerContext,
    e: &mut Entity,
    by: &Actor,
    held: Option<&ItemStack>,
    name: &str,
    taken: &mut Option<ItemStack>,
) -> bool {
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.species != Species::Wolf || m.owner != Some(by.uuid) || m.dying.is_some() {
        return false;
    }
    match name {
        ARMOR if m.gear[slot::BODY].is_none() && !m.is_baby() => {
            let Some(stack) = held else {
                return false;
            };
            m.gear[slot::BODY] = Some(Gear {
                item: stack.item,
                count: 1,
                damage: stack.damage,
                components: stack.components.clone(),
            });
            // It always comes off a dead wolf.
            m.drop_chance[slot::BODY] = 2.0;
            m.persistent = true;
            fx::sound(
                ctx,
                "item.armor.equip_wolf",
                fx::category::NEUTRAL,
                pos,
                1.0,
                1.0,
            );
            true
        }
        "minecraft:armadillo_scute" => {
            let Some(g) = m.gear[slot::BODY].as_mut().filter(|g| g.damage > 0) else {
                return false;
            };
            if ctx.items.name(g.item) != Some(ARMOR) {
                return false;
            }
            g.damage = (g.damage - SCUTE_MENDS).max(0);
            fx::sound(
                ctx,
                "item.armor.equip_wolf",
                fx::category::NEUTRAL,
                pos,
                0.5,
                1.0,
            );
            true
        }
        "minecraft:shears" => {
            let Some(g) = worn(ctx, m) else {
                return false;
            };
            let bound = crate::enchants::levels(&stack_of(g))
                .iter()
                .any(|(n, _)| n == "minecraft:binding_curse");
            if bound && by.game_mode != 1 {
                return false;
            }
            let g = m.gear[slot::BODY].take().expect("worn");
            let stack = stack_of(&g);
            fx::sound(
                ctx,
                "item.armor.unequip_wolf",
                fx::category::NEUTRAL,
                pos,
                1.0,
                1.0,
            );
            crate::tick::spawn_stack_at(ctx, [pos[0], pos[1] + 0.5, pos[2]], &stack);
            *taken = Some(stack);
            true
        }
        _ => false,
    }
}

/// Damage of `kind` comes at a wolf: its armor, if it wears one and the
/// damage does not go past it, takes it all. Whether it did.
pub fn absorb(ctx: &ServerContext, m: &mut Mob, pos: [f64; 3], kind: &str, amount: f32) -> bool {
    if m.species != Species::Wolf || BYPASSES.contains(&kind) || amount <= 0.0 {
        return false;
    }
    let Some(g) = worn(ctx, m) else {
        return false;
    };
    let max = ctx.items.get(g.item).map_or(64, |i| i.max_damage);
    let unbreaking = crate::enchants::levels(&stack_of(g))
        .iter()
        .find(|(n, _)| n == "minecraft:unbreaking")
        .map_or(0, |(_, l)| *l);
    // Each point may be spared by Unbreaking, one in (level + 1) lands.
    let points = (0..amount.ceil() as i32)
        .filter(|_| unbreaking == 0 || m.below(unbreaking as u32 + 1) == 0)
        .count() as i32;
    let Some(g) = m.gear[slot::BODY].as_mut() else {
        return false;
    };
    let before = max - g.damage;
    g.damage += points;
    let left = max - g.damage;
    fx::sound(
        ctx,
        "item.wolf_armor.damage",
        fx::category::NEUTRAL,
        pos,
        1.0,
        1.0,
    );
    if left <= 0 {
        m.gear[slot::BODY] = None;
        fx::sound(
            ctx,
            "item.wolf_armor.break",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
    } else if CRACKS.iter().any(|&c| before > c && left <= c) {
        fx::sound(
            ctx,
            "item.wolf_armor.crack",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
    }
    m.dirty = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit;

    const ME: u128 = 0xd09;

    #[test]
    fn the_owners_wolf_wears_armor_that_takes_its_damage_until_shorn_off() {
        let Some(ctx) = testkit::flat() else { return };
        testkit::join(&ctx, ME, 41, [0.5, -60.0, 0.5], 0);
        crate::commands::load_progress(&ctx, ME, &[], &crate::players::BookState::default());
        let mut wolf = crate::mobs::new_mob(&ctx, Species::Wolf, [2.5, -60.0, 0.5]);
        let by = Actor {
            uuid: ME,
            entity_id: 41,
            name: "P".into(),
            pos: [0.5, -60.0, 0.5],
            game_mode: 0,
            sneaking: false,
        };
        let armor = testkit::stack(&ctx, "wolf_armor", &[]);
        let mut taken = None;
        // Not tamed by this player: no.
        assert!(!used(&ctx, &mut wolf, &by, Some(&armor), ARMOR, &mut taken));
        if let Kind::Mob(m) = &mut wolf.kind {
            m.owner = Some(ME);
        }
        assert!(used(&ctx, &mut wolf, &by, Some(&armor), ARMOR, &mut taken));
        let health = match &wolf.kind {
            Kind::Mob(m) => m.health,
            _ => 0.0,
        };
        let h = crate::mobs::hurt::Harm::new(5.0, "minecraft:mob_attack");
        crate::mobs::hurt::harm(&ctx, &mut wolf, &h);
        let Kind::Mob(m) = &mut wolf.kind else {
            unreachable!()
        };
        assert_eq!(m.health, health);
        assert_eq!(m.gear[slot::BODY].as_ref().map(|g| g.damage), Some(5));
        // Drowning goes past it.
        m.invulnerable = 0;
        let h = crate::mobs::hurt::Harm::new(2.0, "minecraft:drown");
        crate::mobs::hurt::harm(&ctx, &mut wolf, &h);
        let Kind::Mob(m) = &mut wolf.kind else {
            unreachable!()
        };
        assert_eq!(m.health, health - 2.0);
        // A scute mends 8 (down to none worn).
        let scute = testkit::stack(&ctx, "armadillo_scute", &[]);
        assert!(used(
            &ctx,
            &mut wolf,
            &by,
            Some(&scute),
            "minecraft:armadillo_scute",
            &mut taken
        ));
        let Kind::Mob(m) = &wolf.kind else {
            unreachable!()
        };
        assert_eq!(m.gear[slot::BODY].as_ref().map(|g| g.damage), Some(0));
        let shears = testkit::stack(&ctx, "shears", &[]);
        assert!(used(
            &ctx,
            &mut wolf,
            &by,
            Some(&shears),
            "minecraft:shears",
            &mut taken
        ));
        assert_eq!(taken.as_ref().map(|s| s.item), Some(armor.item));
        let Kind::Mob(m) = &wolf.kind else {
            unreachable!()
        };
        assert!(m.gear[slot::BODY].is_none());
    }

    #[test]
    fn armor_cracks_at_its_thresholds_and_breaks_at_the_end() {
        let Some(ctx) = testkit::flat() else { return };
        let mut wolf = crate::mobs::new_mob(&ctx, Species::Wolf, [2.5, -60.0, 0.5]);
        let Kind::Mob(m) = &mut wolf.kind else {
            unreachable!()
        };
        let item = ctx.items.id(ARMOR).unwrap();
        m.gear[slot::BODY] = Some(Gear {
            item,
            count: 1,
            damage: 60,
            components: Default::default(),
        });
        assert!(absorb(&ctx, m, wolf.pos, "minecraft:mob_attack", 3.0));
        assert_eq!(m.gear[slot::BODY].as_ref().map(|g| g.damage), Some(63));
        assert!(absorb(&ctx, m, wolf.pos, "minecraft:mob_attack", 3.0));
        assert!(m.gear[slot::BODY].is_none());
        assert!(!absorb(&ctx, m, wolf.pos, "minecraft:mob_attack", 3.0));
    }
}
