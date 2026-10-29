//! Crossbows (minecraft.wiki, "Crossbow", "Quick Charge", "Multishot",
//! "Piercing"):
//! - held to charge (1.25 s, a quarter second less for each Quick Charge
//!   level) and let go fully charged, it loads: an arrow or tipped arrow
//!   from the inventory, or a firework rocket held in the other hand; with
//!   Multishot two more copies that cost nothing; the loaded projectiles
//!   live in its `charged_projectiles`;
//! - used loaded, it shoots them all at once: arrows at 3.15, always
//!   critical, through Piercing's count of entities; rockets at 1.6 going
//!   straight; Multishot's spread is 10° either side and its copies can be
//!   picked up only in creative. Each arrow wears it by 1, a rocket by 3;
//! - the charging sounds play at a fifth and half of the charge.

use mc_entity::extra::{Arrow, Pickup, Shot};
use mc_nbt::Tag;
use mc_protocol::packets::play::ItemStack;

use crate::context::{Event, ServerContext};
use crate::enchant_effects::{self as ee, Situation};
use crate::fx;
use crate::held::{self, Held};
use crate::hits;
use crate::interact::Actor;
use crate::tick::lock;

/// The stacks loaded in a crossbow.
pub fn loaded(stack: &ItemStack) -> Vec<ItemStack> {
    match stack.components.get("minecraft:charged_projectiles") {
        Some(Tag::List(l)) => l
            .iter()
            .filter_map(|t| match t {
                Tag::Compound(c) => ItemStack::from_nbt(c),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn set_loaded(stack: &mut ItemStack, list: &[ItemStack]) {
    if list.is_empty() {
        stack.components.reset("minecraft:charged_projectiles");
    } else {
        stack.components.set(
            "minecraft:charged_projectiles",
            Tag::List(
                list.iter()
                    .filter_map(|s| s.to_template_nbt().map(Tag::Compound))
                    .collect(),
            ),
        );
    }
}

/// Ticks to charge: `crossbow_charge_time` (1.25 s less Quick Charge).
pub fn charge_ticks(ctx: &ServerContext, stack: &ItemStack) -> i32 {
    let seconds = ee::modify(
        ctx,
        "minecraft:crossbow_charge_time",
        stack,
        &Situation::default(),
        1.25,
    );
    ((seconds * 20.0).floor() as i32).max(0)
}

/// The start, middle and end charging sounds: Quick Charge's by level,
/// else the plain ones.
fn sounds(
    ctx: &ServerContext,
    stack: &ItemStack,
) -> (Option<String>, Option<String>, Option<String>) {
    for (name, level) in crate::enchants::levels(stack) {
        if let Some(e) = ctx.enchantments.get().get(&name)
            && let Some(list) = e.effects["minecraft:crossbow_charging_sounds"].as_array()
            && !list.is_empty()
        {
            let s = &list[(level as usize).saturating_sub(1).min(list.len() - 1)];
            let name = |k: &str| {
                s[k].as_str()
                    .map(|n| n.trim_start_matches("minecraft:").to_owned())
            };
            return (name("start"), name("mid"), name("end"));
        }
    }
    (
        Some("item.crossbow.loading_start".into()),
        Some("item.crossbow.loading_middle".into()),
        Some("item.crossbow.loading_end".into()),
    )
}

fn is_ammo(name: &str) -> bool {
    super::is_arrow(name)
}

/// What the crossbow in `hand` would load: a rocket or an arrow in the
/// other hand first, then arrows in the inventory.
fn ammo(ctx: &ServerContext, uuid: u128, hand: i32) -> Option<Held> {
    let other = held::held(ctx, uuid, 1 - hand)
        .filter(|h| is_ammo(&h.name) || h.name == "minecraft:firework_rocket");
    other.or_else(|| held::find(ctx, uuid, is_ammo))
}

/// A right click with a crossbow: shoots it if loaded, else starts
/// charging when there is ammunition. `true` when it did either.
pub fn use_crossbow(ctx: &ServerContext, actor: Actor, hand: i32, bow: &Held) -> bool {
    if !loaded(&bow.stack).is_empty() {
        shoot(ctx, actor, hand);
        return true;
    }
    if actor.game_mode != 1 && ammo(ctx, actor.uuid, hand).is_none() {
        return false;
    }
    let now = lock(&ctx.time).age;
    let mut st = lock(&ctx.shooting);
    st.draws.insert(actor.uuid, (now, hand));
    st.charging
        .insert(actor.uuid, (actor.entity_id, false, false));
    drop(st);
    let _ = ctx.events.send(Event::UsingItem {
        uuid: actor.uuid,
        entity_id: actor.entity_id,
        hand: Some(hand),
    });
    true
}

/// The charging sounds, every tick, for every crossbow being charged.
pub fn tick_charging(ctx: &ServerContext) {
    let now = lock(&ctx.time).age;
    type Charging = (u128, (i32, bool, bool), Option<(i64, i32)>);
    let charging: Vec<Charging> = {
        let st = lock(&ctx.shooting);
        st.charging
            .iter()
            .map(|(u, c)| (*u, *c, st.draws.get(u).copied()))
            .collect()
    };
    for (uuid, (entity_id, mut start, mut mid), draw) in charging {
        let Some((since, hand)) = draw else {
            lock(&ctx.shooting).charging.remove(&uuid);
            continue;
        };
        let here = ctx.of_player(uuid);
        let ctx = &*here;
        let Some(bow) = held::held(ctx, uuid, hand).filter(|h| h.name == "minecraft:crossbow")
        else {
            lock(&ctx.shooting).charging.remove(&uuid);
            continue;
        };
        let Some(pose) = lock(&ctx.players).get(&uuid).and_then(|p| p.pose) else {
            continue;
        };
        let at = [pose.x, pose.y, pose.z];
        let total = charge_ticks(ctx, &bow.stack).max(1) as f32;
        let f = (now - since) as f32 / total;
        let (s, m, _) = sounds(ctx, &bow.stack);
        if f < 0.2 {
            (start, mid) = (false, false);
        }
        if f >= 0.2 && !start {
            start = true;
            if let Some(s) = s {
                fx::sound(ctx, &s, fx::category::PLAYERS, at, 0.5, 1.0);
            }
        }
        if f >= 0.5 && !mid {
            mid = true;
            if let Some(m) = m {
                fx::sound(ctx, &m, fx::category::PLAYERS, at, 0.5, 1.0);
            }
        }
        if let Some(c) = lock(&ctx.shooting).charging.get_mut(&uuid) {
            *c = (entity_id, start, mid);
        }
    }
}

/// The player let go of a crossbow charged for `ticks`: loaded if the
/// charge was full.
pub fn release(ctx: &ServerContext, actor: Actor, hand: i32, ticks: i32) {
    lock(&ctx.shooting).charging.remove(&actor.uuid);
    let Some(bow) = held::held(ctx, actor.uuid, hand).filter(|h| h.name == "minecraft:crossbow")
    else {
        return;
    };
    if !loaded(&bow.stack).is_empty() || ticks < charge_ticks(ctx, &bow.stack) {
        return;
    }
    crate::stats::item(ctx, actor.uuid, crate::stats::USED, bow.stack.item, 1);
    let count = 1 + ee::modify(
        ctx,
        "minecraft:projectile_count",
        &bow.stack,
        &Situation::default(),
        0.0,
    )
    .max(0.0) as usize;
    let creative = actor.game_mode == 1;
    let first = match ammo(ctx, actor.uuid, hand) {
        Some(a) => {
            if !creative {
                held::consume(ctx, actor.uuid, a.slot, 1);
            }
            a.stack.with_count(1)
        }
        None if creative => {
            let Some(arrow) = ctx.items.id("minecraft:arrow") else {
                return;
            };
            let mut s = ItemStack::new(arrow, 1);
            // Made from nothing: only creative players take it back.
            s.components.set(
                "minecraft:intangible_projectile",
                Tag::Compound(Default::default()),
            );
            s
        }
        None => return,
    };
    let list = vec![first; count];
    {
        let mut inventories = lock(&ctx.inventories);
        let Some(hands) = inventories.get_mut(&actor.uuid) else {
            return;
        };
        let Some(s) = hands.slots[bow.slot].as_mut() else {
            return;
        };
        set_loaded(s, &list);
        let _ = ctx.events.send(Event::InventorySlot {
            uuid: actor.uuid,
            slot: bow.slot as i16,
            item: Some(s.clone()),
        });
    }
    if let (_, _, Some(end)) = sounds(ctx, &bow.stack) {
        fx::sound(
            ctx,
            &end,
            fx::category::PLAYERS,
            [actor.pose.x, actor.pose.y, actor.pose.z],
            1.0,
            1.0 / (hits::rnd() as f32 * 0.5 + 1.0) + 0.2,
        );
    }
}

/// `v` turned by `degrees` about the axis `k` (unit length).
fn rotate(v: [f64; 3], k: [f64; 3], degrees: f64) -> [f64; 3] {
    let (s, c) = degrees.to_radians().sin_cos();
    let cross = [
        k[1] * v[2] - k[2] * v[1],
        k[2] * v[0] - k[0] * v[2],
        k[0] * v[1] - k[1] * v[0],
    ];
    let dot = k[0] * v[0] + k[1] * v[1] + k[2] * v[2];
    [0, 1, 2].map(|i| v[i] * c + cross[i] * s + k[i] * dot * (1.0 - c))
}

/// The angles of `count` projectiles spread over `spread` degrees either
/// side: 0, then alternating sides.
pub fn angles(count: usize, spread: f32) -> Vec<f32> {
    let step = if count <= 1 {
        0.0
    } else {
        2.0 * spread / (count - 1) as f32
    };
    let start = ((count as i32 - 1) % 2) as f32 * step / 2.0;
    let mut sign = 1.0;
    (0..count)
        .map(|i| {
            let a = start + sign * i.div_ceil(2) as f32 * step;
            sign = -sign;
            a
        })
        .collect()
}

/// Shoots what the crossbow in `hand` holds.
pub fn shoot(ctx: &ServerContext, actor: Actor, hand: i32) {
    let Some(bow) = held::held(ctx, actor.uuid, hand).filter(|h| h.name == "minecraft:crossbow")
    else {
        return;
    };
    let list = loaded(&bow.stack);
    if list.is_empty() {
        return;
    }
    crate::criteria::with_item(ctx, actor.uuid, "minecraft:shot_crossbow", &bow.stack);
    let spread = ee::modify(
        ctx,
        "minecraft:projectile_spread",
        &bow.stack,
        &Situation::default(),
        0.0,
    );
    let pierce = ee::modify(
        ctx,
        "minecraft:projectile_piercing",
        &bow.stack,
        &Situation::default(),
        0.0,
    ) as i32;
    let creative = actor.game_mode == 1;
    let at = [actor.pose.x, actor.pose.y + 1.62 - 0.1, actor.pose.z];
    let view = super::look(actor.pose.yaw, actor.pose.pitch);
    let up = super::look(actor.pose.yaw, actor.pose.pitch - 90.0);
    let mut wear = 0;
    for (i, (stack, angle)) in list.iter().zip(angles(list.len(), spread)).enumerate() {
        let dir = rotate(view, up, f64::from(angle));
        let name = ctx.items.name(stack.item).unwrap_or("");
        if name == "minecraft:firework_rocket" {
            let shot = super::firework::rocket(stack, Some(actor.uuid), true);
            let from = [actor.pose.x, actor.pose.y + 1.62 - 0.15, actor.pose.z];
            super::shoot(
                ctx,
                Shot::Firework(shot),
                Some(actor.uuid),
                from,
                dir,
                1.6,
                1.0,
            );
            wear += 3;
        } else {
            let mut a = Arrow::new(stack.item);
            a.spectral = name == "minecraft:spectral_arrow";
            if name == "minecraft:tipped_arrow" {
                a.effects = super::potions::contents(held::comps_of(stack).as_ref());
            }
            a.stack = Some(stack.clone());
            a.crit = true;
            a.crossbow = true;
            a.pierce = pierce.max(0);
            if creative
                || i > 0
                || stack
                    .components
                    .get("minecraft:intangible_projectile")
                    .is_some()
            {
                a.pickup = Pickup::Creative;
            }
            super::shoot(ctx, Shot::Arrow(a), Some(actor.uuid), at, dir, 3.15, 1.0);
            wear += 1;
        }
        let pitch = if i == 0 {
            1.0
        } else {
            1.0 / (hits::rnd() as f32 * 0.5 + 1.8) + if i % 2 == 1 { 0.63 } else { 0.43 }
        };
        fx::sound(
            ctx,
            "item.crossbow.shoot",
            fx::category::PLAYERS,
            at,
            1.0,
            pitch,
        );
    }
    {
        let mut inventories = lock(&ctx.inventories);
        if let Some(s) = inventories
            .get_mut(&actor.uuid)
            .and_then(|h| h.slots[bow.slot].as_mut())
        {
            set_loaded(s, &[]);
        }
    }
    if !creative {
        held::wear(ctx, actor.uuid, bow.slot, wear);
    } else {
        held::changed(ctx, actor.uuid, &[bow.slot]);
    }
}

/// A rocket loaded into nothing: for the tests.
#[cfg(test)]
pub fn rocket_stack(ctx: &ServerContext, flight: i8, stars: usize) -> ItemStack {
    let mut s = ItemStack::new(ctx.items.id("minecraft:firework_rocket").unwrap(), 1);
    let mut f = mc_nbt::Compound::new();
    f.insert("flight_duration", Tag::Byte(flight));
    let mut star = mc_nbt::Compound::new();
    star.insert("shape", Tag::String("large_ball".into()));
    star.insert("colors", Tag::IntArray(vec![0xff0000]));
    f.insert(
        "explosions",
        Tag::List((0..stars).map(|_| Tag::Compound(star.clone())).collect()),
    );
    s.components.set("minecraft:fireworks", Tag::Compound(f));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{flat, join, stack, wear};
    use mc_entity::{Entity, Kind};

    const U: u128 = 0x9001;

    fn actor(ctx: &ServerContext) -> Actor {
        let p = lock(&ctx.players).get(&U).cloned().unwrap();
        Actor {
            uuid: U,
            entity_id: p.entity_id,
            game_mode: p.game_mode,
            pose: p.pose.unwrap(),
            sneaking: false,
        }
    }

    fn bow(ctx: &ServerContext) -> ItemStack {
        lock(&ctx.inventories).get(&U).unwrap().slots[36]
            .clone()
            .unwrap()
    }

    fn shots(ctx: &ServerContext) -> Vec<Entity> {
        std::mem::take(&mut *lock(&ctx.entities))
            .into_iter()
            .filter(|e| matches!(e.kind, Kind::Projectile(_)))
            .collect()
    }

    #[test]
    fn quick_charge_shortens_the_charge_and_spread_alternates() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        assert_eq!(charge_ticks(&ctx, &stack(&ctx, "crossbow", &[])), 25);
        assert_eq!(
            charge_ticks(&ctx, &stack(&ctx, "crossbow", &[("quick_charge", 1)])),
            20
        );
        assert_eq!(
            charge_ticks(&ctx, &stack(&ctx, "crossbow", &[("quick_charge", 3)])),
            10
        );
        assert_eq!(angles(1, 0.0), vec![0.0]);
        assert_eq!(angles(3, 10.0), vec![0.0, -10.0, 10.0]);
    }

    #[test]
    fn multishot_loads_three_for_one_arrow_and_shoots_them_all() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 9101, [0.5, -60.0, 0.5], 0);
        wear(
            &ctx,
            U,
            36,
            stack(&ctx, "crossbow", &[("multishot", 1), ("piercing", 2)]),
        );
        let arrow = ctx.items.id("minecraft:arrow").unwrap();
        wear(&ctx, U, 9, ItemStack::new(arrow, 2));
        let h = held::held(&ctx, U, 0).unwrap();
        assert!(use_crossbow(&ctx, actor(&ctx), 0, &h));
        // Let go too early: nothing loads.
        release(&ctx, actor(&ctx), 0, 24);
        assert!(loaded(&bow(&ctx)).is_empty());
        release(&ctx, actor(&ctx), 0, 25);
        let list = loaded(&bow(&ctx));
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|s| s.item == arrow && s.count == 1));
        assert_eq!(
            lock(&ctx.inventories).get(&U).unwrap().slots[9]
                .as_ref()
                .unwrap()
                .count,
            1
        );
        // Loaded, a click shoots.
        let h = held::held(&ctx, U, 0).unwrap();
        assert!(use_crossbow(&ctx, actor(&ctx), 0, &h));
        assert!(loaded(&bow(&ctx)).is_empty());
        assert_eq!(bow(&ctx).damage, 3);
        let shots = shots(&ctx);
        assert_eq!(shots.len(), 3);
        let mut pickups = Vec::new();
        for e in &shots {
            let Kind::Projectile(p) = &e.kind else {
                panic!()
            };
            let Shot::Arrow(a) = &p.shot else { panic!() };
            assert!(a.crit && a.crossbow && a.pierce == 2);
            let speed = e.vel.iter().map(|c| c * c).sum::<f64>().sqrt();
            assert!((speed - 3.15).abs() < 0.3, "{speed}");
            pickups.push(a.pickup);
        }
        assert_eq!(
            pickups,
            vec![Pickup::Yes, Pickup::Creative, Pickup::Creative]
        );
        // The side shots leave 10° off the middle one.
        let flat = |e: &Entity| e.vel[0].atan2(e.vel[2]).to_degrees();
        let spread = (flat(&shots[1]) - flat(&shots[2])).abs();
        assert!((spread - 20.0).abs() < 4.0, "{spread}");
    }

    #[test]
    fn a_rocket_in_the_other_hand_is_shot_straight() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        join(&ctx, U, 9102, [0.5, -60.0, 0.5], 0);
        wear(&ctx, U, 36, stack(&ctx, "crossbow", &[]));
        wear(&ctx, U, 45, rocket_stack(&ctx, 2, 1));
        let arrow = ctx.items.id("minecraft:arrow").unwrap();
        wear(&ctx, U, 9, ItemStack::new(arrow, 2));
        let h = held::held(&ctx, U, 0).unwrap();
        use_crossbow(&ctx, actor(&ctx), 0, &h);
        release(&ctx, actor(&ctx), 0, 30);
        assert!(lock(&ctx.inventories).get(&U).unwrap().slots[45].is_none());
        let h = held::held(&ctx, U, 0).unwrap();
        use_crossbow(&ctx, actor(&ctx), 0, &h);
        assert_eq!(bow(&ctx).damage, 3);
        let shots = shots(&ctx);
        let Kind::Projectile(p) = &shots[0].kind else {
            panic!()
        };
        let Shot::Firework(f) = &p.shot else { panic!() };
        assert!(f.shot_at_angle && f.stars.len() == 1);
        assert!(
            f.stack
                .as_ref()
                .unwrap()
                .components
                .get("minecraft:fireworks")
                .is_some()
        );
        let speed = shots[0].vel.iter().map(|c| c * c).sum::<f64>().sqrt();
        assert!((speed - 1.6).abs() < 0.2);
    }
}
