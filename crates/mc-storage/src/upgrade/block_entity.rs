//! Block entity fixes.

use mc_nbt::{Compound, Tag};

use super::nbt::{block_pos, compound_mut, dye, flag, int, json_text, long, rename, rename_with};

const SIGNS: [&str; 2] = ["minecraft:sign", "minecraft:hanging_sign"];

/// `{"text":""}`, the empty line old signs were filled with.
fn empty_line() -> Tag {
    json_text(r#"{"text":""}"#)
}

/// 3097: signs lose their filtered copies.
pub(super) fn filtered_signs(id: &str, be: &mut Compound) {
    if id == "minecraft:sign" {
        for i in 1..=4 {
            be.remove(&format!("FilteredText{i}"));
        }
    }
}

/// 3438: suspicious sand is one brushable block entity, with the loot
/// fields of other loot containers.
pub(super) fn brushable(id: &str, be: &mut Compound) {
    if id == "minecraft:suspicious_sand" {
        be.insert("id", Tag::String("minecraft:brushable_block".into()));
    }
    if id == "minecraft:suspicious_sand" || id == "minecraft:brushable_block" {
        rename(be, "loot_table", "LootTable");
        rename(be, "loot_table_seed", "LootTableSeed");
    }
}

const OLD_SIGN_FIELDS: [&str; 10] = [
    "Text1",
    "Text2",
    "Text3",
    "Text4",
    "FilteredText1",
    "FilteredText2",
    "FilteredText3",
    "FilteredText4",
    "Color",
    "GlowingText",
];

/// 3439: signs have a front and a back.
pub(super) fn double_sided(id: &str, be: &mut Compound) {
    if !SIGNS.contains(&id) {
        return;
    }
    let lines: Vec<Tag> = (1..=4)
        .map(|i| {
            be.get(&format!("Text{i}"))
                .cloned()
                .unwrap_or_else(empty_line)
        })
        .collect();
    let mut front = Compound::new();
    front.insert("messages", Tag::List(lines.clone()));
    front.insert(
        "color",
        be.get("Color")
            .cloned()
            .unwrap_or(Tag::String("black".into())),
    );
    front.insert(
        "has_glowing_text",
        be.get("GlowingText").cloned().unwrap_or(flag(false)),
    );
    let filtered: Vec<Option<Tag>> = (1..=4)
        .map(|i| be.get(&format!("FilteredText{i}")).cloned())
        .collect();
    if filtered.iter().any(Option::is_some) {
        let list = filtered
            .into_iter()
            .zip(&lines)
            .map(|(f, line)| f.unwrap_or_else(|| line.clone()))
            .collect();
        front.insert("filtered_messages", Tag::List(list));
    }
    let mut back = Compound::new();
    back.insert("messages", Tag::List(vec![empty_line(); 4]));
    back.insert("color", Tag::String("black".into()));
    back.insert("has_glowing_text", flag(false));
    be.insert("front_text", Tag::Compound(front));
    be.insert("back_text", Tag::Compound(back));
    be.insert("is_waxed", flag(false));
    be.insert("_filtered_correct", flag(true));
    for k in OLD_SIGN_FIELDS {
        be.remove(k);
    }
}

/// 3564: filtered sign lines that are empty fall back to the real line, and
/// matching filtered lines are dropped. Signs made by 3439 are already right.
pub(super) fn sign_filtered_lines(id: &str, be: &mut Compound) {
    if !SIGNS.contains(&id) {
        return;
    }
    if be.remove("_filtered_correct").is_some() {
        return;
    }
    for side in ["front_text", "back_text"] {
        let Some(text) = compound_mut(be, side) else {
            continue;
        };
        let Some(Tag::List(filtered)) = text.get("filtered_messages").cloned() else {
            continue;
        };
        let lines = super::nbt::list(text, "messages").to_vec();
        let fixed: Vec<Tag> = filtered
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if line == empty_line() {
                    lines.get(i).cloned().unwrap_or_else(empty_line)
                } else {
                    line
                }
            })
            .collect();
        if fixed == lines {
            text.remove("filtered_messages");
        } else {
            text.insert("filtered_messages", Tag::List(fixed));
        }
    }
    for k in OLD_SIGN_FIELDS {
        be.remove(k);
    }
}

/// 3568: beacon effects are named.
pub(super) fn beacon_effects(id: &str, be: &mut Compound) {
    if id != "minecraft:beacon" {
        return;
    }
    for (from, to) in [
        ("Primary", "primary_effect"),
        ("Secondary", "secondary_effect"),
    ] {
        if let Some(old) = be.remove(from)
            && let Some(name) = super::entity::effect_name(&old)
        {
            be.insert(to, name);
        }
    }
}

/// 3813: block positions are `[I; x, y, z]` under snake_case names.
pub(super) fn block_positions(id: &str, be: &mut Compound) {
    match id {
        "minecraft:beehive" => rename_with(be, "FlowerPos", "flower_pos", block_pos),
        "minecraft:end_gateway" => rename_with(be, "ExitPortal", "exit_portal", block_pos),
        _ => {}
    }
}

