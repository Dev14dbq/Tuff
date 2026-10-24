//! Blocks that react to a right click on their own (minecraft.wiki pages of
//! each): cakes are eaten and take a candle, candles go out, flower pots
//! take and give back a plant, chiseled bookshelves and lecterns hold
//! books, shears carve pumpkins and take honeycomb, signs are dyed, waxed
//! and edited, redstone ore glows, the dragon egg flees, campfires take
//! food.

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;

use super::{Click, exchange, give, set, state_of, use_up, wear};
use crate::context::Event;
use crate::interact::Outcome;
use crate::tick::lock;

pub fn use_on(c: &Click, outcome: &mut Outcome) -> bool {
    let n = c.name.trim_start_matches("minecraft:");
    match n {
        "cake" => cake(c, outcome),
        _ if n.ends_with("candle_cake") => candle_cake(c, outcome),
        _ if n.ends_with("candle") => {
            // An empty hand puts a lit candle out.
            if c.held.is_none() && c.prop("lit") == Some("true") {
                extinguish(c, outcome);
                true
            } else {
                false
            }
        }
        "campfire" | "soul_campfire" => {
            // Food goes on, one at a time.
            let Some(h) = &c.held else {
                return false;
            };
            if !crate::campfires::place(c.ctx, c.at, &h.stack) {
                return false;
            }
            use_up(c, 1);
            true
        }
        "flower_pot" => pot(c, outcome, true),
        _ if n.starts_with("potted_") => pot(c, outcome, false),
        "chiseled_bookshelf" => bookshelf(c, outcome),
        "lectern" => lectern(c, outcome),
        "pumpkin" if c.held_name() == "minecraft:shears" => carve(c, outcome),
        "beehive" | "bee_nest" => hive(c, outcome),
        "redstone_ore" | "deepslate_redstone_ore" => {
            if c.prop("lit") == Some("false") {
                set(c.ctx, outcome, c.at, c.with("lit", "true"));
            }
            // A block in hand is still placed against it.
            let placing = c
                .held
                .as_ref()
                .is_some_and(|h| c.ctx.rules.block_for_item(h.stack.item).is_some());
            !placing
        }
        "dragon_egg" => {
            dragon_egg(c, outcome);
            true
        }
        "daylight_detector" => {
            let inverted = c.prop("inverted") == Some("true");
            let new = c.with("inverted", if inverted { "false" } else { "true" });
            set(c.ctx, outcome, c.at, new);
            crate::daylight::update(c.ctx, c.at);
            true
        }
        _ if crate::interact::sign_entity(c.name).is_some() => sign(c, outcome),
        _ => false,
    }
}

/// Eats a slice of the cake at the click, if the player may: 2 food and
/// 0.4 saturation; the last slice takes the cake.
fn eat_slice(c: &Click, outcome: &mut Outcome, cake: u32) -> bool {
    let ctx = c.ctx;
    let hungry = lock(&ctx.vitals)
        .get(&c.actor.uuid)
        .is_some_and(|v| v.food < 20);
    if !(hungry || c.creative()) {
        return false;
    }
    let packet = {
        let mut all = lock(&ctx.vitals);
        all.get_mut(&c.actor.uuid).map(|v| {
            v.food = (v.food + 2).min(20);
            v.saturation = (v.saturation + 0.4).min(v.food as f32);
            v.packet()
        })
    };
    if let Some(health) = packet {
        outcome.events.push(Event::Health {
            uuid: c.actor.uuid,
            health,
        });
    }
    let bites: i32 = ctx
        .data
        .blocks
        .property(cake, "bites")
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    let new = if bites >= 6 {
        ctx.shapes.air()
    } else {
        ctx.shapes.with(cake, "bites", &(bites + 1).to_string())
    };
    set(ctx, outcome, c.at, new);
    true
}

