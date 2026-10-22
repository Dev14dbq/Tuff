//! A player's hands for the things they throw, shoot and place: what is
//! held, using up and wearing it, and handing things over.

use mc_nbt::Tag;
use mc_protocol::packets::play::ItemStack;

use crate::context::{Event, ServerContext};
use crate::tick::lock;

/// What a hand holds.
#[derive(Clone, Debug)]
pub struct Held {
    pub slot: usize,
    pub stack: ItemStack,
    /// `minecraft:bow`
    pub name: String,
    /// The stack's other components, as saved.
    pub comps: Option<Tag>,
}

/// A stack's components (damage aside) as saved.
pub fn comps_of(stack: &ItemStack) -> Option<Tag> {
    stack.components.to_nbt(0).map(Tag::Compound)
}

pub fn held(ctx: &ServerContext, uuid: u128, hand: i32) -> Option<Held> {
    let inventories = lock(&ctx.inventories);
    let hands = inventories.get(&uuid)?;
    let slot = hands.hand_slot(hand);
    let stack = hands.slots[slot].clone()?;
    Some(Held {
        slot,
        name: ctx.items.name(stack.item)?.to_owned(),
        comps: comps_of(&stack),
        stack,
    })
}

/// The slot of the first stack passing `want`, off hand first, then the
/// hotbar and the rest.
pub fn find(ctx: &ServerContext, uuid: u128, want: impl Fn(&str) -> bool) -> Option<Held> {
    let inventories = lock(&ctx.inventories);
    let hands = inventories.get(&uuid)?;
    let order = std::iter::once(45).chain(36..45).chain(9..36);
    for slot in order {
        let Some(stack) = &hands.slots[slot] else {
            continue;
        };
        let Some(name) = ctx.items.name(stack.item) else {
            continue;
        };
        if want(name) {
            return Some(Held {
                slot,
                name: name.to_owned(),
                comps: comps_of(stack),
                stack: stack.clone(),
            });
        }
    }
    None
}

fn changed(ctx: &ServerContext, uuid: u128, slots: &[usize]) {
    let inventories = lock(&ctx.inventories);
    let Some(hands) = inventories.get(&uuid) else {
        return;
    };
    for &slot in slots {
        let _ = ctx.events.send(Event::InventorySlot {
            uuid,
            slot: slot as i16,
            item: hands.slots[slot].clone(),
        });
    }
}

/// Takes `n` from a slot.
pub fn consume(ctx: &ServerContext, uuid: u128, slot: usize, n: i32) {
    {
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            return;
        };
        if let Some(stack) = &mut hands.slots[slot] {
            stack.count -= n;
            if stack.count <= 0 {
                hands.slots[slot] = None;
            }
        }
    }
    changed(ctx, uuid, &[slot]);
}

/// Wears the item in a slot by `amount`; it breaks at its limit.
pub fn wear(ctx: &ServerContext, uuid: u128, slot: usize, amount: i32) {
    let mut broke = false;
    {
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            return;
        };
        let Some(stack) = &mut hands.slots[slot] else {
            return;
        };
        let max = ctx.items.max_damage(stack.item);
        if max == 0 {
            return;
        }
        let amount = crate::enchant_effects::wear(ctx, stack, amount);
        if amount == 0 {
            return;
        }
        stack.damage += amount;
        if stack.damage >= max {
            hands.slots[slot] = None;
            broke = true;
        }
    }
    changed(ctx, uuid, &[slot]);
    if broke {
        let entity_id = lock(&ctx.players).get(&uuid).map(|p| p.entity_id);
        if let Some(entity_id) = entity_id {
            // Entity events 47..52: main hand, off hand, head, chest, legs, feet.
            let status = match slot {
                45 => 48,
                5..=8 => 44 + slot as i8,
                _ => 47,
            };
            let _ = ctx.events.send(Event::HandBroke {
                uuid,
                entity_id,
                status,
            });
        }
    }
}

/// Puts a stack in the inventory; what does not fit is returned.
pub fn give(ctx: &ServerContext, uuid: u128, stack: &ItemStack) -> i32 {
    let max = ctx.items.max_stack(stack.item);
    let (taken, slots) = {
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            return stack.count;
        };
        hands.insert_stack(stack, max)
    };
    changed(ctx, uuid, &slots);
    stack.count - taken
}

/// A level of an enchantment on a stack's components (`power`,
/// `minecraft:power`); 0 if it has none.
pub fn enchant(comps: Option<&Tag>, name: &str) -> i32 {
    let Some(Tag::Compound(c)) = comps else {
        return 0;
    };
    let Some(Tag::Compound(e)) = c.get("minecraft:enchantments") else {
        return 0;
    };
    // Written flat, or under `levels` as before 1.21.5.
    let levels = match e.get("levels") {
        Some(Tag::Compound(l)) => l,
        _ => e,
    };
    let full = format!("minecraft:{}", name.trim_start_matches("minecraft:"));
    match levels.get(&full) {
        Some(Tag::Int(v)) => *v,
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Byte(v)) => i32::from(*v),
        _ => 0,
    }
}
