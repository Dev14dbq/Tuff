//! Fixes to item component maps after 24w09a (3818), and to item stacks
//! whose meaning depends on their components.

use mc_nbt::{Compound, Tag};

use super::nbt::{
    compound_mut, flag, json_translate, json_translation, list_mut, ns, num, plain_translation,
    rename, string,
};

/// Ids attribute modifiers had as UUIDs, by UUID (as four ints).
const MODIFIER_IDS: [(&str, &str); 40] = [
    (
        "736565d2-e1a7-403d-a3f8-1aeb3e302542",
        "creative_mode_block_range",
    ),
    (
        "98491ef6-97b1-4584-ae82-71a8cc85cf73",
        "creative_mode_entity_range",
    ),
    ("91aeaa56-376b-4498-935b-2f7f68070635", "effect.speed"),
    ("7107de5e-7ce8-4030-940e-514c1f160890", "effect.slowness"),
    ("af8b6e3f-3328-4c0a-aa36-5ba2bb9dbef3", "effect.haste"),
    (
        "55fced67-e92a-486e-9800-b47f202c4386",
        "effect.mining_fatigue",
    ),
    ("648d7064-6a60-4f59-8abe-c2c23a6dd7a9", "effect.strength"),
    ("c0105bf3-aef8-46b0-9ebc-92943757ccbe", "effect.jump_boost"),
    ("22653b89-116e-49dc-9b6b-9971489b5be5", "effect.weakness"),
    (
        "5d6f0ba2-1186-46ac-b896-c61c5cee99cc",
        "effect.health_boost",
    ),
    ("eae29cf0-701e-4ed6-883a-96f798f3dab5", "effect.absorption"),
    ("03c3c89d-7037-4b42-869f-b146bcb64d2e", "effect.luck"),
    ("cc5af142-2bd2-4215-b636-2605aed11727", "effect.unluck"),
    ("6555be74-63b3-41f1-a245-77833b3c2562", "evil"),
    ("1eaf83ff-7207-4596-b37a-d7a07b3ec4ce", "powder_snow"),
    ("662a6b8d-da3e-4c1c-8813-96ea6097278d", "sprinting"),
    ("020e0dfb-87ae-4653-9556-831010e291a0", "attacking"),
    ("766bfa64-11f3-11ea-8d71-362b9e155667", "baby"),
    ("7e0292f2-9434-48d5-a29f-9583af7df27f", "covered"),
    ("9e362924-01de-4ddd-a2b2-d0f7a405a174", "suffocating"),
    ("5cd17e52-a79a-43d3-a529-90fde04b181e", "drinking"),
    ("b9766b59-9566-4402-bc1f-2ee2a276d836", "baby"),
    ("49455a49-7ec5-45ba-b886-3b90b23a1718", "attacking"),
    ("845db27c-c624-495f-8c9f-6020a9a58b6b", "armor.boots"),
    ("d8499b04-0e66-4726-ab29-64469d734e0d", "armor.leggings"),
    ("9f3d476d-c118-4544-8365-64846904b48e", "armor.chestplate"),
    ("2ad3f246-fee1-4e67-b886-69fd380bb150", "armor.helmet"),
    ("c1c72771-8b8e-ba4a-ace0-81a93c8928b2", "armor.body"),
    (
        "b572ecd2-ac0c-4071-abde-9594af072a37",
        "enchantment.fire_protection",
    ),
    (
        "40a9968f-5c66-4e2f-b7f4-2ec2f4b3e450",
        "enchantment.blast_protection",
    ),
    (
        "07a65791-f64d-4e79-86c7-f83932f007ec",
        "enchantment.respiration",
    ),
    (
        "60b1b7db-fffd-4ad0-817c-d6c6a93d8a45",
        "enchantment.aqua_affinity",
    ),
    (
        "11dc269a-4476-46c0-aff3-9e17d7eb6801",
        "enchantment.depth_strider",
    ),
    (
        "87f46a96-686f-4796-b035-22e16ee9e038",
        "enchantment.soul_speed",
    ),
    (
        "b9716dbd-50df-4080-850e-70347d24e687",
        "enchantment.soul_speed",
    ),
    (
        "92437d00-c3a7-4f2e-8f6c-1f21585d5dd0",
        "enchantment.swift_sneak",
    ),
    (
        "5d3d087b-debe-4037-b53e-d84f3ff51f17",
        "enchantment.sweeping_edge",
    ),
    (
        "3ceb37c0-db62-46b5-bd02-785457b01d96",
        "enchantment.efficiency",
    ),
    ("cb3f55d3-645c-4f38-a497-9c13a33db5cf", "base_attack_damage"),
    ("fa233e1c-4180-4865-b01b-bcce9785aca3", "base_attack_speed"),
];

