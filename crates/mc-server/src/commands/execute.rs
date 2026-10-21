//! `execute` tests and modifiers beyond the basic ones (`if dimension`,
//! `loaded`, `biome`, `predicate`, `items`, `stopwatch`; `on`, `summon`,
//! `facing`) and the `/stopwatch` command.

use mc_command::CommandError;
use mc_command::args::{Arg, Env, Item};
use mc_command::dispatch::Context;
use mc_nbt::Tag;
use mc_protocol::Position;
use mc_protocol::packets::play::ItemStack;
use mc_protocol::text::translate;
use serde_json::Value;

use super::run::{err, resolve};
use super::target::{self, Target};
use super::{Result, Run, Src, Who};
use crate::context::ServerContext;
use crate::tick::lock;

fn str_tag(s: &str) -> Tag {
    Tag::String(s.to_owned())
}

fn block_pos(c: &Context, s: &Src, name: &str) -> Option<Position> {
    let Some(Arg::Pos(p)) = c.arg(name) else {
        return None;
    };
    let [x, y, z] = resolve(p, s);
    Some(Position::new(
        x.floor() as i32,
        y.floor() as i32,
        z.floor() as i32,
    ))
}

/// The `dimension` argument; vanilla's error when there is no such one.
pub fn dimension(ctx: &ServerContext, c: &Context) -> Result<String> {
    let Some(Arg::Id(d)) = c.arg("dimension") else {
        return Ok("minecraft:overworld".into());
    };
    let d = mc_command::args::full_id(d);
    if !ctx.commands.env.exists("minecraft:dimension", &d) {
        return Err(err("argument.dimension.invalid", vec![str_tag(&d)]));
    }
    Ok(d)
}

/// `execute if ...` tests handled here: whether it passed, and what it
/// counted (`if items`).
pub fn condition(ctx: &ServerContext, c: &Context, s: &Src, rest: &[&str]) -> Option<Result<bool>> {
    Some(match rest {
        ["dimension", "dimension"] => {
            dimension(ctx, c).map(|d| d == ctx.at(s.dim).dimension_name())
        }
        ["loaded", "pos"] => {
            let p = block_pos(c, s, "pos")?;
            Ok(super::world::in_world(ctx, p) && ctx.world.is_loaded(p))
        }
        ["biome", "pos", "biome"] => biome(ctx, c, s),
        ["predicate", "predicate"] => predicate_arg(ctx, c),
        ["items", ..] => count(ctx, c, s, rest)?.map(|n| n > 0),
        ["stopwatch", "id", "range"] => stopwatch_test(ctx, c),
        _ => return None,
    })
}

/// What a counting test (`if items`) counted.
pub fn count(ctx: &ServerContext, c: &Context, s: &Src, rest: &[&str]) -> Option<Result<i32>> {
    let (Some(Arg::Str(slots)), Some(Arg::Item(item))) = (c.arg("slots"), c.arg("item_predicate"))
    else {
        return None;
    };
    let wanted = mc_command::args::slot_range(slots).unwrap_or_default();
    let stacks = match rest {
        ["items", "block", ..] => {
            let p = block_pos(c, s, "source")?;
            match block_stacks(ctx, p, &wanted) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            }
        }
        ["items", "entity", ..] => {
            let Some(Arg::Entity(sel)) = c.arg("source") else {
                return None;
            };
            target::resolve(ctx, sel, s)
                .iter()
                .flat_map(|t| entity_stacks(ctx, t, &wanted))
                .collect()
        }
        _ => return None,
    };
    Some(Ok(stacks
        .iter()
        .filter(|st| item_matches(ctx, item, st))
        .map(|st| st.count)
        .sum()))
}

fn block_stacks(ctx: &ServerContext, p: Position, wanted: &[i32]) -> Result<Vec<ItemStack>> {
    let pos = vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)];
    if !ctx.world.is_loaded(p) {
        return Err(err("argument.pos.unloaded", Vec::new()));
    }
    let state = ctx.world.block(p, &ctx.data.blocks).unwrap_or(0);
    let name = ctx.data.blocks.block_name(state).unwrap_or("");
    let Some(kind) = crate::containers::kind_of(name) else {
        return Err(err("commands.item.source.not_a_container", pos));
    };
    let slots = crate::containers::slots(ctx, p, kind);
    Ok(wanted
        .iter()
        .filter_map(|&i| {
            usize::try_from(i)
                .ok()
                .and_then(|i| slots.get(i))
                .cloned()
                .flatten()
        })
        .collect())
}

