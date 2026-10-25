//! What players do to mobs with what they hold (minecraft.wiki, the pages
//! of each mob, "Breeding", "Taming", "Name Tag", "Dye", "Saddle"), and the
//! items mobs pick up themselves.
//!
//! In order, a use on a mob:
//! - a name tag with a name names it (and it never despawns);
//! - shears take a sheep's wool; a dye colours a sheep or a pet's collar;
//! - a bucket milks a cow (a bowl a mooshroom);
//! - its food feeds it: a baby grows up a tenth faster, a grown-up that may
//!   breed falls in love; a tamed wolf eats to heal;
//! - the taming item tames it now and then (a wolf's bone one time in
//!   three, a cat's fish one in three, a parrot's seeds one in ten); horses
//!   are tamed by riding them;
//! - a saddle saddles a pig or a tamed horse; a horse armor armors it;
//! - an owner's empty hand makes a pet sit or stand;
//! - an empty hand gets on a saddled pig or a horse;
//! - flint and steel lights a creeper; gold makes a piglin look at it; a
//!   golden apple starts curing a zombie villager.

use mc_entity::mob::{Gear, slot};
use mc_entity::{Entity, Kind, Species};
use mc_protocol::packets::play::{self, SetEntityData};

use super::{Actor, WOOL_COLORS};
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::tick::lock;

/// Takes one of the held item (not in creative).
fn consume(ctx: &ServerContext, by: &Actor, slot: usize) {
    if by.game_mode != 1 {
        crate::held::consume(ctx, by.uuid, slot, 1);
    }
}

/// A goat's eat sound (a screamer has its own).
fn goat_eat(ctx: &ServerContext, m: &mc_entity::Mob, pos: [f64; 3]) {
    if m.species != Species::Goat {
        return;
    }
    let name = if m.quirks.land.screaming {
        "entity.goat.screaming.eat"
    } else {
        "entity.goat.eat"
    };
    fx::sound(ctx, name, fx::category::NEUTRAL, pos, 1.0, 1.0);
}

/// Swaps one of the held item for `into` (a bucket for milk).
fn swap_one(ctx: &ServerContext, by: &Actor, slot: usize, into: &str) {
    let Some(item) = ctx.items.id(into) else {
        return;
    };
    if by.game_mode != 1 {
        crate::held::consume(ctx, by.uuid, slot, 1);
    }
    let stack = play::ItemStack::new(item, 1);
    let left = crate::held::give(ctx, by.uuid, &stack);
    if left > 0 {
        crate::tick::spawn_item_at(ctx, by.pos, item, left);
    }
}

fn swing(ctx: &ServerContext, by: &Actor) {
    let _ = ctx.events.send(Event::Swing {
        uuid: by.uuid,
        entity_id: by.entity_id,
        times: 1,
    });
}

fn dye_color(name: &str) -> Option<u8> {
    let color = name.strip_prefix("minecraft:")?.strip_suffix("_dye")?;
    WOOL_COLORS
        .iter()
        .position(|c| *c == color)
        .map(|i| i as u8)
}

fn tame(ctx: &ServerContext, e: &mut Entity, by: &Actor) {
    let id = e.id;
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    m.owner = Some(by.uuid);
    m.tame = true;
    m.persistent = true;
    m.nav.stop();
    m.target = None;
    m.revenge = None;
    if matches!(m.species, Species::Wolf | Species::Cat | Species::Parrot) {
        m.sitting = true;
    }
    if m.species == Species::Wolf {
        m.set_max_health(40.0);
    }
    m.dirty = true;
    fx::status(ctx, id, 7);
}