/// Ids of modifiers that had a fixed name and a random UUID.
const MODIFIER_NAMES: [(&str, &str); 5] = [
    ("Random spawn bonus", "random_spawn_bonus"),
    ("Random zombie-spawn bonus", "zombie_random_spawn_bonus"),
    ("Leader zombie bonus", "leader_zombie_bonus"),
    (
        "Zombie reinforcement callee charge",
        "reinforcement_callee_charge",
    ),
    (
        "Zombie reinforcement caller charge",
        "reinforcement_caller_charge",
    ),
];

fn uuid_string(tag: Option<&Tag>) -> Option<String> {
    let Some(Tag::IntArray(v)) = tag else {
        return None;
    };
    let [a, b, c, d] = v[..] else {
        return None;
    };
    let hex = format!(
        "{:08x}{:08x}{:08x}{:08x}",
        a as u32, b as u32, c as u32, d as u32
    );
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// 3945: modifiers are keyed by a namespaced id instead of a UUID and name.
/// Modifiers with the same known name add up.
pub(super) fn modifier_ids(modifiers: &mut Vec<Tag>) {
    let mut out: Vec<(String, Compound)> = Vec::new();
    for tag in modifiers.drain(..) {
        let Tag::Compound(mut m) = tag else {
            continue;
        };
        let uuid = uuid_string(m.get("uuid"));
        let name = string(&m, "name").unwrap_or("").to_owned();
        let from_uuid = uuid
            .as_deref()
            .and_then(|u| MODIFIER_IDS.iter().find(|(k, _)| *k == u))
            .map(|(_, id)| format!("minecraft:{id}"));
        let from_name = MODIFIER_NAMES
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, id)| format!("minecraft:{id}"));
        m.remove("uuid");
        m.remove("name");
        let id = if let Some(id) = from_uuid {
            id
        } else if let Some(id) = from_name {
            if let Some((_, existing)) = out.iter_mut().find(|(k, _)| *k == id) {
                let sum = num(existing, "amount").unwrap_or(0.0) + num(&m, "amount").unwrap_or(0.0);
                existing.insert("amount", Tag::Double(sum));
                continue;
            }
            id
        } else {
            format!("minecraft:{}", uuid.as_deref().unwrap_or("unknown"))
        };
        m.insert("id", Tag::String(id.clone()));
        match out.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = m,
            None => out.push((id, m)),
        }
    }
    modifiers.extend(out.into_iter().map(|(_, m)| Tag::Compound(m)));
}

pub(super) fn attribute_modifier_ids(_: &str, c: &mut Compound) {
    if let Some(attrs) = compound_mut(c, "minecraft:attribute_modifiers")
        && let Some(list) = list_mut(attrs, "modifiers")
    {
        modifier_ids(list);
    }
}

/// Renames `type` of every attribute modifier, whether the component is
/// still `{modifiers: [..]}` or already the bare list.
fn rename_modifier_types(c: &mut Compound, f: fn(&str) -> Option<String>) {
    let list = match c.get_mut("minecraft:attribute_modifiers") {
        Some(Tag::List(list)) => list,
        Some(Tag::Compound(attrs)) => match list_mut(attrs, "modifiers") {
            Some(list) => list,
            None => return,
        },
        _ => return,
    };
    for m in list {
        if let Tag::Compound(m) = m
            && let Some(Tag::String(t)) = m.get_mut("type")
            && let Some(new) = f(t)
        {
            *t = new;
        }
    }
}

