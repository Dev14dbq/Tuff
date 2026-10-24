//! Right clicks on blocks that change the block or the held item, besides
//! windows and redstone parts: cauldrons, cakes, candles, flower pots,
//! bookshelves, lecterns, signs, plants and the tools used on blocks
//! (minecraft.wiki pages of each).
//!
//! A click goes through two steps, as in the game: first the clicked block
//! may react to the held item or to an empty hand ([`use_block`]; skipped
//! while sneaking with something in hand), then the held item may act on
//! the block ([`use_item_on`]).

mod cauldron;
mod plants;
mod simple;
mod tools;

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use crate::context::{Event, ServerContext};
use crate::fx;
use crate::held::Held;
use crate::interact::{Actor, Outcome};
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

const CREATIVE: i32 = 1;

/// How many pages a written book or book and quill has.
pub fn book_pages(stack: &mc_protocol::packets::play::ItemStack) -> usize {
    ["written_book_content", "writable_book_content"]
        .iter()
        .find_map(|k| match stack.components.get(k) {
            Some(Tag::Compound(c)) => match c.get("pages") {
                Some(Tag::List(l)) => Some(l.len()),
                _ => Some(0),
            },
            _ => None,
        })
        .unwrap_or(0)
}

/// What a comparator reads from a block entity: the slot of a chiseled
/// bookshelf used last (plus one), how far a lectern's book is open, the
/// song in a jukebox.
pub fn comparator_signal(ctx: &ServerContext, p: Position) -> Option<u8> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    let name = blocks.block_name(state)?;
    if name == "minecraft:decorated_pot" {
        return Some(crate::pots::signal(ctx, p));
    }
    if !matches!(
        name,
        "minecraft:chiseled_bookshelf" | "minecraft:lectern" | "minecraft:jukebox"
    ) {
        return None;
    }
    let be = ctx.world.block_entity(p, blocks);
    let data = be.as_ref().map(|be| &be.data);
    let int = |key: &str| match data.and_then(|d| d.get(key)) {
        Some(Tag::Int(v)) => Some(*v),
        _ => None,
    };
    Some(match name {
        "minecraft:chiseled_bookshelf" => (int("last_interacted_slot").unwrap_or(-1) + 1) as u8,
        "minecraft:lectern" => {
            let Some(Tag::Compound(book)) = data.and_then(|d| d.get("Book")) else {
                return Some(0);
            };
            let pages = match book.get("components") {
                Some(Tag::Compound(c)) => [
                    "minecraft:written_book_content",
                    "minecraft:writable_book_content",
                ]
                .iter()
                .find_map(|k| match c.get(k) {
                    Some(Tag::Compound(content)) => match content.get("pages") {
                        Some(Tag::List(l)) => Some(l.len()),
                        _ => None,
                    },
                    _ => None,
                })
                .unwrap_or(0),
                _ => 0,
            };
            let page = int("Page").unwrap_or(0).max(0) as f64;
            let share = if pages > 1 {
                page / (pages - 1) as f64
            } else {
                1.0
            };
            (share * 14.0).floor() as u8 + 1
        }
        _ => {
            let Some(Tag::Compound(record)) = data.and_then(|d| d.get("RecordItem")) else {
                return Some(0);
            };
            let Some(Tag::String(id)) = record.get("id") else {
                return Some(0);
            };
            crate::jukeboxes::song_of(ctx, id)?.comparator
        }
    })
}

/// One click: who, with what, on which block.
pub(crate) struct Click<'a> {
    pub ctx: &'a ServerContext,
    pub actor: Actor,
    pub hand: i32,
    pub at: Position,
    pub state: u32,
    /// The clicked block's name, `minecraft:` included.
    pub name: &'a str,
    pub held: Option<Held>,
    /// The clicked face, 0 down .. 5 east.
    pub face: usize,
    /// Where on the face, 0..1 within the block.
    pub cursor: [f32; 3],
}

impl Click<'_> {
    fn held_name(&self) -> &str {
        self.held.as_ref().map_or("", |h| h.name.as_str())
    }

    fn creative(&self) -> bool {
        self.actor.game_mode == CREATIVE
    }

    fn prop(&self, key: &str) -> Option<&str> {
        self.ctx.data.blocks.property(self.state, key)
    }

    fn int(&self, key: &str) -> i32 {
        self.prop(key).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn with(&self, key: &str, value: &str) -> u32 {
        self.ctx.shapes.with(self.state, key, value)
    }

    fn centre(&self) -> [f64; 3] {
        centre(self.at)
    }

    /// Plays a block sound at the clicked block.
    fn sound(&self, name: &str) {
        fx::sound(
            self.ctx,
            name,
            fx::category::BLOCKS,
            self.centre(),
            1.0,
            1.0,
        );
    }

    /// The held item's components, as a compound.
    fn comps(&self) -> Option<&Compound> {
        match self.held.as_ref()?.comps.as_ref()? {
            Tag::Compound(c) => Some(c),
            _ => None,
        }
    }
}

pub(crate) fn centre(p: Position) -> [f64; 3] {
    [
        f64::from(p.x) + 0.5,
        f64::from(p.y) + 0.5,
        f64::from(p.z) + 0.5,
    ]
}

/// The state of a block by name, with these properties.
pub(crate) fn state_of(ctx: &ServerContext, name: &str, props: &[(&str, &str)]) -> Option<u32> {
    ctx.data.blocks.state_id(name, props)
}

