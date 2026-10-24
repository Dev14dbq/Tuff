//! Each kind of entity as NBT, both ways. Keys and their types follow the
//! wiki's "Entity format": the keys every entity has first (`Pos` ..
//! `UUID`), then the living ones, then the kind's own, then `Passengers`.

use mc_entity::extra::{
    Arrow, Cart, Craft, Decor, Firework, Hung, Pickup, Projectile, Shot, Stand, Vehicle,
};
use mc_entity::trade::{Cost, Merchant, Offer};
use mc_entity::{Entity, Kind, Mob, Species};
use mc_nbt::{Compound, Tag};
use mc_protocol::components::ItemStack;

use super::{Leash, Loaded, leash_of, state_from, state_tag, uuid_of, uuid_tag};
use crate::context::ServerContext;

/// Keys some entities of a kind write and others do not: they never count
/// as not understood.
const SOMETIMES: &[&str] = &[
    "CustomName",
    "Tags",
    "Passengers",
    "leash",
    "Owner",
    "Thrower",
    "owner",
    "AngerTime",
    "equipment",
    "drop_chances",
    "LoveCause",
    "carriedBlockState",
    "VillagerData",
    "Item",
    "Items",
    "LootTable",
    "LootTableSeed",
    "LastOutput",
    "variant",
    "IsBaby",
    "weapon",
    "Sitting",
    "Leash",
    "active_effects",
];

/// Keys written from what is known and from what was read alike.
const MERGED: &[&str] = &["attributes", "Brain"];

fn attribute_id(t: &Tag) -> Option<String> {
    match t {
        Tag::Compound(a) => match a.get("id") {
            Some(Tag::String(id)) => Some(mc_command::args::full_id(id)),
            _ => None,
        },
        _ => None,
    }
}

/// Our attributes with the modifiers they had when read, then the
/// attributes we do not keep, as read.
fn merge_attributes(ours: Vec<Tag>, read: &Tag) -> Vec<Tag> {
    let Tag::List(read) = read else {
        return ours;
    };
    let mut out: Vec<Tag> = ours
        .into_iter()
        .map(|t| {
            let id = attribute_id(&t);
            let old = read.iter().find(|r| attribute_id(r) == id);
            match (t, old) {
                (Tag::Compound(mut a), Some(Tag::Compound(o))) => {
                    if let Some(m) = o.get("modifiers") {
                        a.insert("modifiers", m.clone());
                    }
                    Tag::Compound(a)
                }
                (t, _) => t,
            }
        })
        .collect();
    let known: Vec<Option<String>> = out.iter().map(attribute_id).collect();
    for r in read {
        if !known.contains(&attribute_id(r)) {
            out.push(r.clone());
        }
    }
    out
}

/// Our memories over the ones read.
fn merge_brain(ours: Compound, read: &Tag) -> Compound {
    let Tag::Compound(read) = read else {
        return ours;
    };
    let mut out = read.clone();
    let mut memories = match read.get("memories") {
        Some(Tag::Compound(m)) => m.clone(),
        _ => Compound::new(),
    };
    // A villager's job site is ours to say, there or not.
    memories.remove("minecraft:job_site");
    if let Some(Tag::Compound(m)) = ours.get("memories") {
        for (k, v) in m.iter() {
            memories.insert(k, v.clone());
        }
    }
    out.insert("memories", Tag::Compound(memories));
    out
}

/// Stack size and position of every item a stack's NBT can carry.
fn stack_tag(ctx: &ServerContext, stack: &ItemStack) -> Option<Compound> {
    Some(stack.to_nbt_with(ctx.items.name(stack.item)?))
}

fn stack_of(ctx: &ServerContext, tag: Option<&Tag>) -> Option<ItemStack> {
    let Some(Tag::Compound(c)) = tag else {
        return None;
    };
    let Some(Tag::String(id)) = c.get("id") else {
        return None;
    };
    let item = ctx.items.id(&mc_command::args::full_id(id))?;
    let mut stack = ItemStack::from_nbt_with(c, item);
    stack.count = stack.count.max(1);
    Some(stack)
}

/// A plain stack of `item` (`count` of it).
fn plain(ctx: &ServerContext, item: i32, count: i32) -> Option<Compound> {
    stack_tag(ctx, &ItemStack::new(item, count))
}

fn named(ctx: &ServerContext, name: &str) -> Option<Compound> {
    plain(ctx, ctx.items.id(name)?, 1)
}

fn int(c: &Compound, key: &str) -> Option<i32> {
    match c.get(key)? {
        Tag::Byte(v) => Some(i32::from(*v)),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Int(v) => Some(*v),
        Tag::Long(v) => Some(*v as i32),
        _ => None,
    }
}

fn long(c: &Compound, key: &str) -> Option<i64> {
    match c.get(key)? {
        Tag::Long(v) => Some(*v),
        _ => int(c, key).map(i64::from),
    }
}

fn float(c: &Compound, key: &str) -> Option<f32> {
    match c.get(key)? {
        Tag::Float(v) => Some(*v),
        Tag::Double(v) => Some(*v as f32),
        _ => int(c, key).map(|v| v as f32),
    }
}

fn double(c: &Compound, key: &str) -> Option<f64> {
    match c.get(key)? {
        Tag::Double(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v)),
        _ => int(c, key).map(f64::from),
    }
}

fn flag(c: &Compound, key: &str) -> bool {
    int(c, key).is_some_and(|v| v != 0)
}

fn string<'a>(c: &'a Compound, key: &str) -> Option<&'a str> {
    match c.get(key)? {
        Tag::String(s) => Some(s),
        _ => None,
    }
}

fn byte(v: bool) -> Tag {
    Tag::Byte(i8::from(v))
}

fn doubles(v: [f64; 3]) -> Tag {
    Tag::List(v.iter().map(|d| Tag::Double(*d)).collect())
}

fn triple(c: &Compound, key: &str) -> Option<[f64; 3]> {
    let Some(Tag::List(l)) = c.get(key) else {
        return None;
    };
    let d = |i: usize| match l.get(i) {
        Some(Tag::Double(v)) => Some(*v),
        Some(Tag::Float(v)) => Some(f64::from(*v)),
        _ => None,
    };
    let v = [d(0)?, d(1)?, d(2)?];
    v.iter().all(|x| x.is_finite()).then_some(v)
}