/// 4055: `generic.`, `horse.`, `player.` and `zombie.` left attribute ids.
pub(super) fn attribute_prefix(id: &str) -> Option<String> {
    let full = ns(id);
    for prefix in ["generic.", "horse.", "player.", "zombie."] {
        if let Some(rest) = full.strip_prefix(&format!("minecraft:{prefix}")) {
            return Some(format!("minecraft:{rest}"));
        }
    }
    None
}

pub(super) fn attribute_prefixes(_: &str, c: &mut Compound) {
    rename_modifier_types(c, attribute_prefix);
}

/// 4888: `nameplate_distance` is `name_tag_distance`.
pub(super) fn name_tag_attribute(id: &str) -> Option<String> {
    (ns(id) == "minecraft:nameplate_distance").then(|| "minecraft:name_tag_distance".to_owned())
}

pub(super) fn name_tag_attributes(_: &str, c: &mut Compound) {
    rename_modifier_types(c, name_tag_attribute);
}

/// 3820: a lodestone target holds `pos` and `dimension` together.
pub(super) fn lodestone_target(_: &str, c: &mut Compound) {
    let Some(Tag::Compound(mut old)) = c.remove("minecraft:lodestone_target") else {
        return;
    };
    let pos = old.remove("pos");
    let dimension = old.remove("dimension");
    if let (Some(pos), Some(dimension)) = (pos, dimension) {
        let mut target = Compound::new();
        target.insert("pos", pos);
        target.insert("dimension", dimension);
        old.insert("target", Tag::Compound(target));
    }
    c.insert("minecraft:lodestone_tracker", Tag::Compound(old));
}

const MAP_NAMES: [&str; 11] = [
    "filled_map.buried_treasure",
    "filled_map.explorer_jungle",
    "filled_map.explorer_swamp",
    "filled_map.mansion",
    "filled_map.monument",
    "filled_map.trial_chambers",
    "filled_map.village_desert",
    "filled_map.village_plains",
    "filled_map.village_savanna",
    "filled_map.village_snowy",
    "filled_map.village_taiga",
];

const OMINOUS: &str = "block.minecraft.ominous_banner";

/// 3825: explorer maps and the ominous banner carry their names as the
/// item's own name, not a custom one.
pub(super) fn name_to_item_name(owner: &str, c: &mut Compound) {
    let fits = |key: &str| match owner {
        "minecraft:filled_map" => MAP_NAMES.contains(&key),
        "minecraft:white_banner" => key == OMINOUS,
        _ => false,
    };
    let key = c.get("minecraft:custom_name").and_then(json_translation);
    if key.as_deref().is_some_and(fits) {
        rename(c, "minecraft:custom_name", "minecraft:item_name");
    }
}

/// 4054: ominous banners are uncommon.
pub(super) fn ominous_rarity(owner: &str, c: &mut Compound) {
    if owner != "minecraft:white_banner" && owner != "minecraft:banner" {
        return;
    }
    let ominous = c
        .get("minecraft:item_name")
        .and_then(json_translation)
        .is_some_and(|k| k == OMINOUS);
    if ominous {
        c.insert("minecraft:item_name", json_translate(OMINOUS));
        c.insert("minecraft:rarity", Tag::String("uncommon".into()));
    }
}

/// 4059: food's eating time and effects moved to a `consumable` component.
pub(super) fn food_to_consumable(_: &str, c: &mut Compound) {
    let Some(Tag::Compound(mut food)) = c.remove("minecraft:food") else {
        return;
    };
    let seconds = num(&food, "eat_seconds").unwrap_or(1.6) as f32;
    let effects: Vec<Tag> = super::nbt::list(&food, "effects")
        .iter()
        .filter_map(|e| match e {
            Tag::Compound(e) => {
                let mut out = Compound::new();
                out.insert("type", Tag::String("minecraft:apply_effects".into()));
                out.insert(
                    "effects",
                    Tag::List(e.get("effect").cloned().into_iter().collect()),
                );
                out.insert(
                    "probability",
                    Tag::Float(num(e, "probability").unwrap_or(1.0) as f32),
                );
                Some(Tag::Compound(out))
            }
            _ => None,
        })
        .collect();
    if let Some(remainder) = food.remove("using_converts_to") {
        c.insert("minecraft:use_remainder", remainder);
    }
    food.remove("eat_seconds");
    food.remove("effects");
    c.insert("minecraft:food", Tag::Compound(food));
    let mut consumable = Compound::new();
    consumable.insert("consume_seconds", Tag::Float(seconds));
    consumable.insert("on_consume_effects", Tag::List(effects));
    c.insert("minecraft:consumable", Tag::Compound(consumable));
}

