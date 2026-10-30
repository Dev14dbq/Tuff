//! Entity and player fixes.

use mc_nbt::{Compound, Tag};

use super::nbt::{
    block_pos, compound_mut, each_compound, flag, inline_pos, int, list_mut, ns, num, rename,
    rename_value, rename_with, string,
};

/// Effect ids before 23w41a (3568), by number.
const EFFECTS: [&str; 33] = [
    "speed",
    "slowness",
    "haste",
    "mining_fatigue",
    "strength",
    "instant_health",
    "instant_damage",
    "jump_boost",
    "nausea",
    "regeneration",
    "resistance",
    "fire_resistance",
    "water_breathing",
    "invisibility",
    "blindness",
    "night_vision",
    "hunger",
    "weakness",
    "poison",
    "wither",
    "health_boost",
    "absorption",
    "saturation",
    "glowing",
    "levitation",
    "luck",
    "unluck",
    "slow_falling",
    "conduit_power",
    "dolphins_grace",
    "bad_omen",
    "hero_of_the_village",
    "darkness",
];

pub(super) fn effect_name(tag: &Tag) -> Option<Tag> {
    let n = super::nbt::number(tag)? as i32;
    let name = EFFECTS.get(usize::try_from(n - 1).ok()?)?;
    Some(Tag::String(format!("minecraft:{name}")))
}

/// Moves `from` to `to` as an effect name; an unknown number drops it.
fn effect_field(c: &mut Compound, from: &str, to: &str) {
    if let Some(old) = c.remove(from)
        && let Some(name) = effect_name(&old)
    {
        c.insert(to, name);
    }
}

/// 3568: one effect instance with a named id and snake_case fields.
fn effect_instance(e: &mut Compound) {
    effect_field(e, "Id", "id");
    rename(e, "Ambient", "ambient");
    rename(e, "Amplifier", "amplifier");
    rename(e, "Duration", "duration");
    rename(e, "ShowParticles", "show_particles");
    rename(e, "ShowIcon", "show_icon");
    if let Some(Tag::Compound(mut hidden)) = e.remove("HiddenEffect") {
        effect_instance(&mut hidden);
        e.insert("hidden_effect", Tag::Compound(hidden));
    }
}

pub(super) fn effect_instances(c: &mut Compound, from: &str, to: &str) {
    if let Some(Tag::List(mut list)) = c.remove(from) {
        for e in &mut list {
            if let Tag::Compound(e) = e {
                effect_instance(e);
            }
        }
        c.insert(to, Tag::List(list));
    }
}

/// A suspicious stew effect (`EffectId`, `EffectDuration`) → `{id, duration}`.
pub(super) fn stew_effect(e: &mut Compound) {
    effect_field(e, "EffectId", "id");
    rename(e, "EffectDuration", "duration");
}

/// 3322: `FactorCalculationData` counts active ticks instead of a timestamp.
pub(super) fn effect_list_durations(c: &mut Compound, key: &str) {
    each_compound(c, key, |effect| {
        let duration = int(effect, "Duration").unwrap_or(-1);
        if let Some(factor) = compound_mut(effect, "FactorCalculationData") {
            let stamp = factor
                .remove("effect_changed_timestamp")
                .and_then(|t| super::nbt::number(&t))
                .map_or(-1, |v| v as i32);
            factor.insert("ticks_active", Tag::Int(stamp - duration));
        }
    });
}

pub(super) fn effect_durations(_: &str, e: &mut Compound) {
    for key in ["Effects", "ActiveEffects", "CustomPotionEffects"] {
        effect_list_durations(e, key);
    }
}

