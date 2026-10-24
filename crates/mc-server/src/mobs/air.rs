//! Flying mobs (flying-mobs): what the hits on them and their blows do in
//! the world (minecraft.wiki, the page of each mob).

use mc_entity::{Entity, Kind, Species};

use super::hurt::Harm;
use crate::context::{Event, ServerContext};
use crate::fx;
use crate::tick::lock;

/// What a hit does before the mob's own health is touched: the damage that
/// goes through, or `None` when the mob takes none.
pub fn filter(ctx: &ServerContext, m: &mc_entity::Mob, h: &Harm, amount: f32) -> Option<f32> {
    match m.species {
        // A creaking its heart keeps is hurt by nothing but the void and
        // /kill.
        Species::Creaking
            if m.quirks.air.home.is_some()
                && !matches!(h.kind, "minecraft:out_of_world" | "minecraft:generic_kill") =>
        {
            // Hit by a player at night: resin grows by its heart.
            if let (Some(home), Some(_)) = (m.quirks.air.home, h.player)
                && super::creaking::awake_time(super::day_time(ctx))
            {
                super::creaking::hit(ctx, home, lock(&ctx.time).age);
            }
            None
        }
        Species::Breeze if h.kind == "minecraft:fall" => None,
        // A shut shell turns arrows away and counts for 20 armor points.
        Species::Shulker if m.quirks.air.peek <= 0 => {
            if matches!(h.kind, "minecraft:arrow") {
                return None;
            }
            Some(crate::damage::armor_reduce(h.amount, 20.0, 0.0, h.kind))
        }
        _ => Some(amount),
    }
}

/// What a hit that counted does next: a shulker hit hard teleports, one hit
/// by a bullet with its shell open may leave a copy behind; a bee hit turns
/// angry for 20 to 39 seconds.
pub fn hurt(ctx: &ServerContext, e: &mut Entity, h: &Harm) {
    let Kind::Mob(m) = &mut e.kind else {
        return;
    };
    match m.species {
        Species::Shulker => {
            let low = m.health < m.max_health / 2.0 && m.health > 0.0;
            let open = m.quirks.air.peek > 0;
            let bullet = h.kind == "minecraft:mob_projectile" && open;
            let color = m.quirks.air.color;
            let healthy = m.health > m.max_health / 2.0;
            let tele = (low && m.below(4) == 0) || bullet;
            if !tele {
                return;
            }
            let from = e.pos;
            let ground = super::Ground::new(ctx);
            if !mc_entity::mob::air::shulker_teleport(e, &ground) {
                return;
            }
            fx::sound(ctx, "entity.shulker.teleport", 5, e.pos, 1.0, 1.0);
            if bullet && healthy {
                duplicate(ctx, from, color);
            }
        }
        Species::Bee => {
            if h.by.is_some() || h.player.is_some() {
                m.anger = 400 + m.below(380) as i32;
                m.dirty = true;
            }
        }
        _ => {}
    }
}

/// A shulker hit by a bullet leaves a copy of itself where it stood, unless
/// other shulkers crowd it: 20% fewer chances for each within 8 blocks, none
/// at five.
fn duplicate(ctx: &ServerContext, at: [f64; 3], color: u8) {
    let crowd = lock(&ctx.entities)
        .iter()
        .filter(|o| {
            matches!(&o.kind, Kind::Mob(m) if m.species == Species::Shulker && !m.is_dead())
                && (o.pos[0] - at[0]).powi(2)
                    + (o.pos[1] - at[1]).powi(2)
                    + (o.pos[2] - at[2]).powi(2)
                    < 64.0
        })
        .count();
    // The one that moved is among those counted.
    let others = crowd.saturating_sub(1) as f64;
    if crate::hits::rnd() >= (1.0 - 0.2 * others).max(0.0) {
        return;
    }
    let mut s = super::new_mob(ctx, Species::Shulker, at);
    if let Kind::Mob(sm) = &mut s.kind {
        sm.quirks.air.color = color;
    }
    let _ = ctx.events.send(Event::EntitySpawned(s.clone()));
    lock(&ctx.entities).push(s);
}