/// 4064: `fire_resistant` is a damage resistance to fire.
pub(super) fn fire_resistant(_: &str, c: &mut Compound) {
    if c.remove("minecraft:fire_resistant").is_some() {
        let mut d = Compound::new();
        d.insert("types", Tag::String("#minecraft:is_fire".into()));
        c.insert("minecraft:damage_resistant", Tag::Compound(d));
    }
}

/// 4068: a lock is an item predicate; an old lock name matches items with
/// that custom name. `None` when there is no lock.
pub(super) fn lock_predicate(old: &Tag) -> Option<Tag> {
    let Tag::String(name) = old else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let mut components = Compound::new();
    components.insert("minecraft:custom_name", super::item::json_string(name));
    let mut lock = Compound::new();
    lock.insert("components", Tag::Compound(components));
    Some(Tag::Compound(lock))
}

pub(super) fn lock(_: &str, c: &mut Compound) {
    if let Some(old) = c.remove("minecraft:lock")
        && let Some(new) = lock_predicate(&old)
    {
        c.insert("minecraft:lock", new);
    }
}

/// 4176: a lock that matches only an empty name (what 4068 made of broken
/// locks) is no lock.
pub(super) fn is_broken_lock(lock: &Tag) -> bool {
    let Tag::Compound(lock) = lock else {
        return false;
    };
    let Some(Tag::Compound(components)) = lock.get("components") else {
        return false;
    };
    lock.len() == 1
        && components.len() == 1
        && components.get("minecraft:custom_name") == Some(&Tag::String("\"\"".into()))
}

pub(super) fn broken_lock(_: &str, c: &mut Compound) {
    if c.get("minecraft:lock").is_some_and(is_broken_lock) {
        c.remove("minecraft:lock");
    }
}

/// 4175: `equippable.model` is `asset_id`; custom model data is a list of
/// floats.
pub(super) fn equippable_and_model_data(_: &str, c: &mut Compound) {
    if let Some(e) = compound_mut(c, "minecraft:equippable") {
        rename(e, "model", "asset_id");
    }
    if let Some(old) = c.get("minecraft:custom_model_data") {
        let value = super::nbt::number(old).unwrap_or(0.0) as f32;
        let mut cmd = Compound::new();
        cmd.insert("floats", Tag::List(vec![Tag::Float(value)]));
        c.insert("minecraft:custom_model_data", Tag::Compound(cmd));
    }
}

const TROPICAL_PATTERNS: [(i32, &str); 11] = [
    (1, "flopper"),
    (256, "sunstreak"),
    (257, "stripey"),
    (512, "snooper"),
    (513, "glitter"),
    (768, "dasher"),
    (769, "blockfish"),
    (1024, "brinely"),
    (1025, "betty"),
    (1280, "spotty"),
    (1281, "clayfish"),
];

