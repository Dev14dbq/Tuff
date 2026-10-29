//! Item stack fixes, and 24w09a's (3818) move from `Count` + free-form `tag`
//! to `count` + typed `components`. Whatever of `tag` has no component left
//! goes to `minecraft:custom_data`, so data plugins put there survives.

use mc_nbt::{Compound, Tag};

use super::nbt::{
    block_pos, child, compound_mut, dye, each_compound, flag, int, json_text, list, ns, num,
    rename, rename_value, string,
};

const POTIONS: [&str; 4] = [
    "minecraft:potion",
    "minecraft:splash_potion",
    "minecraft:lingering_potion",
    "minecraft:tipped_arrow",
];

/// `tag` of an old stack.
fn tag_mut(item: &mut Compound) -> Option<&mut Compound> {
    compound_mut(item, "tag")
}

/// Renames an item id through `table`.
pub(super) fn rename_id(item: &mut Compound, table: &[(&str, &str)]) {
    rename_value(item, "id", table);
}

/// 3097: books lose their filtered copies.
pub(super) fn filtered_books(_: &str, item: &mut Compound) {
    if let Some(tag) = tag_mut(item) {
        tag.remove("filtered_title");
        tag.remove("filtered_pages");
    }
}

/// 3094: goat horns name their instrument instead of a sound index.
pub(super) fn goat_horn(id: &str, item: &mut Compound) {
    const SOUNDS: [&str; 8] = [
        "ponder", "sing", "seek", "feel", "admire", "call", "yearn", "dream",
    ];
    if id != "minecraft:goat_horn" {
        return;
    }
    if let Some(tag) = tag_mut(item) {
        let index = tag
            .remove("SoundVariant")
            .and_then(|t| super::nbt::number(&t))
            .map_or(0, |v| v as usize);
        let sound = SOUNDS.get(index).unwrap_or(&SOUNDS[0]);
        tag.insert(
            "instrument",
            Tag::String(format!("minecraft:{sound}_goat_horn")),
        );
    }
}

/// 3322: potion effects count how long they have been active, not when they
/// changed.
pub(super) fn effect_durations(id: &str, item: &mut Compound) {
    if !POTIONS.contains(&id) {
        return;
    }
    if let Some(tag) = tag_mut(item) {
        super::entity::effect_list_durations(tag, "CustomPotionEffects");
    }
}

/// 3568: numeric effect ids in potions and suspicious stew become names.
pub(super) fn effect_ids(id: &str, item: &mut Compound) {
    let Some(tag) = tag_mut(item) else {
        return;
    };
    if id == "minecraft:suspicious_stew" {
        if let Some(Tag::List(mut effects)) = tag.remove("Effects") {
            for e in &mut effects {
                if let Tag::Compound(e) = e {
                    super::entity::stew_effect(e);
                }
            }
            tag.insert("effects", Tag::List(effects));
        }
    } else if POTIONS.contains(&id) {
        super::entity::effect_instances(tag, "CustomPotionEffects", "custom_potion_effects");
    }
}

/// 3803: the sweeping enchantment is `sweeping_edge`.
pub(super) fn sweeping_edge(_: &str, item: &mut Compound) {
    if let Some(tag) = tag_mut(item) {
        for key in ["Enchantments", "StoredEnchantments"] {
            each_compound(tag, key, |e| {
                rename_value(
                    e,
                    "id",
                    &[("minecraft:sweeping", "minecraft:sweeping_edge")],
                );
            });
        }
    }
}

/// 3813: a lodestone compass stores its target as `[I; x, y, z]`.
pub(super) fn compass_pos(id: &str, item: &mut Compound) {
    if id != "minecraft:compass" {
        return;
    }
    if let Some(tag) = tag_mut(item)
        && let Some(pos) = tag.remove("LodestonePos")
    {
        tag.insert("LodestonePos", block_pos(pos));
    }
}

/// 3814: `horse.jump_strength` became a generic attribute.
pub(super) fn jump_strength(_: &str, item: &mut Compound) {
    if let Some(tag) = tag_mut(item) {
        each_compound(tag, "AttributeModifiers", |m| {
            rename_value(
                m,
                "AttributeName",
                &[(
                    "minecraft:horse.jump_strength",
                    "minecraft:generic.jump_strength",
                )],
            );
        });
    }
}

/// Block state properties that were booleans in item `BlockStateTag`s.
const BOOLEAN_PROPERTIES: [&str; 47] = [
    "attached",
    "bottom",
    "conditional",
    "disarmed",
    "drag",
    "enabled",
    "extended",
    "eye",
    "falling",
    "hanging",
    "has_bottle_0",
    "has_bottle_1",
    "has_bottle_2",
    "has_record",
    "has_book",
    "inverted",
    "in_wall",
    "lit",
    "locked",
    "occupied",
    "open",
    "persistent",
    "powered",
    "short",
    "signal_fire",
    "snowy",
    "triggered",
    "unstable",
    "waterlogged",
    "berries",
    "bloom",
    "shrieking",
    "can_summon",
    "up",
    "down",
    "north",
    "east",
    "south",
    "west",
    "slot_0_occupied",
    "slot_1_occupied",
    "slot_2_occupied",
    "slot_3_occupied",
    "slot_4_occupied",
    "slot_5_occupied",
    "cracked",
    "crafting",
];