fn cake(c: &Click, outcome: &mut Outcome) -> bool {
    let held = c.held_name();
    if held.ends_with("candle") && c.int("bites") == 0 {
        let Some(new) = state_of(c.ctx, &format!("{held}_cake"), &[]) else {
            return false;
        };
        set(c.ctx, outcome, c.at, new);
        use_up(c, 1);
        c.sound("block.cake.add_candle");
        return true;
    }
    eat_slice(c, outcome, c.state)
}

fn candle_cake(c: &Click, outcome: &mut Outcome) -> bool {
    let held = c.held_name();
    if matches!(held, "minecraft:flint_and_steel" | "minecraft:fire_charge") {
        return false;
    }
    let lit = c.prop("lit") == Some("true");
    // The candle sits on top: a click up there with an empty hand puts it
    // out.
    if c.held.is_none() && lit && c.cursor[1] > 0.5 {
        extinguish(c, outcome);
        return true;
    }
    let Some(cake) = state_of(c.ctx, "minecraft:cake", &[]) else {
        return false;
    };
    if !eat_slice(c, outcome, cake) {
        return false;
    }
    // The candle drops.
    let candle = c.name.trim_end_matches("_cake").to_owned();
    if let Some(item) = c.ctx.items.id(&candle) {
        outcome.drops.push((
            c.at,
            crate::loot::Drop {
                item,
                count: 1,
                components: Compound::new(),
            },
        ));
    }
    true
}

/// A candle (or candle cake) goes out with a puff.
fn extinguish(c: &Click, outcome: &mut Outcome) {
    set(c.ctx, outcome, c.at, c.with("lit", "false"));
    c.sound("block.candle.extinguish");
}

/// The potted block for an item, if it fits a pot.
fn potted_for(c: &Click, item: &str) -> Option<u32> {
    let plant = item.strip_prefix("minecraft:")?;
    let potted = match plant {
        "azalea" => "potted_azalea_bush".to_owned(),
        "flowering_azalea" => "potted_flowering_azalea_bush".to_owned(),
        _ => format!("potted_{plant}"),
    };
    state_of(c.ctx, &format!("minecraft:{potted}"), &[])
}

/// A plant goes into an empty pot; anything else in the main hand (or
/// nothing) takes the plant out of a full one.
fn pot(c: &Click, outcome: &mut Outcome, empty: bool) -> bool {
    if let Some(potted) = potted_for(c, c.held_name()) {
        if empty {
            set(c.ctx, outcome, c.at, potted);
            use_up(c, 1);
        }
        return true;
    }
    if c.hand != 0 {
        return false;
    }
    empty || unpot(c, outcome)
}

/// Takes the plant out of a pot into the hand.
fn unpot(c: &Click, outcome: &mut Outcome) -> bool {
    let Some(item) = crate::interact::clone_item(c.name, &c.ctx.items) else {
        return false;
    };
    let Some(empty) = state_of(c.ctx, "minecraft:flower_pot", &[]) else {
        return false;
    };
    give(c, outcome, item, 1, None);
    set(c.ctx, outcome, c.at, empty);
    true
}

const BOOKS: &[&str] = &[
    "minecraft:book",
    "minecraft:written_book",
    "minecraft:enchanted_book",
    "minecraft:writable_book",
    "minecraft:knowledge_book",
];

/// The bookshelf slot (0..6, top row first) under the cursor on its front.
fn shelf_slot(c: &Click) -> Option<usize> {
    let facing = c.prop("facing")?;
    let front = match facing {
        "north" => 2,
        "south" => 3,
        "west" => 4,
        "east" => 5,
        _ => return None,
    };
    if c.face != front {
        return None;
    }
    let [x, y, z] = c.cursor.map(f64::from);
    let across = match facing {
        "north" => 1.0 - x,
        "south" => x,
        "west" => z,
        _ => 1.0 - z,
    };
    let column = if across < 0.375 {
        0
    } else if across < 0.6875 {
        1
    } else {
        2
    };
    let row = if y >= 0.5 { 0 } else { 1 };
    Some(column + row * 3)
}