/// 4299: what buckets and paintings spawn is in components of their own.
pub(super) fn spawner_variants(owner: &str, c: &mut Compound) {
    if owner == "minecraft:painting" {
        painting_variant(c);
        return;
    }
    let Some(bucket) = compound_mut(c, "minecraft:bucket_entity_data") else {
        return;
    };
    let mut set: Vec<(&str, Tag)> = Vec::new();
    match owner {
        "minecraft:salmon_bucket" => {
            if let Some(t) = bucket.remove("type") {
                set.push(("minecraft:salmon/size", t));
            }
        }
        "minecraft:axolotl_bucket" => {
            if let Some(v) = bucket
                .remove("Variant")
                .and_then(|t| super::nbt::number(&t))
            {
                let name = match v as i32 {
                    1 => "wild",
                    2 => "gold",
                    3 => "cyan",
                    4 => "blue",
                    _ => "lucy",
                };
                set.push(("minecraft:axolotl/variant", Tag::String(name.into())));
            }
        }
        "minecraft:tropical_fish_bucket" => {
            if let Some(v) = bucket
                .remove("BucketVariantTag")
                .and_then(|t| super::nbt::number(&t))
            {
                let packed = v as i32;
                let pattern = TROPICAL_PATTERNS
                    .iter()
                    .find(|(k, _)| *k == packed & 0xffff)
                    .map_or("kob", |(_, n)| n);
                let base = super::nbt::dye((packed >> 16) & 0xff);
                let colour = super::nbt::dye((packed >> 24) & 0xff);
                set.push((
                    "minecraft:tropical_fish/pattern",
                    Tag::String(pattern.into()),
                ));
                set.push((
                    "minecraft:tropical_fish/base_color",
                    Tag::String(base.into()),
                ));
                set.push((
                    "minecraft:tropical_fish/pattern_color",
                    Tag::String(colour.into()),
                ));
            }
        }
        _ => {}
    }
    for (k, v) in set {
        c.insert(k, v);
    }
}

fn painting_variant(c: &mut Compound) {
    let Some(data) = compound_mut(c, "minecraft:entity_data") else {
        return;
    };
    if string(data, "id") != Some("minecraft:painting") {
        return;
    }
    let variant = data.remove("variant");
    let only_id = data.len() == 1;
    if only_id {
        c.remove("minecraft:entity_data");
    }
    if let Some(v) = variant {
        c.insert("minecraft:painting/variant", v);
    }
}

/// Components whose tooltip `hide_additional_tooltip` used to hide.
const ADDITIONAL_TOOLTIPS: [&str; 17] = [
    "minecraft:banner_patterns",
    "minecraft:bees",
    "minecraft:block_entity_data",
    "minecraft:block_state",
    "minecraft:bundle_contents",
    "minecraft:charged_projectiles",
    "minecraft:container",
    "minecraft:container_loot",
    "minecraft:firework_explosion",
    "minecraft:fireworks",
    "minecraft:instrument",
    "minecraft:map_id",
    "minecraft:painting/variant",
    "minecraft:pot_decorations",
    "minecraft:potion_contents",
    "minecraft:tropical_fish/pattern",
    "minecraft:written_book_content",
];

/// 4307: `show_in_tooltip` flags move into one `tooltip_display` component;
/// components that only wrapped their value lose the wrapper.
pub(super) fn tooltip_display(_: &str, c: &mut Compound) {
    let mut hidden: Vec<String> = Vec::new();
    for key in ["minecraft:can_place_on", "minecraft:can_break"] {
        if let Some(Tag::Compound(p)) = c.get(key)
            && let Some(predicates) = p.get("predicates").cloned()
        {
            if super::nbt::boolean(p, "show_in_tooltip") == Some(false) {
                hidden.push(key.to_owned());
            }
            c.insert(key, predicates);
        }
    }
    let unwrap: [(&str, Option<&str>); 7] = [
        ("minecraft:trim", None),
        ("minecraft:unbreakable", None),
        ("minecraft:dyed_color", Some("rgb")),
        ("minecraft:attribute_modifiers", Some("modifiers")),
        ("minecraft:enchantments", Some("levels")),
        ("minecraft:stored_enchantments", Some("levels")),
        ("minecraft:jukebox_playable", Some("song")),
    ];
    for (key, field) in unwrap {
        let Some(Tag::Compound(component)) = c.get_mut(key) else {
            continue;
        };
        if super::nbt::boolean(component, "show_in_tooltip") == Some(false) {
            hidden.push(key.to_owned());
        }
        component.remove("show_in_tooltip");
        if let Some(field) = field
            && let Some(inner) = component.get(field).cloned()
        {
            c.insert(key, inner);
        }
    }
    let hide_tooltip = c.remove("minecraft:hide_tooltip").is_some();
    if c.remove("minecraft:hide_additional_tooltip").is_some() {
        for key in ADDITIONAL_TOOLTIPS {
            if c.get(key).is_some() && !hidden.iter().any(|h| h == key) {
                hidden.push(key.to_owned());
            }
        }
    }
    if hidden.is_empty() && !hide_tooltip {
        return;
    }
    let mut display = Compound::new();
    display.insert("hide_tooltip", flag(hide_tooltip));
    display.insert(
        "hidden_components",
        Tag::List(hidden.into_iter().map(Tag::String).collect()),
    );
    c.insert("minecraft:tooltip_display", Tag::Compound(display));
}