pub(super) fn effect_ids(id: &str, e: &mut Compound) {
    match id {
        "minecraft:mooshroom" => {
            let mut entry = Compound::new();
            if let Some(old) = e.remove("EffectId")
                && let Some(name) = effect_name(&old)
            {
                entry.insert("id", name);
            }
            if let Some(d) = e.remove("EffectDuration") {
                entry.insert("duration", d);
            }
            if !entry.is_empty() {
                e.insert("stew_effects", Tag::List(vec![Tag::Compound(entry)]));
            }
        }
        "minecraft:arrow" => effect_instances(e, "CustomPotionEffects", "custom_potion_effects"),
        "minecraft:area_effect_cloud" => effect_instances(e, "Effects", "effects"),
        _ => {}
    }
    effect_instances(e, "ActiveEffects", "active_effects");
}

/// 3086 / 3087: cat and frog variants are names. The old field is left,
/// as vanilla leaves it (nothing reads it).
pub(super) fn cat_and_frog_variants(id: &str, e: &mut Compound) {
    const CATS: [&str; 11] = [
        "tabby",
        "black",
        "red",
        "siamese",
        "british",
        "calico",
        "persian",
        "ragdoll",
        "white",
        "jellie",
        "all_black",
    ];
    const FROGS: [&str; 3] = ["temperate", "warm", "cold"];
    let (field, names, fallback): (&str, &[&str], Option<&str>) = match id {
        "minecraft:cat" => ("CatType", &CATS, Some("tabby")),
        "minecraft:frog" => ("Variant", &FROGS, None),
        _ => return,
    };
    let Some(n) = e.get(field).and_then(super::nbt::number) else {
        return;
    };
    let name = usize::try_from(n as i32)
        .ok()
        .and_then(|i| names.get(i).copied())
        .or(fallback);
    if let Some(name) = name {
        e.insert("variant", Tag::String(format!("minecraft:{name}")));
    }
}

/// 3097: the British cat variant is `british_shorthair`.
pub(super) fn british_shorthair(id: &str, e: &mut Compound) {
    if id == "minecraft:cat" {
        rename_value(
            e,
            "variant",
            &[("minecraft:british", "minecraft:british_shorthair")],
        );
    }
}

/// 3090: painting fields are snake_case.
pub(super) fn painting_fields(id: &str, e: &mut Compound) {
    if id == "minecraft:painting" {
        rename(e, "Motive", "variant");
        rename(e, "Facing", "facing");
    }
}

/// 3093: goats saved before horns could drop have both.
pub(super) fn goat_horns(id: &str, e: &mut Compound) {
    if id == "minecraft:goat" {
        e.insert("HasLeftHorn", flag(true));
        e.insert("HasRightHorn", flag(true));
    }
}

/// 3683: primed TNT stores the block it shows.
pub(super) fn primed_tnt(id: &str, e: &mut Compound) {
    if id != "minecraft:tnt" {
        return;
    }
    let mut state = Compound::new();
    state.insert("Name", Tag::String("minecraft:tnt".into()));
    e.insert("block_state", Tag::Compound(state));
    if let Some(fuse) = e.get("Fuse").cloned() {
        e.insert("fuse", fuse);
    }
}

/// 3808: horse armor and llama carpets are body armor.
pub(super) fn body_armor(id: &str, e: &mut Compound) {
    let (field, clear) = match id {
        "minecraft:horse" => ("ArmorItem", true),
        "minecraft:llama" | "minecraft:trader_llama" => ("DecorItem", false),
        _ => return,
    };
    let Some(armor) = e.remove(field) else {
        return;
    };
    if clear {
        if let Some(items) = list_mut(e, "ArmorItems")
            && let Some(slot) = items.get_mut(2)
        {
            *slot = Tag::Compound(Compound::new());
        }
        if let Some(chances) = list_mut(e, "ArmorDropChances")
            && let Some(slot) = chances.get_mut(2)
        {
            *slot = Tag::Float(0.085);
        }
    }
    e.insert("body_armor_item", armor);
    e.insert("body_armor_drop_chance", Tag::Float(2.0));
}

/// 3809: chests on llamas, mules and donkeys count slots from 0.
pub(super) fn chested_slots(id: &str, e: &mut Compound) {
    if !matches!(
        id,
        "minecraft:llama" | "minecraft:trader_llama" | "minecraft:mule" | "minecraft:donkey"
    ) {
        return;
    }
    each_compound(e, "Items", |item| {
        let slot = int(item, "Slot").unwrap_or(2);
        item.insert("Slot", Tag::Byte((slot - 2) as i8));
    });
}

