//! `/item`, and the places a stack can sit that it and `/loot` share:
//! player inventories, container blocks, item entities. What it does is as
//! observed on a vanilla 26.3 server: `replace` changes the first slot of the
//! range, `override` also clears the rest, `fill` fills them all, and with a
//! source the slots are paired in order.

use mc_command::args::{Arg, slot_range};
use mc_command::text::translate;
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;

use super::run::{err, str_tag};
use super::target::Target;
use super::{Output, Result, Run};
use crate::containers::{self, Kind};
use crate::context::{Event, ServerContext};
use crate::loot::{Drop, LootCtx};
use crate::tick::lock;

/// A stack with its component data.
pub(super) type Held = (ItemStack, Compound);

/// Somewhere a stack can sit.
#[derive(Clone, Debug)]
pub(super) enum Place {
    /// A slot of a player's inventory window.
    Window { uuid: u128, idx: usize },
    /// The stack on a player's cursor.
    Carried(u128),
    Block { pos: Position, kind: Kind, idx: usize },
    /// The item of an item entity.
    ItemEntity(u128),
    /// A slot that exists but is not kept (a mob's equipment).
    Void,
    /// A slot that exists but takes nothing.
    Refused,
}

fn comps_of(tag: Option<&Tag>) -> Compound {
    match tag {
        Some(Tag::Compound(c)) => c.clone(),
        _ => Compound::new(),
    }
}

impl Place {
    pub(super) fn get(&self, ctx: &ServerContext) -> Option<Held> {
        match self {
            Place::Window { uuid, idx } => {
                let inv = lock(&ctx.inventories);
                let h = inv.get(uuid)?;
                Some((h.slots.get(*idx)?.clone()?, comps_of(h.components.get(idx))))
            }
            Place::Carried(uuid) => {
                let inv = lock(&ctx.inventories);
                Some((inv.get(uuid)?.carried.clone()?, Compound::new()))
            }
            Place::Block { pos, kind, idx } => {
                let stack = containers::slots(ctx, *pos, *kind).get(*idx)?.clone()?;
                let comps = containers::components(ctx, *pos, *kind);
                Some((stack, comps_of(comps.get(idx))))
            }
            Place::ItemEntity(uuid) => lock(&ctx.entities)
                .iter()
                .filter(|e| e.uuid == *uuid)
                .find_map(|e| match &e.kind {
                    mc_entity::Kind::Item { item, count, .. } => Some((
                        ItemStack {
                            count: *count,
                            item: *item,
                            has_component_changes: false,
                        },
                        Compound::new(),
                    )),
                    _ => None,
                }),
            Place::Void | Place::Refused => None,
        }
    }

    /// Puts a stack (or nothing) in the place; false if it takes none.
    pub(super) fn set(&self, ctx: &ServerContext, out: &mut Output, held: Option<Held>) -> bool {
        let (stack, comps) = match held {
            Some((mut s, c)) => {
                s.has_component_changes = !c.is_empty();
                (Some(s), c)
            }
            None => (None, Compound::new()),
        };
        match self {
            Place::Window { uuid, idx } => {
                let mut inv = lock(&ctx.inventories);
                let Some(h) = inv.get_mut(uuid) else {
                    return false;
                };
                h.slots[*idx].clone_from(&stack);
                if comps.is_empty() {
                    h.components.remove(idx);
                } else {
                    h.components.insert(*idx, Tag::Compound(comps));
                }
                out.events.push(Event::InventorySlot {
                    uuid: *uuid,
                    slot: *idx as i16,
                    item: stack,
                });
                true
            }
            Place::Carried(uuid) => {
                let mut inv = lock(&ctx.inventories);
                let Some(h) = inv.get_mut(uuid) else {
                    return false;
                };
                h.carried = stack;
                true
            }
            Place::Block { pos, kind, idx } => {
                containers::put_with(ctx, *pos, *kind, &[(*idx, stack, Some(Tag::Compound(comps)))]);
                true
            }
            Place::ItemEntity(uuid) => {
                let mut entities = lock(&ctx.entities);
                for e in entities.iter_mut().filter(|e| e.uuid == *uuid) {
                    if let mc_entity::Kind::Item { item, count, .. } = &mut e.kind {
                        match &stack {
                            Some(s) => {
                                *item = s.item;
                                *count = s.count;
                            }
                            None => e.removed = true,
                        }
                    }
                }
                true
            }
            Place::Void => true,
            Place::Refused => false,
        }
    }
}

