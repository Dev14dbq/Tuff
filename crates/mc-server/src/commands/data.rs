//! `/data`, `execute store block|entity|storage` and `execute if data`,
//! with vanilla's messages and results.

use mc_command::CommandError;
use mc_command::args::Arg;
use mc_command::dispatch::Context;
use mc_command::nbtpath::{self, NbtPath};
use mc_command::text::pretty;
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::text::translate;

use super::run::err;
use super::target::{self, Target};
use super::{DataTarget, Output, Result, Run, Src, Store, StoreTarget};
use crate::context::ServerContext;
use crate::tick::lock;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["data", "get", kind, "target", rest @ ..] => get(r, kind, rest),
        ["data", "merge", kind, "target", "nbt"] => merge(r, kind),
        ["data", "remove", kind, "target", "path"] => remove(r, kind),
        ["data", "modify", kind, "target", "targetPath", op @ ..] => modify(r, kind, op),
        _ => return None,
    })
}

/// What `/data` reads and writes.
#[derive(Clone, Debug)]
pub enum Holder {
    Block(Position),
    Entity(Box<Target>),
    Storage(String),
}

fn id(s: &str) -> String {
    if s.contains(':') {
        s.to_owned()
    } else {
        format!("minecraft:{s}")
    }
}

/// The holder named by `arg` (a position, an entity or a storage id).
fn holder_arg(
    ctx: &ServerContext,
    c: &Context,
    src: &Src,
    kind: &str,
    arg: &str,
) -> Result<Holder> {
    match kind {
        "block" => {
            let Some(Arg::Pos(p)) = c.arg(arg) else {
                return Err(err("argument.pos.unloaded", Vec::new()));
            };
            let [x, y, z] = super::run::resolve(p, src);
            let p = Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
            if !super::world::in_world(ctx, p) {
                return Err(err("argument.pos.outofworld", Vec::new()));
            }
            if !ctx.world.is_loaded(p) || ctx.world.block(p, &ctx.data.blocks).is_none() {
                return Err(err("argument.pos.unloaded", Vec::new()));
            }
            Ok(Holder::Block(p))
        }
        "entity" => {
            let Some(Arg::Entity(sel)) = c.arg(arg) else {
                return Err(err("argument.entity.notfound.entity", Vec::new()));
            };
            let found = target::resolve(ctx, sel, src);
            match found.into_iter().next() {
                Some(t) => Ok(Holder::Entity(Box::new(t))),
                None => Err(err("argument.entity.notfound.entity", Vec::new())),
            }
        }
        _ => match c.arg(arg) {
            Some(Arg::Id(s) | Arg::Str(s) | Arg::Raw(s)) => Ok(Holder::Storage(id(s))),
            _ => Ok(Holder::Storage(String::new())),
        },
    }
}

pub(super) fn holder(r: &Run, kind: &str, arg: &str) -> Result<Holder> {
    holder_arg(r.ctx, r.c, &r.src, kind, arg)
}

/// The data a block entity of `be_id` has before anything is stored.
fn block_defaults(be: &str, c: &mut Compound) {
    let s = |v: &str| Tag::String(v.to_owned());
    match be.trim_start_matches("minecraft:") {
        "sign" | "hanging_sign" => {
            for side in ["front_text", "back_text"] {
                let mut t = Compound::new();
                t.insert("has_glowing_text", Tag::Byte(0));
                t.insert("color", s("black"));
                t.insert("messages", Tag::List(vec![s(""), s(""), s(""), s("")]));
                c.insert(side, Tag::Compound(t));
            }
            c.insert("is_waxed", Tag::Byte(0));
        }
        "furnace" | "blast_furnace" | "smoker" => {
            c.insert("lit_total_time", Tag::Int(0));
            c.insert("cooking_time_spent", Tag::Int(0));
            c.insert("cooking_total_time", Tag::Int(0));
            c.insert("lit_time_remaining", Tag::Int(0));
            c.insert("speed_multiplier", Tag::Float(1.0));
            c.insert("Items", Tag::List(Vec::new()));
            c.insert("RecipesUsed", Tag::Compound(Compound::new()));
        }
        _ => {}
    }
}