const BUCKET_MOBS: [&str; 6] = [
    "minecraft:pufferfish_bucket",
    "minecraft:salmon_bucket",
    "minecraft:cod_bucket",
    "minecraft:tropical_fish_bucket",
    "minecraft:axolotl_bucket",
    "minecraft:tadpole_bucket",
];

const BUCKET_KEYS: [&str; 10] = [
    "NoAI",
    "Silent",
    "NoGravity",
    "Glowing",
    "Invulnerable",
    "Health",
    "Age",
    "Variant",
    "HuntingCooldown",
    "BucketVariantTag",
];

/// Containers whose `BlockEntityTag.Items` become `minecraft:container`.
const CONTAINERS: [&str; 15] = [
    "minecraft:shulker_box",
    "minecraft:chest",
    "minecraft:trapped_chest",
    "minecraft:furnace",
    "minecraft:ender_chest",
    "minecraft:dispenser",
    "minecraft:dropper",
    "minecraft:brewing_stand",
    "minecraft:hopper",
    "minecraft:barrel",
    "minecraft:smoker",
    "minecraft:blast_furnace",
    "minecraft:campfire",
    "minecraft:chiseled_bookshelf",
    "minecraft:crafter",
];

/// One old stack being turned into components.
struct Stack<'a> {
    id: &'a str,
    tag: Compound,
    components: Compound,
}

impl Stack<'_> {
    fn take(&mut self, key: &str) -> Option<Tag> {
        self.tag.remove(key)
    }

    fn set(&mut self, component: &str, value: Tag) {
        self.components.insert(component, value);
    }

    /// Moves a tag entry to a component, unless it is `default`.
    fn move_to_component(&mut self, key: &str, component: &str, default: Option<&Tag>) {
        if let Some(value) = self.take(key)
            && Some(&value) != default
        {
            self.set(component, value);
        }
    }

    fn into(&mut self, key: &str, target: &mut Compound, to: &str) {
        if let Some(value) = self.take(key) {
            target.insert(to, value);
        }
    }
}

/// 3818: an old stack (`id`, `Count`, `tag`) becomes `id`, `count`,
/// `components`.
pub(super) fn componentize(_: &str, item: &mut Compound) {
    let Some(raw_id) = string(item, "id").map(str::to_owned) else {
        return;
    };
    let Some(count) = num(item, "Count") else {
        return;
    };
    let id = ns(&raw_id);
    let tag = match item.remove("tag") {
        Some(Tag::Compound(tag)) => tag,
        _ => Compound::new(),
    };
    item.remove("Count");
    let mut stack = Stack {
        id: &id,
        tag,
        components: Compound::new(),
    };
    fix_stack(&mut stack);
    let Stack {
        tag,
        mut components,
        ..
    } = stack;
    if !tag.is_empty() {
        components.insert("minecraft:custom_data", Tag::Compound(tag));
    }
    item.insert("id", Tag::String(id.clone()));
    item.insert("count", Tag::Int(count as i32));
    if !components.is_empty() {
        item.insert("components", Tag::Compound(components));
    }
}