/// 3812: wolves have 40 health, not 20.
pub(super) fn wolf_health(id: &str, e: &mut Compound) {
    if id != "minecraft:wolf" {
        return;
    }
    let mut doubled = false;
    each_compound(e, "Attributes", |a| {
        let name = string(a, "Name").map(ns);
        if name.as_deref() == Some("minecraft:generic.max_health") && num(a, "Base") == Some(20.0) {
            a.insert("Base", Tag::Double(40.0));
            doubled = true;
        }
    });
    if doubled {
        let health = num(e, "Health").unwrap_or(0.0) as f32;
        e.insert("Health", Tag::Float(health * 2.0));
    }
}

/// 3813: block positions on entities are `[I; x, y, z]` under snake_case
/// names.
pub(super) fn block_positions(id: &str, e: &mut Compound) {
    let fields: &[(&str, &str)] = match id {
        "minecraft:bee" => &[("HivePos", "hive_pos"), ("FlowerPos", "flower_pos")],
        "minecraft:end_crystal" => &[("BeamTarget", "beam_target")],
        "minecraft:wandering_trader" => &[("WanderTarget", "wander_target")],
        "minecraft:witch"
        | "minecraft:ravager"
        | "minecraft:pillager"
        | "minecraft:illusioner"
        | "minecraft:evoker"
        | "minecraft:vindicator" => &[("PatrolTarget", "patrol_target")],
        _ => &[],
    };
    for (from, to) in fields {
        rename_with(e, from, to, block_pos);
    }
    rename_with(e, "Leash", "leash", block_pos);
}

/// 3814: `horse.jump_strength` became a generic attribute.
pub(super) fn jump_strength(_: &str, e: &mut Compound) {
    each_compound(e, "Attributes", |a| {
        rename_value(
            a,
            "Name",
            &[(
                "minecraft:horse.jump_strength",
                "minecraft:generic.jump_strength",
            )],
        );
    });
}

/// 3818: an area effect cloud keeps its potion like a potion item does.
pub(super) fn cloud_potion(id: &str, e: &mut Compound) {
    if id != "minecraft:area_effect_cloud" {
        return;
    }
    let color = e.remove("Color");
    let effects = e.remove("effects");
    let potion = e.remove("Potion");
    if color.is_none() && effects.is_none() && potion.is_none() {
        return;
    }
    let mut c = Compound::new();
    if let Some(v) = color {
        c.insert("custom_color", v);
    }
    if let Some(v) = effects {
        c.insert("custom_effects", v);
    }
    if let Some(v) = potion {
        c.insert("potion", v);
    }
    e.insert("potion_contents", Tag::Compound(c));
}

/// 3945: `Attributes` → `attributes`, each `{id, base, modifiers}` with
/// modifiers keyed by id.
pub(super) fn attributes(_: &str, e: &mut Compound) {
    let Some(Tag::List(mut list)) = e.remove("Attributes") else {
        return;
    };
    for a in &mut list {
        let Tag::Compound(a) = a else {
            continue;
        };
        rename(a, "Name", "id");
        rename(a, "Base", "base");
        if let Some(Tag::List(mut modifiers)) = a.remove("Modifiers") {
            for m in &mut modifiers {
                if let Tag::Compound(m) = m {
                    rename(m, "UUID", "uuid");
                    rename(m, "Name", "name");
                    rename(m, "Amount", "amount");
                    if let Some(op) = m.remove("Operation") {
                        let name = match super::nbt::number(&op).map(|v| v as i32) {
                            Some(0) => "add_value",
                            Some(1) => "add_multiplied_base",
                            Some(2) => "add_multiplied_total",
                            _ => "invalid",
                        };
                        m.insert("operation", Tag::String(name.into()));
                    }
                }
            }
            super::components::modifier_ids(&mut modifiers);
            a.insert("modifiers", Tag::List(modifiers));
        }
    }
    e.insert("attributes", Tag::List(list));
}