/// A block entity's data as vanilla shows it; `None` where there is none.
fn block_nbt(ctx: &ServerContext, p: Position) -> Option<Compound> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    let name = blocks.block_name(state)?;
    let stored = ctx.world.block_entity(p, blocks);
    let be_id = match &stored {
        Some(be) => be.id.clone(),
        None => {
            let id = super::world::block_entity_id(name);
            ctx.world.block_entity_kind(&id)?;
            id
        }
    };
    let mut c = Compound::new();
    c.insert("components", Tag::Compound(Compound::new()));
    block_defaults(&be_id, &mut c);
    if crate::containers::kind_of(name).is_some() && c.get("Items").is_none() {
        c.insert("Items", Tag::List(Vec::new()));
    }
    if let Some(be) = stored {
        for (k, v) in be.data.iter() {
            c.insert(k, v.clone());
        }
    }
    c.insert("x", Tag::Int(p.x));
    c.insert("y", Tag::Int(p.y));
    c.insert("z", Tag::Int(p.z));
    c.insert("id", Tag::String(be_id));
    Some(c)
}

/// An entity's data as vanilla shows it.
pub fn entity_nbt(ctx: &ServerContext, t: &Target) -> Compound {
    let uuid = Tag::IntArray(target::uuid_ints(t.uuid()));
    if t.is_player() {
        let mut root = player_defaults();
        if let Some(saved) = crate::saved_of(ctx, t.uuid()) {
            crate::players::Store::fill(&mut root, &saved, &|i| {
                crate::containers::item_name(ctx, i)
            });
            let motion = if saved.pose.on_ground {
                -0.0784000015258789
            } else {
                0.0
            };
            root.insert(
                "Motion",
                Tag::List(vec![
                    Tag::Double(0.0),
                    Tag::Double(motion),
                    Tag::Double(0.0),
                ]),
            );
        }
        root.insert("UUID", uuid);
        root.insert("DataVersion", Tag::Int(ctx.data.version.world_version));
        if matches!(root.get("equipment"), Some(Tag::Compound(e)) if e.is_empty()) {
            root.remove("equipment");
        }
        // The held stack, as vanilla adds it when asked.
        if let Some(Tag::Int(slot)) = root.get("SelectedItemSlot").cloned()
            && let Some(Tag::List(inv)) = root.get("Inventory").cloned()
            && let Some(Tag::Compound(held)) = inv.iter().find(
                |e| matches!(e, Tag::Compound(c) if c.get("Slot") == Some(&Tag::Byte(slot as i8))),
            )
        {
            let mut held = held.clone();
            held.remove("Slot");
            root.insert("SelectedItem", Tag::Compound(held));
        }
        if let Some(Tag::Compound(a)) = root.get("abilities") {
            let a = in_order(a, ABILITIES);
            root.insert("abilities", Tag::Compound(a));
        }
        return in_order(&root, PLAYER);
    }
    // Other entities show what their chunk keeps of them, without `id`.
    let entities = lock(&ctx.entities).clone();
    let Some(mut c) = entities
        .iter()
        .find(|e| e.uuid == t.uuid())
        .and_then(|e| crate::persist::write(ctx, e, &entities))
    else {
        return Compound::new();
    };
    c.remove("id");
    c
}

/// The order vanilla writes a player's keys in; it decides the order of
/// keys that share a hash bucket.
const PLAYER: &[&str] = &[
    "Pos",
    "Motion",
    "Rotation",
    "fall_distance",
    "Fire",
    "Air",
    "OnGround",
    "Invulnerable",
    "PortalCooldown",
    "UUID",
    "Health",
    "HurtTime",
    "HurtByTimestamp",
    "DeathTime",
    "AbsorptionAmount",
    "attributes",
    "active_effects",
    "FallFlying",
    "Brain",
    "DataVersion",
    "Inventory",
    "SelectedItemSlot",
    "SleepTimer",
    "XpP",
    "XpLevel",
    "XpTotal",
    "XpSeed",
    "Score",
    "foodLevel",
    "foodTickTimer",
    "foodSaturationLevel",
    "foodExhaustionLevel",
    "abilities",
    "EnderItems",
    "warden_spawn_tracker",
    "playerGameType",
    "seenCredits",
    "recipeBook",
    "Dimension",
    "spawn_extra_particles_on_fall",
    "current_impulse_context_reset_grace_time",
    "post_effects",
];