fn fix_stack(s: &mut Stack) {
    let hide = s
        .take("HideFlags")
        .and_then(|t| super::nbt::number(&t))
        .map_or(0, |v| v as i32);
    s.move_to_component("Damage", "minecraft:damage", Some(&Tag::Int(0)));
    s.move_to_component("RepairCost", "minecraft:repair_cost", Some(&Tag::Int(0)));
    s.move_to_component("CustomModelData", "minecraft:custom_model_data", None);
    if let Some(Tag::Compound(state)) = s.take("BlockStateTag") {
        s.set(
            "minecraft:block_state",
            Tag::Compound(block_state_tag(state)),
        );
    }
    s.move_to_component("EntityTag", "minecraft:entity_data", None);
    if let Some(Tag::Compound(mut be)) = s.take("BlockEntityTag") {
        let be_id = string(&be, "id").map(ns).unwrap_or_default();
        block_entity_tag(s, &mut be, &be_id);
        let only_id = be.len() == 1 && be.get("id").is_some();
        if !be.is_empty() && !only_id {
            s.set("minecraft:block_entity_data", Tag::Compound(be));
        }
    }
    if s.take("Unbreakable")
        .is_some_and(|t| super::nbt::number(&t).is_some_and(|v| v != 0.0))
    {
        let mut c = Compound::new();
        if hide & 4 != 0 {
            c.insert("show_in_tooltip", flag(false));
        }
        s.set("minecraft:unbreakable", Tag::Compound(c));
    }
    enchantments(s, "Enchantments", "minecraft:enchantments", hide & 1 != 0);
    if s.id == "minecraft:enchanted_book" {
        enchantments(
            s,
            "StoredEnchantments",
            "minecraft:stored_enchantments",
            hide & 32 != 0,
        );
    }
    if let Some(Tag::Compound(mut display)) = s.take("display") {
        fix_display(s, &mut display, hide);
        if !display.is_empty() {
            s.tag.insert("display", Tag::Compound(display));
        }
    }
    predicates(s, "CanDestroy", "minecraft:can_break", hide & 8 != 0);
    predicates(s, "CanPlaceOn", "minecraft:can_place_on", hide & 16 != 0);
    if let Some(Tag::List(modifiers)) = s.take("AttributeModifiers") {
        let mut c = Compound::new();
        let list: Vec<Tag> = modifiers
            .iter()
            .filter_map(|m| match m {
                Tag::Compound(m) => Some(Tag::Compound(attribute_modifier(m))),
                _ => None,
            })
            .collect();
        c.insert("modifiers", Tag::List(list));
        if hide & 2 != 0 {
            c.insert("show_in_tooltip", flag(false));
        }
        s.set("minecraft:attribute_modifiers", Tag::Compound(c));
    }
    if let Some(mut trim) = s.take("Trim") {
        if hide & 128 != 0
            && let Tag::Compound(t) = &mut trim
        {
            t.insert("show_in_tooltip", flag(false));
        }
        s.set("minecraft:trim", trim);
    }
    if hide & 32 != 0 {
        s.set(
            "minecraft:hide_additional_tooltip",
            Tag::Compound(Compound::new()),
        );
    }
    let empty_list = Tag::List(Vec::new());
    match s.id {
        "minecraft:crossbow" => {
            s.take("Charged");
            s.move_to_component(
                "ChargedProjectiles",
                "minecraft:charged_projectiles",
                Some(&empty_list),
            );
        }
        "minecraft:bundle" => {
            s.move_to_component("Items", "minecraft:bundle_contents", Some(&empty_list))
        }
        "minecraft:filled_map" => {
            s.move_to_component("map", "minecraft:map_id", None);
            if let Some(Tag::List(decorations)) = s.take("Decorations") {
                let mut map = Compound::new();
                for d in &decorations {
                    if let Tag::Compound(d) = d {
                        let key = string(d, "id").unwrap_or("").to_owned();
                        if map.get(&key).is_none() {
                            map.insert(key, Tag::Compound(map_decoration(d)));
                        }
                    }
                }
                if !map.is_empty() {
                    s.set("minecraft:map_decorations", Tag::Compound(map));
                }
            }
        }
        "minecraft:writable_book" => {
            if let Some(pages) = book_pages(s) {
                let mut c = Compound::new();
                c.insert("pages", pages);
                s.set("minecraft:writable_book_content", Tag::Compound(c));
            }
        }
        "minecraft:written_book" => written_book(s),
        "minecraft:suspicious_stew" => {
            s.move_to_component("effects", "minecraft:suspicious_stew_effects", None);
        }
        "minecraft:debug_stick" => {
            s.move_to_component("DebugProperty", "minecraft:debug_stick_state", None)
        }
        "minecraft:goat_horn" => s.move_to_component("instrument", "minecraft:instrument", None),
        "minecraft:knowledge_book" => s.move_to_component("Recipes", "minecraft:recipes", None),
        "minecraft:compass" => lodestone(s),
        "minecraft:firework_rocket" => fireworks(s),
        "minecraft:firework_star" => {
            if let Some(Tag::Compound(mut explosion)) = s.take("Explosion") {
                s.set(
                    "minecraft:firework_explosion",
                    Tag::Compound(firework_explosion(&explosion)),
                );
                for key in ["Type", "Colors", "FadeColors", "Trail", "Flicker"] {
                    explosion.remove(key);
                }
                if !explosion.is_empty() {
                    s.tag.insert("Explosion", Tag::Compound(explosion));
                }
            }
        }
        "minecraft:player_head" => {
            if let Some(owner) = s.take("SkullOwner") {
                s.set("minecraft:profile", Tag::Compound(profile(&owner)));
            }
        }
        _ => {}
    }
    if POTIONS.contains(&s.id) {
        let mut c = Compound::new();
        if let Some(Tag::String(potion)) = s.take("Potion")
            && potion != "minecraft:empty"
        {
            c.insert("potion", Tag::String(potion));
        }
        s.into("CustomPotionColor", &mut c, "custom_color");
        s.into("custom_potion_effects", &mut c, "custom_effects");
        if !c.is_empty() {
            s.set("minecraft:potion_contents", Tag::Compound(c));
        }
    }
    if BUCKET_MOBS.contains(&s.id) {
        let mut c = Compound::new();
        for key in BUCKET_KEYS {
            s.into(key, &mut c, key);
        }
        if !c.is_empty() {
            s.set("minecraft:bucket_entity_data", Tag::Compound(c));
        }
    }
}