fn int3(c: &Compound, key: &str) -> Option<[i32; 3]> {
    match c.get(key)? {
        Tag::IntArray(v) if v.len() == 3 => Some([v[0], v[1], v[2]]),
        _ => None,
    }
}

/// `Rotation`: yaw and pitch.
fn rotation(c: &Compound) -> (f32, f32) {
    let Some(Tag::List(l)) = c.get("Rotation") else {
        return (0.0, 0.0);
    };
    let f = |i: usize| match l.get(i) {
        Some(Tag::Float(v)) if v.is_finite() => *v,
        _ => 0.0,
    };
    (f(0), f(1))
}

/// The yaw and pitch of a frame or painting facing `facing` (0 down .. 5
/// east), as vanilla stores them.
fn hung_rotation(facing: u8) -> (f32, f32) {
    match facing {
        0 => (0.0, 90.0),
        1 => (0.0, -90.0),
        2 => (180.0, 0.0),
        4 => (90.0, 0.0),
        5 => (270.0, 0.0),
        _ => (0.0, 0.0),
    }
}

/// A painting's `facing` (horizontal: 0 south, 1 west, 2 north, 3 east) from
/// a side, and back.
fn painting_facing(facing: u8) -> i8 {
    match facing {
        4 => 1,
        2 => 2,
        5 => 3,
        _ => 0,
    }
}

fn side_of_painting(facing: i32) -> u8 {
    match facing.rem_euclid(4) {
        1 => 4,
        2 => 2,
        3 => 5,
        _ => 3,
    }
}

/// The entity type name an entity is stored as; `None` for those that are
/// not kept.
fn type_id(e: &Entity) -> Option<String> {
    Some(match &e.kind {
        Kind::Item { .. } => "minecraft:item".into(),
        Kind::FallingBlock { .. } => "minecraft:falling_block".into(),
        Kind::Tnt { .. } => "minecraft:tnt".into(),
        Kind::Merchant(m) if m.wandering => "minecraft:wandering_trader".into(),
        Kind::Merchant(_) => "minecraft:villager".into(),
        Kind::Mob(m) => m.species.id().into(),
        Kind::ExperienceOrb { .. } => "minecraft:experience_orb".into(),
        // Neither a bobber nor a bolt of lightning outlives its moment.
        Kind::Projectile(Projectile {
            shot: Shot::Bobber { .. } | Shot::Lightning { .. } | Shot::Fangs { .. },
            ..
        }) => return None,
        other => format!("minecraft:{}", crate::fx::type_name(other)?),
    })
}

/// The entity as the chunk files keep it; `None` for those that are not
/// kept. `all` holds the entities its passengers and lead knot are among.
pub fn write(ctx: &ServerContext, e: &Entity, all: &[Entity]) -> Option<Compound> {
    let id = type_id(e)?;
    let mut c = Compound::new();
    c.insert("id", Tag::String(id));
    c.insert("Pos", doubles(e.pos));
    c.insert("Motion", doubles(e.vel));
    let (yaw, pitch) = match &e.kind {
        Kind::Mob(m) => (m.yaw, m.pitch),
        Kind::Projectile(p) => (p.yaw, p.pitch),
        Kind::Hung(h) => match h.decor {
            Decor::Stand(_) => (h.yaw, 0.0),
            Decor::LeashKnot => (0.0, 0.0),
            _ => hung_rotation(h.facing),
        },
        Kind::Vehicle(v) => (v.yaw, 0.0),
        _ => (0.0, 0.0),
    };
    c.insert(
        "Rotation",
        Tag::List(vec![Tag::Float(yaw), Tag::Float(pitch)]),
    );
    c.insert("fall_distance", Tag::Double(0.0));
    c.insert("Fire", Tag::Short(-1));
    c.insert("Air", Tag::Short(300));
    c.insert("OnGround", byte(e.on_ground));
    c.insert("Invulnerable", Tag::Byte(0));
    c.insert("PortalCooldown", Tag::Int(0));
    c.insert("UUID", uuid_tag(e.uuid));
    if !e.tags.is_empty() {
        c.insert(
            "Tags",
            Tag::List(e.tags.iter().map(|t| Tag::String(t.clone())).collect()),
        );
    }
    match &e.kind {
        Kind::Item { .. } => item(ctx, e, &mut c)?,
        Kind::ExperienceOrb { value, count } => {
            c.insert("Age", Tag::Short(e.age.min(i16::MAX as u32) as i16));
            c.insert("Health", Tag::Short(5));
            c.insert("Value", Tag::Short(*value as i16));
            c.insert("Count", Tag::Int(*count));
        }
        Kind::Tnt { fuse, owner } => {
            c.insert("fuse", Tag::Short(*fuse as i16));
            c.insert("explosion_power", Tag::Float(4.0));
            if let Some(tnt) = ctx.data.blocks.default_state("minecraft:tnt")
                && let Some(b) = state_tag(ctx, tnt)
            {
                c.insert("block_state", Tag::Compound(b));
            }
            if let Some(o) = owner {
                c.insert("owner", uuid_tag(*o));
            }
        }
        Kind::FallingBlock { state } => {
            c.insert("BlockState", Tag::Compound(state_tag(ctx, *state)?));
            c.insert("Time", Tag::Int(e.age as i32));
            c.insert("DropItem", Tag::Byte(1));
            c.insert("HurtEntities", Tag::Byte(0));
            c.insert("FallHurtMax", Tag::Int(40));
            c.insert("FallHurtAmount", Tag::Float(0.0));
            c.insert("CancelDrop", Tag::Byte(0));
        }
        Kind::Mob(m) => crate::mobs::nbt::write(ctx, m, &mut c),
        Kind::Merchant(m) => merchant(ctx, e, m, &mut c),
        Kind::Projectile(p) => projectile(ctx, e, p, &mut c),
        Kind::Hung(h) => hung(ctx, e, h, &mut c),
        Kind::Vehicle(v) => vehicle(ctx, e, v, &mut c),
        Kind::Crystal(crystal) => crate::bosses::crystal::to_nbt(crystal, &mut c),
    }
    match leash_of(ctx, e, all) {
        Some(Leash::Entity(u)) => {
            let mut l = Compound::new();
            l.insert("UUID", uuid_tag(u));
            c.insert("leash", Tag::Compound(l));
        }
        Some(Leash::Knot(at)) => {
            c.insert("leash", Tag::IntArray(at.to_vec()));
        }
        None => {}
    }
    if let Some(extra) = super::extra_of(ctx, e.uuid) {
        for (k, v) in extra.iter() {
            match (k, c.get(k).cloned()) {
                (_, None) => {
                    c.insert(k, v.clone());
                }
                ("attributes", Some(Tag::List(ours))) => {
                    c.insert("attributes", Tag::List(merge_attributes(ours, v)));
                }
                ("Brain", Some(Tag::Compound(ours))) => {
                    c.insert("Brain", Tag::Compound(merge_brain(ours, v)));
                }
                _ => {}
            }
        }
    }
    let passengers: Vec<Tag> = super::riders(e)
        .iter()
        .filter_map(|r| all.iter().find(|o| o.id == *r && !o.removed))
        .filter_map(|r| write(ctx, r, all).map(Tag::Compound))
        .collect();
    if !passengers.is_empty() {
        c.insert("Passengers", Tag::List(passengers));
    }
    Some(c)
}