/// A bee that stings hurts the victim as its difficulty says and is done:
/// the stinger stays, and it dies about a minute later.
pub fn bee_stung(e: &mut Entity) {
    if let Kind::Mob(m) = &mut e.kind {
        m.quirks.air.stung = true;
        m.quirks.air.sting_age = 0;
        m.anger = 0;
        m.target = None;
        m.dirty = true;
    }
}

/// What a death leaves besides the loot: an allay's bag, and a large sulfur
/// cube splits into two small ones.
pub fn died(ctx: &ServerContext, e: &Entity) {
    let Kind::Mob(m) = &e.kind else {
        return;
    };
    // An allay's bag falls with it.
    if let Some((item, count)) = m.quirks.air.carried {
        crate::tick::spawn_item_at(ctx, [e.pos[0], e.pos[1] + 0.3, e.pos[2]], item, count);
    }
    if m.species != Species::SulfurCube || m.is_baby() {
        return;
    }
    for k in 0..2 {
        let dx = (f64::from(k) - 0.5) * 0.5;
        let mut s = super::new_mob(
            ctx,
            Species::SulfurCube,
            [e.pos[0] + dx, e.pos[1] + 0.3, e.pos[2]],
        );
        if let Kind::Mob(sm) = &mut s.kind {
            sm.set_baby(true);
            sm.persistent = m.persistent;
        }
        let _ = ctx.events.send(Event::EntitySpawned(s.clone()));
        lock(&ctx.entities).push(s);
    }
}

/// A player used the held item on an allay: it takes one of it and sets out
/// to find more, or gives back what it carries when the hand is empty; an
/// amethyst shard splits a dancing one in two. Whether the use was an
/// allay's.
pub fn used(
    ctx: &ServerContext,
    e: &mut Entity,
    by: &super::Actor,
    name: &str,
    hslot: usize,
    stack: Option<&mc_protocol::packets::play::ItemStack>,
) -> bool {
    use mc_entity::mob::{Gear, slot};
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    if m.species == Species::CopperGolem && m.dying.is_none() {
        return copper_used(ctx, e, by, name, hslot);
    }
    if m.species != Species::Allay || m.dying.is_some() {
        return false;
    }
    let consume = || {
        if by.game_mode != 1 {
            crate::held::consume(ctx, by.uuid, hslot, 1);
        }
    };
    // An amethyst shard for a dancing allay that may split.
    if name == "minecraft:amethyst_shard" && m.quirks.air.dancing && m.quirks.air.dup_cd <= 0 {
        m.quirks.air.dup_cd = 6000;
        m.dirty = true;
        consume();
        fx::sound(ctx, "entity.allay.item_thrown", 5, pos, 1.0, 1.0);
        let mut twin = super::new_mob(ctx, Species::Allay, pos);
        if let Kind::Mob(t) = &mut twin.kind {
            t.quirks.air.dup_cd = 6000;
            t.quirks.air.liked = m.quirks.air.liked;
            t.dirty = true;
        }
        let _ = ctx.events.send(Event::EntitySpawned(twin.clone()));
        lock(&ctx.entities).push(twin);
        super::interact::send_now(ctx, e);
        return true;
    }
    let holding = m.gear[slot::MAINHAND].is_some();
    match (stack, holding) {
        // A gift: the allay wants more of the same for this player.
        (Some(s), false) if !name.is_empty() => {
            m.gear[slot::MAINHAND] = Some(Gear {
                item: s.item,
                count: 1,
                damage: s.damage,
                components: s.components.clone(),
            });
            m.drop_chance[slot::MAINHAND] = 2.0;
            m.quirks.air.liked = Some(by.uuid);
            m.persistent = true;
            m.dirty = true;
            consume();
            fx::sound(ctx, "entity.allay.item_given", 5, pos, 1.0, 1.0);
            super::interact::send_equipment(ctx, e);
            true
        }
        // An empty hand takes it back.
        (None, true) => {
            if let Some(g) = m.gear[slot::MAINHAND].take() {
                crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.5, pos[2]], g.item, 1);
            }
            m.quirks.air.liked = None;
            m.quirks.air.errand = None;
            m.dirty = true;
            fx::sound(ctx, "entity.allay.item_taken", 5, pos, 1.0, 1.0);
            super::interact::send_equipment(ctx, e);
            true
        }
        _ => false,
    }
}