/// Item block state properties are strings; booleans and numbers become
/// their text.
fn block_state_tag(state: Compound) -> Compound {
    let mut out = Compound::new();
    for (k, v) in state.iter() {
        let value = match v {
            Tag::Byte(b) if BOOLEAN_PROPERTIES.contains(&k) => {
                Tag::String(if *b != 0 { "true" } else { "false" }.into())
            }
            Tag::Byte(n) => Tag::String(n.to_string()),
            Tag::Short(n) => Tag::String(n.to_string()),
            Tag::Int(n) => Tag::String(n.to_string()),
            Tag::Long(n) => Tag::String(n.to_string()),
            Tag::Float(n) => Tag::String(n.to_string()),
            Tag::Double(n) => Tag::String(n.to_string()),
            other => other.clone(),
        };
        out.insert(k, value);
    }
    out
}

/// What of an item's block entity data has its own component now.
fn block_entity_tag(s: &mut Stack, be: &mut Compound, be_id: &str) {
    if let Some(lock) = be.remove("Lock") {
        s.set("minecraft:lock", lock);
    }
    if let Some(table) = be.remove("LootTable") {
        let mut c = Compound::new();
        c.insert("loot_table", table);
        let seed = match be.remove("LootTableSeed") {
            Some(Tag::Long(v)) => v,
            Some(other) => super::nbt::number(&other).map_or(0, |v| v as i64),
            None => 0,
        };
        if seed != 0 {
            c.insert("seed", Tag::Long(seed));
        }
        s.set("minecraft:container_loot", Tag::Compound(c));
    }
    match be_id {
        "minecraft:skull" => {
            if let Some(sound) = be.remove("note_block_sound") {
                s.set("minecraft:note_block_sound", sound);
            }
        }
        "minecraft:decorated_pot" => {
            if let Some(sherds) = be.remove("sherds") {
                s.set("minecraft:pot_decorations", sherds);
            }
            if let Some(item) = be.remove("item") {
                let mut slot = Compound::new();
                slot.insert("slot", Tag::Int(0));
                slot.insert("item", item);
                s.set("minecraft:container", Tag::List(vec![Tag::Compound(slot)]));
            }
        }
        "minecraft:banner" => {
            if let Some(patterns) = be.remove("patterns") {
                s.set("minecraft:banner_patterns", patterns);
            }
            if let Some(base) = be.remove("Base").and_then(|t| super::nbt::number(&t)) {
                s.set("minecraft:base_color", Tag::String(dye(base as i32).into()));
            }
        }
        "minecraft:beehive" => {
            if let Some(bees) = be.remove("bees") {
                s.set("minecraft:bees", bees);
            }
        }
        id if CONTAINERS.contains(&id) => {
            if let Some(Tag::List(items)) = be.remove("Items") {
                let slots: Vec<Tag> = items
                    .into_iter()
                    .filter_map(|t| match t {
                        Tag::Compound(mut item) => {
                            let slot = int(&item, "Slot").unwrap_or(0) & 0xff;
                            item.remove("Slot");
                            let mut entry = Compound::new();
                            entry.insert("slot", Tag::Int(slot));
                            entry.insert("item", Tag::Compound(item));
                            Some(Tag::Compound(entry))
                        }
                        _ => None,
                    })
                    .collect();
                if !slots.is_empty() {
                    s.set("minecraft:container", Tag::List(slots));
                }
            }
        }
        _ => {}
    }
}

fn enchantments(s: &mut Stack, key: &str, component: &str, hidden: bool) {
    let raw = s.take(key);
    let mut levels = Compound::new();
    let mut any = false;
    if let Some(Tag::List(list)) = &raw {
        for e in list {
            if let Tag::Compound(e) = e
                && let (Some(id), Some(level)) = (string(e, "id"), num(e, "lvl"))
            {
                let level = (level as i32).clamp(0, 255);
                if level > 0 {
                    levels.insert(id, Tag::Int(level));
                    any = true;
                }
            }
        }
    }
    if any || hidden {
        let mut c = Compound::new();
        c.insert("levels", Tag::Compound(levels));
        if hidden {
            c.insert("show_in_tooltip", flag(false));
        }
        s.set(component, Tag::Compound(c));
    }
    if raw.is_some() && !any {
        s.set("minecraft:enchantment_glint_override", flag(true));
    }
}

/// Whether a stored name is JSON a strict parser accepts (vanilla drops
/// names and lore lines that are not).
fn strict_json(tag: &Tag) -> bool {
    matches!(tag, Tag::String(s) if serde_json::from_str::<serde_json::Value>(s).is_ok())
}