fn bookshelf(c: &Click, outcome: &mut Outcome) -> bool {
    let Some(slot) = shelf_slot(c) else {
        return false;
    };
    let key = format!("slot_{slot}_occupied");
    let occupied = c.prop(&key) == Some("true");
    let ctx = c.ctx;
    let blocks = &ctx.data.blocks;
    let mut be = ctx
        .world
        .block_entity(c.at, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: c.at.x,
            y: c.at.y,
            z: c.at.z,
            id: "minecraft:chiseled_bookshelf".into(),
            data: Compound::new(),
        });
    let mut items: Vec<Tag> = match be.data.get("Items") {
        Some(Tag::List(l)) => l.clone(),
        _ => Vec::new(),
    };
    let in_slot =
        |t: &Tag| matches!(t, Tag::Compound(e) if e.get("Slot") == Some(&Tag::Byte(slot as i8)));
    let held = c.held_name();
    if BOOKS.contains(&held) && !occupied {
        let mut e = Compound::new();
        e.insert("Slot", Tag::Byte(slot as i8));
        e.insert("id", Tag::String(held.to_owned()));
        e.insert("count", Tag::Int(1));
        if let Some(h) = &c.held
            && let Some(comps) = h.stack.components.to_nbt(h.stack.damage)
        {
            e.insert("components", Tag::Compound(comps));
        }
        items.retain(|t| !in_slot(t));
        items.push(Tag::Compound(e));
        use_up(c, 1);
        c.sound(if held == "minecraft:enchanted_book" {
            "block.chiseled_bookshelf.insert.enchanted"
        } else {
            "block.chiseled_bookshelf.insert"
        });
    } else if c.hand != 0 {
        return false;
    } else if !occupied {
        // An empty slot and no book: nothing happens.
        return true;
    } else {
        let Some(Tag::Compound(book)) = items.iter().find(|t| in_slot(t)).cloned() else {
            return false;
        };
        items.retain(|t| !in_slot(t));
        let id = match book.get("id") {
            Some(Tag::String(id)) => id.clone(),
            _ => String::new(),
        };
        if let Some(item) = ctx.items.id(&id) {
            let comps = match book.get("components") {
                Some(Tag::Compound(c)) => Some(c.clone()),
                _ => None,
            };
            give(c, outcome, item, 1, comps);
        }
        c.sound(if id == "minecraft:enchanted_book" {
            "block.chiseled_bookshelf.pickup.enchanted"
        } else {
            "block.chiseled_bookshelf.pickup"
        });
    }
    be.data.insert("Items", Tag::List(items));
    be.data
        .insert("last_interacted_slot", Tag::Int(slot as i32));
    ctx.world.set_block_entity(be, blocks);
    let new = c.with(&key, if occupied { "false" } else { "true" });
    set(ctx, outcome, c.at, new);
    true
}

/// A signed book on a lectern gets the text it holds resolved for whoever
/// opens it, as in hand.
fn resolve_book(c: &Click) {
    let ctx = c.ctx;
    let blocks = &ctx.data.blocks;
    let Some(mut be) = ctx.world.block_entity(c.at, blocks) else {
        return;
    };
    let Some(Tag::Compound(book)) = be.data.get("Book") else {
        return;
    };
    let Some(Tag::String(id)) = book.get("id") else {
        return;
    };
    let Some(item) = ctx.items.id(id) else {
        return;
    };
    let mut stack = mc_protocol::packets::play::ItemStack::from_nbt_with(book, item);
    if crate::books::resolve(ctx, c.actor.uuid, &mut stack) {
        let name = id.clone();
        be.data
            .insert("Book", Tag::Compound(stack.to_nbt_with(&name)));
        ctx.world.set_block_entity(be, blocks);
    }
}