fn item(ctx: &ServerContext, e: &Entity, c: &mut Compound) -> Option<()> {
    let stack = e.kind.as_stack()?;
    let Kind::Item {
        pickup_delay,
        health,
        owner,
        thrower,
        ageless,
        ..
    } = e.kind
    else {
        return None;
    };
    let stack = stack_tag(ctx, &stack)?;
    c.insert("Health", Tag::Short(health.ceil() as i16));
    let age = if ageless {
        i16::MIN
    } else {
        e.age.min(i16::MAX as u32) as i16
    };
    c.insert("Age", Tag::Short(age));
    c.insert("PickupDelay", Tag::Short(pickup_delay as i16));
    if let Some(o) = owner {
        c.insert("Owner", uuid_tag(o));
    }
    if let Some(t) = thrower {
        c.insert("Thrower", uuid_tag(t));
    }
    c.insert("Item", Tag::Compound(stack));
    Some(())
}

/// The keys of living entities other than mobs (villagers, armor stands).
fn living(c: &mut Compound, health: f32, attributes: &[(&str, f64)]) {
    c.insert("Health", Tag::Float(health));
    c.insert("HurtTime", Tag::Short(0));
    c.insert("HurtByTimestamp", Tag::Int(0));
    c.insert("DeathTime", Tag::Short(0));
    c.insert("AbsorptionAmount", Tag::Float(0.0));
    c.insert(
        "attributes",
        Tag::List(
            attributes
                .iter()
                .map(|(id, base)| {
                    let mut a = Compound::new();
                    a.insert("id", Tag::String(format!("minecraft:{id}")));
                    a.insert("base", Tag::Double(*base));
                    Tag::Compound(a)
                })
                .collect(),
        ),
    );
    c.insert("FallFlying", Tag::Byte(0));
}

fn cost_tag(ctx: &ServerContext, cost: Cost) -> Option<Tag> {
    plain(ctx, cost.item, cost.count).map(Tag::Compound)
}

fn merchant(ctx: &ServerContext, e: &Entity, m: &Merchant, c: &mut Compound) {
    living(
        c,
        ctx.mobs.villager_health(e.id),
        &[("max_health", 20.0), ("movement_speed", 0.5)],
    );
    let mut memories = Compound::new();
    for (name, site) in [
        ("minecraft:home", m.home),
        ("minecraft:meeting_point", m.meeting),
        ("minecraft:job_site", m.job_site),
    ] {
        let Some((x, y, z)) = site else { continue };
        let mut at = Compound::new();
        at.insert("pos", Tag::IntArray(vec![x, y, z]));
        at.insert(
            "dimension",
            Tag::String(crate::context::DIMENSIONS[ctx.dim].into()),
        );
        let mut memory = Compound::new();
        memory.insert("value", Tag::Compound(at));
        memories.insert(name, Tag::Compound(memory));
    }
    let mut brain = Compound::new();
    brain.insert("memories", Tag::Compound(memories));
    c.insert("Brain", Tag::Compound(brain));
    c.insert("CanPickUpLoot", byte(!m.wandering));
    c.insert("PersistenceRequired", Tag::Byte(0));
    c.insert("LeftHanded", Tag::Byte(0));
    let recipes: Vec<Tag> = m
        .offers
        .iter()
        .filter_map(|o| {
            let mut r = Compound::new();
            r.insert("buy", cost_tag(ctx, o.cost_a)?);
            if let Some(b) = o.cost_b {
                r.insert("buyB", cost_tag(ctx, b)?);
            }
            r.insert("sell", cost_tag(ctx, o.result)?);
            r.insert("uses", Tag::Int(o.uses));
            r.insert("maxUses", Tag::Int(o.max_uses));
            r.insert("xp", Tag::Int(o.xp));
            r.insert("priceMultiplier", Tag::Float(o.price_multiplier));
            r.insert("demand", Tag::Int(o.demand));
            r.insert("specialPrice", Tag::Int(o.special_price));
            r.insert("rewardExp", byte(o.reward_xp));
            Some(Tag::Compound(r))
        })
        .collect();
    let mut offers = Compound::new();
    offers.insert("Recipes", Tag::List(recipes));
    c.insert("Offers", Tag::Compound(offers));
    if m.wandering {
        let left = crate::trading::TRADER_STAY.saturating_sub(e.age);
        c.insert("DespawnDelay", Tag::Int(left as i32));
        return;
    }
    let mut data = Compound::new();
    data.insert("level", Tag::Int(m.level));
    data.insert(
        "profession",
        Tag::String(format!(
            "minecraft:{}",
            m.profession.as_deref().unwrap_or("none")
        )),
    );
    data.insert("type", Tag::String(m.variant.clone()));
    c.insert("VillagerData", Tag::Compound(data));
    c.insert("Xp", Tag::Int(m.xp));
    c.insert("LastRestock", Tag::Long(m.last_restock));
    c.insert("RestocksToday", Tag::Int(m.restocks_today));
    c.insert("Age", Tag::Int(m.age));
    c.insert("FoodLevel", Tag::Byte(m.food.clamp(0, 127) as i8));
    let inventory: Vec<Tag> = m
        .pantry
        .iter()
        .filter_map(|&cost| cost_tag(ctx, cost))
        .collect();
    c.insert("Inventory", Tag::List(inventory));
    let gossips: Vec<Tag> = m
        .gossip
        .entries()
        .map(|(about, kind, value)| {
            let mut g = Compound::new();
            g.insert("Target", uuid_tag(about));
            g.insert("Type", Tag::String(kind.name().to_owned()));
            g.insert("Value", Tag::Int(value));
            Tag::Compound(g)
        })
        .collect();
    c.insert("Gossips", Tag::List(gossips));
}