fn fix_display(s: &mut Stack, display: &mut Compound, hide: i32) {
    if let Some(name) = display.remove("Name")
        && strict_json(&name)
    {
        s.set("minecraft:custom_name", name);
    }
    if let Some(Tag::List(lore)) = display.remove("Lore") {
        s.set(
            "minecraft:lore",
            Tag::List(lore.into_iter().filter(strict_json).collect()),
        );
    }
    let color = display
        .remove("color")
        .and_then(|t| super::nbt::number(&t))
        .map(|v| v as i32);
    let hide_dye = hide & 64 != 0;
    if color.is_some() || hide_dye {
        let mut c = Compound::new();
        c.insert("rgb", Tag::Int(color.unwrap_or(10_511_680)));
        if hide_dye {
            c.insert("show_in_tooltip", flag(false));
        }
        s.set("minecraft:dyed_color", Tag::Compound(c));
    }
    if let Some(Tag::String(key)) = display.remove("LocName") {
        s.set("minecraft:item_name", super::nbt::json_translate(&key));
    }
    if s.id == "minecraft:filled_map"
        && let Some(color) = display.remove("MapColor")
    {
        s.set("minecraft:map_color", color);
    }
}

fn predicates(s: &mut Stack, key: &str, component: &str, hidden: bool) {
    let Some(Tag::List(old)) = s.take(key) else {
        return;
    };
    let list: Vec<Tag> = old
        .into_iter()
        .map(|t| match t {
            Tag::String(text) => Tag::Compound(block_predicate(&text)),
            other => other,
        })
        .collect();
    let mut c = Compound::new();
    c.insert("predicates", Tag::List(list));
    if hidden {
        c.insert("show_in_tooltip", flag(false));
    }
    s.set(component, Tag::Compound(c));
}

/// `stone[facing=up]{nbt}` → `{blocks, state, nbt}`.
fn block_predicate(text: &str) -> Compound {
    let props_at = text.find('[');
    let nbt_at = text.find('{');
    let name_end = [props_at, nbt_at]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text.len());
    let mut c = Compound::new();
    c.insert("blocks", Tag::String(text[..name_end].trim().to_owned()));
    if let (Some(start), Some(end)) = (props_at, text.find(']'))
        && start < end
    {
        let mut state = Compound::new();
        for pair in text[start + 1..end].split(',') {
            if let Some((k, v)) = pair.split_once('=') {
                state.insert(k.trim(), Tag::String(v.trim().to_owned()));
            }
        }
        c.insert("state", Tag::Compound(state));
    }
    if let (Some(start), Some(end)) = (nbt_at, text.find('}'))
        && start < end
    {
        c.insert("nbt", Tag::String(text[start..=end].to_owned()));
    }
    c
}

fn attribute_modifier(old: &Compound) -> Compound {
    let mut m = Compound::new();
    m.insert("name", Tag::String(String::new()));
    m.insert("amount", Tag::Double(0.0));
    m.insert("operation", Tag::String("add_value".into()));
    for (from, to) in [
        ("AttributeName", "type"),
        ("Slot", "slot"),
        ("UUID", "uuid"),
        ("Name", "name"),
        ("Amount", "amount"),
    ] {
        if let Some(v) = old.get(from) {
            m.insert(to, v.clone());
        }
    }
    if let Some(op) = old.get("Operation") {
        m.insert("operation", Tag::String(operation(op).into()));
    }
    m
}

pub(super) fn operation(op: &Tag) -> &'static str {
    match super::nbt::number(op).map(|v| v as i32) {
        Some(1) => "add_multiplied_base",
        Some(2) => "add_multiplied_total",
        _ => "add_value",
    }
}

fn map_decoration(old: &Compound) -> Compound {
    const TYPES: [&str; 34] = [
        "player",
        "frame",
        "red_marker",
        "blue_marker",
        "target_x",
        "target_point",
        "player_off_map",
        "player_off_limits",
        "mansion",
        "monument",
        "banner_white",
        "banner_orange",
        "banner_magenta",
        "banner_light_blue",
        "banner_yellow",
        "banner_lime",
        "banner_pink",
        "banner_gray",
        "banner_light_gray",
        "banner_cyan",
        "banner_purple",
        "banner_blue",
        "banner_brown",
        "banner_green",
        "banner_red",
        "banner_black",
        "red_x",
        "village_desert",
        "village_plains",
        "village_savanna",
        "village_snowy",
        "village_taiga",
        "jungle_temple",
        "swamp_hut",
    ];
    let kind = int(old, "type")
        .and_then(|t| usize::try_from(t).ok())
        .and_then(|t| TYPES.get(t))
        .unwrap_or(&"player");
    let mut c = Compound::new();
    c.insert("type", Tag::String((*kind).into()));
    c.insert("x", Tag::Double(num(old, "x").unwrap_or(0.0)));
    c.insert("z", Tag::Double(num(old, "z").unwrap_or(0.0)));
    c.insert(
        "rotation",
        Tag::Float(num(old, "rot").unwrap_or(0.0) as f32),
    );
    c
}

