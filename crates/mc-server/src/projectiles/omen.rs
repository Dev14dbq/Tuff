//! Ominous item spawners (minecraft.wiki, "Ominous Item Spawner"): an
//! ominous trial spawner hangs one over a player or one of its mobs. It
//! shows its item for a while, then lets it go: projectiles are shot
//! straight down, other items dropped.

use mc_entity::extra::{Arrow, Pickup, Projectile, Shot};
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::ItemStack;

use crate::hits::Tick;

/// A new spawner at `pos`, letting `item` go after `after` ticks.
pub fn new(
    ctx: &crate::context::ServerContext,
    pos: [f64; 3],
    item: ItemStack,
    after: i32,
) -> Entity {
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Projectile(Projectile {
            shot: Shot::ItemSpawner { item, after },
            owner: None,
            mob: None,
            yaw: 0.0,
            pitch: 0.0,
        }),
        pos,
        vel: [0.0; 3],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

pub fn tick(t: &mut Tick, e: &mut Entity) {
    if e.removed {
        return;
    }
    let Kind::Projectile(p) = &e.kind else {
        return;
    };
    let Shot::ItemSpawner { item, after } = &p.shot else {
        return;
    };
    e.vel = [0.0; 3];
    e.age += 1;
    if (e.age as i32) < *after {
        return;
    }
    let item = item.clone();
    e.removed = true;
    let ctx = t.ctx;
    let name = ctx.items.name(item.item).unwrap_or("");
    let down = [0.0, -1.0, 0.0];
    let comps = crate::held::comps_of(&item);
    let shot = match name.trim_start_matches("minecraft:") {
        "arrow" | "tipped_arrow" | "spectral_arrow" => {
            let mut a = Arrow::new(item.item);
            a.spectral = name.ends_with("spectral_arrow");
            a.effects = super::potions::contents(comps.as_ref());
            a.pickup = Pickup::Yes;
            Some((Shot::Arrow(a), 1.1))
        }
        "splash_potion" | "lingering_potion" => {
            let effects = super::potions::contents(comps.as_ref());
            Some((
                Shot::Potion {
                    item: item.item,
                    lingering: name.ends_with("lingering_potion"),
                    color: super::potions::color(&effects),
                    effects,
                },
                1.1 * 1.25,
            ))
        }
        "fire_charge" => Some((Shot::Fireball { small: true }, 1.0)),
        "wind_charge" => Some((Shot::WindCharge { breeze: false }, 1.5)),
        _ => None,
    };
    match shot {
        Some((shot, speed)) => {
            super::shoot(ctx, shot, None, e.pos, down, speed, 0.0);
        }
        None => crate::tick::spawn_stack_at(ctx, e.pos, &item),
    }
    crate::fx::sound(
        ctx,
        "block.trial_spawner.spawn_item",
        crate::fx::category::BLOCKS,
        e.pos,
        1.0,
        0.8 + crate::hits::rnd() as f32 * 0.4,
    );
}