fn rename_attribute_ids(e: &mut Compound, f: fn(&str) -> Option<String>) {
    each_compound(e, "attributes", |a| {
        if let Some(Tag::String(id)) = a.get_mut("id")
            && let Some(new) = f(id)
        {
            *id = new;
        }
    });
}

/// 4055: attribute ids lost their `generic.` style prefixes.
pub(super) fn attribute_prefixes(_: &str, e: &mut Compound) {
    rename_attribute_ids(e, super::components::attribute_prefix);
}

/// 4888: `nameplate_distance` is `name_tag_distance`.
pub(super) fn name_tag_attribute(_: &str, e: &mut Compound) {
    rename_attribute_ids(e, super::components::name_tag_attribute);
}

/// 4067: boats are one entity type per wood.
pub(super) fn boats(id: &str, e: &mut Compound) {
    let chest = match id {
        "minecraft:boat" => false,
        "minecraft:chest_boat" => true,
        _ => return,
    };
    let wood = match e.remove("Type") {
        Some(Tag::String(t)) => t,
        _ => "oak".to_owned(),
    };
    let wood = match wood.as_str() {
        "spruce" | "birch" | "jungle" | "acacia" | "cherry" | "dark_oak" | "mangrove" => wood,
        "bamboo" => "bamboo".to_owned(),
        _ => "oak".to_owned(),
    };
    let new = match (wood.as_str(), chest) {
        ("bamboo", false) => "minecraft:bamboo_raft".to_owned(),
        ("bamboo", true) => "minecraft:bamboo_chest_raft".to_owned(),
        (w, false) => format!("minecraft:{w}_boat"),
        (w, true) => format!("minecraft:{w}_chest_boat"),
    };
    e.insert("id", Tag::String(new));
}

/// 4081: a salmon saved before sizes is medium unless it was already large.
pub(super) fn salmon_size(id: &str, e: &mut Compound) {
    if id == "minecraft:salmon" && string(e, "type") != Some("large") {
        e.insert("type", Tag::String("medium".into()));
    }
}

/// 4173: the TNT minecart's fuse is `fuse`.
pub(super) fn tnt_minecart(id: &str, e: &mut Compound) {
    if id == "minecraft:tnt_minecart" {
        rename(e, "TNTFuse", "fuse");
    }
}

/// 4187: follow ranges saved as the old defaults become the new defaults.
pub(super) fn follow_range(id: &str, e: &mut Compound) {
    let (old, new) = match id {
        "minecraft:villager" | "minecraft:bee" | "minecraft:allay" => (48.0, 16.0),
        "minecraft:llama" => (40.0, 16.0),
        "minecraft:piglin_brute" => (16.0, 12.0),
        "minecraft:warden" => (16.0, 24.0),
        _ => return,
    };
    each_compound(e, "attributes", |a| {
        if string(a, "id").map(ns).as_deref() == Some("minecraft:follow_range")
            && num(a, "base") == Some(old)
        {
            a.insert("base", Tag::Double(new));
        }
    });
}

/// 4293: drop chances are one map by slot, holding only non-defaults.
pub(super) fn drop_chances(_: &str, e: &mut Compound) {
    const DEFAULT: f32 = 0.085;
    let read = |e: &mut Compound, key: &str| -> Vec<f32> {
        match e.remove(key) {
            Some(Tag::List(list)) => list
                .iter()
                .map(|t| super::nbt::number(t).map_or(DEFAULT, |v| v as f32))
                .collect(),
            _ => Vec::new(),
        }
    };
    let armor = read(e, "ArmorDropChances");
    let hands = read(e, "HandDropChances");
    let body = e
        .remove("body_armor_drop_chance")
        .and_then(|t| super::nbt::number(&t))
        .map_or(DEFAULT, |v| v as f32);
    let mut out = Compound::new();
    for (chances, names) in [
        (&armor, &["feet", "legs", "chest", "head"][..]),
        (&hands, &["mainhand", "offhand"][..]),
    ] {
        for (chance, slot) in chances.iter().zip(names) {
            if *chance != DEFAULT {
                out.insert(*slot, Tag::Float(*chance));
            }
        }
    }
    if body != DEFAULT {
        out.insert("body", Tag::Float(body));
    }
    if !out.is_empty() {
        e.insert("drop_chances", Tag::Compound(out));
    }
}

