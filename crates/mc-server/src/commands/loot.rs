//! `/loot`: roll a loot table (named or inline), a block's, an entity's or a
//! fishing catch, and give the stacks to players, put them in a container or
//! a slot, or drop them. Messages as vanilla 26.3 sends them.

use mc_command::args::{Arg, slot_range};
use mc_command::text::translate;
use mc_nbt::Tag;
use mc_protocol::packets::play::ItemStack;

use super::item::{
    Held, Place, container_at, entity_place, from_drop, item_components, shown, tag_json,
};
use super::run::{err, str_tag};
use super::{Output, Result, Run, Who};
use crate::context::{Event, ServerContext};
use crate::loot::{Drop, Ent, LootCtx, LootEnv, Tool};
use crate::tick::lock;

/// What loot tables ask of the server.
pub(crate) struct Env<'a>(pub &'a ServerContext);

impl LootEnv for Env<'_> {
    fn tag(&self, registry: &str, tag: &str) -> Vec<String> {
        self.0.commands.env.tag(registry, tag)
    }
    fn max_stack(&self, item: i32) -> i32 {
        self.0.items.max_stack(item)
    }
    fn max_damage(&self, item: i32) -> i32 {
        self.0.items.get(item).map_or(0, |i| i.max_damage)
    }
    fn item_name(&self, item: i32) -> String {
        self.0.items.name(item).unwrap_or("").to_owned()
    }
    fn smelt(&self, item: i32) -> Option<i32> {
        self.0
            .recipes
            .cooking
            .iter()
            .find(|c| c.kind == "minecraft:smelting" && c.input.contains(&item))
            .map(|c| c.result.item)
    }
}

/// A rolled result: the stacks and the table message names, if any.
struct Rolled {
    drops: Vec<Drop>,
    table: Option<String>,
}

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    let ["loot", mode, rest @ ..] = path else {
        return None;
    };
    Some(loot(r, mode, rest))
}

fn loot(r: &mut Run, mode: &str, rest: &[&str]) -> Result<i32> {
    // The destination's nodes come first, then the source's.
    let skip = match (mode, rest) {
        ("give" | "insert" | "spawn", _) => 1,
        ("replace", _) => 3 + usize::from(rest.get(3) == Some(&"count")),
        _ => return Err(err("command.failed", Vec::new())),
    };
    let source = &rest[skip.min(rest.len())..];
    let rolled = roll(r, source)?;
    match mode {
        "give" => give(r, rolled),
        "spawn" => spawn(r, rolled),
        "insert" => insert(r, rolled),
        _ => replace(r, rolled, rest[0] == "block"),
    }
}

fn hand_tool(r: &Run, offhand: bool) -> Option<Tool> {
    let Who::Player(uuid) = r.src.who else {
        return None;
    };
    let inv = lock(&r.ctx.inventories);
    let h = inv.get(&uuid)?;
    let slot = if offhand { 45 } else { h.hand_slot(0) };
    let stack = h.slots[slot].as_ref()?;
    let mut enchants = Vec::new();
    if let Some(Tag::Compound(e)) = stack.components.get("enchantments") {
        for (name, level) in e.iter() {
            if let Tag::Int(l) = level {
                enchants.push((name.to_owned(), *l));
            }
        }
    }
    Some(Tool {
        item: r.ctx.items.name(stack.item)?.to_owned(),
        enchants,
    })
}

fn tool_arg(r: &Run, source: &[&str]) -> Option<Tool> {
    match source.last() {
        Some(&"mainhand") => hand_tool(r, false),
        Some(&"offhand") => hand_tool(r, true),
        Some(&"tool") => {
            let Some(Arg::Item(i)) = r.arg("tool") else {
                return None;
            };
            let comps = item_components(i.components.as_deref());
            let mut enchants = Vec::new();
            if let Some(Tag::Compound(e)) = comps.get("minecraft:enchantments") {
                for (name, level) in e.iter() {
                    if let Tag::Int(l) = level {
                        enchants.push((name.to_owned(), *l));
                    }
                }
            }
            Some(Tool {
                item: mc_command::args::full_id(&i.id),
                enchants,
            })
        }
        _ => None,
    }
}

