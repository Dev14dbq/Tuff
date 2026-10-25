//! Chests and inventories of horses (minecraft.wiki, "Donkey", "Mule",
//! "Horse"): a chest used on a tamed donkey or mule puts it on its back (15
//! slots), and a sneaking use on a tamed horse of any kind - or the
//! inventory key while riding - opens its window: saddle, armor and the
//! chest. The window is a container of the server's keyed by the entity;
//! its first two slots mirror what the animal wears.

use mc_entity::mob::{Gear, slot};
use mc_entity::{Entity, Kind};
use mc_protocol::packets::play::{ItemStack, SetEquipment};

use crate::containers;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::interact::{Actor, Outcome};
use crate::tick::lock;

/// Columns of chest slots (three each) of donkeys and mules.
const COLUMNS: i32 = 5;

/// The window slots of the saddle and the armor.
const SADDLE: usize = 0;
const ARMOR: usize = 1;

/// Has a window of saddle and armor: horses of every kind (undead too), llamas and
/// ("Nautilus") nautiluses.
fn has_window(m: &mc_entity::mob::Mob) -> bool {
    use mc_entity::mob::Species;
    m.species.horse()
        || m.species.llama()
        || matches!(
            m.species,
            Species::SkeletonHorse
                | Species::ZombieHorse
                | Species::Nautilus
                | Species::ZombieNautilus
        )
}

fn gear_stack(g: &Option<Gear>) -> Option<ItemStack> {
    g.as_ref().map(|g| ItemStack {
        damage: g.damage,
        components: g.components.clone(),
        ..ItemStack::new(g.item, g.count)
    })
}

/// Opens the window of `e` for `actor`.
fn open(ctx: &ServerContext, e: &Entity, outcome: &mut Outcome) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    let columns = if !m.chested {
        0
    } else if m.species.llama() {
        m.strength.clamp(1, 5)
    } else {
        COLUMNS
    };
    // What it wears goes in the first two slots (an open window keeps its
    // own).
    if containers::peek_entity(ctx, e.id).is_none() {
        let mut slots = containers::read_items(
            ctx,
            &lock(&ctx.entity_items)
                .get(&e.id)
                .cloned()
                .unwrap_or_default(),
            2 + 3 * COLUMNS as usize,
        );
        slots[SADDLE] = gear_stack(&m.gear[slot::SADDLE]);
        slots[ARMOR] = gear_stack(&m.gear[slot::BODY]);
        containers::set_entity_slots(ctx, e.id, &slots);
    }
    outcome.open_mount = Some((e.id, columns));
}

/// A player used the held item on a mob. Whether this was about a chest or
/// an inventory (the click is then done).
pub fn use_on(
    ctx: &ServerContext,
    actor: &Actor,
    e: &mut Entity,
    hand: i32,
    attack: bool,
    outcome: &mut Outcome,
) -> bool {
    if attack {
        return false;
    }
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if !has_window(m) || m.dying.is_some() || m.is_baby() && !m.is_tame() {
        return false;
    }
    let held = crate::held::held(ctx, actor.uuid, hand);
    if let Some(h) = &held
        && h.name == "minecraft:chest"
        && m.species.chested()
        && m.is_tame()
        && !m.chested
    {
        m.chested = true;
        m.dirty = true;
        if actor.game_mode != 1 {
            crate::held::consume(ctx, actor.uuid, h.slot, 1);
        }
        let sound = format!(
            "entity.{}.chest",
            if m.species.llama() {
                "llama"
            } else {
                m.species.path()
            }
        );
        fx::sound(ctx, &sound, fx::category::NEUTRAL, pos, 1.0, 1.0);
        let _ = ctx.events.send(Event::Swing {
            uuid: actor.uuid,
            entity_id: actor.entity_id,
            times: 1,
        });
        return true;
    }
    if m.is_tame() && actor.sneaking && m.riders.is_empty() {
        open(ctx, e, outcome);
        return true;
    }
    false
}