/// 4296: clouds made before 1.21.5 shortened potion effects to a quarter.
pub(super) fn cloud_duration_scale(id: &str, e: &mut Compound) {
    if id == "minecraft:area_effect_cloud" {
        e.insert("potion_duration_scale", Tag::Float(0.25));
    }
}

/// 4300: a saddle is an equipment slot.
pub(super) fn saddles(id: &str, e: &mut Compound) {
    let with_item = [
        "minecraft:horse",
        "minecraft:skeleton_horse",
        "minecraft:zombie_horse",
        "minecraft:donkey",
        "minecraft:mule",
        "minecraft:camel",
        "minecraft:llama",
        "minecraft:trader_llama",
    ];
    if with_item.contains(&id) {
        if e.get("SaddleItem").is_none() {
            return;
        }
        rename(e, "SaddleItem", "saddle");
    } else if id == "minecraft:pig" || id == "minecraft:strider" {
        let saddled = super::nbt::boolean(e, "Saddle").unwrap_or(false);
        e.remove("Saddle");
        if !saddled {
            return;
        }
        let mut saddle = Compound::new();
        saddle.insert("id", Tag::String("minecraft:saddle".into()));
        saddle.insert("count", Tag::Int(1));
        e.insert("saddle", Tag::Compound(saddle));
    } else {
        return;
    }
    super::nbt::child(e, "drop_chances").insert("saddle", Tag::Float(2.0));
}

/// 4301: `ArmorItems`, `HandItems`, `body_armor_item` and `saddle` become one
/// `equipment` map by slot; empty slots are left out.
pub(super) fn equipment(_: &str, e: &mut Compound) {
    let take_list = |e: &mut Compound, key: &str| match e.remove(key) {
        Some(Tag::List(list)) => list,
        _ => Vec::new(),
    };
    let armor = take_list(e, "ArmorItems");
    let hands = take_list(e, "HandItems");
    let body = e.remove("body_armor_item");
    let saddle = e.remove("saddle");
    let real = |t: Option<&Tag>| match t {
        Some(Tag::Compound(item)) if item.get("id").is_some() => Some(Tag::Compound(item.clone())),
        _ => None,
    };
    let slots = [
        ("mainhand", real(hands.first())),
        ("offhand", real(hands.get(1))),
        ("feet", real(armor.first())),
        ("legs", real(armor.get(1))),
        ("chest", real(armor.get(2))),
        ("head", real(armor.get(3))),
        ("body", real(body.as_ref())),
        ("saddle", real(saddle.as_ref())),
    ];
    let mut out = Compound::new();
    for (slot, item) in slots {
        if let Some(item) = item {
            out.insert(slot, item);
        }
    }
    if !out.is_empty() {
        e.insert("equipment", Tag::Compound(out));
    }
}

/// 4303: fall distance is a double.
pub(super) fn fall_distance(_: &str, e: &mut Compound) {
    rename_with(e, "FallDistance", "fall_distance", |t| {
        Tag::Double(f64::from(super::nbt::number(&t).unwrap_or(0.0) as f32))
    });
}

/// 4306: thrown potions are a splash or a lingering potion entity.
pub(super) fn thrown_potions(id: &str, e: &mut Compound) {
    if id != "minecraft:potion" {
        return;
    }
    let item = super::nbt::compound(e, "Item").and_then(|i| string(i, "id").map(ns));
    let new = if item.as_deref() == Some("minecraft:lingering_potion") {
        "minecraft:lingering_potion"
    } else {
        "minecraft:splash_potion"
    };
    e.insert("id", Tag::String(new.into()));
}