/// A player used the held item on a mob.
pub fn used(ctx: &ServerContext, e: &mut Entity, by: &Actor, hand: i32, others: &[Entity]) {
    let id = e.id;
    let pos = e.pos;
    let held = crate::held::held(ctx, by.uuid, hand);
    let name = held.as_ref().map_or("", |h| h.name.as_str()).to_owned();
    let hslot = held.as_ref().map_or(0, |h| h.slot);
    let item = held.as_ref().map(|h| h.stack.item);
    if name.ends_with("_spawn_egg") && super::eggs::breed_with_egg(ctx, e, by, &name, hslot) {
        swing(ctx, by);
        return;
    }
    // Leads come first: snipping, tying to it, letting go, tying it.
    let leash_held = held.as_ref().map(|_| (name.as_str(), hslot));
    let mut leads = Vec::new();
    let leashing = crate::decor::leash::use_on(
        ctx,
        e,
        (by.uuid, by.game_mode, by.sneaking),
        leash_held,
        others,
        &mut leads,
    );
    crate::decor::spawn_all(ctx, leads);
    if leashing {
        swing(ctx, by);
        return;
    }
    // Aquatic (Claude): buckets, tadpoles, dolphins, nautiluses, axolotls.
    if let Kind::Mob(m0) = &e.kind
        && m0.species.swims()
    {
        let food = m0
            .species
            .food_tag()
            .is_some_and(|t| ctx.commands.env.tag("minecraft:item", t).contains(&name));
        if super::aqua::used(ctx, e, by, &name, hslot, food) {
            send_now(ctx, e);
            return;
        }
    }

    if super::air::used(ctx, e, by, &name, hslot, held.as_ref().map(|h| &h.stack)) {
        swing(ctx, by);
        return;
    }
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    if m.dying.is_some() {
        return;
    }
    let species = m.species;
    let tagged = |tag: Option<&str>| {
        tag.is_some_and(|t| ctx.commands.env.tag("minecraft:item", t).contains(&name))
    };
    // A name tag.
    if name == "minecraft:name_tag" {
        let custom = held.as_ref().and_then(|h| match &h.comps {
            Some(mc_nbt::Tag::Compound(c)) => match c.get("minecraft:custom_name") {
                Some(mc_nbt::Tag::String(s)) => Some(s.clone()),
                Some(mc_nbt::Tag::Compound(t)) => match t.get("text") {
                    Some(mc_nbt::Tag::String(s)) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
        if let Some(text) = custom {
            m.custom_name = Some(text);
            m.persistent = true;
            m.dirty = true;
            consume(ctx, by, hslot);
            swing(ctx, by);
            send_now(ctx, e);
        }
        return;
    }
    // Iron mends an iron golem: a quarter of its health.
    if name == "minecraft:iron_ingot" && species == Species::IronGolem && m.health < m.max_health {
        m.health = (m.health + 25.0).min(m.max_health);
        m.dirty = true;
        consume(ctx, by, hslot);
        let pitch = 1.0 + (m.next_f64() - m.next_f64()) as f32 * 0.2;
        fx::sound(ctx, "entity.iron_golem.repair", 5, pos, 1.0, pitch);
        swing(ctx, by);
        send_now(ctx, e);
        return;
    }
    // Land animals (Cursor): a brush takes a scute off an armadillo, and
    // wears by 16.
    if name == "minecraft:brush" && species == Species::Armadillo && !m.is_baby() {
        if let Some(scute) = ctx.items.id("minecraft:armadillo_scute") {
            crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.5, pos[2]], scute, 1);
        }
        fx::sound(
            ctx,
            "entity.armadillo.brush",
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
        if by.game_mode != 1 {
            crate::held::wear(ctx, by.uuid, hslot, 16);
        }
        swing(ctx, by);
        return;
    }
    // Shears.
    if name == "minecraft:shears" {
        if shear(ctx, e) {
            if by.game_mode != 1 {
                crate::held::wear(ctx, by.uuid, hslot, 1);
            }
            swing(ctx, by);
        }
        return;
    }
    // Dyes.
    if let Some(color) = dye_color(&name) {
        if species == Species::Sheep && !m.sheared && m.color != color {
            m.set_color(color);
            consume(ctx, by, hslot);
            fx::sound(ctx, "item.dye.use", fx::category::PLAYERS, pos, 1.0, 1.0);
            send_now(ctx, e);
            return;
        }
        if matches!(species, Species::Wolf | Species::Cat)
            && m.owner == Some(by.uuid)
            && m.collar != color
        {
            m.collar = color;
            m.dirty = true;
            consume(ctx, by, hslot);
            send_now(ctx, e);
            return;
        }
    }
    // Milk.
    if name == "minecraft:bucket" && !m.is_baby() {
        let milk = match species {
            Species::Cow | Species::Mooshroom => Some("entity.cow.milk"),
            // Land animals (Cursor)
            Species::Goat => Some(if m.quirks.land.screaming {
                "entity.goat.screaming.milk"
            } else {
                "entity.goat.milk"
            }),
            _ => None,
        };
        if let Some(sound) = milk {
            fx::sound(ctx, sound, fx::category::PLAYERS, pos, 1.0, 1.0);
            swap_one(ctx, by, hslot, "minecraft:milk_bucket");
            return;
        }
    }
    if name == "minecraft:bowl" && species == Species::Mooshroom && !m.is_baby() {
        fx::sound(
            ctx,
            "entity.mooshroom.milk",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        swap_one(ctx, by, hslot, "minecraft:mushroom_stew");
        return;
    }
    // Flint and steel lights a creeper.
    if name == "minecraft:flint_and_steel" && species == Species::Creeper && !m.ignited {
        m.ignited = true;
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
        swing(ctx, by);
        return;
    }
    // Curing a zombie villager.
    if name == "minecraft:golden_apple" && species == Species::ZombieVillager && m.conversion < 0 {
        m.conversion = 3600 + m.below(2401) as i32;
        m.curer = Some(by.uuid);
        m.persistent = true;
        m.dirty = true;
        consume(ctx, by, hslot);
        fx::status(ctx, id, 16);
        fx::sound(ctx, "entity.zombie_villager.cure", 5, pos, 1.0, 1.0);
        send_now(ctx, e);
        return;
    }
    // Gold for a piglin.
    if species == Species::Piglin
        && !m.is_baby()
        && m.charge == 0
        && m.target.is_none()
        && tagged(Some("minecraft:piglin_loved"))
        && let Some(item) = item
    {
        m.gear[slot::OFFHAND] = Some(Gear {
            item,
            count: 1,
            damage: 0,
            components: Default::default(),
        });
        m.charge = 120;
        m.nav.stop();
        consume(ctx, by, hslot);
        fx::sound(ctx, "entity.piglin.admiring_item", 5, pos, 1.0, 1.0);
        send_equipment(ctx, e);
        return;
    }
    let owner = m.owner == Some(by.uuid);
    let food = tagged(species.food_tag());

    // Taming.
    let taming = match species {
        Species::Wolf => name == "minecraft:bone",
        Species::Cat => food,
        Species::Parrot => food,
        _ => false,
    };
    if taming && !m.is_tame() && m.target.is_none() {
        consume(ctx, by, hslot);
        let chance = match species {
            Species::Parrot => 10,
            _ => 3,
        };
        if m.below(chance) == 0 {
            tame(ctx, e, by);
            send_now(ctx, e);
        } else {
            fx::status(ctx, id, 6);
        }
        return;
    }
    // A tamed wolf eats to heal.
    if species == Species::Wolf && m.is_tame() && food && m.health < m.max_health {
        let heal = item
            .and_then(|i| ctx.items.get(i))
            .and_then(|i| i.food.as_ref())
            .map_or(1.0, |f| f.nutrition as f32);
        m.health = (m.health + heal).min(m.max_health);
        m.dirty = true;
        consume(ctx, by, hslot);
        send_now(ctx, e);
        return;
    }
    // Llamas: wheat and hay bales heal and, with luck, tame; a tamed llama
    // that is fed in love breeds.
    if species.llama() && food {
        let hay = name == "minecraft:hay_block";
        let (heal, temper, grow) = if hay { (10.0, 6, 90) } else { (2.0, 3, 20) };
        let mut used = false;
        if m.health < m.max_health {
            m.health = (m.health + heal).min(m.max_health);
            used = true;
        }
        if m.is_baby() {
            m.baby_age = (m.baby_age + grow * 20).min(0);
            used = true;
        } else if !m.is_tame() {
            m.temper = (m.temper + temper).min(30);
            used = true;
            if (m.below(30) as i32) < m.temper {
                tame(ctx, e, by);
                if let Kind::Mob(m) = &mut e.kind {
                    m.sitting = false;
                }
            } else {
                fx::status(ctx, id, 6);
            }
        } else if m.baby_age == 0 && !m.in_love() && hay {
            m.fall_in_love(Some(by.uuid));
            used = true;
        }
        if used {
            consume(ctx, by, hslot);
            fx::sound(
                ctx,
                "entity.llama.eat",
                fx::category::NEUTRAL,
                pos,
                1.0,
                1.0,
            );
            send_now(ctx, e);
            return;
        }
        return;
    }
    // Horses: food heals and calms; golden food makes tamed ones breed.
    if species.horse() && food {
        let golden = matches!(
            name.as_str(),
            "minecraft:golden_carrot"
                | "minecraft:golden_apple"
                | "minecraft:enchanted_golden_apple"
        );
        let mut used = false;
        if m.health < m.max_health {
            m.health = (m.health + 2.0).min(m.max_health);
            used = true;
        }
        if !m.is_tame() {
            m.temper = (m.temper + 3).min(100);
            used = true;
        }
        if golden && m.is_tame() && !m.is_baby() && m.baby_age == 0 && !m.in_love() {
            m.fall_in_love(Some(by.uuid));
            used = true;
        }
        if m.is_baby() {
            m.baby_age = (m.baby_age + 200).min(0);
            used = true;
        }
        if used {
            consume(ctx, by, hslot);
            fx::sound(
                ctx,
                &format!("entity.{}.eat", species.path()),
                fx::category::NEUTRAL,
                pos,
                1.0,
                1.0,
            );
            m.dirty = true;
            send_now(ctx, e);
            return;
        }
    }
    // Breeding food.
    if food && species.ageable() && !species.horse() {
        if m.is_baby() {
            m.baby_age += -m.baby_age / 10;
            consume(ctx, by, hslot);
            fx::particles(
                ctx,
                "minecraft:happy_villager",
                [pos[0], pos[1] + 0.5, pos[2]],
                0.5,
                0.0,
                5,
            );
            // Land animals (Cursor)
            goat_eat(ctx, m, pos);
            return;
        }
        let may_breed = m.baby_age == 0
            && !m.in_love()
            && (!species.tameable() || m.is_tame())
            && species != Species::SulfurCube;
        if may_breed {
            m.fall_in_love(Some(by.uuid));
            consume(ctx, by, hslot);
            fx::status(ctx, id, 18);
            // Land animals (Cursor)
            goat_eat(ctx, m, pos);
            return;
        }
    }
    // Aquatic (Claude): a golden dandelion stops a baby growing, or lets it go on.
    if name == "minecraft:golden_dandelion"
        && (m.is_baby() || species == Species::Tadpole)
        && species != Species::SulfurCube
    {
        let locked = !m.quirks.aqua.age_locked;
        m.quirks.aqua.age_locked = locked;
        if locked {
            m.baby_age = if species == Species::Tadpole {
                0
            } else {
                -24000
            };
            m.quirks.aqua.growth = 0;
            m.persistent = true;
        }
        consume(ctx, by, hslot);
        fx::sound(
            ctx,
            if locked {
                "item.golden_dandelion.use"
            } else {
                "item.golden_dandelion.unuse"
            },
            fx::category::NEUTRAL,
            pos,
            1.0,
            1.0,
        );
        send_now(ctx, e);
        return;
    }
    // Saddles and horse armor.
    if name == "minecraft:saddle" {
        if let Some(item) = item
            && super::ride::saddle(ctx, e, item)
        {
            consume(ctx, by, hslot);
        }
        return;
    }
    // A carpet on a tamed llama.
    if name.ends_with("_carpet") && species.llama() && m.is_tame() && m.gear[slot::BODY].is_none() {
        if let Some(item) = item {
            m.gear[slot::BODY] = Some(Gear {
                item,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
            consume(ctx, by, hslot);
            fx::sound(
                ctx,
                "entity.llama.swag",
                fx::category::NEUTRAL,
                pos,
                0.5,
                1.0,
            );
            send_equipment(ctx, e);
        }
        return;
    }
    if name.ends_with("_horse_armor")
        && species == Species::Horse
        && m.is_tame()
        && m.gear[slot::BODY].is_none()
    {
        if let Some(item) = item {
            m.gear[slot::BODY] = Some(Gear {
                item,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
            consume(ctx, by, hslot);
            fx::sound(
                ctx,
                "entity.horse.armor",
                fx::category::NEUTRAL,
                pos,
                0.5,
                1.0,
            );
            send_equipment(ctx, e);
        }
        return;
    }
    // Aquatic (Claude): nautilus armor on a tamed nautilus.
    if name.ends_with("_nautilus_armor")
        && matches!(species, Species::Nautilus | Species::ZombieNautilus)
        && m.is_tame()
        && !m.is_baby()
        && m.gear[slot::BODY].is_none()
    {
        if let Some(item) = item {
            m.gear[slot::BODY] = Some(Gear {
                item,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
            m.persistent = true;
            consume(ctx, by, hslot);
            fx::sound(
                ctx,
                "item.armor.equip_nautilus",
                fx::category::NEUTRAL,
                pos,
                0.5,
                1.0,
            );
            send_equipment(ctx, e);
        }
        return;
    }
    // A harness on a grown happy ghast.
    if name.ends_with("_harness")
        && species == Species::HappyGhast
        && !m.is_baby()
        && m.gear[slot::BODY].is_none()
    {
        if let Some(item) = item {
            m.gear[slot::BODY] = Some(Gear {
                item,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
            consume(ctx, by, hslot);
            fx::sound(
                ctx,
                "entity.happy_ghast.equip",
                fx::category::NEUTRAL,
                pos,
                1.0,
                1.0,
            );
            send_equipment(ctx, e);
        }
        return;
    }
    // An owner's empty-handed (or non-food) use: sit or stand.
    if owner && matches!(species, Species::Wolf | Species::Cat | Species::Parrot) && !by.sneaking {
        m.sitting = !m.sitting;
        m.nav.stop();
        m.target = None;
        m.dirty = true;
        send_now(ctx, e);
        return;
    }
    // Getting on.
    let rideable = match species {
        Species::Pig | Species::Strider => m.saddled(),
        // Land animals (Cursor)
        Species::Camel | Species::CamelHusk => m.saddled() && !m.is_baby(),
        Species::Nautilus => m.saddled() && m.is_tame() && !m.is_baby(),
        Species::ZombieNautilus => m.saddled() && m.is_tame() && !m.is_baby(),
        // Flying mobs (flying-mobs): a harnessed happy ghast.
        Species::HappyGhast => m.gear[slot::BODY].is_some() && !m.is_baby(),
        Species::Horse | Species::Donkey | Species::Mule => !m.is_baby(),
        _ => false,
    };
    if rideable && !by.sneaking && item.is_none_or(|_| !food) {
        super::ride::mount(ctx, e, by.entity_id);
    }
}

/// Shears used on a mob (by a player or a dispenser): a sheep gives its
/// wool, a mooshroom its mushrooms and becomes a cow. Whether it did.
pub fn shear(ctx: &ServerContext, e: &mut Entity) -> bool {
    let id = e.id;
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.dying.is_some() {
        return false;
    }
    let species = m.species;
    if species == Species::SnowGolem && m.quirks.pumpkin {
        // The pumpkin comes off whole.
        m.quirks.pumpkin = false;
        fx::sound(
            ctx,
            "entity.snow_golem.shear",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        if let Some(item) = ctx.items.id("minecraft:carved_pumpkin") {
            crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 1.7, pos[2]], item, 1);
        }
        send_now(ctx, e);
        return true;
    }
    if super::cube::shear(ctx, e) {
        return true;
    }
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    // Flying mobs (flying-mobs): shears take a happy ghast's harness off.
    if species == Species::HappyGhast
        && let Some(g) = m.gear[slot::BODY].take()
    {
        m.dirty = true;
        fx::sound(
            ctx,
            "entity.happy_ghast.unequip",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 2.0, pos[2]], g.item, 1);
        send_equipment(ctx, e);
        return true;
    }
    // Aquatic (Claude): shears take a nautilus's armor off first, then the saddle.
    if matches!(species, Species::Nautilus | Species::ZombieNautilus) {
        let worn = if m.gear[slot::BODY].is_some() {
            slot::BODY
        } else if m.gear[slot::SADDLE].is_some() {
            slot::SADDLE
        } else {
            return false;
        };
        let gear = m.gear[worn].take();
        let rider_off = worn == slot::SADDLE && !m.riders.is_empty();
        fx::sound(
            ctx,
            "item.armor.unequip_nautilus",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        if let Some(g) = gear {
            crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.5, pos[2]], g.item, 1);
        }
        if rider_off {
            super::ride::eject(ctx, e);
        }
        send_equipment(ctx, e);
        return true;
    }
    // Flying mobs (flying-mobs): a bogged gives up its two mushrooms, once.
    if species == Species::Bogged && !m.quirks.air.sheared && !m.is_baby() {
        m.quirks.air.sheared = true;
        let drops = [m.below(2), m.below(2)];
        fx::sound(
            ctx,
            "entity.bogged.shear",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        for red in drops {
            let name = if red == 0 {
                "minecraft:brown_mushroom"
            } else {
                "minecraft:red_mushroom"
            };
            if let Some(item) = ctx.items.id(name) {
                crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 1.4, pos[2]], item, 1);
            }
        }
        send_now(ctx, e);
        return true;
    }
    if m.can_shear() {
        m.shear();
        let color = WOOL_COLORS[usize::from(m.color & 15)];
        let count = 1 + m.below(3) as i32;
        fx::sound(
            ctx,
            "entity.sheep.shear",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        if let Some(wool) = ctx.items.id(&format!("minecraft:{color}_wool")) {
            crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 1.0, pos[2]], wool, count);
        }
        send_now(ctx, e);
        true
    } else if species == Species::Mooshroom && !m.is_baby() {
        // A mooshroom gives its mushrooms and becomes a cow.
        let mushroom = if m.variant == 1 {
            "minecraft:brown_mushroom"
        } else {
            "minecraft:red_mushroom"
        };
        fx::sound(
            ctx,
            "entity.mooshroom.shear",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        if let Some(i) = ctx.items.id(mushroom) {
            crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 1.0, pos[2]], i, 5);
        }
        if let Some(cow) = super::hurt::convert(ctx, e, Species::Cow) {
            let _ = ctx.events.send(Event::EntitySpawned(cow.clone()));
            let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
            lock(&ctx.entities).push(cow);
        }
        true
    } else {
        false
    }
}

/// Sends the mob's metadata now.
pub(super) fn send_now(ctx: &ServerContext, e: &mut Entity) {
    if let Kind::Mob(m) = &mut e.kind {
        m.dirty = false;
    }
    super::hurt::send_data(ctx, e);
}

pub(super) fn send_equipment(ctx: &ServerContext, e: &Entity) {
    if let Kind::Mob(m) = &e.kind {
        fx::emit(
            ctx,
            fx::Fx::Equip(play::SetEquipment {
                entity_id: e.id,
                slots: super::meta::equipment_of(m),
            }),
        );
    }
}

/// How good an item is in a slot: armor points, or attack damage.
fn worth(ctx: &ServerContext, item: i32) -> f32 {
    ctx.items
        .get(item)
        .map_or(0.0, |i| i.armor * 10.0 + i.toughness + i.attack_damage)
}

/// Mobs that may pick things up take items they stand on: better armor or
/// weapons (dropping what they had), and piglins gold.
pub fn pick_up(
    ctx: &ServerContext,
    entities: &mut [Entity],
    data: &mut Vec<SetEntityData>,
    now: i64,
) {
    let _ = (data, now);
    let pickers: Vec<usize> = entities
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(&e.kind, Kind::Mob(m) if m.dying.is_none() && !e.removed
                && (m.can_pick_up || m.species == Species::Piglin && !m.is_baby()))
        })
        .map(|(i, _)| i)
        .collect();
    if pickers.is_empty() || !ctx.game_rule_bool("minecraft:mob_griefing") {
        return;
    }
    for i in pickers {
        let mbox = entities[i].aabb();
        let Some(j) = entities.iter().position(|o| {
            matches!(
                o.kind,
                Kind::Item {
                    pickup_delay: 0,
                    ..
                }
            ) && !o.removed
                && {
                    let b = o.aabb();
                    b.max[0] > mbox.min[0] - 1.0
                        && b.min[0] < mbox.max[0] + 1.0
                        && b.max[1] > mbox.min[1]
                        && b.min[1] < mbox.max[1]
                        && b.max[2] > mbox.min[2] - 1.0
                        && b.min[2] < mbox.max[2] + 1.0
                }
        }) else {
            continue;
        };
        let Kind::Item {
            item,
            count,
            damage,
            ref components,
            ..
        } = entities[j].kind
        else {
            continue;
        };
        let components = components.clone();
        let item_id = entities[j].id;
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        let name = ctx.items.name(item).unwrap_or("");
        let loved = ctx
            .commands
            .env
            .tag("minecraft:item", "minecraft:piglin_loved")
            .iter()
            .any(|n| n == name);
        let mut dropped = None;
        if m.species == Species::Piglin && loved {
            if m.charge > 0 || m.target.is_some() {
                continue;
            }
            m.gear[slot::OFFHAND] = Some(Gear {
                item,
                count: 1,
                damage: 0,
                components: Default::default(),
            });
            m.charge = 120;
        } else if m.can_pick_up {
            let Some(s) = super::meta::slot_of(ctx, item)
                .filter(|s| matches!(*s, slot::HEAD | slot::CHEST | slot::LEGS | slot::FEET))
                .or_else(|| {
                    (ctx.items.get(item).is_some_and(|i| i.attack_damage > 0.0))
                        .then_some(slot::MAINHAND)
                })
            else {
                continue;
            };
            let better = m.gear[s]
                .as_ref()
                .is_none_or(|g| worth(ctx, item) > worth(ctx, g.item));
            if !better {
                continue;
            }
            if let Some(old) = m.gear[s].take()
                && m.drop_chance[s] > 1.0
            {
                dropped = Some(old);
            }
            m.gear[s] = Some(Gear {
                item,
                count: 1,
                damage,
                components,
            });
            m.drop_chance[s] = 2.0;
            m.persistent = true;
        } else {
            continue;
        }
        let pos = entities[i].pos;
        let collector = entities[i].id;
        send_equipment(ctx, &entities[i]);
        if count > 1 {
            if let Kind::Item { count: c, .. } = &mut entities[j].kind {
                *c -= 1;
            }
        } else {
            entities[j].removed = true;
        }
        let _ = ctx.events.send(Event::ItemTaken {
            item_entity: item_id,
            collector,
            count: 1,
            slots: Vec::new(),
        });
        if count <= 1 {
            let _ = ctx.events.send(Event::EntitiesRemoved(vec![item_id]));
        }
        if let Some(old) = dropped {
            let e = Entity {
                id: ctx.next_entity_id(),
                uuid: crate::util::random_uuid(),
                kind: Kind::item(old.item, old.count, old.damage, 10),
                pos: [pos[0], pos[1] + 0.3, pos[2]],
                vel: [0.0, 0.2, 0.0],
                on_ground: false,
                age: 0,
                removed: false,
                tags: Vec::new(),
            };
            let _ = ctx.events.send(Event::EntitySpawned(e.clone()));
            lock(&ctx.entities).push(e);
        }
    }
}

/// Piglins look at their gold for six seconds, then barter.
pub fn admire(ctx: &ServerContext, entities: &mut [Entity]) {
    for e in entities.iter_mut() {
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if m.species != Species::Piglin || m.charge <= 0 {
            continue;
        }
        m.charge -= 1;
        if m.charge > 0 {
            continue;
        }
        let gold = m.gear[slot::OFFHAND].take();
        let is_ingot = gold
            .as_ref()
            .and_then(|g| ctx.items.name(g.item))
            .is_some_and(|n| n == "minecraft:gold_ingot");
        send_equipment(ctx, e);
        if is_ingot {
            let drops =
                super::hurt::roll(ctx, e, "minecraft:gameplay/piglin_bartering", false, None);
            for d in drops {
                crate::tick::spawn_item_at(
                    ctx,
                    [e.pos[0], e.pos[1] + 1.0, e.pos[2]],
                    d.item,
                    d.count,
                );
            }
        } else if let Some(g) = gold {
            // Other gold it keeps... in its inventory, which it does not
            // have: it drops it again.
            crate::tick::spawn_item_at(ctx, [e.pos[0], e.pos[1] + 1.0, e.pos[2]], g.item, 1);
        }
    }
}
