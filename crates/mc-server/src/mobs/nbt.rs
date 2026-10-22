//! Mobs as entity NBT (minecraft.wiki, "Entity format" and the pages of each
//! mob): what `/summon` may set, and what `/data` shows.

use mc_entity::mob::{Gear, slot};
use mc_entity::{Mob, Species};
use mc_nbt::{Compound, Tag};

use crate::context::ServerContext;

/// Equipment slots by their NBT names.
const SLOTS: [(&str, usize); 8] = [
    ("mainhand", slot::MAINHAND),
    ("offhand", slot::OFFHAND),
    ("feet", slot::FEET),
    ("legs", slot::LEGS),
    ("chest", slot::CHEST),
    ("head", slot::HEAD),
    ("body", slot::BODY),
    ("saddle", slot::SADDLE),
];

fn int(c: &Compound, key: &str) -> Option<i32> {
    match c.get(key)? {
        Tag::Byte(v) => Some(i32::from(*v)),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Int(v) => Some(*v),
        Tag::Long(v) => Some(*v as i32),
        Tag::Float(v) => Some(*v as i32),
        Tag::Double(v) => Some(*v as i32),
        _ => None,
    }
}

fn float(c: &Compound, key: &str) -> Option<f32> {
    match c.get(key)? {
        Tag::Float(v) => Some(*v),
        Tag::Double(v) => Some(*v as f32),
        _ => int(c, key).map(|v| v as f32),
    }
}

fn flag(c: &Compound, key: &str) -> Option<bool> {
    int(c, key).map(|v| v != 0)
}

fn string<'a>(c: &'a Compound, key: &str) -> Option<&'a str> {
    match c.get(key)? {
        Tag::String(s) => Some(s),
        _ => None,
    }
}

fn uuid(c: &Compound, key: &str) -> Option<u128> {
    match c.get(key)? {
        Tag::IntArray(v) if v.len() == 4 => Some(
            v.iter()
                .fold(0u128, |acc, &x| (acc << 32) | u128::from(x as u32)),
        ),
        Tag::String(s) => crate::util::parse_uuid(s),
        _ => None,
    }
}

fn uuid_tag(u: u128) -> Tag {
    Tag::IntArray((0..4).map(|i| (u >> (96 - 32 * i)) as u32 as i32).collect())
}

fn text_of(tag: &Tag) -> Option<String> {
    match tag {
        Tag::String(s) => Some(s.clone()),
        Tag::Compound(c) => string(c, "text").map(str::to_owned),
        _ => None,
    }
}

fn gear(ctx: &ServerContext, tag: &Tag) -> Option<Gear> {
    let Tag::Compound(c) = tag else {
        return None;
    };
    let item = ctx.items.id(&mc_command::args::full_id(string(c, "id")?))?;
    let damage = match c.get("components") {
        Some(Tag::Compound(comps)) => mc_protocol::components::Components::from_nbt(comps).0,
        _ => 0,
    };
    Some(Gear {
        item,
        count: int(c, "count").unwrap_or(1),
        damage,
    })
}

