//! `/enchant`.

use mc_command::args::full_id;
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{AttributeModifier, AttributeSnapshot, UpdateAttributes};
use mc_protocol::text::translate;

use super::item::entity_place;
use super::run::err;
use super::{Result, Run};
use crate::context::{Event, ServerContext};
use crate::tick::lock;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["enchant", ..] => enchant(r),
        ["attribute", ..] => attribute(r, path),
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
        serde_json::Value::Array(list) => {
            list.iter().flat_map(|e| ids_of(r, registry, e)).collect()
        }
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
fn hover_name(r: &Run, item: i32, stack: &mc_protocol::packets::play::ItemStack) -> String {
    if let Some(Tag::String(n)) = stack.components.get("custom_name") {
        return n.clone();
    }
    let name = r
        .ctx
        .items
        .name(item)
        .unwrap_or("")
        .trim_start_matches("minecraft:")
        .to_owned();
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
        let Some(mut stack) = place.get(r.ctx) else {
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
        let mut have = match stack.components.get(key) {
            Some(Tag::Compound(c)) => c.clone(),
            _ => Compound::new(),
        };
        let compatible = have.iter().all(|(other, _)| {
            let other_exclusive = definition(r, other)
                .map(|d| ids_of(r, "minecraft:enchantment", &d["exclusive_set"]))
                .unwrap_or_default();
            other != id && !exclusive.iter().any(|e| e == other) && !other_exclusive.contains(&id)
        });
        if !supported.contains(&item) || !compatible {
            if targets.len() == 1 {
                let name = hover_name(r, stack.item, &stack);
                return Err(err(
                    "commands.enchant.failed.incompatible",
                    vec![Tag::String(name)],
                ));
            }
            continue;
        }
        if level == 0 {
            have.remove(&id);
        } else {
            have.insert(id.as_str(), Tag::Int(level));
        }
        if have.is_empty() {
            stack.components.reset(key);
        } else {
            stack.components.set(key, Tag::Compound(have));
        }
        place.set(r.ctx, r.out, Some(stack));
        done += 1;
    }
    if done == 0 {
        return Err(err("commands.enchant.failed", Vec::new()));
    }
    let mut full = translate(
        &format!("enchantment.{}", id.replacen(':', ".", 1)),
        Vec::new(),
    );
    if let Tag::Compound(c) = &mut full {
        let cursed = r
            .ctx
            .commands
            .env
            .tag("minecraft:enchantment", "minecraft:curse")
            .contains(&id);
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
        translate(
            "commands.enchant.success.single",
            vec![full, targets[0].display()],
        )
    } else {
        translate(
            "commands.enchant.success.multiple",
            vec![full, Tag::Int(done)],
        )
    };
    r.success(msg, true);
    Ok(done)
}

/// Every attribute a player has: (name, default base, max, min).
const ATTRIBUTES: [(&str, f64, f64, f64); 36] = [
    ("air_drag_modifier", 1.0, 2048.0, 0.0),
    ("armor", 0.0, 30.0, 0.0),
    ("armor_toughness", 0.0, 20.0, 0.0),
    ("attack_damage", 1.0, 2048.0, 0.0),
    ("attack_knockback", 0.0, 5.0, 0.0),
    ("attack_speed", 4.0, 1024.0, 0.0),
    ("below_name_distance", 10.0, 512.0, 0.0),
    ("block_break_speed", 1.0, 1024.0, 0.0),
    ("block_interaction_range", 4.5, 64.0, 0.0),
    ("bounciness", 0.0, 1.0, 0.0),
    ("burning_time", 1.0, 1024.0, 0.0),
    ("camera_distance", 4.0, 32.0, 0.0),
    ("entity_interaction_range", 3.0, 64.0, 0.0),
    ("explosion_knockback_resistance", 0.0, 1.0, 0.0),
    ("fall_damage_multiplier", 1.0, 100.0, 0.0),
    ("friction_modifier", 1.0, 2048.0, 0.0),
    ("gravity", 0.08, 1.0, -1.0),
    ("jump_strength", 0.419_999_986_886_978_15, 32.0, 0.0),
    ("knockback_resistance", 0.0, 1.0, -2.0),
    ("luck", 0.0, 1024.0, -1024.0),
    ("max_absorption", 0.0, 2048.0, 0.0),
    ("max_health", 20.0, 1024.0, 1.0),
    ("mining_efficiency", 0.0, 1024.0, 0.0),
    ("movement_efficiency", 0.0, 1.0, 0.0),
    ("movement_speed", 0.100_000_001_490_116_12, 1024.0, 0.0),
    ("name_tag_distance", 64.0, 512.0, 0.0),
    ("oxygen_bonus", 0.0, 1024.0, 0.0),
    ("safe_fall_distance", 3.0, 1024.0, -1024.0),
    ("scale", 1.0, 16.0, 0.0625),
    ("sneaking_speed", 0.3, 1.0, 0.0),
    ("step_height", 0.6, 10.0, 0.0),
    ("submerged_mining_speed", 0.2, 20.0, 0.0),
    ("sweeping_damage_ratio", 0.0, 1.0, 0.0),
    ("water_movement_efficiency", 0.0, 1.0, 0.0),
    ("waypoint_receive_range", 60_000_000.0, 60_000_000.0, 0.0),
    ("waypoint_transmit_range", 60_000_000.0, 60_000_000.0, 0.0),
];