fn lectern(c: &Click, outcome: &mut Outcome) -> bool {
    let held = c.held_name();
    let book = matches!(held, "minecraft:written_book" | "minecraft:writable_book");
    if c.prop("has_book") == Some("true") {
        // Reading: the lectern's window, on the main hand's click.
        if c.hand == 0 {
            resolve_book(c);
            outcome.open_menu = Some((c.at, crate::menus::Menu::Lectern));
        }
        return c.hand == 0;
    }
    if !book {
        return false;
    }
    let Some(h) = &c.held else { return false };
    let mut book = Compound::new();
    book.insert("id", Tag::String(held.to_owned()));
    book.insert("count", Tag::Int(1));
    if let Some(comps) = h.stack.components.to_nbt(h.stack.damage) {
        book.insert("components", Tag::Compound(comps));
    }
    let mut data = Compound::new();
    data.insert("Book", Tag::Compound(book));
    data.insert("Page", Tag::Int(0));
    c.ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: c.at.x,
            y: c.at.y,
            z: c.at.z,
            id: "minecraft:lectern".into(),
            data,
        },
        &c.ctx.data.blocks,
    );
    set(c.ctx, outcome, c.at, c.with("has_book", "true"));
    use_up(c, 1);
    c.sound("item.book.put");
    true
}

/// The horizontal way the player looks (`south`, `west`, `north`, `east`).
fn looking(yaw: f32) -> &'static str {
    let i = ((f64::from(yaw) / 90.0 + 0.5).floor() as i64).rem_euclid(4);
    ["south", "west", "north", "east"][i as usize]
}

fn face_name(face: usize) -> &'static str {
    ["down", "up", "north", "south", "west", "east"][face.min(5)]
}

fn opposite(dir: &str) -> &'static str {
    match dir {
        "north" => "south",
        "south" => "north",
        "west" => "east",
        _ => "west",
    }
}

/// Shears carve the clicked face of a pumpkin; four seeds fly out of it.
fn carve(c: &Click, outcome: &mut Outcome) -> bool {
    let side = match c.face {
        0 | 1 => opposite(looking(c.actor.pose.yaw)),
        f => face_name(f),
    };
    let Some(carved) = state_of(c.ctx, "minecraft:carved_pumpkin", &[("facing", side)]) else {
        return false;
    };
    c.sound("block.pumpkin.carve");
    set(c.ctx, outcome, c.at, carved);
    let (dx, dz) = match side {
        "north" => (0.0, -1.0),
        "south" => (0.0, 1.0),
        "west" => (-1.0, 0.0),
        _ => (1.0, 0.0),
    };
    if let Some(mut seeds) = crate::decor::dropped(
        c.ctx,
        [
            f64::from(c.at.x) + 0.5 + dx * 0.65,
            f64::from(c.at.y) + 0.1,
            f64::from(c.at.z) + 0.5 + dz * 0.65,
        ],
        "minecraft:pumpkin_seeds",
        4,
        0,
    ) {
        let r = || crate::util::random_u64() as f64 / u64::MAX as f64;
        seeds.vel = [0.05 * dx + r() * 0.02, 0.05, 0.05 * dz + r() * 0.02];
        crate::tick::spawn_entity(c.ctx, seeds);
    }
    wear(c);
    true
}

/// A full hive gives three honeycomb to shears or honey to a bottle.
fn hive(c: &Click, outcome: &mut Outcome) -> bool {
    if c.int("honey_level") < 5 {
        return false;
    }
    match c.held_name() {
        "minecraft:shears" => {
            c.sound("block.beehive.shear");
            if let Some(item) = c.ctx.items.id("minecraft:honeycomb") {
                outcome.drops.push((
                    c.at,
                    crate::loot::Drop {
                        item,
                        count: 3,
                        components: Compound::new(),
                    },
                ));
            }
            wear(c);
        }
        "minecraft:glass_bottle" => {
            c.sound("item.bottle.fill");
            exchange(c, outcome, "minecraft:honey_bottle", None);
        }
        _ => return false,
    }
    set(c.ctx, outcome, c.at, c.with("honey_level", "0"));
    true
}