#[allow(dead_code)] // nothing in the server plays note blocks yet
/// A note block at `cell` played: allays of this player's bring what they
/// find there for 30 seconds.
pub fn note_played(entities: &mut [Entity], cell: [i32; 3], now: i64) {
    for e in entities.iter_mut() {
        let Kind::Mob(m) = &mut e.kind else {
            continue;
        };
        if m.species != Species::Allay || m.quirks.air.liked.is_none() {
            continue;
        }
        let d = (e.pos[0] - f64::from(cell[0])).powi(2)
            + (e.pos[1] - f64::from(cell[1])).powi(2)
            + (e.pos[2] - f64::from(cell[2])).powi(2);
        if d <= 16.0 * 16.0 {
            m.quirks.air.note = Some((cell, now + 600));
        }
    }
}

/// What the flying mobs do each tick that is not their own thinking: allays
/// dance to a jukebox and run their errands, a bee that stung dies, a
/// copper golem weathers.
pub fn tick(ctx: &ServerContext, entities: &mut [Entity], targets: &[super::Target], now: i64) {
    let jukeboxes: Vec<(i32, i32, i32)> = lock(&ctx.jukeboxes).keys().copied().collect();
    let griefing = ctx.game_rule_bool("minecraft:mob_griefing");
    for i in 0..entities.len() {
        let Kind::Mob(m) = &mut entities[i].kind else {
            continue;
        };
        if m.dying.is_some() || entities[i].removed {
            continue;
        }
        match m.species {
            Species::Bee if m.quirks.air.stung => {
                m.quirks.air.sting_age += 1;
                if m.quirks.air.sting_age >= 1200 {
                    super::hurt::die(ctx, &mut entities[i], None, 0);
                }
            }
            Species::Allay => allay(ctx, entities, i, targets, &jukeboxes, griefing, now),
            Species::CopperGolem => weather(m, now),
            _ => {}
        }
    }
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn allay(
    ctx: &ServerContext,
    entities: &mut [Entity],
    i: usize,
    targets: &[super::Target],
    jukeboxes: &[(i32, i32, i32)],
    griefing: bool,
    now: i64,
) {
    use mc_entity::mob::slot;
    let pos = entities[i].pos;
    let Kind::Mob(m) = &mut entities[i].kind else {
        return;
    };
    let near_disc = jukeboxes.iter().any(|&(x, y, z)| {
        dist_sq(
            pos,
            [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5],
        ) <= 3.46 * 3.46
    });
    if m.quirks.air.dancing != near_disc {
        m.quirks.air.dancing = near_disc;
        m.dirty = true;
    }
    if let Some((_, until)) = m.quirks.air.note
        && now >= until
    {
        m.quirks.air.note = None;
    }
    let want = m.gear[slot::MAINHAND].as_ref().map(|g| g.item);
    let owner = m.quirks.air.liked.and_then(|u| {
        targets
            .iter()
            .find(|t| t.uuid == u && dist_sq(t.pos, pos) <= 64.0 * 64.0)
    });
    let (Some(want), Some(owner)) = (want, owner) else {
        m.quirks.air.errand = None;
        return;
    };
    let goal = match m.quirks.air.note {
        Some((c, _)) => [
            f64::from(c[0]) + 0.5,
            f64::from(c[1]) + 1.0,
            f64::from(c[2]) + 0.5,
        ],
        None => [owner.pos[0], owner.pos[1] + 1.0, owner.pos[2]],
    };
    if let Some((item, count)) = m.quirks.air.carried {
        m.quirks.air.errand = Some(goal);
        if dist_sq(pos, goal) <= 3.0 * 3.0 {
            crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.3, pos[2]], item, count);
            m.quirks.air.carried = None;
            m.quirks.air.errand = None;
            m.quirks.air.errand_cd = 60;
            fx::sound(ctx, "entity.allay.item_thrown", 5, pos, 1.0, 1.0);
        }
        return;
    }
    if m.quirks.air.errand_cd > 0 || !griefing {
        m.quirks.air.errand = None;
        return;
    }
    let found = entities
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            !o.removed
                && matches!(&o.kind, Kind::Item { item, .. } if *item == want)
                && dist_sq(o.pos, pos) <= 32.0 * 32.0
        })
        .min_by(|a, b| dist_sq(a.1.pos, pos).total_cmp(&dist_sq(b.1.pos, pos)))
        .map(|(j, o)| (j, o.pos, o.id));
    let Kind::Mob(m) = &mut entities[i].kind else {
        return;
    };
    let Some((j, at, id)) = found else {
        m.quirks.air.errand = None;
        return;
    };
    m.quirks.air.errand = Some(at);
    if dist_sq(pos, at) > 1.3 * 1.3 {
        return;
    }
    let Kind::Item { count, .. } = &mut entities[j].kind else {
        return;
    };
    let take = (*count).min(ctx.items.max_stack(want));
    *count -= take;
    let gone = *count <= 0;
    if gone {
        entities[j].removed = true;
    }
    if let Kind::Mob(m) = &mut entities[i].kind {
        m.quirks.air.carried = Some((want, take));
        m.quirks.air.errand = None;
    }
    let collector = entities[i].id;
    let _ = ctx.events.send(Event::ItemTaken {
        item_entity: id,
        collector,
        count: take,
        slots: Vec::new(),
    });
    if gone {
        let _ = ctx.events.send(Event::EntitiesRemoved(vec![id]));
    }
}