/// Entity kinds that hold nothing in equipment slots.
fn is_living(kind: &str) -> bool {
    const NOT: &[&str] = &[
        "item", "marker", "area_effect_cloud", "tnt", "falling_block", "arrow", "spectral_arrow",
        "trident", "experience_orb", "painting", "item_frame", "glow_item_frame", "text_display",
        "block_display", "item_display", "interaction", "lightning_bolt", "snowball", "egg",
        "ender_pearl", "fireball", "small_fireball", "wind_charge", "breeze_wind_charge",
        "fishing_bobber", "end_crystal", "eye_of_ender", "firework_rocket", "llama_spit",
        "shulker_bullet", "wither_skull", "dragon_fireball", "experience_bottle", "potion",
        "splash_potion", "lingering_potion", "evoker_fangs", "leash_knot", "oak_boat",
    ];
    let short = kind.trim_start_matches("minecraft:");
    !NOT.contains(&short) && !short.ends_with("_boat") && !short.ends_with("_raft")
        && !short.ends_with("_minecart") && short != "minecart"
}

/// The place vanilla's slot number `n` is for this entity, if it has one.
pub(super) fn entity_place(ctx: &ServerContext, t: &Target, n: i32) -> Option<Place> {
    let uuid = t.uuid();
    if t.is_player() {
        let window = |i: usize| Some(Place::Window { uuid, idx: i });
        return match n {
            0..=8 => window(36 + n as usize),
            9..=35 => window(n as usize),
            98 => window(lock(&ctx.inventories).get(&uuid)?.hand_slot(0)),
            99 => window(45),
            100..=103 => window(8 - (n as usize - 100)),
            499 => Some(Place::Carried(uuid)),
            500..=503 => window(1 + (n as usize - 500)),
            105 | 400 => Some(Place::Refused),
            200..=226 | 401 | 1000 => Some(Place::Void),
            _ => None,
        };
    }
    if t.kind() == "minecraft:item" {
        return (n == 1000).then_some(Place::ItemEntity(uuid));
    }
    if is_living(t.kind()) && matches!(n, 98 | 99 | 100..=103 | 105) {
        return Some(Place::Void);
    }
    None
}

/// The places of an entity a slot name covers, in the name's order.
pub(super) fn entity_places(ctx: &ServerContext, t: &Target, slots: &str) -> Vec<Place> {
    slot_range(slots)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|n| entity_place(ctx, t, n))
        .collect()
}

pub(super) fn pos_args(p: Position) -> Vec<Tag> {
    vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)]
}

/// The container at a loaded position, or vanilla's complaint.
pub(super) fn container_at(ctx: &ServerContext, p: Position, key: &str) -> Result<Kind> {
    let state = ctx.world.block(p, &ctx.data.blocks).unwrap_or(0);
    let block = ctx.data.blocks.block_name(state).unwrap_or("");
    containers::kind_of(block).ok_or_else(|| err(key, pos_args(p)))
}

/// The places of a container a slot name covers (only slots it has).
pub(super) fn block_places(p: Position, kind: Kind, slots: &str) -> Vec<Place> {
    slot_range(slots)
        .unwrap_or_default()
        .into_iter()
        .filter(|&n| (0..kind.slots as i32).contains(&n))
        .map(|n| Place::Block {
            pos: p,
            kind,
            idx: n as usize,
        })
        .collect()
}

