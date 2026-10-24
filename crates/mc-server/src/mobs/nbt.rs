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
    let stack = mc_protocol::components::ItemStack::from_nbt_with(c, item);
    Some(Gear {
        item,
        count: int(c, "count").unwrap_or(1).max(1),
        damage: stack.damage,
        components: stack.components,
    })
}

/// Sets what `nbt` says on a mob (`/summon` data).
pub fn apply(ctx: &ServerContext, m: &mut Mob, nbt: &Compound) {
    let s = m.species;
    crate::bosses::load_nbt(m, nbt);
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
    if let Some(v) = flag(nbt, "ChestedHorse") {
        m.chested = v && s.chested();
    }
    if let Some(items @ Tag::List(_)) = nbt.get("Items") {
        m.chest_items = Some(items.clone());
        m.chest_loaded = false;
    }
    if s.llama() {
        if let Some(v) = int(nbt, "Strength") {
            m.strength = v.clamp(1, 5);
        }
        if let Some(v) = int(nbt, "Variant") {
            m.variant = v.clamp(0, 3);
        }
    }
    if let Some(v) = flag(nbt, "CanBreakDoors") {
        m.breaks_doors = v && s.zombie();
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
    if let Some(Tag::Compound(b)) = nbt.get("carriedBlockState") {
        m.carried = crate::persist::state_from(ctx, b);
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
    if let Some(u) = uuid(nbt, "LoveCause") {
        m.love_by = Some(u);
    }
    if let Some(Tag::List(rot)) = nbt.get("Rotation") {
        let angle = |i: usize| match rot.get(i) {
            Some(Tag::Float(v)) => Some(*v),
            _ => None,
        };
        if let Some(yaw) = angle(0) {
            m.yaw = yaw;
            m.head_yaw = yaw;
        }
        if let Some(pitch) = angle(1) {
            m.pitch = pitch;
        }
    }
    if let Some(Tag::Double(v)) = nbt.get("fall_distance") {
        m.fall_distance = *v;
    }
    if let Some(Tag::List(attrs)) = nbt.get("attributes") {
        for a in attrs {
            let Tag::Compound(a) = a else { continue };
            let (Some(id), Some(Tag::Double(base))) = (string(a, "id"), a.get("base")) else {
                continue;
            };
            match id.trim_start_matches("minecraft:") {
                "max_health" => m.set_max_health(*base as f32),
                "movement_speed" => m.speed_attr = *base,
                "jump_strength" => m.jump_strength = *base,
                _ => {}
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
    crate::bosses::save_nbt(m, c);
    c.insert("fall_distance", Tag::Double(m.fall_distance));
    c.insert("Fire", Tag::Short(m.burning.max(-1) as i16));
    c.insert("Health", Tag::Float(m.health));
    c.insert("HurtTime", Tag::Short(m.invulnerable.max(0) as i16));
    c.insert("HurtByTimestamp", Tag::Int(0));
    c.insert(
        "DeathTime",
        Tag::Short(m.dying.map_or(0, |left| 20 - left) as i16),
    );
    c.insert("AbsorptionAmount", Tag::Float(0.0));
    c.insert("attributes", attributes(m));
    c.insert("FallFlying", Tag::Byte(0));
    let mut brain = Compound::new();
    brain.insert("memories", Tag::Compound(Compound::new()));
    c.insert("Brain", Tag::Compound(brain));
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
            let stack = mc_protocol::components::ItemStack {
                damage: g.damage,
                components: g.components.clone(),
                ..mc_protocol::components::ItemStack::new(g.item, g.count)
            };
            eq.insert(name, Tag::Compound(stack.to_nbt_with(id)));
        }
    }
    if !eq.is_empty() {
        c.insert("equipment", Tag::Compound(eq));
    }
    // Only the chances that differ from a new mob's.
    let usual = Mob::new(m.species, 1).drop_chance;
    let mut dc = Compound::new();
    for (name, s) in SLOTS {
        if m.drop_chance[s] != usual[s] {
            dc.insert(name, Tag::Float(m.drop_chance[s]));
        }
    }
    if !dc.is_empty() {
        c.insert("drop_chances", Tag::Compound(dc));
    }
    let s = m.species;
    if s.ageable() {
        c.insert("Age", Tag::Int(m.baby_age));
        c.insert("ForcedAge", Tag::Int(0));
        c.insert("InLove", Tag::Int(m.love));
        if let Some(by) = m.love_by {
            c.insert("LoveCause", uuid_tag(by));
        }
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
    if s.horse() || s.llama() {
        c.insert("Tame", Tag::Byte(i8::from(m.is_tame())));
        c.insert("Temper", Tag::Int(m.temper));
    }
    if s.chested() {
        c.insert("ChestedHorse", Tag::Byte(i8::from(m.chested)));
        if m.chested {
            c.insert(
                "Items",
                m.chest_items
                    .clone()
                    .unwrap_or_else(|| Tag::List(Vec::new())),
            );
        }
    }
    if s.llama() {
        c.insert("Strength", Tag::Int(m.strength));
        c.insert("Variant", Tag::Int(m.variant));
    }
    if s.zombie() {
        c.insert("CanBreakDoors", Tag::Byte(i8::from(m.breaks_doors)));
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
            let (kind, profession, level) = m
                .villager
                .clone()
                .unwrap_or_else(|| ("minecraft:plains".to_owned(), None, 1));
            let mut data = Compound::new();
            data.insert("level", Tag::Int(level));
            data.insert(
                "profession",
                Tag::String(format!(
                    "minecraft:{}",
                    profession.as_deref().unwrap_or("none")
                )),
            );
            data.insert("type", Tag::String(kind));
            c.insert("VillagerData", Tag::Compound(data));
        }
        Species::Enderman => {
            if let Some(state) = m.carried
                && let Some(b) = crate::persist::state_tag(ctx, state)
            {
                c.insert("carriedBlockState", Tag::Compound(b));
            }
        }
        Species::Skeleton => {
            c.insert("StrayConversionTime", Tag::Int(m.conversion));
        }
        _ => {}
    }
}

/// The attributes the mob keeps, with their base values.
fn attributes(m: &Mob) -> Tag {
    let s = m.species;
    let mut list = vec![
        ("max_health", f64::from(m.max_health)),
        ("movement_speed", m.speed_attr),
        ("follow_range", s.follow_range()),
    ];
    if s.attack_damage() > 0.0 {
        list.push(("attack_damage", f64::from(s.attack_damage())));
    }
    if s.armor() > 0.0 {
        list.push(("armor", f64::from(s.armor())));
    }
    if s.horse() {
        list.push(("jump_strength", m.jump_strength));
    }
    Tag::List(
        list.into_iter()
            .map(|(id, base)| {
                let mut a = Compound::new();
                a.insert("id", Tag::String(format!("minecraft:{id}")));
                a.insert("base", Tag::Double(base));
                Tag::Compound(a)
            })
            .collect(),
    )
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