fn entity_stacks(ctx: &ServerContext, t: &Target, wanted: &[i32]) -> Vec<ItemStack> {
    if t.is_player() {
        let inventories = lock(&ctx.inventories);
        let Some(h) = inventories.get(&t.uuid()) else {
            return Vec::new();
        };
        return wanted
            .iter()
            .filter_map(|&i| {
                let window = match i {
                    0..=8 => 36 + i as usize,
                    9..=35 => i as usize,
                    98 => h.hand_slot(0),
                    99 => 45,
                    100..=103 => (108 - i) as usize,
                    499 => return h.carried.clone(),
                    500..=503 => (i - 499) as usize,
                    _ => return None,
                };
                h.slots.get(window).cloned().flatten()
            })
            .collect();
    }
    if !wanted.contains(&1000) {
        return Vec::new();
    }
    let entities = lock(&ctx.entities);
    entities
        .iter()
        .filter(|e| e.uuid == t.uuid())
        .filter_map(|e| match &e.kind {
            mc_entity::Kind::Item {
                item,
                count,
                damage,
                ..
            } => Some(ItemStack {
                count: *count,
                item: *item,
                damage: *damage,
                has_component_changes: false,
            }),
            _ => None,
        })
        .collect()
}

/// Whether `stack` passes an item predicate (`*`, an id or `#tag`, and
/// `[count=..., damage=...]` tests).
fn item_matches(ctx: &ServerContext, p: &Item, stack: &ItemStack) -> bool {
    let name = ctx.items.name(stack.item).unwrap_or("");
    let id_ok = if p.id == "*" {
        true
    } else if p.tag {
        ctx.commands
            .env
            .tag("minecraft:item", &p.id)
            .iter()
            .any(|i| i == name)
    } else {
        mc_command::args::full_id(&p.id) == name
    };
    if !id_ok {
        return false;
    }
    let Some(tests) = p.components.as_deref() else {
        return true;
    };
    let inner = tests.trim_start_matches('[').trim_end_matches(']');
    inner.split(',').filter(|t| !t.trim().is_empty()).all(|t| {
        let (key, value) = t.split_once(['=', '~']).unwrap_or((t, ""));
        let key = key.trim().trim_start_matches('!');
        let negate = t.trim().starts_with('!');
        let pass = match key.trim_start_matches("minecraft:") {
            "count" => in_range(value.trim(), stack.count),
            "damage" => {
                ctx.items.get(stack.item).is_some_and(|i| i.damageable)
                    && in_range(value.trim(), stack.damage)
            }
            _ => false,
        };
        pass != negate
    })
}

fn in_range(text: &str, v: i32) -> bool {
    match text.split_once("..") {
        Some((lo, hi)) => {
            lo.parse::<i32>().map_or(lo.is_empty(), |lo| v >= lo)
                && hi.parse::<i32>().map_or(hi.is_empty(), |hi| v <= hi)
        }
        None => text.parse::<i32>() == Ok(v),
    }
}

fn biome(ctx: &ServerContext, c: &Context, s: &Src) -> Result<bool> {
    let Some(p) = block_pos(c, s, "pos") else {
        return Ok(false);
    };
    if !super::world::in_world(ctx, p) {
        return Err(err("argument.pos.outofworld", Vec::new()));
    }
    if !ctx.world.is_loaded(p) {
        return Err(err("argument.pos.unloaded", Vec::new()));
    }
    let Some(id) = ctx.world.biome(p, &ctx.data.blocks) else {
        return Err(err("argument.pos.unloaded", Vec::new()));
    };
    let name = ctx.world.biome_name(id).unwrap_or("");
    Ok(match c.arg("biome") {
        Some(Arg::IdOrTag { id, tag: true }) => ctx
            .commands
            .env
            .tag("minecraft:worldgen/biome", id)
            .iter()
            .any(|b| b == name),
        Some(Arg::IdOrTag { id, .. }) => mc_command::args::full_id(id) == name,
        _ => false,
    })
}