/// Copper golems oxidise one step every 420 to 460 minutes of game time;
/// wax (`-2`) stops it.
fn weather(m: &mut mc_entity::Mob, now: i64) {
    if m.quirks.air.next_weather == -2 || m.quirks.air.weather >= 3 {
        return;
    }
    let step = i64::from(m.rand_range(504_000, 552_000));
    let air = &mut m.quirks.air;
    if air.next_weather < 0 {
        air.next_weather = now + step;
    } else if now >= air.next_weather {
        air.weather += 1;
        air.next_weather = now + step;
        m.dirty = true;
    }
}

/// Honeycomb waxes a copper golem, an axe takes the wax off or scrapes one
/// stage of weathering, and an empty hand makes it put down what it carries.
fn copper_used(
    ctx: &ServerContext,
    e: &mut Entity,
    by: &super::Actor,
    name: &str,
    hslot: usize,
) -> bool {
    let pos = e.pos;
    let Kind::Mob(m) = &mut e.kind else {
        return false;
    };
    let air = &mut m.quirks.air;
    let waxed = air.next_weather == -2;
    if name == "minecraft:honeycomb" && !waxed {
        air.next_weather = -2;
        m.dirty = true;
        if by.game_mode != 1 {
            crate::held::consume(ctx, by.uuid, hslot, 1);
        }
        fx::level_event(ctx, 3003, pos, 0);
        fx::sound(
            ctx,
            "item.honeycomb.wax_on",
            fx::category::PLAYERS,
            pos,
            1.0,
            1.0,
        );
        return true;
    }
    if name.ends_with("_axe") && (waxed || air.weather > 0) {
        if waxed {
            air.next_weather = -1;
            fx::level_event(ctx, 3004, pos, 0);
        } else {
            air.weather -= 1;
            air.next_weather = -1;
            fx::level_event(ctx, 3005, pos, 0);
        }
        m.dirty = true;
        if by.game_mode != 1 {
            crate::held::wear(ctx, by.uuid, hslot, 1);
        }
        fx::sound(ctx, "item.axe.scrape", fx::category::PLAYERS, pos, 1.0, 1.0);
        return true;
    }
    if name.is_empty()
        && let Some((item, count)) = air.carried.take()
    {
        air.wait = 0;
        air.at_chest = None;
        m.dirty = true;
        crate::tick::spawn_item_at(ctx, [pos[0], pos[1] + 0.6, pos[2]], item, count);
        return true;
    }
    false
}