const OPERATIONS: [&str; 3] = ["add_value", "add_multiplied_base", "add_multiplied_total"];

/// All modifiers on an attribute of a player: effects', the gear's
/// enchantments' and items', creative mode's and those `/attribute` added.
pub fn all_modifiers(
    ctx: &ServerContext,
    uuid: u128,
    name: &str,
    game_mode: i32,
) -> Vec<AttributeModifier> {
    let full = format!("minecraft:{name}");
    let mut mods = lock(&ctx.vitals)
        .get(&uuid)
        .map(|v| {
            let mut m = v.effects.modifiers_named(&full);
            m.extend(v.worn.enchanted(&full).iter().cloned());
            m
        })
        .unwrap_or_default();
    // What is worn and held now.
    let gear = crate::enchant_effects::gear(ctx, uuid);
    mods.extend(
        crate::worn::gear_modifiers(ctx, &gear)
            .into_iter()
            .filter(|(a, _)| *a == full)
            .map(|(_, m)| m),
    );
    if game_mode == 1 {
        let creative = match name {
            "block_interaction_range" => Some(("minecraft:creative_mode_block_range", 0.5)),
            "entity_interaction_range" => Some(("minecraft:creative_mode_entity_range", 2.0)),
            _ => None,
        };
        if let Some((id, amount)) = creative {
            mods.push(AttributeModifier {
                id: id.into(),
                amount,
                operation: 0,
            });
        }
    }
    if let Some(state) = lock(&ctx.rare)
        .attributes
        .get(&uuid)
        .and_then(|a| a.get(name))
    {
        mods.extend(
            state
                .modifiers
                .iter()
                .map(|(id, amount, operation)| AttributeModifier {
                    id: id.clone(),
                    amount: *amount,
                    operation: *operation,
                }),
        );
    }
    mods
}

/// A player's attribute (`sneaking_speed`) as clients get it: its base and
/// every modifier on it.
pub fn snapshot(ctx: &ServerContext, uuid: u128, name: &str) -> Option<AttributeSnapshot> {
    let name = name.trim_start_matches("minecraft:");
    let &(_, default, _, _) = ATTRIBUTES.iter().find(|a| a.0 == name)?;
    let mode = lock(&ctx.players).get(&uuid).map_or(0, |p| p.game_mode);
    let base = lock(&ctx.rare)
        .attributes
        .get(&uuid)
        .and_then(|a| a.get(name))
        .and_then(|s| s.base)
        .unwrap_or(default);
    Some(AttributeSnapshot {
        attribute: ctx
            .data
            .builtin
            .id("minecraft:attribute", &format!("minecraft:{name}"))?,
        base,
        modifiers: all_modifiers(ctx, uuid, name, mode),
    })
}

/// Attributes clients are not told about (only the server uses them).
const UNSYNCED: [&str; 5] = [
    "attack_damage",
    "attack_knockback",
    "follow_range",
    "knockback_resistance",
    "tempt_range",
];