/// 4314: block positions spelled as three fields are one `[I; x, y, z]`.
pub(super) fn inline_positions(id: &str, e: &mut Compound) {
    inline_pos(e, "SleepingX", "SleepingY", "SleepingZ", "sleeping_pos");
    match id {
        "minecraft:vex" => {
            rename(e, "LifeTicks", "life_ticks");
            inline_pos(e, "BoundX", "BoundY", "BoundZ", "bound_pos");
        }
        "minecraft:phantom" => {
            rename(e, "Size", "size");
            inline_pos(e, "AX", "AY", "AZ", "anchor_pos");
        }
        "minecraft:turtle" => {
            for k in ["TravelPosX", "TravelPosY", "TravelPosZ"] {
                e.remove(k);
            }
            inline_pos(e, "HomePosX", "HomePosY", "HomePosZ", "home_pos");
            rename(e, "HasEgg", "has_egg");
        }
        "minecraft:item_frame"
        | "minecraft:glow_item_frame"
        | "minecraft:painting"
        | "minecraft:leash_knot" => inline_pos(e, "TileX", "TileY", "TileZ", "block_pos"),
        _ => {}
    }
}

/// 4535: a copper golem's oxidation is named.
pub(super) fn copper_golem(id: &str, e: &mut Compound) {
    if id != "minecraft:copper_golem" {
        return;
    }
    if let Some(state) = e.get("weather_state").and_then(super::nbt::number) {
        let name = match state as i32 {
            1 => "exposed",
            2 => "weathered",
            3 => "oxidized",
            _ => "unaffected",
        };
        e.insert("weather_state", Tag::String(name.into()));
    }
}

/// 4763: villagers saved before 25w31a have settled their trades.
pub(super) fn villager_data_finalized(id: &str, e: &mut Compound) {
    if id == "minecraft:villager" || id == "minecraft:zombie_villager" {
        e.insert("VillagerDataFinalized", flag(true));
    }
}

/// 4885: a falling bed carries no block entity data.
pub(super) fn falling_bed(id: &str, e: &mut Compound) {
    if id == "minecraft:falling_block"
        && super::nbt::compound(e, "TileEntityData")
            .and_then(|be| string(be, "id").map(ns))
            .is_some_and(|be| be == "minecraft:bed")
    {
        e.remove("TileEntityData");
    }
}

// ---- players ----

/// 4312: armor and off-hand slots leave the inventory list for `equipment`.
pub(super) fn player_equipment(p: &mut Compound) {
    let mut equipment = Compound::new();
    if let Some(inventory) = list_mut(p, "Inventory") {
        inventory.retain(|item| {
            let Tag::Compound(item) = item else {
                return true;
            };
            let slot = match int(item, "Slot") {
                Some(100) => "feet",
                Some(101) => "legs",
                Some(102) => "chest",
                Some(103) => "head",
                Some(-106) => "offhand",
                _ => return true,
            };
            let mut stack = item.clone();
            stack.remove("Slot");
            equipment.insert(slot, Tag::Compound(stack));
            false
        });
    }
    p.insert("equipment", Tag::Compound(equipment));
}

/// 4314: the respawn point and the Nether entry point are one field each.
pub(super) fn player_positions(p: &mut Compound) {
    inline_pos(p, "SleepingX", "SleepingY", "SleepingZ", "sleeping_pos");
    if let (Some(x), Some(y), Some(z)) = (int(p, "SpawnX"), int(p, "SpawnY"), int(p, "SpawnZ")) {
        let mut respawn = Compound::new();
        respawn.insert("pos", super::nbt::int_array(x, y, z));
        for (from, to) in [
            ("SpawnAngle", "angle"),
            ("SpawnDimension", "dimension"),
            ("SpawnForced", "forced"),
        ] {
            if let Some(v) = p.get(from) {
                respawn.insert(to, v.clone());
            }
        }
        for k in [
            "SpawnX",
            "SpawnY",
            "SpawnZ",
            "SpawnAngle",
            "SpawnDimension",
            "SpawnForced",
        ] {
            p.remove(k);
        }
        p.insert("respawn", Tag::Compound(respawn));
    }
    if let Some(Tag::Compound(pos)) = p.remove("enteredNetherPosition") {
        let coord = |k: &str| Tag::Double(num(&pos, k).unwrap_or(0.0));
        p.insert(
            "entered_nether_pos",
            Tag::List(vec![coord("x"), coord("y"), coord("z")]),
        );
    }
}