/// 3818: beehive fields are snake_case.
pub(super) fn beehive(id: &str, be: &mut Compound) {
    if id != "minecraft:beehive" {
        return;
    }
    if let Some(Tag::List(mut bees)) = be.remove("Bees") {
        for bee in &mut bees {
            if let Tag::Compound(bee) = bee {
                rename(bee, "EntityData", "entity_data");
                rename(bee, "TicksInHive", "ticks_in_hive");
                rename(bee, "MinOccupationTicks", "min_ticks_in_hive");
            }
        }
        be.insert("bees", Tag::List(bees));
    }
}

const PATTERNS: [(&str, &str); 41] = [
    ("b", "base"),
    ("bl", "square_bottom_left"),
    ("br", "square_bottom_right"),
    ("tl", "square_top_left"),
    ("tr", "square_top_right"),
    ("bs", "stripe_bottom"),
    ("ts", "stripe_top"),
    ("ls", "stripe_left"),
    ("rs", "stripe_right"),
    ("cs", "stripe_center"),
    ("ms", "stripe_middle"),
    ("drs", "stripe_downright"),
    ("dls", "stripe_downleft"),
    ("ss", "small_stripes"),
    ("cr", "cross"),
    ("sc", "straight_cross"),
    ("bt", "triangle_bottom"),
    ("tt", "triangle_top"),
    ("bts", "triangles_bottom"),
    ("tts", "triangles_top"),
    ("ld", "diagonal_left"),
    ("rd", "diagonal_up_right"),
    ("lud", "diagonal_up_left"),
    ("rud", "diagonal_right"),
    ("mc", "circle"),
    ("mr", "rhombus"),
    ("vh", "half_vertical"),
    ("hh", "half_horizontal"),
    ("vhr", "half_vertical_right"),
    ("hhb", "half_horizontal_bottom"),
    ("bo", "border"),
    ("cbo", "curly_border"),
    ("gra", "gradient"),
    ("gru", "gradient_up"),
    ("bri", "bricks"),
    ("glb", "globe"),
    ("cre", "creeper"),
    ("sku", "skull"),
    ("flo", "flower"),
    ("moj", "mojang"),
    ("pig", "piglin"),
];

/// 3818: banner layers name their pattern and colour.
pub(super) fn banner_patterns(id: &str, be: &mut Compound) {
    if id != "minecraft:banner" {
        return;
    }
    let Some(Tag::List(mut layers)) = be.remove("Patterns") else {
        return;
    };
    for layer in &mut layers {
        let Tag::Compound(layer) = layer else {
            continue;
        };
        if let Some(Tag::String(code)) = layer.remove("Pattern") {
            let name = PATTERNS
                .iter()
                .find(|(k, _)| *k == code)
                .map_or(code.clone(), |(_, n)| format!("minecraft:{n}"));
            layer.insert("pattern", Tag::String(name));
        }
        let color = int(layer, "Color").unwrap_or(0);
        layer.remove("Color");
        layer.insert("color", Tag::String(dye(color).into()));
    }
    be.insert("patterns", Tag::List(layers));
}

/// 3820: a player head stores a profile.
pub(super) fn skull_profile(id: &str, be: &mut Compound) {
    if id != "minecraft:skull" {
        return;
    }
    let owner = be.remove("SkullOwner");
    let extra = be.remove("ExtraType");
    if let Some(owner) = owner.or(extra) {
        be.insert("profile", Tag::Compound(super::item::profile(&owner)));
    }
}

/// 3825: an ominous banner block keeps its name as an item name.
pub(super) fn ominous_banner_name(id: &str, be: &mut Compound) {
    if id != "minecraft:banner" {
        return;
    }
    let ominous = be
        .get("CustomName")
        .and_then(super::nbt::json_translation)
        .is_some_and(|k| k == "block.minecraft.ominous_banner");
    if !ominous {
        return;
    }
    let Some(name) = be.remove("CustomName") else {
        return;
    };
    let mut components = Compound::new();
    components.insert("minecraft:item_name", name);
    components.insert(
        "minecraft:hide_additional_tooltip",
        Tag::Compound(Compound::new()),
    );
    be.insert("components", Tag::Compound(components));
}

/// 3825: a trial spawner's settings are its `normal_config`.
pub(super) fn trial_spawner_config(id: &str, be: &mut Compound) {
    if id != "minecraft:trial_spawner" {
        return;
    }
    let mut config = Compound::new();
    for key in [
        "spawn_range",
        "total_mobs",
        "simultaneous_mobs",
        "total_mobs_added_per_player",
        "simultaneous_mobs_added_per_player",
        "ticks_between_spawn",
        "spawn_potentials",
        "loot_tables_to_eject",
        "items_to_drop_when_ominous",
    ] {
        if let Some(v) = be.remove(key) {
            config.insert(key, v);
        }
    }
    if !config.is_empty() {
        be.insert("normal_config", Tag::Compound(config));
    }
}