const ABILITIES: &[&str] = &[
    "invulnerable",
    "flying",
    "mayfly",
    "instabuild",
    "mayBuild",
    "flySpeed",
    "walkSpeed",
];

/// `c` with the keys named in `order` first, in that order.
fn in_order(c: &Compound, order: &[&str]) -> Compound {
    let mut out = Compound::new();
    for k in order {
        if let Some(v) = c.get(k) {
            out.insert(*k, v.clone());
        }
    }
    for (k, v) in c.iter() {
        if !order.contains(&k) {
            out.insert(k, v.clone());
        }
    }
    out
}

fn player_defaults() -> Compound {
    let mut c = Compound::new();
    let mut brain = Compound::new();
    brain.insert("memories", Tag::Compound(Compound::new()));
    c.insert("Brain", Tag::Compound(brain));
    c.insert("post_effects", Tag::List(Vec::new()));
    c.insert("SleepTimer", Tag::Short(0));
    c.insert("Invulnerable", Tag::Byte(0));
    c.insert("FallFlying", Tag::Byte(0));
    c.insert("PortalCooldown", Tag::Int(0));
    c.insert("AbsorptionAmount", Tag::Float(0.0));
    let mut book = Compound::new();
    book.insert("recipes", Tag::List(Vec::new()));
    book.insert("toBeDisplayed", Tag::List(Vec::new()));
    c.insert("recipeBook", Tag::Compound(book));
    c.insert("DeathTime", Tag::Short(0));
    c.insert("XpSeed", Tag::Int(0));
    c.insert("XpTotal", Tag::Int(0));
    c.insert("seenCredits", Tag::Byte(0));
    c.insert("fall_distance", Tag::Double(0.0));
    c.insert("Air", Tag::Short(300));
    c.insert("Dimension", Tag::String("minecraft:overworld".into()));
    c.insert("current_impulse_context_reset_grace_time", Tag::Int(0));
    c.insert("Score", Tag::Int(0));
    c.insert("Fire", Tag::Short(-20));
    c.insert("EnderItems", Tag::List(Vec::new()));
    c.insert("foodTickTimer", Tag::Int(0));
    c.insert("spawn_extra_particles_on_fall", Tag::Byte(0));
    c.insert("HurtTime", Tag::Short(0));
    let attribute = |id: &str, base: f64| {
        let mut a = Compound::new();
        a.insert("id", Tag::String(format!("minecraft:{id}")));
        a.insert("base", Tag::Double(base));
        Tag::Compound(a)
    };
    c.insert(
        "attributes",
        Tag::List(vec![
            attribute("block_interaction_range", 4.5),
            attribute("waypoint_transmit_range", 6.0e7),
            attribute("movement_speed", 0.10000000149011612),
            attribute("entity_interaction_range", 3.0),
        ]),
    );
    c
}

/// The holder's data; vanilla's error when it has none.
pub fn read(ctx: &ServerContext, h: &Holder) -> Result<Compound> {
    match h {
        Holder::Storage(id) => Ok(lock(&ctx.storage).get(id)),
        Holder::Block(p) => {
            block_nbt(ctx, *p).ok_or_else(|| err("commands.data.block.invalid", Vec::new()))
        }
        Holder::Entity(t) => Ok(entity_nbt(ctx, t)),
    }
}