fn filtered_text(raw: String, filtered: Option<String>) -> Tag {
    let mut c = Compound::new();
    c.insert("raw", Tag::String(raw));
    if let Some(f) = filtered {
        c.insert("filtered", Tag::String(f));
    }
    Tag::Compound(c)
}

fn book_pages(s: &mut Stack) -> Option<Tag> {
    let pages = s.take("pages");
    let filtered = s.take("filtered_pages");
    let pages: Vec<String> = match pages {
        Some(Tag::List(list)) => list
            .into_iter()
            .map(|t| match t {
                Tag::String(s) => s,
                _ => String::new(),
            })
            .collect(),
        _ => Vec::new(),
    };
    if pages.is_empty() {
        return None;
    }
    let filtered = match filtered {
        Some(Tag::Compound(c)) => c,
        _ => Compound::new(),
    };
    let out = pages
        .into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let f = string(&filtered, &i.to_string()).map(str::to_owned);
            filtered_text(raw, f)
        })
        .collect();
    Some(Tag::List(out))
}

fn written_book(s: &mut Stack) {
    let pages = book_pages(s);
    let title = match s.take("title") {
        Some(Tag::String(t)) => t,
        _ => String::new(),
    };
    let filtered_title = match s.take("filtered_title") {
        Some(Tag::String(t)) => Some(t),
        _ => None,
    };
    let mut c = Compound::new();
    c.insert("title", filtered_text(title, filtered_title));
    s.into("author", &mut c, "author");
    s.into("resolved", &mut c, "resolved");
    s.into("generation", &mut c, "generation");
    if let Some(pages) = pages {
        c.insert("pages", pages);
    }
    s.set("minecraft:written_book_content", Tag::Compound(c));
}

fn lodestone(s: &mut Stack) {
    let pos = s.take("LodestonePos");
    let dimension = s.take("LodestoneDimension");
    if pos.is_none() && dimension.is_none() {
        return;
    }
    let tracked = s
        .take("LodestoneTracked")
        .and_then(|t| super::nbt::number(&t))
        .is_none_or(|v| v != 0.0);
    let mut c = Compound::new();
    if let (Some(pos), Some(dimension)) = (pos, dimension) {
        let mut target = Compound::new();
        target.insert("pos", pos);
        target.insert("dimension", dimension);
        c.insert("target", Tag::Compound(target));
    }
    if !tracked {
        c.insert("tracked", flag(false));
    }
    s.set("minecraft:lodestone_tracker", Tag::Compound(c));
}

fn fireworks(s: &mut Stack) {
    let Some(Tag::Compound(mut rocket)) = s.take("Fireworks") else {
        return;
    };
    let explosions: Vec<Tag> = list(&rocket, "Explosions")
        .iter()
        .filter_map(|e| match e {
            Tag::Compound(e) => Some(Tag::Compound(firework_explosion(e))),
            _ => None,
        })
        .collect();
    let flight = int(&rocket, "Flight").unwrap_or(0);
    let mut c = Compound::new();
    c.insert("explosions", Tag::List(explosions));
    c.insert("flight_duration", Tag::Byte(flight as i8));
    s.set("minecraft:fireworks", Tag::Compound(c));
    rocket.remove("Explosions");
    rocket.remove("Flight");
    if !rocket.is_empty() {
        s.tag.insert("Fireworks", Tag::Compound(rocket));
    }
}

fn firework_explosion(old: &Compound) -> Compound {
    let mut e = old.clone();
    let shape = match int(&e, "Type").unwrap_or(0) {
        1 => "large_ball",
        2 => "star",
        3 => "creeper",
        4 => "burst",
        _ => "small_ball",
    };
    e.remove("Type");
    e.insert("shape", Tag::String(shape.into()));
    rename(&mut e, "Colors", "colors");
    rename(&mut e, "FadeColors", "fade_colors");
    rename(&mut e, "Trail", "has_trail");
    rename(&mut e, "Flicker", "has_twinkle");
    e
}

fn valid_name(name: &str) -> bool {
    name.chars().count() <= 16 && !name.chars().any(|c| c <= ' ' || c >= '\u{7f}')
}

