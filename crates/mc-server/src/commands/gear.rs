//! `/enchant`.

use mc_command::args::full_id;
use mc_nbt::{Compound, Tag};
use mc_protocol::text::translate;

use super::item::entity_place;
use super::run::err;
use super::{Result, Run};

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["enchant", ..] => enchant(r),
        _ => return None,
    })
}

/// The ids an `exclusive_set`/`supported_items` field names (an id, a list
/// of ids or a `#tag`).
fn ids_of(r: &Run, registry: &str, v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => match s.strip_prefix('#') {
            Some(tag) => r.ctx.commands.env.tag(registry, tag),
            None => vec![full_id(s)],
        },
        serde_json::Value::Array(list) => list.iter().flat_map(|e| ids_of(r, registry, e)).collect(),
        _ => Vec::new(),
    }
}

fn definition(r: &Run, id: &str) -> Option<serde_json::Value> {
    r.ctx
        .data
        .data_pack()
        .read_entry("minecraft:enchantment", id)
        .ok()
}

/// The name of an item as `getHoverName` gives it.
fn hover_name(r: &Run, item: i32, comps: &Compound) -> String {
    if let Some(Tag::String(n)) = comps.get("minecraft:custom_name") {
        return n.clone();
    }
    let name = r.ctx.items.name(item).unwrap_or("").trim_start_matches("minecraft:").to_owned();
    let lang = &r.ctx.commands.lang;
    for kind in ["item", "block"] {
        let key = format!("{kind}.minecraft.{name}");
        let text = lang.get(&key);
        if text != key {
            return text.to_owned();
        }
    }
    name
}

/// A target's name as plain text.
fn plain(r: &Run, t: &super::target::Target) -> Tag {
    match t {
        super::target::Target::Player { name, .. } => Tag::from(name.as_str()),
        super::target::Target::Entity { name_key, .. } => {
            Tag::from(r.ctx.commands.lang.get(name_key))
        }
    }
}

fn enchant(r: &mut Run) -> Result<i32> {
    let targets = r.targets("targets", false)?;
    let id = full_id(&r.string("enchantment").unwrap_or_default());
    let level = r.int("level").unwrap_or(1);
    let def = definition(r, &id).unwrap_or_default();
    let max = def["max_level"].as_i64().unwrap_or(1) as i32;
    if level > max {
        return Err(err(
            "commands.enchant.failed.level",
            vec![Tag::Int(level), Tag::Int(max)],
        ));
    }
    let supported = ids_of(r, "minecraft:item", &def["supported_items"]);
    let exclusive = ids_of(r, "minecraft:enchantment", &def["exclusive_set"]);
    let mut done = 0;
    for t in &targets {
        let Some(place) = entity_place(r.ctx, t, 98).filter(|_| t.is_player()) else {
            if targets.len() == 1 {
                return Err(err("commands.enchant.failed.entity", vec![plain(r, t)]));
            }
            continue;
        };
        let Some((stack, mut comps)) = place.get(r.ctx) else {
            if targets.len() == 1 {
                return Err(err("commands.enchant.failed.itemless", vec![plain(r, t)]));
            }
            continue;
        };
        let item = r.ctx.items.name(stack.item).unwrap_or("").to_owned();
        let key = if item == "minecraft:enchanted_book" {
            "minecraft:stored_enchantments"
        } else {
            "minecraft:enchantments"
        };
        let mut have = match comps.get(key) {
            Some(Tag::Compound(c)) => c.clone(),
            _ => Compound::new(),
        };
        let compatible = have.iter().all(|(other, _)| {
            let other_exclusive = definition(r, other)
                .map(|d| ids_of(r, "minecraft:enchantment", &d["exclusive_set"]))
                .unwrap_or_default();
            !exclusive.iter().any(|e| e == other) && !other_exclusive.contains(&id)
        });
        if !supported.contains(&item) || !compatible {
            if targets.len() == 1 {
                let name = hover_name(r, stack.item, &comps);
                return Err(err("commands.enchant.failed.incompatible", vec![Tag::String(name)]));
            }
            continue;
        }
        if level == 0 {
            have.remove(&id);
        } else {
            have.insert(id.as_str(), Tag::Int(level));
        }
        if have.is_empty() {
            comps.remove(key);
        } else {
            comps.insert(key, Tag::Compound(have));
        }
        place.set(r.ctx, r.out, Some((stack, comps)));
        done += 1;
    }
    if done == 0 {
        return Err(err("commands.enchant.failed", Vec::new()));
    }
    let mut full = translate(&format!("enchantment.{}", id.replacen(':', ".", 1)), Vec::new());
    if let Tag::Compound(c) = &mut full {
        let cursed = r.ctx.commands.env.tag("minecraft:enchantment", "minecraft:curse").contains(&id);
        c.insert("color", if cursed { "red" } else { "gray" });
        if level != 1 || max != 1 {
            let mut sp = Compound::new();
            sp.insert("", " ");
            c.insert(
                "extra",
                mc_protocol::text::list(vec![
                    Tag::Compound(sp),
                    translate(&format!("enchantment.level.{level}"), Vec::new()),
                ]),
            );
        }
    }
    let msg = if targets.len() == 1 {
        translate("commands.enchant.success.single", vec![full, targets[0].display()])
    } else {
        translate("commands.enchant.success.multiple", vec![full, Tag::Int(done)])
    };
    r.success(msg, true);
    Ok(done)
}