/// Whether clients are told about the attribute (`armor`,
/// `minecraft:attack_speed`).
pub fn synced(name: &str) -> bool {
    !UNSYNCED.contains(&name.trim_start_matches("minecraft:"))
}

/// A player's attribute value with every modifier.
pub fn value(ctx: &ServerContext, uuid: u128, name: &str) -> f64 {
    let short = name.trim_start_matches("minecraft:");
    let Some(&(_, _, max, min)) = ATTRIBUTES.iter().find(|a| a.0 == short) else {
        return 0.0;
    };
    snapshot(ctx, uuid, short).map_or(0.0, |s| compute(s.base, &s.modifiers, max, min))
}

/// The attribute as the player has it now (base, then modifiers).
pub(crate) fn number(ctx: &crate::context::ServerContext, uuid: u128, name: &str) -> f64 {
    let Some(&(_, default, max, min)) = ATTRIBUTES.iter().find(|a| a.0 == name) else {
        return 0.0;
    };
    let base = lock(&ctx.rare)
        .attributes
        .get(&uuid)
        .and_then(|a| a.get(name))
        .and_then(|s| s.base)
        .unwrap_or(default);
    let mode = lock(&ctx.players).get(&uuid).map_or(0, |p| p.game_mode);
    compute(base, &all_modifiers(ctx, uuid, name, mode), max, min)
}

fn compute(base: f64, mods: &[AttributeModifier], max: f64, min: f64) -> f64 {
    let v: f64 = base
        + mods
            .iter()
            .filter(|m| m.operation == 0)
            .map(|m| m.amount)
            .sum::<f64>();
    let mut x = v;
    for m in mods.iter().filter(|m| m.operation == 1) {
        x += v * m.amount;
    }
    for m in mods.iter().filter(|m| m.operation == 2) {
        x *= 1.0 + m.amount;
    }
    x.clamp(min, max)
}

