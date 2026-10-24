//! What clients are told about a mob: its entity type, metadata fields
//! (indices and serializer types observed from vanilla 26.3,
//! `observed/commands-mobs*.json`) and equipment.

use std::collections::HashMap;
use std::sync::OnceLock;

use mc_entity::mob::slot;
use mc_entity::{Mob, Species};
use mc_protocol::packets::play::{ItemStack, MetadataValue, metadata};

use crate::context::ServerContext;

/// Field indices.
mod field {
    pub const FLAGS: u8 = 0;
    pub const CUSTOM_NAME: u8 = 2;
    /// `Mob` flags: 1 no AI, 2 left-handed, 4 aggressive.
    pub const MOB_FLAGS: u8 = 15;
    /// Ageable mobs, zombies: a baby.
    pub const BABY: u8 = 16;
    /// Animals: the first field of the kind.
    pub const ANIMAL: u8 = 18;
}

/// Serializer types of the variant holders.
mod holder {
    pub const CAT: i32 = 21;
    pub const COW: i32 = 23;
    pub const WOLF: i32 = 25;
    pub const PIG: i32 = 28;
    pub const CHICKEN: i32 = 30;
    pub const SNIFFER_STATE: i32 = 35;
    pub const ARMADILLO_STATE: i32 = 36;
    // Aquatic (Claude)
    pub const FROG: i32 = 27;
    pub const ZOMBIE_NAUTILUS: i32 = 32;
}

/// The data-driven variant registry of a kind, if it has one.
pub fn variant_registry(species: Species) -> Option<&'static str> {
    Some(match species {
        Species::Cat => "minecraft:cat_variant",
        Species::Cow => "minecraft:cow_variant",
        Species::Wolf => "minecraft:wolf_variant",
        Species::Pig => "minecraft:pig_variant",
        Species::Chicken => "minecraft:chicken_variant",
        // Aquatic (Claude)
        Species::Frog => "minecraft:frog_variant",
        Species::ZombieNautilus => "minecraft:zombie_nautilus_variant",
        _ => return None,
    })
}

/// Entries of a data-driven registry in network order (sorted by id).
pub fn registry(ctx: &ServerContext, name: &str) -> &'static [String] {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<String, &'static [String]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = crate::tick::lock(cache);
    if let Some(v) = cache.get(name) {
        return v;
    }
    let list: &'static [String] = ctx
        .data
        .data_pack()
        .registry_entries(name)
        .unwrap_or_default()
        .leak();
    cache.insert(name.to_owned(), list);
    list
}

/// Network id of a variant by name, in its kind's registry.
pub fn variant_id(ctx: &ServerContext, species: Species, name: &str) -> Option<i32> {
    let reg = variant_registry(species)?;
    let full = mc_command::args::full_id(name);
    registry(ctx, reg)
        .iter()
        .position(|e| *e == full)
        .map(|i| i as i32)
}

/// A variant's name from its network id.
pub fn variant_name(ctx: &ServerContext, species: Species, id: i32) -> Option<&'static str> {
    let reg = variant_registry(species)?;
    registry(ctx, reg).get(id as usize).map(String::as_str)
}

/// The entity type id clients know the species by.
pub fn type_id(ctx: &ServerContext, species: Species) -> i32 {
    ctx.data
        .builtin
        .id("minecraft:entity_type", species.id())
        .unwrap_or(0)
}

fn tame_flags(m: &Mob) -> MetadataValue {
    let mut f = 0;
    if m.sitting || m.perched {
        f |= 1;
    }
    if m.is_tame() {
        f |= 4;
    }
    MetadataValue::Byte(f)
}

/// The entity flags: burning, and invisible or glowing by its effects.
pub fn flags_of(m: &Mob) -> i8 {
    let burning =
        m.burning > 0 && !m.species.fire_immune() || m.species == Species::Blaze && m.charge > 0;
    i8::from(burning) | crate::effects::flags(&m.effects)
}