/// 3833: a brushable block with an empty item has none.
pub(super) fn brushable_empty_item(id: &str, be: &mut Compound) {
    if id == "minecraft:brushable_block"
        && let Some(Tag::Compound(item)) = be.get("item")
        && super::item::is_empty_stack(item)
    {
        be.remove("item");
    }
}

/// 3945: a jukebox counts ticks since its song started.
pub(super) fn jukebox(id: &str, be: &mut Compound) {
    if id != "minecraft:jukebox" {
        return;
    }
    let ticks = long(be, "TickCount").unwrap_or(0) - long(be, "RecordStartTick").unwrap_or(0);
    for k in ["IsPlaying", "TickCount", "RecordStartTick"] {
        be.remove(k);
    }
    if ticks > 0 {
        be.insert("ticks_since_song_started", Tag::Long(ticks));
    }
}

/// 4068: a container lock is an item predicate.
pub(super) fn lock(_: &str, be: &mut Compound) {
    if let Some(old) = be.remove("Lock")
        && let Some(new) = super::components::lock_predicate(&old)
    {
        be.insert("lock", new);
    }
}

/// 4176: a lock matching only an empty name is no lock.
pub(super) fn broken_lock(_: &str, be: &mut Compound) {
    if be
        .get("lock")
        .is_some_and(super::components::is_broken_lock)
    {
        be.remove("lock");
    }
}

/// 4181: furnace timers are snake_case, with the total burn time saved too.
pub(super) fn furnace(id: &str, be: &mut Compound) {
    if !matches!(
        id,
        "minecraft:furnace" | "minecraft:smoker" | "minecraft:blast_furnace"
    ) {
        return;
    }
    rename(be, "CookTime", "cooking_time_spent");
    rename(be, "CookTimeTotal", "cooking_total_time");
    rename(be, "BurnTime", "lit_time_remaining");
    if let Some(v) = be.get("lit_time_remaining").cloned() {
        be.insert("lit_total_time", v);
    }
}

/// 4996: a decorated pot names the sherd on each side.
pub(super) fn pot_sherds(id: &str, be: &mut Compound) {
    if id != "minecraft:decorated_pot" {
        return;
    }
    let old = be.get("sherds").cloned().unwrap_or(Tag::List(Vec::new()));
    if let Some(new) = super::components::unpack_sherds(&old) {
        be.insert("sherds", new);
    }
}

/// 5002: signs placed before 25w41a may run operator commands.
pub(super) fn sign_op_flag(id: &str, be: &mut Compound) {
    if SIGNS.contains(&id) {
        be.insert("allow_op_features", flag(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_sign_gets_a_front_and_a_back() {
        let mut be = Compound::new();
        be.insert("Text1", json_text(r#"{"text":"Hello"}"#));
        be.insert("Color", Tag::String("red".into()));
        double_sided("minecraft:sign", &mut be);
        sign_filtered_lines("minecraft:sign", &mut be);
        assert!(be.get("Text1").is_none() && be.get("_filtered_correct").is_none());
        let Some(Tag::Compound(front)) = be.get("front_text") else {
            panic!("no front_text");
        };
        let [first, second, ..] = super::super::nbt::list(front, "messages") else {
            panic!("no messages");
        };
        assert_eq!(first, &json_text(r#"{"text":"Hello"}"#));
        assert_eq!(second, &empty_line());
        assert_eq!(front.get("color"), Some(&Tag::String("red".into())));
    }

    #[test]
    fn banners_beacons_and_furnaces() {
        let mut layer = Compound::new();
        layer.insert("Pattern", Tag::String("cre".into()));
        layer.insert("Color", Tag::Int(15));
        let mut banner = Compound::new();
        banner.insert("Patterns", Tag::List(vec![Tag::Compound(layer)]));
        banner_patterns("minecraft:banner", &mut banner);
        let [Tag::Compound(layer)] = super::super::nbt::list(&banner, "patterns") else {
            panic!("no patterns");
        };
        assert_eq!(
            layer.get("pattern"),
            Some(&Tag::String("minecraft:creeper".into()))
        );
        assert_eq!(layer.get("color"), Some(&Tag::String("black".into())));

        let mut beacon = Compound::new();
        beacon.insert("Primary", Tag::Int(1));
        beacon_effects("minecraft:beacon", &mut beacon);
        assert_eq!(
            beacon.get("primary_effect"),
            Some(&Tag::String("minecraft:speed".into()))
        );

        let mut furnace_be = Compound::new();
        furnace_be.insert("BurnTime", Tag::Short(40));
        furnace("minecraft:furnace", &mut furnace_be);
        assert_eq!(furnace_be.get("lit_total_time"), Some(&Tag::Short(40)));
    }
}