pub(super) fn to_drop(h: &Held) -> Drop {
    Drop {
        item: h.0.item,
        count: h.0.count,
        components: h.1.clone(),
    }
}

pub(super) fn from_drop(d: &Drop) -> Option<Held> {
    (d.count > 0).then(|| {
        (
            ItemStack {
                count: d.count,
                item: d.item,
                has_component_changes: !d.components.is_empty(),
            },
            d.components.clone(),
        )
    })
}

/// An item as messages show it: its custom name if it has one.
pub(super) fn shown(ctx: &ServerContext, name: &str, comps: &Compound) -> Tag {
    let mut tag = super::run::item_display(ctx, name);
    if let (Tag::Compound(c), Some(custom)) = (&mut tag, comps.get("minecraft:custom_name"))
        && let Some(Tag::List(with)) = c.get_mut("with")
        && let Some(Tag::Compound(inner)) = with.first_mut()
    {
        inner.insert("extra", Tag::List(vec![custom.clone()]));
    }
    tag
}

/// Runs the items through an inline modifier; the first stack that comes out.
pub(super) fn modified(ctx: &ServerContext, modifier: &serde_json::Value, held: &Held) -> Option<Held> {
    let mut drops = vec![to_drop(held)];
    let cx = LootCtx::default();
    ctx.loot.modify(&super::loot::Env(ctx), &cx, modifier, &mut drops);
    drops.iter().find_map(from_drop)
}

/// `Components` of an item argument (`[minecraft:custom_name='"Q"']`).
pub(super) fn item_components(text: Option<&str>) -> Compound {
    let mut out = Compound::new();
    let Some(inner) = text.map(|t| t.trim().trim_start_matches('[').trim_end_matches(']')) else {
        return out;
    };
    let (mut depth, mut quote, mut start) = (0i32, None::<char>, 0usize);
    let mut parts = Vec::new();
    for (i, ch) in inner.char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(ch),
            (None, '{' | '[') => depth += 1,
            (None, '}' | ']') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    for part in parts {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        if let Ok(tag) = mc_nbt::parse_snbt(value.trim()) {
            out.insert(mc_command::args::full_id(key.trim()), tag);
        }
    }
    out
}

pub(super) fn tag_json(t: &Tag) -> serde_json::Value {
    use serde_json::Value;
    match t {
        Tag::Byte(v) => Value::from(*v),
        Tag::Short(v) => Value::from(*v),
        Tag::Int(v) => Value::from(*v),
        Tag::Long(v) => Value::from(*v),
        Tag::Float(v) => Value::from(*v),
        Tag::Double(v) => Value::from(*v),
        Tag::String(s) => Value::from(s.clone()),
        Tag::List(l) => Value::Array(l.iter().map(tag_json).collect()),
        Tag::Compound(c) => Value::Object(c.iter().map(|(k, v)| (k.to_owned(), tag_json(v))).collect()),
        Tag::ByteArray(a) => Value::Array(a.iter().map(|v| Value::from(*v)).collect()),
        Tag::IntArray(a) => Value::Array(a.iter().map(|v| Value::from(*v)).collect()),
        Tag::LongArray(a) => Value::Array(a.iter().map(|v| Value::from(*v)).collect()),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Replace,
    Override,
    Fill,
    Modify,
}

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    let ["item", mode, kind, "target", "slots", rest @ ..] = path else {
        return None;
    };
    let mode = match *mode {
        "replace" => Mode::Replace,
        "override" => Mode::Override,
        "fill" => Mode::Fill,
        "modify" => Mode::Modify,
        _ => return None,
    };
    Some(item(r, mode, *kind == "block", rest))
}

/// What goes into the slots: a stack made from arguments, or a source's.
enum Source {
    With(Option<Held>),
    From(Vec<Option<Held>>),
    Modify(serde_json::Value),
}