/// `potion_contents` of a list of effects and a colour.
fn potion_contents(effects: &[mc_entity::extra::EffectSpec], color: Option<i32>) -> Tag {
    let mut p = Compound::new();
    if let Some(color) = color {
        p.insert("custom_color", Tag::Int(color));
    }
    if !effects.is_empty() {
        p.insert(
            "custom_effects",
            Tag::List(
                effects
                    .iter()
                    .map(|(id, amplifier, duration)| {
                        let mut e = Compound::new();
                        e.insert("id", Tag::String(id.clone()));
                        e.insert("amplifier", Tag::Byte(*amplifier as i8));
                        e.insert("duration", Tag::Int(*duration));
                        Tag::Compound(e)
                    })
                    .collect(),
            ),
        );
    }
    Tag::Compound(p)
}

fn with_component(mut stack: Compound, name: &str, value: Tag) -> Compound {
    let mut comps = match stack.get("components") {
        Some(Tag::Compound(c)) => c.clone(),
        _ => Compound::new(),
    };
    comps.insert(name, value);
    stack.insert("components", Tag::Compound(comps));
    stack
}

fn projectile(ctx: &ServerContext, e: &Entity, p: &Projectile, c: &mut Compound) {
    if let Some(o) = p.owner {
        c.insert("Owner", uuid_tag(o));
    }
    if !matches!(p.shot, Shot::Cloud { .. }) {
        // Projectiles here start outside their owner's box.
        c.insert("HasBeenShot", Tag::Byte(1));
        c.insert("LeftOwner", Tag::Byte(1));
    }
    match &p.shot {
        Shot::Arrow(a) => {
            c.insert("crit", byte(a.crit));
            c.insert("damage", Tag::Double(a.damage));
            c.insert("inGround", byte(a.stuck > 0));
            c.insert("life", Tag::Short(a.stuck.min(i32::from(i16::MAX)) as i16));
            c.insert(
                "pickup",
                Tag::Byte(match a.pickup {
                    Pickup::No => 0,
                    Pickup::Yes => 1,
                    Pickup::Creative => 2,
                }),
            );
            c.insert("PierceLevel", Tag::Byte(a.pierce as i8));
            c.insert("shake", Tag::Byte(0));
            c.insert("ShotFromCrossbow", Tag::Byte(0));
            c.insert(
                "SoundEvent",
                Tag::String("minecraft:entity.arrow.hit".into()),
            );
            if let Some(mut stack) = plain(ctx, a.item, 1) {
                if !a.effects.is_empty() {
                    stack = with_component(
                        stack,
                        "minecraft:potion_contents",
                        potion_contents(&a.effects, None),
                    );
                }
                c.insert("item", Tag::Compound(stack));
            }
            let mut enchants = Compound::new();
            if a.punch > 0 {
                enchants.insert("minecraft:punch", Tag::Int(a.punch));
            }
            if a.flame {
                enchants.insert("minecraft:flame", Tag::Int(1));
            }
            if !enchants.is_empty()
                && let Some(bow) = named(ctx, "minecraft:bow")
            {
                c.insert(
                    "weapon",
                    Tag::Compound(with_component(
                        bow,
                        "minecraft:enchantments",
                        Tag::Compound(enchants),
                    )),
                );
            }
            if a.spectral {
                c.insert("Duration", Tag::Int(200));
            }
            if a.trident {
                c.insert("DealtDamage", Tag::Byte(0));
            }
        }
        Shot::Snowball | Shot::Egg | Shot::Pearl | Shot::Bottle | Shot::Eye { .. } => {
            let name = match p.shot {
                Shot::Snowball => "minecraft:snowball",
                Shot::Egg => "minecraft:egg",
                Shot::Bottle => "minecraft:experience_bottle",
                Shot::Eye { .. } => "minecraft:ender_eye",
                _ => "minecraft:ender_pearl",
            };
            if let Some(stack) = named(ctx, name) {
                c.insert("Item", Tag::Compound(stack));
            }
            if let Shot::Eye { target, survive } = &p.shot {
                // Not in the game's own files: where the eye is flying to.
                c.insert("Target", doubles(*target));
                c.insert("Survive", byte(*survive));
                c.insert("Life", Tag::Int(e.age as i32));
            }
        }
        Shot::Potion {
            item,
            effects,
            color,
            ..
        } => {
            if let Some(stack) = plain(ctx, *item, 1) {
                let stack = with_component(
                    stack,
                    "minecraft:potion_contents",
                    potion_contents(effects, Some(*color)),
                );
                c.insert("Item", Tag::Compound(stack));
            }
        }
        Shot::Firework(f) => {
            c.insert("Life", Tag::Int(e.age as i32));
            c.insert("LifeTime", Tag::Int(e.age as i32 + f.life));
            c.insert("ShotAtAngle", byte(f.shot_at_angle));
            if let Some(stack) = named(ctx, "minecraft:firework_rocket") {
                let mut fw = Compound::new();
                fw.insert(
                    "explosions",
                    Tag::List(
                        f.stars
                            .iter()
                            .map(|colors| {
                                let mut s = Compound::new();
                                s.insert("shape", Tag::String("small_ball".into()));
                                s.insert("colors", Tag::IntArray(colors.clone()));
                                Tag::Compound(s)
                            })
                            .collect(),
                    ),
                );
                fw.insert("flight_duration", Tag::Byte(1));
                c.insert(
                    "FireworksItem",
                    Tag::Compound(with_component(
                        stack,
                        "minecraft:fireworks",
                        Tag::Compound(fw),
                    )),
                );
            }
        }
        Shot::Cloud {
            effects,
            color,
            radius,
            left,
        } => {
            c.insert("Age", Tag::Int(e.age as i32));
            c.insert("Duration", Tag::Int(e.age as i32 + left));
            c.insert("WaitTime", Tag::Int(10));
            c.insert("ReapplicationDelay", Tag::Int(20));
            c.insert("DurationOnUse", Tag::Int(0));
            c.insert("Radius", Tag::Float(*radius));
            c.insert("RadiusOnUse", Tag::Float(-0.5));
            c.insert("RadiusPerTick", Tag::Float(-0.005));
            c.insert("potion_duration_scale", Tag::Float(0.25));
            c.insert("potion_contents", potion_contents(effects, Some(*color)));
        }
        Shot::Fireball { small } => {
            c.insert("acceleration_power", Tag::Double(0.1));
            if !small {
                c.insert("ExplosionPower", Tag::Byte(1));
            }
            if let Some(stack) = named(ctx, "minecraft:fire_charge") {
                c.insert("Item", Tag::Compound(stack));
            }
        }
        Shot::WitherSkull { dangerous } => {
            c.insert("acceleration_power", Tag::Double(0.1));
            c.insert("dangerous", byte(*dangerous));
        }
        Shot::DragonFireball | Shot::WindCharge { .. } => {
            c.insert("acceleration_power", Tag::Double(0.1));
        }
        Shot::Spit
        | Shot::Breath { .. }
        | Shot::Lightning { .. }
        | Shot::Bobber { .. }
        | Shot::Fangs { .. } => {}
    }
}