/// An old `SkullOwner` (a name, or `{Name, Id, Properties}`) → a profile.
pub(super) fn profile(owner: &Tag) -> Compound {
    let mut p = Compound::new();
    match owner {
        Tag::String(name) => {
            if valid_name(name) {
                p.insert("name", Tag::String(name.clone()));
            }
        }
        Tag::Compound(old) => {
            let name = string(old, "Name").unwrap_or("");
            if valid_name(name) {
                p.insert("name", Tag::String(name.to_owned()));
            }
            if let Some(id) = old.get("Id") {
                p.insert("id", id.clone());
            }
            if let Some(Tag::Compound(props)) = old.get("Properties") {
                let mut out = Vec::new();
                for (key, values) in props.iter() {
                    if let Tag::List(values) = values {
                        for v in values {
                            let Tag::Compound(v) = v else {
                                continue;
                            };
                            let mut prop = Compound::new();
                            prop.insert("name", Tag::String(key.to_owned()));
                            prop.insert(
                                "value",
                                Tag::String(string(v, "Value").unwrap_or("").into()),
                            );
                            if let Some(sig) = string(v, "Signature") {
                                prop.insert("signature", Tag::String(sig.to_owned()));
                            }
                            out.push(Tag::Compound(prop));
                        }
                    }
                }
                if !out.is_empty() {
                    p.insert("properties", Tag::List(out));
                }
            }
        }
        _ => {}
    }
    p
}

/// 3818: an arrow's potion moves into its pickup item, so the
/// componentization after it carries it on.
pub(super) fn arrow_potion_into_item(id: &str, arrow: &mut Compound) {
    if id != "minecraft:arrow" {
        return;
    }
    let potion = arrow.remove("Potion");
    let effects = arrow.remove("custom_potion_effects");
    let color = arrow.remove("Color");
    if potion.is_none() && effects.is_none() && color.is_none() {
        return;
    }
    let item = child(arrow, "item");
    let tag = child(item, "tag");
    if let Some(p) = potion {
        tag.insert("Potion", p);
    }
    if let Some(e) = effects {
        tag.insert("custom_potion_effects", e);
    }
    if let Some(c) = color {
        tag.insert("CustomPotionColor", c);
    }
}

/// 3685: arrows remember the item they were shot as.
pub(super) fn projectile_item(id: &str, arrow: &mut Compound) {
    let item = match id {
        "minecraft:arrow" => {
            let plain = string(arrow, "Potion").is_none_or(|p| p == "minecraft:empty");
            if plain {
                "minecraft:arrow"
            } else {
                "minecraft:tipped_arrow"
            }
        }
        "minecraft:spectral_arrow" => "minecraft:spectral_arrow",
        _ => return,
    };
    let mut stack = Compound::new();
    stack.insert("id", Tag::String(item.into()));
    stack.insert("Count", Tag::Int(1));
    arrow.insert("item", Tag::Compound(stack));
}

/// Whether an old-format stack holds nothing.
pub(super) fn is_empty_stack(item: &Compound) -> bool {
    let id = string(item, "id").map_or_else(|| "minecraft:air".to_owned(), ns);
    id == "minecraft:air" || int(item, "count").unwrap_or(0) == 0
}

/// A JSON string literal, the way old saves stored a plain name.
pub(super) fn json_string(text: &str) -> Tag {
    json_text(&serde_json::Value::String(text.to_owned()).to_string())
}