fn item(r: &mut Run, mode: Mode, block: bool, rest: &[&str]) -> Result<i32> {
    let slots = r.string("slots").unwrap_or_default();
    let source = match rest {
        ["with", ..] => {
            let Some(Arg::Item(item)) = r.arg("item").cloned() else {
                return Err(err("command.failed", Vec::new()));
            };
            let count = r.int("count").unwrap_or(1);
            let id = r.ctx.items.id(&item.id).unwrap_or(0);
            let max = r.ctx.items.max_stack(id);
            if count > max {
                return Err(err(
                    "arguments.item.overstacked",
                    vec![str_tag(&item.id), Tag::Int(max)],
                ));
            }
            let held = (item.id != "minecraft:air" && count > 0).then(|| {
                (
                    ItemStack {
                        count,
                        item: id,
                        has_component_changes: false,
                    },
                    item_components(item.components.as_deref()),
                )
            });
            Source::With(held)
        }
        ["from", ..] => Source::From(source_stacks(r, rest)?),
        _ => match r.arg("modifier") {
            Some(Arg::Nbt(t)) => Source::Modify(tag_json(t)),
            _ => return Err(err("command.failed", Vec::new())),
        },
    };
    let modifier = match (&source, r.arg("modifier")) {
        (Source::From(_), Some(Arg::Nbt(t))) => Some(tag_json(t)),
        _ => None,
    };
    let source = match (source, modifier) {
        (Source::From(list), Some(m)) => Source::From(
            list.into_iter()
                .map(|h| h.and_then(|h| modified(r.ctx, &m, &h)))
                .collect(),
        ),
        (s, _) => s,
    };
    let with_known = matches!(source, Source::With(_));
    let known = match &source {
        Source::With(h) => Some(h.clone()),
        _ => None,
    };
    let known_display = |r: &Run| match r.arg("item") {
        Some(Arg::Item(i)) => {
            let comps = known
                .as_ref()
                .and_then(|h| h.as_ref().map(|h| h.1.clone()))
                .unwrap_or_default();
            shown(r.ctx, &i.id, &comps)
        }
        _ => Tag::from(""),
    };

    if block {
        let p = r.loaded_pos("target")?;
        let kind = container_at(r.ctx, p, "commands.item.target.not_a_container")?;
        let places = block_places(p, kind, &slots);
        let n = apply(r, mode, &source, &places);
        if n == 0 {
            return Err(failure(r, mode, with_known, &slots, &places, &known));
        }
        let msg = match (mode, with_known) {
            (Mode::Modify, _) => translate(
                "commands.item.block.modify.success",
                [vec![Tag::Int(n)], pos_args(p)].concat(),
            ),
            (_, true) => translate(
                "commands.item.block.replace.success.known_item",
                [vec![Tag::Int(n)], pos_args(p), vec![known_display(r)]].concat(),
            ),
            _ => translate(
                "commands.item.block.replace.success",
                [vec![Tag::Int(n)], pos_args(p)].concat(),
            ),
        };
        r.success(msg, true);
        return Ok(n);
    }

    let targets = r.targets("target", false)?;
    let (mut total, mut touched, mut last_places) = (0, Vec::new(), Vec::new());
    for t in &targets {
        let places = entity_places(r.ctx, t, &slots);
        let n = apply(r, mode, &source, &places);
        if n > 0 {
            total += n;
            touched.push((t.clone(), n));
        }
        last_places = places;
    }
    if touched.is_empty() {
        let places = if targets.len() == 1 { last_places } else { Vec::new() };
        return Err(failure(r, mode, with_known, &slots, &places, &known));
    }
    let word = if mode == Mode::Modify { "modify" } else { "replace" };
    let msg = if touched.len() == 1 {
        let mut args = vec![Tag::Int(touched[0].1), touched[0].0.display()];
        let key = if with_known && mode != Mode::Modify {
            args.push(known_display(r));
            format!("commands.item.entity.{word}.success.single.known_item")
        } else {
            format!("commands.item.entity.{word}.success.single")
        };
        translate(&key, args)
    } else {
        let mut args = vec![Tag::Int(touched.len() as i32)];
        let key = if with_known && mode != Mode::Modify {
            args.push(known_display(r));
            format!("commands.item.entity.{word}.success.multiple.known_item")
        } else {
            format!("commands.item.entity.{word}.success.multiple")
        };
        translate(&key, args)
    };
    r.success(msg, true);
    Ok(total)
}