/// 4649: the `spear` use animation of 1.21.x tridents is `trident`.
pub(super) fn trident_animation(_: &str, c: &mut Compound) {
    if let Some(consumable) = compound_mut(c, "minecraft:consumable")
        && let Some(Tag::String(a)) = consumable.get_mut("animation")
        && a == "spear"
    {
        *a = "trident".into();
    }
}

/// 4996: pot decorations name their side.
pub(super) fn unpack_sherds(old: &Tag) -> Option<Tag> {
    let Tag::List(list) = old else {
        return None;
    };
    let mut out = Compound::new();
    for (i, side) in ["back", "left", "right", "front"].into_iter().enumerate() {
        let id = match list.get(i) {
            Some(Tag::String(s)) => s.clone(),
            _ => "minecraft:brick".to_owned(),
        };
        if id.is_empty() {
            return None;
        }
        let mut entry = Compound::new();
        entry.insert("id", Tag::String(id));
        out.insert(side, Tag::Compound(entry));
    }
    Some(Tag::Compound(out))
}

pub(super) fn pot_decorations(_: &str, c: &mut Compound) {
    if let Some(old) = c.get("minecraft:pot_decorations")
        && let Some(new) = unpack_sherds(old)
    {
        c.insert("minecraft:pot_decorations", new);
    }
}

/// 5007: one swing animation became an attack and an interact animation.
pub(super) fn swing_animation(_: &str, c: &mut Compound) {
    if let Some(swing) = c.remove("minecraft:swing_animation") {
        c.insert("minecraft:attack_animation", swing.clone());
        c.insert("minecraft:interact_animation", swing);
    }
}

/// 5008: map colour is not a component any more.
pub(super) fn map_color(_: &str, c: &mut Compound) {
    c.remove("minecraft:map_color");
}

/// 5014: the inline block transformer component is gone.
pub(super) fn block_transformer(_: &str, c: &mut Compound) {
    c.remove("minecraft:block_transformer");
}

const EXPLORER_MAPS: [(&str, &str, &str); 11] = [
    (
        "minecraft:mansion",
        "minecraft:woodland_explorer_map",
        "filled_map.mansion",
    ),
    (
        "minecraft:monument",
        "minecraft:ocean_explorer_map",
        "filled_map.monument",
    ),
    (
        "minecraft:trial_chambers",
        "minecraft:trial_explorer_map",
        "filled_map.trial_chambers",
    ),
    (
        "minecraft:jungle_temple",
        "minecraft:jungle_explorer_map",
        "filled_map.explorer_jungle",
    ),
    (
        "minecraft:swamp_hut",
        "minecraft:swamp_explorer_map",
        "filled_map.explorer_swamp",
    ),
    (
        "minecraft:village_desert",
        "minecraft:desert_village_map",
        "filled_map.village_desert",
    ),
    (
        "minecraft:village_plains",
        "minecraft:plains_village_map",
        "filled_map.village_plains",
    ),
    (
        "minecraft:village_savanna",
        "minecraft:savanna_village_map",
        "filled_map.village_savanna",
    ),
    (
        "minecraft:village_snowy",
        "minecraft:snowy_village_map",
        "filled_map.village_snowy",
    ),
    (
        "minecraft:village_taiga",
        "minecraft:taiga_village_map",
        "filled_map.village_taiga",
    ),
    (
        "minecraft:red_x",
        "minecraft:buried_treasure_map",
        "filled_map.buried_treasure",
    ),
];