fn attribute(r: &mut Run, path: &[&str]) -> Result<i32> {
    let mut found = r.targets("target", false)?;
    if found.len() > 1 {
        return Err(err("argument.entity.toomany", Vec::new()));
    }
    let t = found.remove(0);
    let full = full_id(&r.string("attribute").unwrap_or_default());
    let name = full.trim_start_matches("minecraft:").to_owned();
    let label = || translate(&format!("attribute.name.{name}"), Vec::new());
    if !t.is_player() {
        return Err(err("commands.attribute.failed.entity", vec![t.display()]));
    }
    let Some(&(_, default, max, min)) = ATTRIBUTES.iter().find(|a| a.0 == name) else {
        return Err(err(
            "commands.attribute.failed.no_attribute",
            vec![t.display(), label()],
        ));
    };
    let uuid = t.uuid();
    let mode = match &t {
        super::target::Target::Player { game_mode, .. } => *game_mode,
        _ => 0,
    };
    let scale = |r: &Run| match r.arg("scale") {
        Some(mc_command::args::Arg::Double(v)) => *v,
        _ => 1.0,
    };
    let base = |r: &Run| {
        lock(&r.ctx.rare)
            .attributes
            .get(&uuid)
            .and_then(|a| a.get(&name))
            .and_then(|s| s.base)
            .unwrap_or(default)
    };
    let d = |v: f64| Tag::Double(v);
    let rest = &path[3..];
    let result = match rest {
        ["get", ..] => {
            let v = compute(base(r), &all_modifiers(r.ctx, uuid, &name, mode), max, min);
            r.success(
                translate(
                    "commands.attribute.value.get.success",
                    vec![label(), t.display(), d(v)],
                ),
                false,
            );
            v * scale(r)
        }
        ["base", "get", ..] => {
            let v = base(r);
            r.success(
                translate(
                    "commands.attribute.base_value.get.success",
                    vec![label(), t.display(), d(v)],
                ),
                false,
            );
            v * scale(r)
        }
        ["base", "set", _] => {
            let v = match r.arg("value") {
                Some(mc_command::args::Arg::Double(v)) => *v,
                _ => 0.0,
            };
            lock(&r.ctx.rare)
                .attributes
                .entry(uuid)
                .or_default()
                .entry(name.clone())
                .or_default()
                .base = Some(v);
            r.success(
                translate(
                    "commands.attribute.base_value.set.success",
                    vec![label(), t.display(), d(v)],
                ),
                true,
            );
            1.0
        }
        ["base", "reset"] => {
            if let Some(s) = lock(&r.ctx.rare)
                .attributes
                .get_mut(&uuid)
                .and_then(|a| a.get_mut(&name))
            {
                s.base = None;
            }
            r.success(
                translate(
                    "commands.attribute.base_value.reset.success",
                    vec![label(), t.display(), d(default)],
                ),
                true,
            );
            1.0
        }
        ["modifier", "add", ..] => {
            let id = full_id(&r.string("id").unwrap_or_default());
            let amount = match r.arg("value") {
                Some(mc_command::args::Arg::Double(v)) => *v,
                _ => 0.0,
            };
            let op = OPERATIONS
                .iter()
                .position(|o| path.last() == Some(o))
                .unwrap_or(0) as i32;
            let have = all_modifiers(r.ctx, uuid, &name, mode)
                .iter()
                .any(|m| m.id == id);
            if have {
                return Err(err(
                    "commands.attribute.failed.modifier_already_present",
                    vec![Tag::from(id.as_str()), label(), t.display()],
                ));
            }
            lock(&r.ctx.rare)
                .attributes
                .entry(uuid)
                .or_default()
                .entry(name.clone())
                .or_default()
                .modifiers
                .push((id.clone(), amount, op));
            r.success(
                translate(
                    "commands.attribute.modifier.add.success",
                    vec![Tag::from(id.as_str()), label(), t.display()],
                ),
                true,
            );
            1.0
        }
        ["modifier", "remove", _] => {
            let id = full_id(&r.string("id").unwrap_or_default());
            let removed = {
                let mut st = lock(&r.ctx.rare);
                st.attributes
                    .get_mut(&uuid)
                    .and_then(|a| a.get_mut(&name))
                    .is_some_and(|s| {
                        let n = s.modifiers.len();
                        s.modifiers.retain(|m| m.0 != id);
                        s.modifiers.len() != n
                    })
            };
            if !removed {
                return Err(err(
                    "commands.attribute.failed.no_modifier",
                    vec![label(), t.display(), Tag::from(id.as_str())],
                ));
            }
            r.success(
                translate(
                    "commands.attribute.modifier.remove.success",
                    vec![Tag::from(id.as_str()), label(), t.display()],
                ),
                true,
            );
            1.0
        }
        ["modifier", "value", "get", ..] => {
            let id = full_id(&r.string("id").unwrap_or_default());
            let Some(m) = all_modifiers(r.ctx, uuid, &name, mode)
                .into_iter()
                .find(|m| m.id == id)
            else {
                return Err(err(
                    "commands.attribute.failed.no_modifier",
                    vec![label(), t.display(), Tag::from(id.as_str())],
                ));
            };
            r.success(
                translate(
                    "commands.attribute.modifier.value.get.success",
                    vec![Tag::from(id.as_str()), label(), t.display(), d(m.amount)],
                ),
                false,
            );
            m.amount * scale(r)
        }
        _ => return Err(err("command.failed", Vec::new())),
    };
    if rest[0] != "get"
        && !(rest[0] == "base" && rest[1] == "get")
        && !(rest[0] == "modifier" && rest[1] == "value")
    {
        send_attribute(r, uuid, &name, default, mode);
    }
    Ok(result as i32)
}

/// Tells the player (and who sees them) the attribute now.
fn send_attribute(r: &mut Run, uuid: u128, name: &str, default: f64, mode: i32) {
    let base = lock(&r.ctx.rare)
        .attributes
        .get(&uuid)
        .and_then(|a| a.get(name))
        .and_then(|s| s.base)
        .unwrap_or(default);
    let Some(entity_id) = lock(&r.ctx.players).get(&uuid).map(|p| p.entity_id) else {
        return;
    };
    let attribute = r
        .ctx
        .data
        .builtin
        .id("minecraft:attribute", &format!("minecraft:{name}"))
        .unwrap_or(0);
    let modifiers = all_modifiers(r.ctx, uuid, name, mode);
    r.out.events.push(Event::Attributes {
        uuid,
        packet: UpdateAttributes {
            entity_id,
            attributes: vec![AttributeSnapshot {
                attribute,
                base,
                modifiers,
            }],
        },
    });
}