/// Armor stand slots in [`Stand::gear`] order, by their NBT names.
const STAND_SLOTS: [&str; 6] = ["mainhand", "offhand", "feet", "legs", "chest", "head"];
const POSE: [&str; 6] = ["Head", "Body", "LeftArm", "RightArm", "LeftLeg", "RightLeg"];

fn hung(ctx: &ServerContext, e: &Entity, h: &Hung, c: &mut Compound) {
    let (dx, dy, dz) = crate::blocks::FACES[usize::from(h.facing.min(5))];
    let cell = [h.anchor[0] + dx, h.anchor[1] + dy, h.anchor[2] + dz];
    match &h.decor {
        Decor::Frame(f) => {
            c.insert("block_pos", Tag::IntArray(cell.to_vec()));
            c.insert("Facing", Tag::Byte(h.facing as i8));
            if let Some(stack) = f.item.as_ref().and_then(|s| stack_tag(ctx, s)) {
                c.insert("Item", Tag::Compound(stack));
            }
            c.insert("ItemDropChance", Tag::Float(1.0));
            c.insert("ItemRotation", Tag::Byte(f.rotation as i8));
            c.insert("Invisible", byte(f.invisible));
            c.insert("Fixed", byte(f.fixed));
        }
        Decor::Painting { variant, .. } => {
            c.insert("block_pos", Tag::IntArray(cell.to_vec()));
            c.insert("facing", Tag::Byte(painting_facing(h.facing)));
            c.insert("variant", Tag::String(variant.clone()));
        }
        Decor::LeashKnot => {
            c.insert("block_pos", Tag::IntArray(h.anchor.to_vec()));
        }
        Decor::Stand(s) => {
            living(c, s.health, &[("max_health", 20.0)]);
            let mut eq = Compound::new();
            for (i, name) in STAND_SLOTS.iter().enumerate() {
                if let Some(stack) = s.gear[i].as_ref().and_then(|g| stack_tag(ctx, g)) {
                    eq.insert(*name, Tag::Compound(stack));
                }
            }
            if !eq.is_empty() {
                c.insert("equipment", Tag::Compound(eq));
            }
            c.insert("Invisible", byte(s.invisible));
            c.insert("Small", byte(s.small));
            c.insert("ShowArms", byte(s.arms));
            c.insert("DisabledSlots", Tag::Int(0));
            c.insert("NoBasePlate", byte(s.no_base));
            c.insert("Marker", byte(s.marker));
            let mut pose = Compound::new();
            for (i, key) in POSE.iter().enumerate() {
                pose.insert(
                    *key,
                    Tag::List(s.pose[i].iter().map(|a| Tag::Float(*a)).collect()),
                );
            }
            c.insert("Pose", Tag::Compound(pose));
        }
    }
    let _ = e;
}

fn vehicle(ctx: &ServerContext, e: &Entity, v: &Vehicle, c: &mut Compound) {
    let holds = match &v.craft {
        Craft::Boat { chest, .. } => *chest,
        Craft::Minecart(cart) => matches!(cart, Cart::Chest | Cart::Hopper),
    };
    if holds && let Some(contents) = crate::containers::entity_contents(ctx, e.id) {
        for (k, val) in contents.iter() {
            c.insert(k, val.clone());
        }
    }
    if holds && c.get("Items").is_none() && c.get("LootTable").is_none() {
        c.insert("Items", Tag::List(Vec::new()));
    }
    let Craft::Minecart(cart) = &v.craft else {
        return;
    };
    match cart {
        Cart::Hopper => {
            c.insert("Enabled", byte(v.enabled));
            let cooldown = crate::tick::lock(&ctx.cart_rests)
                .get(&e.id)
                .copied()
                .unwrap_or(0);
            c.insert("TransferCooldown", Tag::Int(cooldown));
        }
        Cart::Furnace => {
            c.insert("Fuel", Tag::Short(v.fuel.min(i32::from(i16::MAX)) as i16));
            let len = e.vel[0].hypot(e.vel[2]);
            let (px, pz) = if v.fuel > 0 && len > 1e-4 {
                (e.vel[0] / len, e.vel[2] / len)
            } else {
                (0.0, 0.0)
            };
            c.insert("PushX", Tag::Double(px));
            c.insert("PushZ", Tag::Double(pz));
        }
        Cart::Tnt => {
            c.insert("fuse", Tag::Int(v.fuse));
            c.insert("explosion_power", Tag::Float(4.0));
            c.insert("explosion_speed_factor", Tag::Float(1.0));
        }
        Cart::Command => {
            let cmd = &v.command;
            c.insert("Command", Tag::String(cmd.command.clone()));
            c.insert("SuccessCount", Tag::Int(cmd.success_count));
            if let Some(out) = &cmd.last_output {
                c.insert("LastOutput", Tag::String(out.clone()));
            }
            c.insert("TrackOutput", byte(cmd.track_output));
        }
        Cart::Rideable | Cart::Chest => {}
    }
}