/// The JSON of the data-pack predicate `id`, from the enabled packs
/// (the last one that has it wins).
fn predicate_file(ctx: &ServerContext, id: &str) -> Option<Value> {
    let id = mc_command::args::full_id(id);
    let (ns, path) = id.split_once(':')?;
    let packs = lock(&ctx.packs);
    packs.enabled.iter().rev().find_map(|p| {
        let dir = packs.pack(p)?.dir.as_ref()?;
        let file = dir
            .join("data")
            .join(ns)
            .join("predicate")
            .join(format!("{path}.json"));
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
    })
}

fn no_predicate(id: &str) -> CommandError {
    err(
        "argument.resource_or_id.no_such_element",
        vec![
            str_tag(&mc_command::args::full_id(id)),
            str_tag("minecraft:predicate"),
        ],
    )
}

/// After a line parsed: the error for a predicate it names that no pack
/// has, at the line's end as vanilla puts it.
pub fn check_line(ctx: &ServerContext, chain: &[Context], line: &str) -> Option<CommandError> {
    chain.iter().find_map(|c| match c.arg("predicate") {
        Some(Arg::Id(id)) if predicate_file(ctx, id).is_none() => {
            Some(no_predicate(id).at(line, line.len()))
        }
        _ => None,
    })
}

fn predicate_arg(ctx: &ServerContext, c: &Context) -> Result<bool> {
    let value = match c.arg("predicate") {
        Some(Arg::Id(id)) => predicate_file(ctx, id).ok_or_else(|| no_predicate(id))?,
        Some(Arg::Nbt(t)) => mc_command::text::to_json(t),
        _ => return Ok(false),
    };
    Ok(test(ctx, &value, 0))
}

fn truthy(v: &Value) -> Option<bool> {
    v.as_bool().or_else(|| v.as_i64().map(|n| n != 0))
}