/// Sets what `nbt` says on a mob (`/summon` data).
pub fn apply(ctx: &ServerContext, m: &mut Mob, nbt: &Compound) {
    let s = m.species;
    if let Some(v) = flag(nbt, "NoAI") {
        m.no_ai = v;
    }
    if let Some(v) = flag(nbt, "PersistenceRequired") {
        m.persistent = v;
    }
    if let Some(v) = flag(nbt, "LeftHanded") {
        m.left_handed = v;
    }
    if let Some(v) = flag(nbt, "CanPickUpLoot") {
        m.can_pick_up = v;
    }
    if let Some(name) = nbt.get("CustomName").and_then(text_of) {
        m.custom_name = Some(name);
        m.persistent = true;
    }
    if let Some(v) = int(nbt, "Fire") {
        m.burning = v.max(0);
    }
    if let Some(v) = flag(nbt, "IsBaby") {
        m.set_baby(v);
    }
    if let Some(v) = int(nbt, "Age") {
        m.baby_age = v;
    }
    if let Some(v) = int(nbt, "InLove") {
        m.love = v;
    }
    if let Some(v) = int(nbt, "Color") {
        m.set_color(v as u8);
    }
    if let Some(v) = flag(nbt, "Sheared") {
        m.sheared = v;
    }
    if let Some(v) = flag(nbt, "powered") {
        m.powered = v;
    }
    if let Some(v) = flag(nbt, "ignited") {
        m.ignited = v;
    }
    if let Some(v) = int(nbt, "EggLayTime") {
        m.egg_time = v;
    }
    match s {
        Species::Slime | Species::MagmaCube => {
            if let Some(v) = int(nbt, "Size") {
                m.set_size(v + 1);
            }
        }
        Species::Phantom => {
            if let Some(v) = int(nbt, "size") {
                m.size = v.clamp(0, 64);
            }
        }
        _ => {}
    }
    if let Some(v) = string(nbt, "variant")
        && let Some(id) = super::meta::variant_id(ctx, s, v)
    {
        m.variant = id;
    }
    match s {
        Species::Horse | Species::Parrot => {
            if let Some(v) = int(nbt, "Variant") {
                m.variant = v;
            }
        }
        Species::Rabbit => {
            if let Some(v) = int(nbt, "RabbitType") {
                m.variant = v;
            }
        }
        Species::Fox => {
            if let Some(v) = string(nbt, "Type") {
                m.variant = i32::from(v.ends_with("snow"));
            }
            if let Some(v) = flag(nbt, "Sleeping") {
                m.sitting = v;
            }
        }
        Species::Mooshroom => {
            if let Some(v) = string(nbt, "Type") {
                m.variant = i32::from(v.ends_with("brown"));
            }
        }
        _ => {}
    }
    if let Some(owner) = uuid(nbt, "Owner") {
        m.owner = Some(owner);
        m.persistent = true;
        if s == Species::Wolf {
            m.set_max_health(40.0);
        }
    }
    if let Some(v) = flag(nbt, "Sitting") {
        m.sitting = v;
    }
    if let Some(v) = int(nbt, "CollarColor") {
        m.collar = (v & 15) as u8;
    }
    if let Some(v) = flag(nbt, "Tame") {
        m.tame = v;
    }
    if let Some(v) = int(nbt, "Temper") {
        m.temper = v;
    }
    if let Some(v) = int(nbt, "AngerTime") {
        m.anger = v.max(0);
    }
    for key in [
        "DrownedConversionTime",
        "ConversionTime",
        "StrayConversionTime",
    ] {
        if let Some(v) = int(nbt, key)
            && v >= 0
        {
            m.conversion = v;
        }
    }
    if let Some(Tag::Compound(b)) = nbt.get("carriedBlockState")
        && let Some(name) = string(b, "Name")
    {
        let props: Vec<(String, String)> = match b.get("Properties") {
            Some(Tag::Compound(p)) => p
                .iter()
                .filter_map(|(k, v)| match v {
                    Tag::String(v) => Some((k.to_string(), v.clone())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let refs: Vec<(&str, &str)> = props
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        m.carried = ctx
            .data
            .blocks
            .state_id(&mc_command::args::full_id(name), &refs);
    }
    if let Some(Tag::Compound(v)) = nbt.get("VillagerData") {
        m.villager = Some((
            string(v, "type").unwrap_or("minecraft:plains").to_owned(),
            string(v, "profession")
                .map(|p| p.trim_start_matches("minecraft:").to_owned())
                .filter(|p| p != "none"),
            int(v, "level").unwrap_or(1),
        ));
    }
    if let Some(Tag::Compound(eq)) = nbt.get("equipment") {
        for (name, s) in SLOTS {
            if let Some(t) = eq.get(name) {
                m.gear[s] = gear(ctx, t);
            }
        }
    }
    if let Some(Tag::Compound(dc)) = nbt.get("drop_chances") {
        for (name, s) in SLOTS {
            if let Some(v) = float(dc, name) {
                m.drop_chance[s] = v;
            }
        }
    }
    if let Some(v) = float(nbt, "Health") {
        m.health = v.min(m.max_health).max(0.0);
    }
    m.dirty = true;
}

/// The mob's own keys, for `/data` (next to the entity's common ones).
pub fn write(ctx: &ServerContext, m: &Mob, c: &mut Compound) {
    c.insert("Health", Tag::Float(m.health));
    c.insert(
        "DeathTime",
        Tag::Short(m.dying.map_or(0, |left| 20 - left) as i16),
    );
    c.insert("HurtTime", Tag::Short(m.invulnerable.max(0) as i16));
    c.insert("Fire", Tag::Short(m.burning.max(-1) as i16));
    c.insert("NoAI", Tag::Byte(i8::from(m.no_ai)));
    c.insert("PersistenceRequired", Tag::Byte(i8::from(m.persistent)));
    c.insert("LeftHanded", Tag::Byte(i8::from(m.left_handed)));
    c.insert("CanPickUpLoot", Tag::Byte(i8::from(m.can_pick_up)));
    if let Some(name) = &m.custom_name {
        c.insert("CustomName", Tag::String(name.clone()));
    }
    let mut eq = Compound::new();
    for (name, s) in SLOTS {
        if let Some(g) = &m.gear[s]
            && let Some(id) = ctx.items.name(g.item)
        {
            let mut item = Compound::new();
            item.insert("id", Tag::String(id.to_owned()));
            item.insert("count", Tag::Int(g.count));
            eq.insert(name, Tag::Compound(item));
        }
    }
    if !eq.is_empty() {
        c.insert("equipment", Tag::Compound(eq));
    }
    let s = m.species;
    if s.ageable() {
        c.insert("Age", Tag::Int(m.baby_age));
        c.insert("InLove", Tag::Int(m.love));
    }
    if s.has_babies() && !s.ageable() {
        c.insert("IsBaby", Tag::Byte(i8::from(m.is_baby())));
    }
    if let Some(name) = super::meta::variant_name(ctx, s, m.variant) {
        c.insert("variant", Tag::String(name.to_owned()));
    }
    if let Some(o) = m.owner {
        c.insert("Owner", uuid_tag(o));
    }
    if s.tameable() && !s.horse() {
        c.insert("Sitting", Tag::Byte(i8::from(m.sitting)));
    }
    match s {
        Species::Sheep => {
            c.insert("Color", Tag::Byte(m.color as i8));
            c.insert("Sheared", Tag::Byte(i8::from(m.sheared)));
        }
        Species::Creeper => {
            c.insert("Fuse", Tag::Short(30));
            c.insert("ExplosionRadius", Tag::Byte(3));
            c.insert("powered", Tag::Byte(i8::from(m.powered)));
            c.insert("ignited", Tag::Byte(i8::from(m.ignited)));
        }
        Species::Chicken => {
            c.insert("EggLayTime", Tag::Int(m.egg_time));
        }
        Species::Slime | Species::MagmaCube => {
            c.insert("Size", Tag::Int(m.size - 1));
        }
        Species::Phantom => {
            c.insert("size", Tag::Int(m.size));
        }
        Species::Wolf | Species::Cat => {
            c.insert("CollarColor", Tag::Byte(m.collar as i8));
        }
        Species::Horse | Species::Parrot => {
            c.insert("Variant", Tag::Int(m.variant));
        }
        Species::Rabbit => {
            c.insert("RabbitType", Tag::Int(m.variant));
        }
        Species::Fox => {
            c.insert(
                "Type",
                Tag::String(if m.variant == 1 { "snow" } else { "red" }.into()),
            );
            c.insert("Sleeping", Tag::Byte(i8::from(m.sitting)));
        }
        Species::Mooshroom => {
            c.insert(
                "Type",
                Tag::String(if m.variant == 1 { "brown" } else { "red" }.into()),
            );
        }
        _ => {}
    }
    if s.horse() {
        c.insert("Tame", Tag::Byte(i8::from(m.is_tame())));
        c.insert("Temper", Tag::Int(m.temper));
    }
    if m.anger > 0 {
        c.insert("AngerTime", Tag::Int(m.anger));
    }
    match s {
        Species::Zombie | Species::Husk => {
            c.insert("DrownedConversionTime", Tag::Int(m.conversion));
        }
        Species::ZombieVillager => {
            c.insert("ConversionTime", Tag::Int(m.conversion));
        }
        Species::Skeleton => {
            c.insert("StrayConversionTime", Tag::Int(m.conversion));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_round_trip() {
        let u = 0x0000_0001_0000_0002_0000_0003_0000_0004u128;
        let mut c = Compound::new();
        c.insert("Owner", uuid_tag(u));
        assert_eq!(uuid(&c, "Owner"), Some(u));
    }
}