/// Stores the holder's changed data.
pub fn write(ctx: &ServerContext, h: &Holder, mut c: Compound) -> Result<()> {
    match h {
        Holder::Storage(id) => {
            lock(&ctx.storage).set(id, c);
            Ok(())
        }
        Holder::Block(p) => {
            let Some(be) = ctx.world.block_entity(*p, &ctx.data.blocks).or_else(|| {
                let old = block_nbt(ctx, *p)?;
                let Some(Tag::String(id)) = old.get("id") else {
                    return None;
                };
                Some(mc_world::BlockEntity {
                    x: p.x,
                    y: p.y,
                    z: p.z,
                    id: id.clone(),
                    data: Compound::new(),
                })
            }) else {
                return Err(err("commands.data.block.invalid", Vec::new()));
            };
            for k in ["x", "y", "z", "id"] {
                c.remove(k);
            }
            ctx.world
                .set_block_entity(mc_world::BlockEntity { data: c, ..be }, &ctx.data.blocks);
            lock(&ctx.containers).remove(&(p.x, p.y, p.z));
            lock(&ctx.container_updates).push(*p);
            Ok(())
        }
        Holder::Entity(t) if t.is_player() => Err(err("commands.data.entity.invalid", Vec::new())),
        Holder::Entity(t) => {
            let int = |k: &str| match c.get(k) {
                Some(Tag::Short(v)) => Some(i32::from(*v)),
                Some(Tag::Int(v)) => Some(*v),
                Some(Tag::Byte(v)) => Some(i32::from(*v)),
                _ => None,
            };
            let mut entities = lock(&ctx.entities);
            if let Some(e) = entities.iter_mut().find(|e| e.uuid == t.uuid()) {
                let age = int("Age");
                if let mc_entity::Kind::Item {
                    item,
                    count,
                    damage,
                    components,
                    pickup_delay,
                    ..
                } = &mut e.kind
                {
                    if let Some(d) = int("PickupDelay") {
                        *pickup_delay = d;
                    }
                    if let Some(Tag::Compound(saved)) = c.get("Item")
                        && let Some(stack) = mc_protocol::packets::play::ItemStack::from_nbt(saved)
                    {
                        *item = stack.item;
                        *count = stack.count;
                        *damage = stack.damage;
                        *components = stack.components;
                    }
                }
                if let Some(a) = age {
                    e.age = a.max(0) as u32;
                }
                if let Some(Tag::List(tags)) = c.get("Tags") {
                    e.tags = tags
                        .iter()
                        .filter_map(|t| match t {
                            Tag::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect();
                }
            }
            Ok(())
        }
    }
}

fn args_of(h: &Holder) -> Vec<Tag> {
    match h {
        Holder::Storage(id) => vec![Tag::String(id.clone())],
        Holder::Block(p) => vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)],
        Holder::Entity(t) => vec![t.display()],
    }
}

fn kind_of(h: &Holder) -> &'static str {
    match h {
        Holder::Storage(_) => "storage",
        Holder::Block(_) => "block",
        Holder::Entity(_) => "entity",
    }
}

fn modified(r: &mut Run, h: &Holder) {
    r.success(
        translate(
            &format!("commands.data.{}.modified", kind_of(h)),
            args_of(h),
        ),
        true,
    );
}

pub(super) fn path_arg(r: &Run, name: &str) -> Option<NbtPath> {
    match r.arg(name) {
        Some(Arg::NbtPath(p)) => Some(p.clone()),
        _ => None,
    }
}

fn number(t: &Tag) -> Option<f64> {
    Some(match t {
        Tag::Byte(v) => f64::from(*v),
        Tag::Short(v) => f64::from(*v),
        Tag::Int(v) => f64::from(*v),
        Tag::Long(v) => *v as f64,
        Tag::Float(v) => f64::from(*v),
        Tag::Double(v) => *v,
        _ => return None,
    })
}

/// What `data get` returns for a tag: a number floored, a string's
/// length, a list's or compound's size.
fn result_of(t: &Tag) -> i32 {
    match t {
        Tag::String(s) => s.chars().count() as i32,
        Tag::List(l) => l.len() as i32,
        Tag::Compound(c) => c.len() as i32,
        Tag::ByteArray(a) => a.len() as i32,
        Tag::IntArray(a) => a.len() as i32,
        Tag::LongArray(a) => a.len() as i32,
        other => number(other).map_or(0, |v| v.floor() as i32),
    }
}

pub(super) fn single(path: &NbtPath, root: &Tag) -> Result<Tag> {
    let mut found = path.get(root)?;
    if found.len() > 1 {
        return Err(err("commands.data.get.multiple", Vec::new()));
    }
    Ok(found.remove(0))
}