/// Evaluates a loot predicate (the kinds a command source can answer).
fn test(ctx: &ServerContext, v: &Value, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    match v {
        Value::String(id) => predicate_file(ctx, id).is_some_and(|p| test(ctx, &p, depth + 1)),
        Value::Array(all) => all.iter().all(|p| test(ctx, p, depth + 1)),
        Value::Object(o) => {
            let kind = o
                .get("condition")
                .or_else(|| o.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let terms = || {
                o.get("terms")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            };
            match kind.trim_start_matches("minecraft:") {
                "random_chance" => {
                    let chance = o.get("chance").and_then(Value::as_f64).unwrap_or(0.0);
                    ((crate::util::random_u64() >> 11) as f64 / (1u64 << 53) as f64) < chance
                }
                "inverted" => !o.get("term").is_some_and(|t| test(ctx, t, depth + 1)),
                "all_of" => terms().all(|t| test(ctx, t, depth + 1)),
                "any_of" => terms().any(|t| test(ctx, t, depth + 1)),
                "reference" => o
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(|n| predicate_file(ctx, n))
                    .is_some_and(|p| test(ctx, &p, depth + 1)),
                "weather_check" => {
                    let w = lock(&ctx.weather);
                    o.get("raining")
                        .and_then(truthy)
                        .is_none_or(|r| r == w.raining)
                        && o.get("thundering")
                            .and_then(truthy)
                            .is_none_or(|t| t == w.thundering)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn stopwatch_id(c: &Context) -> String {
    match c.arg("id") {
        Some(Arg::Id(id)) => mc_command::args::full_id(id),
        _ => String::new(),
    }
}

fn stopwatch_test(ctx: &ServerContext, c: &Context) -> Result<bool> {
    let id = stopwatch_id(c);
    let Some(start) = lock(&ctx.stopwatches).get(&id).copied() else {
        return Err(err("commands.stopwatch.does_not_exist", vec![str_tag(&id)]));
    };
    let secs = start.elapsed().as_secs_f64();
    Ok(match c.arg("range") {
        Some(Arg::Range(r)) => r.min.is_none_or(|m| secs >= m) && r.max.is_none_or(|m| secs <= m),
        _ => true,
    })
}

/// `execute on|summon|facing ...`; `None` for paths handled elsewhere.
pub fn modify(
    ctx: &ServerContext,
    c: &Context,
    s: &Src,
    path: &[&str],
) -> Option<Result<Vec<Src>>> {
    Some(match path {
        // No entity here has an owner, a vehicle, riders, a target...
        ["execute", "on", _] => Ok(Vec::new()),
        ["execute", "summon", "entity"] => {
            let mut n = s.clone();
            n.who = Who::Entity(crate::util::random_uuid());
            n.name = match c.arg("entity") {
                Some(Arg::Id(id)) => mc_command::args::full_id(id),
                _ => String::new(),
            };
            Ok(vec![n])
        }
        ["execute", "facing", "pos"] => {
            let Some(Arg::Pos(p)) = c.arg("pos") else {
                return Some(Ok(vec![s.clone()]));
            };
            let at = resolve(p, s);
            Ok(vec![facing(s, at)])
        }
        ["execute", "facing", "entity", "targets", "anchor"] => {
            let Some(Arg::Entity(sel)) = c.arg("targets") else {
                return Some(Ok(Vec::new()));
            };
            let eyes = matches!(c.arg("anchor"), Some(Arg::Str(a)) if a == "eyes");
            Ok(target::resolve(ctx, sel, s)
                .iter()
                .map(|t| {
                    let mut at = t.pos();
                    if eyes {
                        at[1] += if t.is_player() { 1.62 } else { 0.0 };
                    }
                    facing(s, at)
                })
                .collect())
        }
        _ => return None,
    })
}

/// `s` turned to look at `at` (from its eyes when anchored there).
fn facing(s: &Src, at: [f64; 3]) -> Src {
    let from_y = s.pos[1] + if s.eyes { 1.62 } else { 0.0 };
    let (dx, dy, dz) = (at[0] - s.pos[0], at[1] - from_y, at[2] - s.pos[2]);
    let flat = (dx * dx + dz * dz).sqrt();
    let pitch = -(dy.atan2(flat).to_degrees()) as f32;
    let yaw = (dz.atan2(dx).to_degrees() - 90.0) as f32;
    let mut n = s.clone();
    n.rot = [wrap(yaw), wrap(pitch)];
    n
}

fn wrap(d: f32) -> f32 {
    let mut d = d % 360.0;
    if d >= 180.0 {
        d -= 360.0;
    }
    if d < -180.0 {
        d += 360.0;
    }
    d
}

/// `/stopwatch`.
pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    let id = || stopwatch_id(r.c);
    Some(match path {
        ["stopwatch", "create", "id"] => {
            let id = id();
            let mut all = lock(&r.ctx.stopwatches);
            if all.contains_key(&id) {
                return Some(Err(err(
                    "commands.stopwatch.already_exists",
                    vec![str_tag(&id)],
                )));
            }
            all.insert(id.clone(), std::time::Instant::now());
            drop(all);
            r.success(
                translate("commands.stopwatch.create.success", vec![str_tag(&id)]),
                true,
            );
            Ok(1)
        }
        [
            "stopwatch",
            how @ ("query" | "restart" | "remove"),
            "id",
            ..,
        ] => {
            let id = id();
            let mut all = lock(&r.ctx.stopwatches);
            let Some(start) = all.get(&id).copied() else {
                return Some(Err(err(
                    "commands.stopwatch.does_not_exist",
                    vec![str_tag(&id)],
                )));
            };
            match *how {
                "query" => {
                    drop(all);
                    let secs = start.elapsed().as_secs_f64();
                    let scale = match r.c.arg("scale") {
                        Some(Arg::Double(v)) => *v,
                        _ => 1.0,
                    };
                    r.success(
                        translate(
                            "commands.stopwatch.query",
                            vec![str_tag(&id), Tag::Double(secs)],
                        ),
                        false,
                    );
                    Ok((secs * scale).floor() as i32)
                }
                "restart" => {
                    all.insert(id.clone(), std::time::Instant::now());
                    drop(all);
                    r.success(
                        translate("commands.stopwatch.restart.success", vec![str_tag(&id)]),
                        true,
                    );
                    Ok(1)
                }
                _ => {
                    all.remove(&id);
                    drop(all);
                    r.success(
                        translate("commands.stopwatch.remove.success", vec![str_tag(&id)]),
                        true,
                    );
                    Ok(1)
                }
            }
        }
        _ => return None,
    })
}