/// 4548: a respawn point has a dimension, a yaw and a pitch.
pub(super) fn player_respawn(p: &mut Compound) {
    let Some(respawn) = compound_mut(p, "respawn") else {
        return;
    };
    let dimension = string(respawn, "dimension")
        .unwrap_or("minecraft:overworld")
        .to_owned();
    respawn.insert("dimension", Tag::String(dimension));
    let yaw = respawn
        .remove("angle")
        .and_then(|t| super::nbt::number(&t))
        .unwrap_or(0.0) as f32;
    respawn.insert("yaw", Tag::Float(yaw));
    respawn.insert("pitch", Tag::Float(0.0));
}

/// Stonecutter recipe ids that changed in 25w34a (4769).
pub(super) const STONECUTTER_RECIPES: [(&str, &str); 11] = [
    (
        "minecraft:chiseled_stone_bricks_stone_from_stonecutting",
        "minecraft:chiseled_stone_bricks_from_stone_stonecutting",
    ),
    (
        "minecraft:end_stone_brick_slab_from_end_stone_brick_stonecutting",
        "minecraft:end_stone_brick_slab_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:end_stone_brick_stairs_from_end_stone_brick_stonecutting",
        "minecraft:end_stone_brick_stairs_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:end_stone_brick_wall_from_end_stone_brick_stonecutting",
        "minecraft:end_stone_brick_wall_from_end_stone_bricks_stonecutting",
    ),
    (
        "minecraft:mossy_stone_brick_slab_from_mossy_stone_brick_stonecutting",
        "minecraft:mossy_stone_brick_slab_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:mossy_stone_brick_stairs_from_mossy_stone_brick_stonecutting",
        "minecraft:mossy_stone_brick_stairs_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:mossy_stone_brick_wall_from_mossy_stone_brick_stonecutting",
        "minecraft:mossy_stone_brick_wall_from_mossy_stone_bricks_stonecutting",
    ),
    (
        "minecraft:prismarine_brick_slab_from_prismarine_stonecutting",
        "minecraft:prismarine_brick_slab_from_prismarine_bricks_stonecutting",
    ),
    (
        "minecraft:prismarine_brick_stairs_from_prismarine_stonecutting",
        "minecraft:prismarine_brick_stairs_from_prismarine_bricks_stonecutting",
    ),
    (
        "minecraft:quartz_slab_from_stonecutting",
        "minecraft:quartz_slab_from_quartz_block_stonecutting",
    ),
    (
        "minecraft:stone_brick_walls_from_stone_stonecutting",
        "minecraft:stone_brick_wall_from_stone_stonecutting",
    ),
];