/// 5008: a filled map with an explorer marker is that explorer map's item;
/// the old fixed name goes, the item names itself.
pub(super) fn explorer_map(id: &str, item: &mut Compound) {
    if id != "minecraft:filled_map" {
        return;
    }
    let Some(components) = compound_mut(item, "components") else {
        return;
    };
    let kind = compound_mut(components, "minecraft:map_decorations")
        .and_then(|d| compound_mut(d, "+"))
        .and_then(|marker| string(marker, "type").map(ns));
    let Some((_, new_id, old_name)) =
        kind.and_then(|k| EXPLORER_MAPS.iter().find(|(t, _, _)| *t == k))
    else {
        return;
    };
    let named = components
        .get("minecraft:item_name")
        .and_then(plain_translation)
        .is_some_and(|k| k == *old_name);
    if named {
        components.remove("minecraft:item_name");
    }
    item.insert("id", Tag::String((*new_id).into()));
}

/// 5016: an empty map with the buried treasure name uses the new key.
pub(super) fn buried_treasure_name(id: &str, item: &mut Compound) {
    if id != "minecraft:map" {
        return;
    }
    let Some(components) = compound_mut(item, "components") else {
        return;
    };
    if let Some(Tag::Compound(name)) = components.get_mut("minecraft:item_name")
        && name.len() == 1
        && string(name, "translate") == Some("filled_map.buried_treasure")
    {
        name.insert(
            "translate",
            Tag::String("item.minecraft.buried_treasure_map".into()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_flags_become_one_component() {
        let mut c = Compound::new();
        let mut ench = Compound::new();
        let mut levels = Compound::new();
        levels.insert("minecraft:sharpness", Tag::Int(2));
        ench.insert("levels", Tag::Compound(levels.clone()));
        ench.insert("show_in_tooltip", flag(false));
        c.insert("minecraft:enchantments", Tag::Compound(ench));
        let mut dyed = Compound::new();
        dyed.insert("rgb", Tag::Int(7));
        c.insert("minecraft:dyed_color", Tag::Compound(dyed));
        tooltip_display("minecraft:leather_helmet", &mut c);
        assert_eq!(
            c.get("minecraft:enchantments"),
            Some(&Tag::Compound(levels))
        );
        assert_eq!(c.get("minecraft:dyed_color"), Some(&Tag::Int(7)));
        let Some(Tag::Compound(display)) = c.get("minecraft:tooltip_display") else {
            panic!("no tooltip_display");
        };
        assert_eq!(
            display.get("hidden_components"),
            Some(&Tag::List(vec![Tag::String(
                "minecraft:enchantments".into()
            )]))
        );
    }

    #[test]
    fn modifier_uuids_become_ids() {
        let mut m = Compound::new();
        // 2AD3F246-FEE1-4E67-B886-69FD380BB150: the helmet slot modifier.
        m.insert(
            "uuid",
            Tag::IntArray(vec![
                0x2ad3_f246_u32 as i32,
                0xfee1_4e67_u32 as i32,
                0xb886_69fd_u32 as i32,
                0x380b_b150_u32 as i32,
            ]),
        );
        m.insert("name", Tag::String("x".into()));
        m.insert("amount", Tag::Double(2.0));
        let mut list = vec![Tag::Compound(m)];
        modifier_ids(&mut list);
        let Tag::Compound(m) = &list[0] else {
            panic!("not a compound");
        };
        assert_eq!(
            m.get("id"),
            Some(&Tag::String("minecraft:armor.helmet".into()))
        );
        assert!(m.get("uuid").is_none());
        assert_eq!(
            attribute_prefix("minecraft:generic.max_health").as_deref(),
            Some("minecraft:max_health")
        );
    }

    #[test]
    fn sherds_name_their_sides() {
        let old = Tag::List(vec![Tag::String("minecraft:angler_pottery_sherd".into())]);
        let Some(Tag::Compound(sides)) = unpack_sherds(&old) else {
            panic!("not unpacked");
        };
        let Some(Tag::Compound(back)) = sides.get("back") else {
            panic!("no back");
        };
        assert_eq!(
            back.get("id"),
            Some(&Tag::String("minecraft:angler_pottery_sherd".into()))
        );
        let Some(Tag::Compound(front)) = sides.get("front") else {
            panic!("no front");
        };
        assert_eq!(
            front.get("id"),
            Some(&Tag::String("minecraft:brick".into()))
        );
    }
}