fn roll(r: &mut Run, source: &[&str]) -> Result<Rolled> {
    let env = Env(r.ctx);
    let mut cx = LootCtx {
        origin: Some(r.src.pos),
        ..LootCtx::default()
    };
    match source {
        ["loot", ..] | ["fish", ..] => {
            let table = match r.arg("loot_table") {
                Some(Arg::Nbt(t)) => tag_json(t),
                Some(Arg::Id(name) | Arg::Raw(name) | Arg::Str(name)) => r
                    .ctx
                    .loot
                    .table(&mc_command::args::full_id(name))
                    .cloned()
                    .ok_or_else(|| err("command.failed", Vec::new()))?,
                _ => return Err(err("command.failed", Vec::new())),
            };
            if source[0] == "fish" {
                cx.origin = r.vec3("pos");
                cx.tool = tool_arg(r, source);
            }
            Ok(Rolled {
                drops: r.ctx.loot.roll(&env, &table, &cx),
                table: None,
            })
        }
        ["mine", ..] => {
            let p = r.loaded_pos("pos")?;
            if !r.ctx.world.is_loaded(p) {
                return Err(err("argument.pos.unloaded", Vec::new()));
            }
            let state = r.ctx.world.block(p, &r.ctx.data.blocks).unwrap_or(0);
            let blocks = &r.ctx.data.blocks;
            let name = blocks
                .block_name(state)
                .unwrap_or("minecraft:air")
                .to_owned();
            let id = format!("minecraft:blocks/{}", name.trim_start_matches("minecraft:"));
            let Some(table) = r.ctx.loot.table(&id).cloned() else {
                let key = format!("block.{}", name.replacen(':', ".", 1));
                return Err(err(
                    "commands.drop.no_loot_table.block",
                    vec![translate(&key, Vec::new())],
                ));
            };
            let props = crate::loot::BLOCK_PROPS
                .iter()
                .filter_map(|k| Some((k.to_string(), blocks.property(state, k)?.to_owned())))
                .collect();
            cx.origin = Some([
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ]);
            cx.block = Some((name, props));
            cx.tool = tool_arg(r, source);
            Ok(Rolled {
                drops: r.ctx.loot.roll(&env, &table, &cx),
                table: Some(id),
            })
        }
        ["kill", ..] => {
            let found = r.targets("target", false)?;
            let t = &found[0];
            let id = format!(
                "minecraft:entities/{}",
                t.kind().trim_start_matches("minecraft:")
            );
            cx.origin = Some(t.pos());
            cx.this = Some(Ent {
                kind: t.kind().to_owned(),
                ..Ent::default()
            });
            let drops = match r.ctx.loot.table(&id).cloned() {
                Some(table) => r.ctx.loot.roll(&env, &table, &cx),
                None => Vec::new(),
            };
            Ok(Rolled {
                drops,
                table: Some(id),
            })
        }
        _ => Err(err("command.failed", Vec::new())),
    }
}

/// `commands.drop.success.*` for the stacks that went somewhere.
fn report(r: &mut Run, held: &[Held], table: &Option<String>) -> Result<i32> {
    let ctx = r.ctx;
    let total: i32 = held.iter().map(|h| h.count).sum();
    let msg = match (held, table) {
        ([s], t) => {
            let item = shown(ctx, ctx.items.name(s.item).unwrap_or(""), s);
            match t {
                Some(t) => translate(
                    "commands.drop.success.single_with_table",
                    vec![Tag::Int(s.count), item, str_tag(t)],
                ),
                None => translate(
                    "commands.drop.success.single",
                    vec![Tag::Int(s.count), item],
                ),
            }
        }
        (list, Some(t)) => translate(
            "commands.drop.success.multiple_with_table",
            vec![Tag::Int(list.len() as i32), str_tag(t)],
        ),
        (list, None) => translate(
            "commands.drop.success.multiple",
            vec![Tag::Int(list.len() as i32)],
        ),
    };
    r.success(msg, false);
    Ok(total)
}

fn held_of(rolled: &Rolled) -> Vec<Held> {
    rolled.drops.iter().filter_map(from_drop).collect()
}

/// Puts a stack in a player's inventory; what did not fit.
fn give_one(ctx: &ServerContext, out: &mut Output, uuid: u128, h: &Held) -> i32 {
    let max = ctx.items.max_stack(h.item);
    let mut inv = lock(&ctx.inventories);
    let Some(hands) = inv.get_mut(&uuid) else {
        return h.count;
    };
    let (n, changed) = hands.insert_stack(h, max);
    for slot in changed {
        out.events.push(Event::InventorySlot {
            uuid,
            slot: slot as i16,
            item: hands.slots[slot].clone(),
        });
    }
    h.count - n
}