/// 4769: the recipe book knows the renamed stonecutter recipes.
pub(super) fn recipe_book(p: &mut Compound) {
    let Some(book) = compound_mut(p, "recipeBook") else {
        return;
    };
    for key in ["recipes", "toBeDisplayed"] {
        if let Some(list) = list_mut(book, key) {
            for r in list {
                if let Tag::String(id) = r
                    && let Some((_, new)) =
                        STONECUTTER_RECIPES.iter().find(|(old, _)| *old == ns(id))
                {
                    *id = (*new).to_owned();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::nbt::list;
    use super::*;

    fn item(id: &str) -> Tag {
        let mut c = Compound::new();
        c.insert("id", Tag::String(id.into()));
        c.insert("count", Tag::Int(1));
        Tag::Compound(c)
    }

    #[test]
    fn mob_equipment_becomes_slots() {
        let mut e = Compound::new();
        e.insert(
            "ArmorItems",
            Tag::List(vec![
                Tag::Compound(Compound::new()),
                Tag::Compound(Compound::new()),
                Tag::Compound(Compound::new()),
                item("minecraft:iron_helmet"),
            ]),
        );
        e.insert(
            "HandItems",
            Tag::List(vec![item("minecraft:bow"), Tag::Compound(Compound::new())]),
        );
        e.insert("ArmorDropChances", Tag::List(vec![Tag::Float(0.085); 4]));
        e.insert(
            "HandDropChances",
            Tag::List(vec![Tag::Float(2.0), Tag::Float(0.085)]),
        );
        drop_chances("minecraft:skeleton", &mut e);
        equipment("minecraft:skeleton", &mut e);
        assert!(e.get("ArmorItems").is_none());
        let Some(Tag::Compound(eq)) = e.get("equipment") else {
            panic!("no equipment");
        };
        assert_eq!(eq.get("head"), Some(&item("minecraft:iron_helmet")));
        assert_eq!(eq.get("mainhand"), Some(&item("minecraft:bow")));
        assert!(eq.get("feet").is_none());
        let Some(Tag::Compound(chances)) = e.get("drop_chances") else {
            panic!("no drop_chances");
        };
        assert_eq!(chances.get("mainhand"), Some(&Tag::Float(2.0)));
        assert_eq!(chances.len(), 1);
    }

    #[test]
    fn effects_get_names_and_boats_get_woods() {
        let mut eff = Compound::new();
        eff.insert("Id", Tag::Byte(1));
        eff.insert("Amplifier", Tag::Byte(0));
        eff.insert("Duration", Tag::Int(100));
        let mut e = Compound::new();
        e.insert("ActiveEffects", Tag::List(vec![Tag::Compound(eff)]));
        effect_ids("minecraft:zombie", &mut e);
        let [Tag::Compound(eff)] = list(&e, "active_effects") else {
            panic!("no active_effects");
        };
        assert_eq!(eff.get("id"), Some(&Tag::String("minecraft:speed".into())));
        assert_eq!(eff.get("duration"), Some(&Tag::Int(100)));

        let mut boat = Compound::new();
        boat.insert("id", Tag::String("minecraft:chest_boat".into()));
        boat.insert("Type", Tag::String("bamboo".into()));
        boats("minecraft:chest_boat", &mut boat);
        assert_eq!(
            boat.get("id"),
            Some(&Tag::String("minecraft:bamboo_chest_raft".into()))
        );
        assert!(boat.get("Type").is_none());
    }

    #[test]
    fn player_slots_and_respawn() {
        let mut helmet = Compound::new();
        helmet.insert("Slot", Tag::Byte(103));
        helmet.insert("id", Tag::String("minecraft:iron_helmet".into()));
        helmet.insert("count", Tag::Int(1));
        let mut p = Compound::new();
        p.insert("Inventory", Tag::List(vec![Tag::Compound(helmet)]));
        p.insert("SpawnX", Tag::Int(1));
        p.insert("SpawnY", Tag::Int(64));
        p.insert("SpawnZ", Tag::Int(-3));
        p.insert("SpawnAngle", Tag::Float(90.0));
        player_equipment(&mut p);
        player_positions(&mut p);
        player_respawn(&mut p);
        assert!(list(&p, "Inventory").is_empty());
        let Some(Tag::Compound(eq)) = p.get("equipment") else {
            panic!("no equipment");
        };
        assert!(eq.get("head").is_some());
        let Some(Tag::Compound(respawn)) = p.get("respawn") else {
            panic!("no respawn");
        };
        assert_eq!(respawn.get("pos"), Some(&Tag::IntArray(vec![1, 64, -3])));
        assert_eq!(respawn.get("yaw"), Some(&Tag::Float(90.0)));
        assert_eq!(
            respawn.get("dimension"),
            Some(&Tag::String("minecraft:overworld".into()))
        );
    }
}