/// The entity (and those riding it, after it) that `c` stores; nothing if
/// its type is not understood.
pub fn read(ctx: &ServerContext, c: &Compound) -> Vec<Loaded> {
    let Some(id) = string(c, "id") else {
        return Vec::new();
    };
    let name = mc_command::args::full_id(id);
    let Some(pos) = triple(c, "Pos") else {
        return Vec::new();
    };
    let (yaw, pitch) = rotation(c);
    let mut loaded = Loaded {
        entity: Entity {
            id: ctx.next_entity_id(),
            uuid: uuid_of(c.get("UUID")).unwrap_or_else(crate::util::random_uuid),
            kind: Kind::ExperienceOrb { value: 0, count: 0 },
            pos,
            vel: triple(c, "Motion").unwrap_or([0.0; 3]),
            on_ground: flag(c, "OnGround"),
            age: 0,
            removed: false,
            tags: match c.get("Tags") {
                Some(Tag::List(l)) => l
                    .iter()
                    .filter_map(|t| match t {
                        Tag::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            },
        },
        leash: match c.get("leash") {
            Some(Tag::Compound(l)) => uuid_of(l.get("UUID")).map(Leash::Entity),
            Some(Tag::IntArray(v)) if v.len() == 3 => Some(Leash::Knot([v[0], v[1], v[2]])),
            _ => None,
        },
        contents: None,
        merchant_health: None,
        cooldown: None,
        extra: Compound::new(),
    };
    let e = &mut loaded.entity;
    let age = |key: &str| int(c, key).unwrap_or(0).max(0) as u32;
    e.kind = match name.as_str() {
        "minecraft:item" => {
            let Some(stack) = stack_of(ctx, c.get("Item")) else {
                return Vec::new();
            };
            let raw_age = int(c, "Age").unwrap_or(0);
            let mut kind = Kind::stack(&stack, int(c, "PickupDelay").unwrap_or(0));
            if let Kind::Item {
                health,
                owner,
                thrower,
                ageless,
                ..
            } = &mut kind
            {
                *health = float(c, "Health").unwrap_or(5.0);
                *owner = uuid_of(c.get("Owner"));
                *thrower = uuid_of(c.get("Thrower"));
                *ageless = raw_age == i32::from(i16::MIN);
            }
            e.age = raw_age.max(0) as u32;
            kind
        }
        "minecraft:experience_orb" => {
            e.age = age("Age");
            Kind::ExperienceOrb {
                value: int(c, "Value").unwrap_or(0),
                count: int(c, "Count").unwrap_or(1).max(1),
            }
        }
        "minecraft:tnt" => Kind::Tnt {
            fuse: int(c, "fuse").unwrap_or(80),
            owner: uuid_of(c.get("owner")),
        },
        "minecraft:falling_block" => {
            let Some(Tag::Compound(b)) = c.get("BlockState") else {
                return Vec::new();
            };
            e.age = age("Time");
            Kind::FallingBlock {
                state: state_from(ctx, b).unwrap_or(0),
            }
        }
        "minecraft:villager" | "minecraft:wandering_trader" => {
            let Some(fresh) = crate::trading::new_merchant(ctx, &name, pos) else {
                return Vec::new();
            };
            let Kind::Merchant(mut m) = fresh.kind else {
                return Vec::new();
            };
            read_merchant(ctx, c, &mut m);
            if m.wandering {
                e.age = crate::trading::TRADER_STAY
                    .saturating_sub(int(c, "DespawnDelay").unwrap_or(0).max(0) as u32);
            }
            loaded.merchant_health = float(c, "Health");
            Kind::Merchant(m)
        }
        _ => match Species::from_id(&name) {
            Some(species) => {
                let mut m = Mob::new(species, crate::util::random_u64());
                crate::mobs::nbt::apply(ctx, &mut m, c);
                if c.get("LeftHanded").is_none() {
                    m.left_handed = false;
                }
                Kind::Mob(Box::new(m))
            }
            None => {
                let Some(kind) = projectile_kind(ctx, &name, c)
                    .or_else(|| crate::summon::build(ctx, &name, c, pos, yaw).map(|(k, _)| k))
                else {
                    return Vec::new();
                };
                let mut kind = kind;
                match &mut kind {
                    Kind::Projectile(p) => read_projectile(ctx, c, e, p, yaw, pitch),
                    Kind::Hung(h) => read_hung(ctx, c, h, yaw),
                    Kind::Vehicle(v) => {
                        let (contents, cooldown) = read_vehicle(c, v);
                        loaded.contents = contents;
                        loaded.cooldown = cooldown;
                    }
                    _ => {}
                }
                kind
            }
        },
    };
    // A dragon's parts take the ids after its own.
    if matches!(&e.kind, Kind::Mob(m) if m.species == Species::EnderDragon) {
        e.id = crate::bosses::entity_id(ctx, &e.kind);
    }
    // What nothing here understands is kept as it was.
    let mut extra = c.clone();
    if let Some(ours) = write(ctx, &loaded.entity, &[]) {
        for (k, _) in ours.iter() {
            // These two are merged with ours when written (modifiers,
            // memories nothing here keeps).
            if !MERGED.contains(&k) {
                extra.remove(k);
            }
        }
    }
    for k in SOMETIMES {
        extra.remove(k);
    }
    loaded.extra = extra;
    let mut out = vec![loaded];
    if let Some(Tag::List(list)) = c.get("Passengers") {
        for p in list {
            let Tag::Compound(p) = p else { continue };
            let riders = read(ctx, p);
            if let Some(first) = riders.first()
                && let Some(r) = super::riders_mut(&mut out[0].entity)
            {
                r.push(first.entity.id);
            }
            out.extend(riders);
        }
    }
    out
}

fn read_merchant(ctx: &ServerContext, c: &Compound, m: &mut Merchant) {
    if let Some(Tag::Compound(data)) = c.get("VillagerData") {
        if let Some(t) = string(data, "type") {
            m.variant = mc_command::args::full_id(t);
        }
        m.profession = string(data, "profession")
            .map(|p| p.trim_start_matches("minecraft:").to_owned())
            .filter(|p| p != "none");
        m.level = int(data, "level").unwrap_or(1).clamp(1, 5);
    }
    if let Some(xp) = int(c, "Xp") {
        m.xp = xp;
    }
    m.last_restock = long(c, "LastRestock").unwrap_or(0);
    m.restocks_today = int(c, "RestocksToday").unwrap_or(0);
    m.age = int(c, "Age").unwrap_or(0);
    m.food = int(c, "FoodLevel").unwrap_or(0);
    if let Some(Tag::List(list)) = c.get("Inventory") {
        m.pantry = list
            .iter()
            .filter_map(|t| stack_of(ctx, Some(t)))
            .map(|s| Cost {
                item: s.item,
                count: s.count,
            })
            .collect();
    }
    if let Some(Tag::List(list)) = c.get("Gossips") {
        for g in list {
            let Tag::Compound(g) = g else { continue };
            let (Some(about), Some(Tag::String(kind)), Some(value)) =
                (uuid_of(g.get("Target")), g.get("Type"), int(g, "Value"))
            else {
                continue;
            };
            if let Some(kind) = mc_entity::gossip::Kind::from_name(kind) {
                m.gossip.add(about, kind, value);
            }
        }
    }
    if let Some(Tag::Compound(brain)) = c.get("Brain")
        && let Some(Tag::Compound(mem)) = brain.get("memories")
    {
        let site = |name: &str| {
            let Some(Tag::Compound(site)) = mem.get(name) else {
                return None;
            };
            let Some(Tag::Compound(value)) = site.get("value") else {
                return None;
            };
            int3(value, "pos").map(|p| (p[0], p[1], p[2]))
        };
        m.job_site = site("minecraft:job_site");
        m.home = site("minecraft:home");
        m.meeting = site("minecraft:meeting_point");
    }
    let Some(Tag::Compound(offers)) = c.get("Offers") else {
        return;
    };
    let cost = |t: Option<&Tag>| {
        stack_of(ctx, t).map(|s| Cost {
            item: s.item,
            count: s.count,
        })
    };
    m.offers = match offers.get("Recipes") {
        Some(Tag::List(list)) => list
            .iter()
            .filter_map(|r| {
                let Tag::Compound(r) = r else { return None };
                Some(Offer {
                    cost_a: cost(r.get("buy"))?,
                    cost_b: cost(r.get("buyB"))
                        .filter(|b| ctx.items.name(b.item) != Some("minecraft:air")),
                    result: cost(r.get("sell"))?,
                    uses: int(r, "uses").unwrap_or(0),
                    max_uses: int(r, "maxUses").unwrap_or(4),
                    xp: int(r, "xp").unwrap_or(1),
                    price_multiplier: float(r, "priceMultiplier").unwrap_or(0.0),
                    demand: int(r, "demand").unwrap_or(0),
                    special_price: int(r, "specialPrice").unwrap_or(0),
                    reward_xp: r.get("rewardExp").is_none() || flag(r, "rewardExp"),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
}

/// The projectiles `/summon` does not make.
fn projectile_kind(ctx: &ServerContext, name: &str, c: &Compound) -> Option<Kind> {
    let shot = match name {
        "minecraft:fireball" => Shot::Fireball { small: false },
        "minecraft:small_fireball" => Shot::Fireball { small: true },
        "minecraft:trident" => {
            let mut a = Arrow::new(ctx.items.id("minecraft:trident")?);
            a.trident = true;
            a.damage = 8.0;
            a.crit = flag(c, "crit");
            a.pickup = match int(c, "pickup") {
                Some(0) => Pickup::No,
                Some(2) => Pickup::Creative,
                _ => Pickup::Yes,
            };
            Shot::Arrow(a)
        }
        "minecraft:area_effect_cloud" => {
            let pc = c.get("potion_contents");
            let effects = effects_in(pc);
            Shot::Cloud {
                color: color_in(pc, &effects),
                effects,
                radius: float(c, "Radius").unwrap_or(3.0),
                left: (int(c, "Duration").unwrap_or(600) - int(c, "Age").unwrap_or(0)).max(1),
            }
        }
        _ => return None,
    };
    Some(Kind::Projectile(Projectile {
        shot,
        owner: None,
        mob: None,
        yaw: 0.0,
        pitch: 0.0,
    }))
}

/// Effects in a `potion_contents` (a compound or a potion's name).
fn effects_in(tag: Option<&Tag>) -> Vec<mc_entity::extra::EffectSpec> {
    let mut comps = Compound::new();
    match tag {
        Some(Tag::String(name)) => {
            let mut p = Compound::new();
            p.insert("potion", Tag::String(name.clone()));
            comps.insert("minecraft:potion_contents", Tag::Compound(p));
        }
        Some(t @ Tag::Compound(_)) => {
            comps.insert("minecraft:potion_contents", t.clone());
        }
        _ => return Vec::new(),
    }
    crate::projectiles::potions::contents(Some(&Tag::Compound(comps)))
}

fn color_in(tag: Option<&Tag>, effects: &[mc_entity::extra::EffectSpec]) -> i32 {
    match tag {
        Some(Tag::Compound(p)) => int(p, "custom_color"),
        _ => None,
    }
    .unwrap_or_else(|| crate::projectiles::potions::color(effects))
}

fn components_of(stack: Option<&Tag>) -> Option<&Tag> {
    match stack {
        Some(Tag::Compound(s)) => s.get("components"),
        _ => None,
    }
}

fn read_projectile(
    ctx: &ServerContext,
    c: &Compound,
    e: &mut Entity,
    p: &mut Projectile,
    yaw: f32,
    pitch: f32,
) {
    p.owner = uuid_of(c.get("Owner"));
    (p.yaw, p.pitch) = (yaw, pitch);
    match &mut p.shot {
        Shot::Arrow(a) => {
            read_arrow(ctx, c, a);
        }
        Shot::Potion {
            item,
            effects,
            color,
            ..
        } => {
            if let Some(stack) = stack_of(ctx, c.get("Item")) {
                *item = stack.item;
            }
            let pc = components_of(c.get("Item")).and_then(|t| match t {
                Tag::Compound(cs) => cs.get("minecraft:potion_contents"),
                _ => None,
            });
            *effects = effects_in(pc);
            *color = color_in(pc, effects);
        }
        Shot::Firework(f) => read_firework(c, e, f),
        Shot::Cloud { .. } => e.age = int(c, "Age").unwrap_or(0).max(0) as u32,
        Shot::Eye { target, survive } => {
            e.age = int(c, "Life").unwrap_or(0).max(0) as u32;
            // Saved by the game itself there is no target: the eye hovers
            // where it is.
            *target = triple(c, "Target").unwrap_or(e.pos);
            *survive = c.get("Survive").is_none_or(|_| flag(c, "Survive"));
        }
        _ => {}
    }
}

fn read_arrow(ctx: &ServerContext, c: &Compound, a: &mut Arrow) {
    if let Some(d) = double(c, "damage") {
        a.damage = d;
    }
    a.pierce = int(c, "PierceLevel").unwrap_or(0);
    if flag(c, "inGround") {
        a.stuck = int(c, "life").unwrap_or(0).max(1);
    }
    if let Some(stack) = stack_of(ctx, c.get("item")) {
        a.item = stack.item;
        if ctx.items.name(stack.item) == Some("minecraft:tipped_arrow") {
            a.effects = crate::projectiles::potions::contents(components_of(c.get("item")));
        }
    }
    let weapon = components_of(c.get("weapon"));
    a.punch = crate::held::enchant(weapon, "punch");
    a.flame = crate::held::enchant(weapon, "flame") > 0;
}

fn read_firework(c: &Compound, e: &mut Entity, f: &mut Firework) {
    let flown = int(c, "Life").unwrap_or(0).max(0);
    e.age = flown as u32;
    if let Some(total) = int(c, "LifeTime") {
        f.life = (total - flown).max(1);
    }
    f.shot_at_angle = flag(c, "ShotAtAngle");
    let fw = match components_of(c.get("FireworksItem")) {
        Some(Tag::Compound(cs)) => match cs.get("minecraft:fireworks") {
            Some(Tag::Compound(fw)) => Some(fw.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(Tag::List(stars)) = fw.as_ref().and_then(|fw| fw.get("explosions")) {
        f.stars = stars
            .iter()
            .map(|s| match s {
                Tag::Compound(s) => match s.get("colors") {
                    Some(Tag::IntArray(v)) => v.clone(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            })
            .collect();
    }
}

fn read_hung(ctx: &ServerContext, c: &Compound, h: &mut Hung, yaw: f32) {
    let cell = int3(c, "block_pos")
        .or_else(|| Some([int(c, "TileX")?, int(c, "TileY")?, int(c, "TileZ")?]));
    match &mut h.decor {
        Decor::Frame(_) => {
            h.facing = int(c, "Facing").map_or(3, |f| f.clamp(0, 5) as u8);
        }
        Decor::Painting { variant, size } => {
            h.facing = side_of_painting(int(c, "facing").or_else(|| int(c, "Facing")).unwrap_or(0));
            *size = painting_size(ctx, variant);
        }
        Decor::LeashKnot => {
            if let Some(at) = cell {
                h.anchor = at;
            }
            return;
        }
        Decor::Stand(s) => {
            h.yaw = yaw;
            read_stand(ctx, c, s);
            return;
        }
    }
    if let Some(cell) = cell {
        let (dx, dy, dz) = crate::blocks::FACES[usize::from(h.facing)];
        h.anchor = [cell[0] - dx, cell[1] - dy, cell[2] - dz];
    }
}

/// Width and height in blocks of a painting variant.
fn painting_size(ctx: &ServerContext, variant: &str) -> (i32, i32) {
    let entry = variant.trim_start_matches("minecraft:");
    ctx.data
        .data_pack()
        .read_entry("minecraft:painting_variant", entry)
        .ok()
        .and_then(|v| {
            Some((
                i32::try_from(v.get("width")?.as_i64()?).ok()?,
                i32::try_from(v.get("height")?.as_i64()?).ok()?,
            ))
        })
        .unwrap_or((1, 1))
}

fn read_stand(ctx: &ServerContext, c: &Compound, s: &mut Stand) {
    if let Some(h) = float(c, "Health") {
        s.health = h;
    }
    if let Some(Tag::Compound(eq)) = c.get("equipment") {
        for (i, name) in STAND_SLOTS.iter().enumerate() {
            s.gear[i] = stack_of(ctx, eq.get(name));
        }
    }
}

fn read_vehicle(c: &Compound, v: &mut Vehicle) -> (Option<Compound>, Option<i32>) {
    let mut contents = Compound::new();
    for key in ["Items", "LootTable", "LootTableSeed"] {
        if let Some(t) = c.get(key) {
            contents.insert(key, t.clone());
        }
    }
    let Craft::Minecart(cart) = v.craft else {
        return ((!contents.is_empty()).then_some(contents), None);
    };
    let mut cooldown = None;
    match cart {
        Cart::Hopper => {
            v.enabled = c.get("Enabled").is_none() || flag(c, "Enabled");
            cooldown = int(c, "TransferCooldown");
        }
        Cart::Furnace => v.fuel = int(c, "Fuel").unwrap_or(0).max(0),
        Cart::Tnt => v.fuse = int(c, "fuse").unwrap_or(-1),
        Cart::Command => {
            v.command.success_count = int(c, "SuccessCount").unwrap_or(0);
            v.command.last_output = string(c, "LastOutput").map(str::to_owned);
        }
        Cart::Rideable | Cart::Chest => {}
    }
    ((!contents.is_empty()).then_some(contents), cooldown)
}