/// All the metadata fields of a mob, as sent when it appears.
pub fn metadata_of(ctx: &ServerContext, m: &Mob, now: i64) -> Vec<(u8, MetadataValue)> {
    use Species::*;
    let mut v = vec![
        (field::FLAGS, MetadataValue::Byte(flags_of(m))),
        (metadata::HEALTH, MetadataValue::Float(m.health.max(0.0))),
    ];
    v.extend(crate::mob_effects::effect_fields(ctx, m, false));
    v.extend([(
        field::MOB_FLAGS,
        MetadataValue::Byte(
            i8::from(m.no_ai)
                | if m.left_handed { 2 } else { 0 }
                | if m.aggressive() || m.arms_up { 4 } else { 0 },
        ),
    )]);
    if let Some(name) = &m.custom_name {
        let mut c = mc_nbt::Compound::new();
        c.insert("text", mc_nbt::Tag::String(name.clone()));
        v.push((
            field::CUSTOM_NAME,
            MetadataValue::OptComponent(Some(mc_nbt::Tag::Compound(c))),
        ));
    }
    let a = field::ANIMAL;
    if m.species.ageable() || m.species == Parrot {
        v.push((field::BABY, MetadataValue::Boolean(m.is_baby())));
    }
    match m.species {
        Sheep => v.push((
            a,
            MetadataValue::Byte((m.color as i8 & 15) | if m.sheared { 16 } else { 0 }),
        )),
        Cow => v.push((a, MetadataValue::Holder(holder::COW, m.variant))),
        Mooshroom => v.push((a, MetadataValue::VarInt(m.variant))),
        Pig => {
            // Clients run the boost themselves from its length: sent only
            // while it lasts, as a fresh value starts it over.
            if m.boost > 0 {
                v.push((a, MetadataValue::VarInt(m.boost_total)));
            }
            v.push((a + 1, MetadataValue::Holder(holder::PIG, m.variant)));
        }
        Strider => {
            if m.boost > 0 {
                v.push((a, MetadataValue::VarInt(m.boost_total)));
            }
            v.push((a + 1, MetadataValue::Boolean(m.cold)));
        }
        Chicken => v.push((a, MetadataValue::Holder(holder::CHICKEN, m.variant))),
        Rabbit => v.push((a, MetadataValue::VarInt(m.variant))),
        Horse | Donkey | Mule => {
            let mut f = 0;
            if m.is_tame() {
                f |= 2;
            }
            v.push((a, MetadataValue::Byte(f)));
            if m.species == Horse {
                v.push((a + 1, MetadataValue::VarInt(m.variant)));
            } else {
                v.push((a + 1, MetadataValue::Boolean(m.chested)));
            }
        }
        Llama | TraderLlama => {
            v.push((a, MetadataValue::Byte(if m.is_tame() { 2 } else { 0 })));
            v.push((a + 1, MetadataValue::Boolean(m.chested)));
            v.push((a + 2, MetadataValue::VarInt(m.strength)));
            v.push((a + 3, MetadataValue::VarInt(m.variant)));
        }
        Wolf => {
            v.push((a, tame_flags(m)));
            v.push((a + 1, MetadataValue::OptUuid(m.owner)));
            v.push((a + 3, MetadataValue::VarInt(i32::from(m.collar))));
            v.push((
                a + 4,
                MetadataValue::VarLong(if m.anger > 0 {
                    now + i64::from(m.anger)
                } else {
                    -1
                }),
            ));
            v.push((a + 5, MetadataValue::Holder(holder::WOLF, m.variant)));
        }
        Cat => {
            v.push((a, tame_flags(m)));
            v.push((a + 1, MetadataValue::OptUuid(m.owner)));
            v.push((a + 2, MetadataValue::Holder(holder::CAT, m.variant)));
            v.push((a + 3, MetadataValue::Boolean(m.lying)));
            v.push((a + 5, MetadataValue::VarInt(i32::from(m.collar))));
        }
        Parrot => {
            v.push((a, tame_flags(m)));
            v.push((a + 1, MetadataValue::OptUuid(m.owner)));
            v.push((a + 2, MetadataValue::VarInt(m.variant)));
        }
        Fox => {
            v.push((a, MetadataValue::VarInt(m.variant)));
            v.push((a + 1, MetadataValue::Byte(if m.sitting { 32 } else { 0 })));
        }
        Zombie | Husk | Drowned | ZombieVillager | ZombifiedPiglin => {
            v.push((field::BABY, MetadataValue::Boolean(m.is_baby())));
            v.push((
                18,
                MetadataValue::Boolean(m.species != ZombieVillager && m.conversion >= 0),
            ));
            if m.species == ZombieVillager {
                v.push((19, MetadataValue::Boolean(m.conversion >= 0)));
                let (kind, profession, level) =
                    m.villager
                        .clone()
                        .unwrap_or(("minecraft:plains".into(), None, 1));
                let id =
                    |registry: &str, name: &str| ctx.data.builtin.id(registry, name).unwrap_or(0);
                let p = format!("minecraft:{}", profession.as_deref().unwrap_or("none"));
                v.push((
                    20,
                    MetadataValue::VillagerData(
                        id("minecraft:villager_type", &kind),
                        id("minecraft:villager_profession", &p),
                        level,
                    ),
                ));
            }
        }
        Skeleton => v.push((16, MetadataValue::Boolean(m.conversion >= 0))),
        Creeper => {
            v.push((16, MetadataValue::VarInt(m.swell_dir)));
            v.push((17, MetadataValue::Boolean(m.powered)));
            v.push((18, MetadataValue::Boolean(m.ignited)));
        }
        Spider | CaveSpider => v.push((16, MetadataValue::Byte(i8::from(m.climbing)))),
        Enderman => {
            v.push((16, MetadataValue::OptBlockState(m.carried)));
            v.push((17, MetadataValue::Boolean(m.target.is_some())));
            v.push((18, MetadataValue::Boolean(m.stared)));
        }
        Witch => v.push((17, MetadataValue::Boolean(m.drinking > 0))),
        Slime | MagmaCube => v.push((18, MetadataValue::VarInt(m.size))),
        Phantom => v.push((16, MetadataValue::VarInt(m.size))),
        // Bit 16: the pumpkin is on.
        SnowGolem => v.push((
            16,
            MetadataValue::Byte(if m.quirks.pumpkin { 16 } else { 0 }),
        )),
        Ghast => v.push((16, MetadataValue::Boolean(m.charge > 10))),
        Blaze => v.push((16, MetadataValue::Byte(i8::from(m.charge > 0)))),
        Piglin => {
            v.push((16, MetadataValue::Boolean(false)));
            v.push((17, MetadataValue::Boolean(m.is_baby())));
        }
        EnderDragon | Wither => v.extend(crate::bosses::metadata(m)),
        // Land animals (Cursor). Indices 18–20 are the goat fields on the
        // protocol page (screaming, left horn, right horn).
        Goat => {
            v.push((
                metadata::POSE,
                MetadataValue::Pose(if m.quirks.land.jumping { 6 } else { 0 }),
            ));
            v.push((a, MetadataValue::Boolean(m.quirks.land.screaming)));
            v.push((a + 1, MetadataValue::Boolean(m.quirks.land.left_horn)));
            v.push((a + 2, MetadataValue::Boolean(m.quirks.land.right_horn)));
        }
        // Armadillo and sniffer states are enums of their own serializer
        // types (36 and 35); a panda's genes are bytes after three timers.
        Armadillo => {
            let state = if m.quirks.land.rolled { 2 } else { 0 };
            v.push((a, MetadataValue::Holder(holder::ARMADILLO_STATE, state)));
        }
        Sniffer => {
            let state = if m.quirks.land.digging > 0 { 5 } else { 0 };
            v.push((a, MetadataValue::Holder(holder::SNIFFER_STATE, state)));
        }
        Panda => {
            v.push((a + 3, MetadataValue::Byte(m.quirks.land.main_gene as i8)));
            v.push((a + 4, MetadataValue::Byte(m.quirks.land.hidden_gene as i8)));
        }
        // Aquatic (Claude): fields as minecraft.wiki lists them for 26.3.
        Cod => v.push((16, MetadataValue::Boolean(m.quirks.aqua.from_bucket))),
        Salmon => {
            v.push((16, MetadataValue::Boolean(m.quirks.aqua.from_bucket)));
            v.push((17, MetadataValue::VarInt(m.variant)));
        }
        Pufferfish => {
            v.push((16, MetadataValue::Boolean(m.quirks.aqua.from_bucket)));
            v.push((17, MetadataValue::VarInt(i32::from(m.quirks.aqua.puff))));
        }
        TropicalFish => {
            v.push((16, MetadataValue::Boolean(m.quirks.aqua.from_bucket)));
            v.push((17, MetadataValue::VarInt(m.variant)));
        }
        GlowSquid => v.push((18, MetadataValue::VarInt(m.quirks.aqua.dark))),
        Dolphin => {
            v.push((18, MetadataValue::Boolean(m.quirks.aqua.got_fish)));
            v.push((19, MetadataValue::VarInt(m.quirks.aqua.air)));
        }
        Turtle => {
            v.push((18, MetadataValue::Boolean(m.quirks.aqua.has_egg)));
            v.push((19, MetadataValue::Boolean(m.quirks.aqua.laying > 0)));
        }
        Axolotl => {
            v.push((18, MetadataValue::VarInt(m.variant)));
            v.push((19, MetadataValue::Boolean(m.quirks.aqua.dead_play > 0)));
            v.push((20, MetadataValue::Boolean(m.quirks.aqua.from_bucket)));
        }
        Frog => {
            v.push((18, MetadataValue::Holder(holder::FROG, m.variant)));
            v.push((19, MetadataValue::OptVarInt(m.quirks.aqua.caught)));
        }
        Tadpole => {
            v.push((16, MetadataValue::Boolean(m.quirks.aqua.from_bucket)));
            v.push((17, MetadataValue::Boolean(m.quirks.aqua.age_locked)));
        }
        Guardian | ElderGuardian => {
            v.push((16, MetadataValue::Boolean(m.quirks.aqua.retract)));
            v.push((17, MetadataValue::VarInt(m.quirks.aqua.beam)));
        }
        Nautilus | ZombieNautilus => {
            v.push((a, tame_flags(m)));
            v.push((a + 1, MetadataValue::OptUuid(m.owner)));
            v.push((a + 2, MetadataValue::Boolean(m.quirks.aqua.dashing > 0)));
            if m.species == ZombieNautilus {
                v.push((
                    a + 3,
                    MetadataValue::Holder(holder::ZOMBIE_NAUTILUS, m.variant),
                ));
            }
        }
        _ => {}
    }
    v
}

/// The equipment a mob shows (every slot, empty ones too).
pub fn equipment_of(m: &Mob) -> Vec<(u8, Option<ItemStack>)> {
    (0..8)
        .map(|s| {
            (
                s as u8,
                m.gear[s].as_ref().map(|g| ItemStack {
                    damage: g.damage,
                    components: g.components.clone(),
                    ..ItemStack::new(g.item, g.count)
                }),
            )
        })
        .collect()
}

/// Only the slots that hold something (for spawning).
pub fn worn(m: &Mob) -> Vec<(u8, Option<ItemStack>)> {
    equipment_of(m)
        .into_iter()
        .filter(|(_, s)| s.is_some())
        .collect()
}

/// The slot an armor or other wearable item goes in.
pub fn slot_of(ctx: &ServerContext, item: i32) -> Option<usize> {
    let name = ctx.items.get(item)?.slot.as_deref()?.to_owned();
    Some(match name.as_str() {
        "head" => slot::HEAD,
        "chest" => slot::CHEST,
        "legs" => slot::LEGS,
        "feet" => slot::FEET,
        "body" => slot::BODY,
        "saddle" => slot::SADDLE,
        "offhand" => slot::OFFHAND,
        _ => return None,
    })
}