/// 4885: a bed's leftover block entity data goes away.
pub(super) fn drop_bed_data(_: &str, components: &mut Compound) {
    let is_bed = compound_mut(components, "minecraft:block_entity_data")
        .and_then(|be| string(be, "id").map(ns))
        .is_some_and(|id| id == "minecraft:bed");
    if is_bed {
        components.remove("minecraft:block_entity_data");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old(id: &str, count: i8, tag: Compound) -> Compound {
        let mut c = Compound::new();
        c.insert("Slot", Tag::Byte(3));
        c.insert("id", Tag::String(id.into()));
        c.insert("Count", Tag::Byte(count));
        if !tag.is_empty() {
            c.insert("tag", Tag::Compound(tag));
        }
        c
    }

    fn comp<'a>(item: &'a Compound, key: &str) -> &'a Tag {
        let Some(Tag::Compound(components)) = item.get("components") else {
            panic!("no components in {item:?}");
        };
        components
            .get(key)
            .unwrap_or_else(|| panic!("no {key} in {components:?}"))
    }

    #[test]
    fn a_sword_gets_components_and_keeps_plugin_data() {
        let mut tag = Compound::new();
        tag.insert("Damage", Tag::Int(5));
        let mut ench = Compound::new();
        ench.insert("id", Tag::String("minecraft:sharpness".into()));
        ench.insert("lvl", Tag::Short(3));
        tag.insert("Enchantments", Tag::List(vec![Tag::Compound(ench)]));
        let mut display = Compound::new();
        display.insert("Name", json_text(r#"{"text":"Blade"}"#));
        tag.insert("display", Tag::Compound(display));
        let mut pdc = Compound::new();
        pdc.insert("myplugin:owner", Tag::String("Steve".into()));
        tag.insert("PublicBukkitValues", Tag::Compound(pdc.clone()));
        tag.insert("Unbreakable", Tag::Byte(1));
        tag.insert("HideFlags", Tag::Int(4));
        let mut item = old("diamond_sword", 1, tag);
        componentize("minecraft:diamond_sword", &mut item);
        assert_eq!(
            item.get("id"),
            Some(&Tag::String("minecraft:diamond_sword".into()))
        );
        assert_eq!(item.get("count"), Some(&Tag::Int(1)));
        assert_eq!(item.get("Slot"), Some(&Tag::Byte(3)));
        assert!(item.get("tag").is_none() && item.get("Count").is_none());
        assert_eq!(comp(&item, "minecraft:damage"), &Tag::Int(5));
        let Tag::Compound(ench) = comp(&item, "minecraft:enchantments") else {
            panic!("enchantments is not a compound");
        };
        let Some(Tag::Compound(levels)) = ench.get("levels") else {
            panic!("no levels");
        };
        assert_eq!(levels.get("minecraft:sharpness"), Some(&Tag::Int(3)));
        assert_eq!(
            comp(&item, "minecraft:custom_name"),
            &json_text(r#"{"text":"Blade"}"#)
        );
        let Tag::Compound(unbreakable) = comp(&item, "minecraft:unbreakable") else {
            panic!("unbreakable is not a compound");
        };
        assert_eq!(unbreakable.get("show_in_tooltip"), Some(&Tag::Byte(0)));
        let Tag::Compound(custom) = comp(&item, "minecraft:custom_data") else {
            panic!("custom_data is not a compound");
        };
        assert_eq!(custom.get("PublicBukkitValues"), Some(&Tag::Compound(pdc)));
    }

    #[test]
    fn a_shulker_box_moves_its_items_into_a_container() {
        let mut inner = old("stone", 64, Compound::new());
        inner.insert("Slot", Tag::Byte(5));
        let mut be = Compound::new();
        be.insert("id", Tag::String("minecraft:shulker_box".into()));
        be.insert("Items", Tag::List(vec![Tag::Compound(inner)]));
        let mut tag = Compound::new();
        tag.insert("BlockEntityTag", Tag::Compound(be));
        let mut item = old("minecraft:red_shulker_box", 1, tag);
        componentize("minecraft:red_shulker_box", &mut item);
        let Tag::List(slots) = comp(&item, "minecraft:container") else {
            panic!("container is not a list");
        };
        let Tag::Compound(slot) = &slots[0] else {
            panic!("slot is not a compound");
        };
        assert_eq!(slot.get("slot"), Some(&Tag::Int(5)));
        let Some(Tag::Compound(stone)) = slot.get("item") else {
            panic!("no item in the slot");
        };
        // The nested stack is still old; the walker reaches it next.
        assert_eq!(stone.get("Count"), Some(&Tag::Byte(64)));
        let Some(Tag::Compound(components)) = item.get("components") else {
            panic!("no components");
        };
        assert!(components.get("minecraft:block_entity_data").is_none());
    }

    #[test]
    fn potions_books_and_heads() {
        let mut tag = Compound::new();
        tag.insert("Potion", Tag::String("minecraft:swiftness".into()));
        let mut potion = old("minecraft:potion", 1, tag);
        componentize("minecraft:potion", &mut potion);
        let Tag::Compound(contents) = comp(&potion, "minecraft:potion_contents") else {
            panic!("potion_contents is not a compound");
        };
        assert_eq!(
            contents.get("potion"),
            Some(&Tag::String("minecraft:swiftness".into()))
        );

        let mut tag = Compound::new();
        tag.insert("title", Tag::String("T".into()));
        tag.insert("author", Tag::String("A".into()));
        tag.insert("pages", Tag::List(vec![json_text(r#""p1""#)]));
        let mut book = old("minecraft:written_book", 1, tag);
        componentize("minecraft:written_book", &mut book);
        let Tag::Compound(content) = comp(&book, "minecraft:written_book_content") else {
            panic!("written_book_content is not a compound");
        };
        assert_eq!(content.get("author"), Some(&Tag::String("A".into())));

        let mut tag = Compound::new();
        tag.insert("SkullOwner", Tag::String("Notch".into()));
        let mut head = old("minecraft:player_head", 1, tag);
        componentize("minecraft:player_head", &mut head);
        let Tag::Compound(profile) = comp(&head, "minecraft:profile") else {
            panic!("profile is not a compound");
        };
        assert_eq!(profile.get("name"), Some(&Tag::String("Notch".into())));
    }

    #[test]
    fn block_predicates_are_split() {
        let p = block_predicate("minecraft:stone[facing=up, lit=true]{a:1}");
        assert_eq!(
            p.get("blocks"),
            Some(&Tag::String("minecraft:stone".into()))
        );
        assert_eq!(p.get("nbt"), Some(&Tag::String("{a:1}".into())));
        let Some(Tag::Compound(state)) = p.get("state") else {
            panic!("no state");
        };
        assert_eq!(state.get("lit"), Some(&Tag::String("true".into())));
    }
}