fn get(r: &mut Run, kind: &str, rest: &[&str]) -> Result<i32> {
    let h = holder(r, kind, "target")?;
    let data = Tag::Compound(read(r.ctx, &h)?);
    let mut args = args_of(&h);
    let k = kind_of(&h);
    let Some(path) = path_arg(r, "path") else {
        args.push(pretty(&data));
        r.success(translate(&format!("commands.data.{k}.query"), args), false);
        return Ok(1);
    };
    let found = single(&path, &data)?;
    if rest.len() == 2 {
        let scale = match r.arg("scale") {
            Some(Arg::Double(v)) => *v,
            Some(Arg::Float(v)) => f64::from(*v),
            _ => 1.0,
        };
        let Some(v) = number(&found) else {
            return Err(err(
                "commands.data.get.invalid",
                vec![Tag::String(path.text.clone())],
            ));
        };
        let value = (v * scale).floor() as i32;
        let mut a = vec![Tag::String(path.text.clone())];
        a.extend(args);
        a.push(Tag::String(format!("{scale:.2}")));
        a.push(Tag::Int(value));
        r.success(translate(&format!("commands.data.{k}.get"), a), false);
        return Ok(value);
    }
    let value = result_of(&found);
    args.push(pretty(&found));
    r.success(translate(&format!("commands.data.{k}.query"), args), false);
    Ok(value)
}

fn merge(r: &mut Run, kind: &str) -> Result<i32> {
    let h = holder(r, kind, "target")?;
    let old = read(r.ctx, &h)?;
    let Some(Tag::Compound(with)) = r.nbt("nbt") else {
        return Err(err("command.failed", Vec::new()));
    };
    let mut new = old.clone();
    nbtpath::merge(&mut new, &with);
    if matches!(h, Holder::Entity(ref t) if t.is_player()) {
        return Err(err("commands.data.entity.invalid", Vec::new()));
    }
    if new == old {
        return Err(err("commands.data.merge.failed", Vec::new()));
    }
    write(r.ctx, &h, new)?;
    modified(r, &h);
    Ok(1)
}

fn remove(r: &mut Run, kind: &str) -> Result<i32> {
    let h = holder(r, kind, "target")?;
    let mut data = Tag::Compound(read(r.ctx, &h)?);
    let path = path_arg(r, "path").ok_or_else(|| err("command.failed", Vec::new()))?;
    let n = path.remove(&mut data)?;
    if matches!(h, Holder::Entity(ref t) if t.is_player()) {
        return Err(err("commands.data.entity.invalid", Vec::new()));
    }
    if n == 0 {
        return Err(err("commands.data.merge.failed", Vec::new()));
    }
    if let Tag::Compound(c) = data {
        write(r.ctx, &h, c)?;
    }
    modified(r, &h);
    Ok(n)
}

/// A tag as SNBT, for messages.
fn snbt(t: &Tag) -> Tag {
    Tag::String(mc_command::particle::snbt(t))
}

/// The tag as a string (`set string`): strings as they are, numbers as
/// Java writes them.
fn as_string(t: &Tag) -> Result<String> {
    Ok(match t {
        Tag::String(s) => s.clone(),
        Tag::Byte(v) => v.to_string(),
        Tag::Short(v) => v.to_string(),
        Tag::Int(v) => v.to_string(),
        Tag::Long(v) => v.to_string(),
        Tag::Float(v) => mc_command::text::java_float(*v),
        Tag::Double(v) => mc_command::text::java_double(*v),
        other => {
            return Err(err(
                "commands.data.modify.expected_value",
                vec![snbt(other)],
            ));
        }
    })
}