fn give(r: &mut Run, rolled: Rolled) -> Result<i32> {
    let players = r.targets("players", true)?;
    let held = held_of(&rolled);
    for t in &players {
        for h in &held {
            let left = give_one(r.ctx, r.out, t.uuid(), h);
            if left > 0 {
                crate::tick::spawn_stack_at(r.ctx, t.pos(), &h.with_count(left));
            }
        }
    }
    report(r, &held, &rolled.table)
}

fn spawn(r: &mut Run, rolled: Rolled) -> Result<i32> {
    let at = r.vec3("targetPos").unwrap_or(r.src.pos);
    let held = held_of(&rolled);
    for h in &held {
        crate::tick::spawn_stack_at(r.ctx, at, h);
    }
    report(r, &held, &rolled.table)
}

fn insert(r: &mut Run, rolled: Rolled) -> Result<i32> {
    let p = r.loaded_pos("targetPos")?;
    let kind = container_at(r.ctx, p, "commands.item.target.not_a_container")?;
    let held = held_of(&rolled);
    let mut inserted = Vec::new();
    for h in held {
        let mut slots = crate::containers::slots(r.ctx, p, kind);
        let max = r.ctx.items.max_stack(h.item);
        let mut left = h.count;
        let mut changes = Vec::new();
        for (i, s) in slots.iter_mut().enumerate() {
            if let Some(s) = s
                && s.same_kind(&h)
                && s.count < max
            {
                let n = left.min(max - s.count);
                s.count += n;
                left -= n;
                changes.push((i, Some(s.clone())));
            }
        }
        for (i, s) in slots.iter().enumerate() {
            if left == 0 {
                break;
            }
            if s.is_none() {
                let n = left.min(max);
                left -= n;
                changes.push((i, Some(h.with_count(n))));
            }
        }
        if left > 0 {
            continue;
        }
        crate::containers::put(r.ctx, p, kind, &changes);
        inserted.push(h);
    }
    report(r, &inserted, &rolled.table)
}

fn replace(r: &mut Run, rolled: Rolled, block: bool) -> Result<i32> {
    let slot = r.string("slot").unwrap_or_default();
    let first = slot_range(&slot)
        .and_then(|v| v.first().copied())
        .unwrap_or(0);
    let count = r.int("count").unwrap_or(1);
    let mut held = held_of(&rolled);
    held.truncate(count.max(0) as usize);
    let mut placed: Vec<Option<Held>> = held.into_iter().map(Some).collect();
    placed.resize(count.max(0) as usize, None);
    let mut shown_list: Vec<Held> = Vec::new();
    let mut put = |r: &mut Run, place: &Place, h: &Option<Held>| {
        if place.set(r.ctx, r.out, h.clone()) {
            if let Some(h) = h {
                shown_list.push(h.clone());
            } else {
                shown_list.push(ItemStack::new(0, 0));
            }
        }
    };
    if block {
        let p = r.loaded_pos("targetPos")?;
        let kind = container_at(r.ctx, p, "commands.item.target.not_a_container")?;
        if first < 0 || first as usize + placed.len() > kind.slots {
            return Err(err("command.failed", Vec::new()));
        }
        for (i, h) in placed.iter().enumerate() {
            let place = Place::Block {
                pos: p,
                kind,
                idx: first as usize + i,
            };
            put(r, &place, h);
        }
    } else {
        let targets = r.targets("entities", false)?;
        for t in &targets {
            for (i, h) in placed.iter().enumerate() {
                if let Some(place) = entity_place(r.ctx, t, first + i as i32) {
                    put(r, &place, h);
                }
            }
        }
    }
    // Empty padding is counted as a stack but shown as nothing.
    let n = shown_list.len();
    let real: Vec<Held> = shown_list.iter().filter(|h| h.count > 0).cloned().collect();
    if n == 1 && real.len() == 1 {
        return report(r, &real, &rolled.table);
    }
    let msg = match &rolled.table {
        Some(t) => translate(
            "commands.drop.success.multiple_with_table",
            vec![Tag::Int(n as i32), str_tag(t)],
        ),
        None => translate("commands.drop.success.multiple", vec![Tag::Int(n as i32)]),
    };
    r.success(msg, false);
    Ok(real.iter().map(|h| h.count).sum())
}