/// Sets a block with all its updates; changes go to the player and to
/// everyone.
pub(crate) fn set(ctx: &ServerContext, outcome: &mut Outcome, p: Position, state: u32) {
    let mut level = WorldLevel::new(ctx);
    let logic = level.logic();
    mc_tick::set_block(&mut level, logic, to_pos(p), state, mc_tick::Flags::ALL);
    let changes = level.into_changes();
    outcome.updates.extend(changes.iter().copied());
    outcome.events.push(Event::BlocksChanged { by: 0, changes });
}

/// Takes `n` of the held stack (not in creative).
fn use_up(c: &Click, n: i32) {
    if let Some(h) = &c.held
        && !c.creative()
    {
        crate::held::consume(c.ctx, c.actor.uuid, h.slot, n);
    }
}

/// Wears the held tool by one use (not in creative).
fn wear(c: &Click) {
    if let Some(h) = &c.held
        && !c.creative()
    {
        crate::held::wear(c.ctx, c.actor.uuid, h.slot, 1);
    }
}

/// Puts a stack (with its components) into the player's inventory; what
/// does not fit drops at the clicked block.
fn give(c: &Click, outcome: &mut Outcome, item: i32, count: i32, comps: Option<Compound>) {
    let ctx = c.ctx;
    let stack = crate::items::stack_with(item, count, comps.as_ref());
    let left = {
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(&c.actor.uuid) else {
            return;
        };
        let (taken, slots) = hands.insert_stack(&stack, ctx.items.max_stack(item));
        for &slot in &slots {
            outcome.events.push(Event::InventorySlot {
                uuid: c.actor.uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
        count - taken
    };
    if left > 0 {
        outcome.drops.push((
            c.at,
            crate::loot::Drop {
                item,
                count: left,
                components: comps.unwrap_or_default(),
            },
        ));
    }
}

/// The held item is swapped for `result` (a bucket for a filled one...):
/// one of the held stack goes, and `result` takes its place or goes into the
/// inventory. In creative the held stack stays, and `result` is only given
/// if the inventory has none.
fn exchange(c: &Click, outcome: &mut Outcome, result: &str, comps: Option<Compound>) {
    let ctx = c.ctx;
    let Some(item) = ctx.items.id(result) else {
        return;
    };
    let Some(h) = &c.held else {
        return;
    };
    if c.creative() {
        let has = lock(&ctx.inventories)
            .get(&c.actor.uuid)
            .is_some_and(|hands| hands.slots.iter().flatten().any(|s| s.item == item));
        if !has {
            give(c, outcome, item, 1, comps);
        }
        return;
    }
    if h.stack.count > 1 {
        crate::held::consume(ctx, c.actor.uuid, h.slot, 1);
        give(c, outcome, item, 1, comps);
        return;
    }
    let mut inventories = lock(&ctx.inventories);
    let Some(hands) = inventories.get_mut(&c.actor.uuid) else {
        return;
    };
    hands.slots[h.slot] = Some(crate::items::stack_with(item, 1, comps.as_ref()));
    outcome.events.push(Event::InventorySlot {
        uuid: c.actor.uuid,
        slot: h.slot as i16,
        item: hands.slots[h.slot].clone(),
    });
}

/// The player's arm swings (the click did something).
fn swing(c: &Click, outcome: &mut Outcome) {
    outcome.events.push(Event::Swing {
        uuid: c.actor.uuid,
        entity_id: c.actor.entity_id,
        times: 1,
    });
}

fn click<'a>(
    ctx: &'a ServerContext,
    actor: Actor,
    hand: i32,
    at: Position,
    face: usize,
    cursor: [f32; 3],
) -> Option<Click<'a>> {
    let state = ctx.world.block(at, &ctx.data.blocks)?;
    let name = ctx.data.blocks.block_name(state)?;
    Some(Click {
        ctx,
        actor,
        hand,
        at,
        state,
        name,
        held: crate::held::held(ctx, actor.uuid, hand),
        face,
        cursor,
    })
}

/// The clicked block reacts to the held item or the empty hand. `true` if
/// it did (the click is over).
pub fn use_block(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    at: Position,
    face: usize,
    cursor: [f32; 3],
) -> bool {
    let Some(c) = click(ctx, actor, hand, at, face, cursor) else {
        return false;
    };
    let used = crate::pots::use_on(ctx, actor, hand, at)
        || cauldron::use_on(&c, outcome)
        || simple::use_on(&c, outcome)
        || plants::use_on(&c, outcome);
    if used {
        swing(&c, outcome);
    }
    used
}

/// The held item acts on the clicked block (axes strip logs, bone meal
/// grows plants...). `true` if it did.
pub fn use_item_on(
    ctx: &ServerContext,
    outcome: &mut Outcome,
    actor: Actor,
    hand: i32,
    at: Position,
    face: usize,
    cursor: [f32; 3],
) -> bool {
    let Some(c) = click(ctx, actor, hand, at, face, cursor) else {
        return false;
    };
    if c.held.is_none() {
        return false;
    }
    match crate::archaeology::brush(ctx, actor, hand, at, face) {
        crate::archaeology::Press::Start => {
            swing(&c, outcome);
            return true;
        }
        crate::archaeology::Press::Keep => return true,
        crate::archaeology::Press::No => {}
    }
    let used = tools::use_on(&c, outcome) || plants::item_on(&c, outcome);
    if used {
        swing(&c, outcome);
    }
    used
}

#[cfg(test)]
mod tests;