/// The inventory key while riding: the mount's window opens.
pub fn open_riding(ctx: &ServerContext, actor: &Actor, outcome: &mut Outcome) {
    let entities = lock(&ctx.entities);
    if let Some(e) = entities.iter().find(|e| {
        matches!(&e.kind, Kind::Mob(m) if has_window(m) && m.is_tame() && m.riders.contains(&actor.entity_id))
    }) {
        open(ctx, e, outcome);
    }
}

/// Once a tick: what an open window holds in the saddle and armor slots is
/// what the animal wears.
pub fn mirror(ctx: &ServerContext, entities: &mut [Entity]) {
    for e in entities.iter_mut() {
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if !has_window(m) {
            continue;
        }
        if m.chested {
            sync_items(ctx, e.id, m);
        }
        let Some(slots) = containers::peek_entity(ctx, e.id) else {
            continue;
        };
        let mut changed = false;
        for (win, gear_slot) in [(SADDLE, slot::SADDLE), (ARMOR, slot::BODY)] {
            let now = slots.get(win).cloned().flatten();
            let was = gear_stack(&m.gear[gear_slot]);
            if now.as_ref().map(|s| (s.item, s.count, s.damage))
                != was.as_ref().map(|s| (s.item, s.count, s.damage))
            {
                m.gear[gear_slot] = now.map(|s| Gear {
                    item: s.item,
                    count: s.count,
                    damage: s.damage,
                    components: s.components,
                });
                changed = true;
            }
        }
        if changed {
            m.persistent = true;
            fx::emit(
                ctx,
                fx::Fx::Equip(SetEquipment {
                    entity_id: e.id,
                    slots: super::meta::equipment_of(m),
                }),
            );
        }
    }
}

/// Keeps the chest's saved list and the server's container for it the
/// same: a list read from NBT goes in once, afterwards the container is
/// what counts. Saved slots count from the chest's first (the window has the
/// saddle and armor ahead of it).
fn sync_items(ctx: &ServerContext, id: i32, m: &mut mc_entity::Mob) {
    let all = 2 + 3 * COLUMNS as usize;
    if !m.chest_loaded {
        m.chest_loaded = true;
        if let Some(mc_nbt::Tag::List(list)) = &m.chest_items {
            let mut data = mc_nbt::Compound::new();
            let shifted = list
                .iter()
                .filter_map(|t| match t {
                    mc_nbt::Tag::Compound(c) => {
                        let mut c = c.clone();
                        if let Some(mc_nbt::Tag::Byte(s)) = c.get("Slot") {
                            let s = *s + 2;
                            c.insert("Slot", mc_nbt::Tag::Byte(s));
                        }
                        Some(mc_nbt::Tag::Compound(c))
                    }
                    _ => None,
                })
                .collect();
            data.insert("Items", mc_nbt::Tag::List(shifted));
            lock(&ctx.entity_items).insert(id, data);
        }
    }
    let slots = containers::peek_entity(ctx, id).or_else(|| {
        let data = lock(&ctx.entity_items).get(&id).cloned()?;
        Some(containers::read_items(ctx, &data, all))
    });
    if let Some(mut slots) = slots {
        slots.drain(..2.min(slots.len()));
        let mut chest = containers::write_items(ctx, &slots);
        if let mc_nbt::Tag::List(list) = &mut chest {
            list.retain(|t| matches!(t, mc_nbt::Tag::Compound(_)));
        }
        m.chest_items = Some(chest);
    }
}

/// The animal died: what its chest held falls, and windows on it close.
pub fn spill(ctx: &ServerContext, id: i32, pos: [f64; 3]) {
    // Saddle and armor drop through the gear's own chances.
    let (items, viewers) = containers::entity_gone_skipping(ctx, id, 2);
    for stack in items {
        if let Some(name) = ctx.items.name(stack.item) {
            let name = name.to_owned();
            if let Some(d) = crate::decor::dropped(ctx, pos, &name, stack.count, stack.damage) {
                crate::decor::spawn_all(ctx, vec![d]);
            }
        }
    }
    // The chest itself.
    if let Some(d) = crate::decor::dropped(ctx, pos, "minecraft:chest", 1, 0) {
        crate::decor::spawn_all(ctx, vec![d]);
    }
    if !viewers.is_empty() {
        let _ = ctx.events.send(Event::ContainerGone {
            position: containers::entity_key(id),
        });
    }
}