/// Why nothing changed.
fn failure(
    r: &Run,
    mode: Mode,
    with_known: bool,
    slots: &str,
    places: &[Place],
    known: &Option<Option<Held>>,
) -> mc_command::CommandError {
    if mode == Mode::Modify || places.is_empty() {
        return err("commands.item.target.no_such_slot", vec![str_tag(slots)]);
    }
    if with_known {
        let text = match known {
            Some(Some((s, _))) => format!("{} {}", s.count, r.ctx.items.name(s.item).unwrap_or("")),
            _ => "0 minecraft:air".to_owned(),
        };
        return err("commands.item.target.failed.known_item", vec![str_tag(&text)]);
    }
    err("commands.item.target.failed", Vec::new())
}

/// The source's stacks, in slot order, for slots it has.
fn source_stacks(r: &mut Run, rest: &[&str]) -> Result<Vec<Option<Held>>> {
    let slots = r.string("sourceSlots").unwrap_or_default();
    let places: Vec<Place> = match rest {
        ["from", "block", ..] => {
            let p = r.loaded_pos("source")?;
            let kind = container_at(r.ctx, p, "commands.item.source.not_a_container")?;
            block_places(p, kind, &slots)
        }
        _ => {
            let sources = r.targets("source", false)?;
            let t = sources.first().ok_or_else(|| err("argument.entity.notfound.entity", Vec::new()))?;
            entity_places(r.ctx, t, &slots)
        }
    };
    if places.is_empty() {
        return Err(err("commands.item.source.no_such_slot", vec![str_tag(&slots)]));
    }
    Ok(places.iter().map(|p| p.get(r.ctx)).collect())
}

/// Changes the places; how many slots were changed.
fn apply(r: &mut Run, mode: Mode, source: &Source, places: &[Place]) -> i32 {
    let ctx = r.ctx;
    let mut n = 0;
    let mut set = |r: &mut Run, p: &Place, h: Option<Held>| {
        if p.set(ctx, r.out, h) {
            n += 1;
        }
    };
    match (mode, source) {
        (Mode::Modify, Source::Modify(m)) => {
            for p in places {
                let Some(held) = p.get(ctx) else { continue };
                let new = modified(ctx, m, &held);
                set(r, p, new);
            }
        }
        (Mode::Replace, Source::With(h)) => {
            if let Some(p) = places.first() {
                set(r, p, h.clone());
            }
        }
        (Mode::Override, Source::With(h)) => {
            for (i, p) in places.iter().enumerate() {
                set(r, p, if i == 0 { h.clone() } else { None });
            }
        }
        (Mode::Fill, Source::With(h)) => {
            for p in places {
                set(r, p, h.clone());
            }
        }
        (Mode::Replace, Source::From(list)) => {
            for (p, h) in places.iter().zip(list) {
                set(r, p, h.clone());
            }
        }
        (Mode::Override, Source::From(list)) => {
            for (i, p) in places.iter().enumerate() {
                set(r, p, list.get(i).cloned().flatten());
            }
        }
        (Mode::Fill, Source::From(list)) if !list.is_empty() => {
            for (i, p) in places.iter().enumerate() {
                set(r, p, list[i % list.len()].clone());
            }
        }
        _ => {}
    }
    n
}