/// The dragon egg jumps to a free spot up to 15 blocks away.
fn dragon_egg(c: &Click, outcome: &mut Outcome) {
    let ctx = c.ctx;
    let blocks = &ctx.data.blocks;
    let r = |n: u64| (crate::util::random_u64() % n) as i32;
    for _ in 0..1000 {
        let to = Position::new(
            c.at.x + r(16) - r(16),
            c.at.y + r(8) - r(8),
            c.at.z + r(16) - r(16),
        );
        if ctx
            .world
            .block(to, blocks)
            .is_some_and(|s| ctx.shapes.is_air(s))
        {
            set(ctx, outcome, to, c.state);
            set(ctx, outcome, c.at, ctx.shapes.air());
            return;
        }
    }
}

/// Which side of a sign the player stands on (`true` the front).
fn facing_front(c: &Click) -> bool {
    let rotation = if let Some(r) = c.prop("rotation").and_then(|r| r.parse::<f64>().ok()) {
        r * 22.5
    } else {
        match c.prop("facing") {
            Some("south") => 0.0,
            Some("west") => 90.0,
            Some("north") => 180.0,
            _ => 270.0,
        }
    };
    let dx = c.actor.pose.x - (f64::from(c.at.x) + 0.5);
    let dz = c.actor.pose.z - (f64::from(c.at.z) + 0.5);
    let toward = dz.atan2(dx).to_degrees() - 90.0;
    let diff = (rotation - toward).rem_euclid(360.0);
    diff.min(360.0 - diff) <= 90.0
}

/// Dye colours the text, glow ink makes it glow, ink sacs undo that,
/// honeycomb waxes the sign; anything else opens the editor.
fn sign(c: &Click, outcome: &mut Outcome) -> bool {
    let ctx = c.ctx;
    let blocks = &ctx.data.blocks;
    let Some(mut be) = ctx.world.block_entity(c.at, blocks) else {
        return false;
    };
    let front = facing_front(c);
    if matches!(be.data.get("is_waxed"), Some(Tag::Byte(1))) {
        c.sound("block.sign.waxed_interact_fail");
        return true;
    }
    let key = if front { "front_text" } else { "back_text" };
    let mut side = match be.data.get(key) {
        Some(Tag::Compound(s)) => s.clone(),
        _ => Compound::new(),
    };
    let held = c.held_name();
    let glowing = matches!(side.get("has_glowing_text"), Some(Tag::Byte(1)));
    let applied = if let Some(colour) = held
        .strip_prefix("minecraft:")
        .and_then(|h| h.strip_suffix("_dye"))
    {
        let same = matches!(side.get("color"), Some(Tag::String(s)) if s == colour);
        if same {
            false
        } else {
            side.insert("color", Tag::String(colour.to_owned()));
            c.sound("item.dye.use");
            true
        }
    } else {
        match held {
            "minecraft:glow_ink_sac" if !glowing => {
                side.insert("has_glowing_text", Tag::Byte(1));
                c.sound("item.glow_ink_sac.use");
                true
            }
            "minecraft:ink_sac" if glowing => {
                side.insert("has_glowing_text", Tag::Byte(0));
                c.sound("item.ink_sac.use");
                true
            }
            "minecraft:honeycomb" => {
                be.data.insert("is_waxed", Tag::Byte(1));
                crate::fx::level_event(ctx, 3003, super::centre(c.at), 0);
                true
            }
            _ => false,
        }
    };
    if applied {
        be.data.insert(key, Tag::Compound(side));
        let id = be.id.clone();
        let data = crate::world::client_data(&id, &be.data);
        ctx.world.set_block_entity(be, blocks);
        if let (Some(kind), Some(data)) = (ctx.world.block_entity_kind(&id), data) {
            outcome.events.push(Event::BlockEntity {
                position: c.at,
                kind,
                data,
            });
        }
        use_up(c, 1);
        return true;
    }
    outcome.open_sign = Some((c.at, front));
    true
}