/// The values a `modify` takes from its source.
fn sources(r: &Run, op: &[&str]) -> Result<Vec<Tag>> {
    let from = op
        .iter()
        .position(|w| matches!(*w, "value" | "from" | "string" | "compute"))
        .unwrap_or(0);
    let tail = &op[from..];
    match tail {
        ["value", "value"] => Ok(vec![r.nbt("value").unwrap_or_else(|| Tag::from(""))]),
        [how @ ("from" | "string"), kind, "source", rest @ ..] => {
            let h = holder(r, kind, "source")?;
            let data = Tag::Compound(read(r.ctx, &h)?);
            let found = match path_arg(r, "sourcePath") {
                Some(p) => p.get(&data)?,
                None => vec![data],
            };
            if *how == "from" {
                return Ok(found);
            }
            let start = r.int("start");
            let end = r.int("end");
            let _ = rest;
            found
                .iter()
                .map(|t| {
                    let s: Vec<char> = as_string(t)?.chars().collect();
                    let n = s.len() as i32;
                    let fix = |i: i32| if i < 0 { n + i } else { i };
                    let a = start.map_or(0, fix);
                    let b = end.map_or(n, fix);
                    if a < 0 || b > n || a > b {
                        return Err(err(
                            "commands.data.modify.invalid_substring",
                            vec![Tag::Int(start.unwrap_or(0)), Tag::Int(end.unwrap_or(n))],
                        ));
                    }
                    Ok(Tag::String(s[a as usize..b as usize].iter().collect()))
                })
                .collect()
        }
        ["compute", how, ..] => {
            let block = match *how {
                "block" => {
                    let p = r.loaded_pos("computePos")?;
                    let state = r.ctx.world.block(p, &r.ctx.data.blocks).unwrap_or(0);
                    r.ctx.data.blocks.block_name(state).unwrap_or("").to_owned()
                }
                _ => String::new(),
            };
            let float = tail.contains(&"float");
            let registry = if float {
                "minecraft:context_float_provider"
            } else {
                "minecraft:context_int_provider"
            };
            let v = match r.arg("provider") {
                Some(Arg::Id(id)) => serde_json::Value::String(mc_command::args::full_id(id)),
                Some(Arg::Nbt(t)) => mc_command::text::to_json(t),
                _ => return Err(err("command.failed", Vec::new())),
            };
            let n = crate::furnaces::provider(&r.ctx.data, registry, &v, &block, 0)
                .ok_or_else(|| err("command.failed", Vec::new()))?;
            Ok(vec![if float {
                Tag::Float(n as f32)
            } else {
                Tag::Int(n as i32)
            }])
        }
        _ => Err(err("command.failed", Vec::new())),
    }
}

/// Changes `root` at `path` as a `modify` does; how many changed.
fn apply(
    root: &mut Tag,
    path: &NbtPath,
    op: &[&str],
    index: Option<i32>,
    values: &[Tag],
) -> Result<i32> {
    match op.first() {
        Some(&"set") => {
            let Some(last) = values.last() else {
                return Ok(0);
            };
            path.set(root, last)
        }
        Some(&"merge") => {
            for v in values {
                if !matches!(v, Tag::Compound(_)) {
                    return Err(err("commands.data.modify.expected_object", vec![snbt(v)]));
                }
            }
            let mut changed = 0;
            let mut failure = None;
            path.get_or_create(root, &Tag::Compound(Compound::new()), &mut |t| {
                let Tag::Compound(c) = t else {
                    failure.get_or_insert(snbt(t));
                    return;
                };
                let before = c.clone();
                for v in values {
                    if let Tag::Compound(v) = v {
                        nbtpath::merge(c, v);
                    }
                }
                if *c != before {
                    changed += 1;
                }
            })?;
            if let Some(t) = failure {
                return Err(err("commands.data.modify.expected_object", vec![t]));
            }
            Ok(changed)
        }
        Some(&("append" | "prepend" | "insert")) => {
            let at = match op[0] {
                "append" => -1,
                "prepend" => 0,
                _ => index.unwrap_or(0),
            };
            let mut changed = 0;
            let mut failure: Option<CommandError> = None;
            path.get_or_create(root, &Tag::List(Vec::new()), &mut |t| {
                let len = match t {
                    Tag::List(l) => l.len(),
                    Tag::ByteArray(a) => a.len(),
                    Tag::IntArray(a) => a.len(),
                    Tag::LongArray(a) => a.len(),
                    other => {
                        failure.get_or_insert(err(
                            "commands.data.modify.expected_list",
                            vec![snbt(other)],
                        ));
                        return;
                    }
                } as i32;
                let i = if at < 0 { len + at + 1 } else { at };
                if i < 0 || i > len {
                    failure.get_or_insert(err(
                        "commands.data.modify.invalid_index",
                        vec![Tag::Int(at)],
                    ));
                    return;
                }
                for (k, v) in values.iter().enumerate() {
                    if nbtpath::insert_at(t, i as usize + k, v) {
                        changed += 1;
                    }
                }
            })?;
            if let Some(e) = failure {
                return Err(e);
            }
            Ok(changed)
        }
        _ => Err(err("command.failed", Vec::new())),
    }
}

fn modify(r: &mut Run, kind: &str, op: &[&str]) -> Result<i32> {
    let h = holder(r, kind, "target")?;
    let mut data = Tag::Compound(read(r.ctx, &h)?);
    let path = path_arg(r, "targetPath").ok_or_else(|| err("command.failed", Vec::new()))?;
    let values = sources(r, op)?;
    let n = apply(&mut data, &path, op, r.int("index"), &values)?;
    if matches!(h, Holder::Entity(ref t) if t.is_player()) {
        return Err(err("commands.data.entity.invalid", Vec::new()));
    }
    if n == 0 {
        return Err(err("commands.data.merge.failed", Vec::new()));
    }
    if let Tag::Compound(c) = data {
        write(r.ctx, &h, c)?;
    }
    modified(r, &h);
    Ok(n)
}

// Execute.

/// `execute if|unless data ...`: how many tags the path finds.
pub fn found(ctx: &ServerContext, c: &Context, s: &Src, rest: &[&str]) -> Option<Result<usize>> {
    let ["data", kind, "source", "path"] = rest else {
        return None;
    };
    Some((|| {
        let h = holder_arg(ctx, c, s, kind, "source")?;
        let data = Tag::Compound(read(ctx, &h)?);
        let Some(Arg::NbtPath(path)) = c.arg("path") else {
            return Ok(0);
        };
        Ok(path.get(&data).map_or(0, |f| f.len()))
    })())
}

/// `execute store result|success block|entity|storage ...`.
pub fn modify_store(
    ctx: &ServerContext,
    c: &Context,
    s: &Src,
    path: &[&str],
) -> Option<Result<Vec<Src>>> {
    let [
        "execute",
        "store",
        how @ ("result" | "success"),
        kind @ ("block" | "entity" | "storage"),
        arg,
        "path",
        ty,
        "scale",
    ] = path
    else {
        return None;
    };
    Some((|| {
        let h = holder_arg(ctx, c, s, kind, arg)?;
        // The block must have data now; players fail when stored to.
        read(ctx, &h)?;
        let Some(Arg::NbtPath(p)) = c.arg("path") else {
            return Err(err("command.failed", Vec::new()));
        };
        let scale = match c.arg("scale") {
            Some(Arg::Double(v)) => *v,
            Some(Arg::Float(v)) => f64::from(*v),
            _ => 1.0,
        };
        let target = match h {
            Holder::Block(p) => DataTarget::Block(p),
            Holder::Entity(t) => DataTarget::Entity(t.uuid()),
            Holder::Storage(id) => DataTarget::Storage(id),
        };
        let mut n = s.clone();
        n.stores.push(Store {
            success: *how == "success",
            target: StoreTarget::Data {
                target,
                path: p.clone(),
                kind: (*ty).to_owned(),
                scale,
            },
        });
        Ok(vec![n])
    })())
}

/// Puts a stored result into data.
pub fn store(ctx: &ServerContext, store: &Store, value: i32, _out: &mut Output) {
    let StoreTarget::Data {
        target,
        path,
        kind,
        scale,
    } = &store.target
    else {
        return;
    };
    let h = match target {
        DataTarget::Block(p) => Holder::Block(*p),
        DataTarget::Storage(id) => Holder::Storage(id.clone()),
        DataTarget::Entity(u) => match target::everyone(ctx).into_iter().find(|t| t.uuid() == *u) {
            Some(t) => Holder::Entity(Box::new(t)),
            None => return,
        },
    };
    let v = f64::from(value) * scale;
    let tag = match kind.as_str() {
        "byte" => Tag::Byte(v as i32 as i8),
        "short" => Tag::Short(v as i32 as i16),
        "long" => Tag::Long(v as i64),
        "float" => Tag::Float(v as f32),
        "double" => Tag::Double(v),
        _ => Tag::Int(v as i32),
    };
    let Ok(old) = read(ctx, &h) else { return };
    let mut data = Tag::Compound(old);
    if path.set(&mut data, &tag).is_ok()
        && let Tag::Compound(c) = data
    {
        let _ = write(ctx, &h, c);
    }
}
