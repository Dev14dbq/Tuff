//! `/function`, `/return`, `/schedule`, `/datapack` and `/reload`, and
//! running functions from data packs (scheduled ones and the `#tick` and
//! `#load` tags each tick).

use std::sync::Arc;

use mc_command::args::{Arg, full_id};
use mc_command::dispatch::Context;
use mc_nbt::{Compound, Tag};
use mc_protocol::text::{colored, translate};

use super::run::{err, style};
use super::{Output, Result, Run, Source, Src, Who};
use crate::context::{Event, ServerContext};
use crate::packs::{self, Line, Pack, Part};
use crate::tick::{TickControl, lock};

pub use crate::packs::Function;

/// How deep functions may call functions before the rest is dropped.
const MAX_DEPTH: u32 = 512;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["function", "name", rest @ ..] => function(r, rest),
        ["return", "value"] => {
            let v = r.int("value").unwrap_or(0);
            r.out.ret = Some((true, v));
            Ok(v)
        }
        ["return", "fail"] => {
            r.out.ret = Some((false, 0));
            Ok(0)
        }
        ["schedule", "function", "function", "time", mode @ ..] => {
            schedule(r, mode.first() == Some(&"append"))
        }
        ["schedule", "clear", "function"] => schedule_clear(r),
        ["datapack", "list", rest @ ..] => datapack_list(r, rest.first().copied()),
        ["datapack", "enable", "name", rest @ ..] => datapack_enable(r, rest),
        ["datapack", "disable", "name"] => datapack_disable(r),
        ["datapack", "create", "id", "description"] => datapack_create(r),
        ["reload"] => {
            lock(&r.ctx.packs).reload(&r.ctx.commands);
            r.success(translate("commands.reload.success", Vec::new()), true);
            Ok(0)
        }
        _ => return None,
    })
}

/// `execute if|unless data|function|...`; `None` for other tests.
pub fn condition(
    _ctx: &ServerContext,
    _c: &Context,
    _s: &Src,
    _rest: &[&str],
) -> Option<Result<bool>> {
    None
}

/// `execute store|on|summon|facing|...`; `None` for others.
pub fn modify(
    _ctx: &ServerContext,
    _c: &Context,
    _s: &Src,
    _path: &[&str],
) -> Option<Result<Vec<Src>>> {
    None
}

/// Sends the lines so far straight to the player, so they come before
/// what the function's commands send.
fn flush(r: &mut Run) {
    if let Who::Player(uuid) = r.src.who {
        for line in std::mem::take(&mut r.out.lines) {
            r.out.events.push(Event::Message {
                uuid,
                line,
                overlay: false,
            });
        }
    }
}

fn function_arg(r: &Run, name: &str) -> (String, bool) {
    match r.arg(name) {
        Some(Arg::IdOrTag { id, tag }) => (full_id(id), *tag),
        Some(Arg::Id(id)) => (full_id(id), false),
        _ => (String::new(), false),
    }
}

/// NBT type names as vanilla's messages give them.
fn type_name(t: &Tag) -> &'static str {
    match t {
        Tag::Byte(_) => "BYTE",
        Tag::Short(_) => "SHORT",
        Tag::Int(_) => "INT",
        Tag::Long(_) => "LONG",
        Tag::Float(_) => "FLOAT",
        Tag::Double(_) => "DOUBLE",
        Tag::ByteArray(_) => "BYTE[]",
        Tag::String(_) => "STRING",
        Tag::List(_) => "LIST",
        Tag::Compound(_) => "COMPOUND",
        Tag::IntArray(_) => "INT[]",
        Tag::LongArray(_) => "LONG[]",
    }
}

fn function(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let (id, tag) = function_arg(r, "name");
    let functions: Vec<Arc<Function>> = {
        let packs = lock(&r.ctx.packs);
        if tag {
            packs.tag(&id).unwrap_or_default()
        } else {
            packs.function(&id).into_iter().collect()
        }
    };
    if functions.is_empty() {
        let key = if tag {
            "commands.function.scheduled.no_functions"
        } else {
            "arguments.function.unknown"
        };
        return Err(err(key, vec![Tag::String(id)]));
    }
    let args: Option<Compound> = match rest {
        ["arguments"] => match r.nbt("arguments") {
            Some(Tag::Compound(c)) => Some(c),
            _ => Some(Compound::new()),
        },
        ["with", kind, "source", more @ ..] => {
            let h = super::data::holder(r, kind, "source")?;
            let mut data = Tag::Compound(super::data::read(r.ctx, &h)?);
            if !more.is_empty()
                && let Some(path) = super::data::path_arg(r, "path")
            {
                data = super::data::single(&path, &data)?;
            }
            match data {
                Tag::Compound(c) => Some(c),
                other => {
                    return Err(err(
                        "commands.function.error.argument_not_compound",
                        vec![Tag::from(type_name(&other))],
                    ));
                }
            }
        }
        _ => None,
    };
    if functions.len() == 1 {
        let name = Tag::String(functions[0].id.clone());
        r.success(
            translate("commands.function.scheduled.single", vec![name]),
            false,
        );
    } else {
        let mut parts = Vec::new();
        for (i, f) in functions.iter().enumerate() {
            if i > 0 {
                parts.push(colored(", ", "gray"));
            }
            parts.push(Tag::String(f.id.clone()));
        }
        let mut list = Compound::new();
        list.insert("text", "");
        list.insert("extra", mc_protocol::text::list(parts));
        r.success(
            translate(
                "commands.function.scheduled.multiple",
                vec![Tag::Compound(list)],
            ),
            false,
        );
    }
    flush(r);
    let mut total = 0;
    for f in functions {
        let lines = match instantiate(r.ctx, &f, args.as_ref()) {
            Ok(lines) => lines,
            Err(why) => {
                if !r.src.silent {
                    let e = err(
                        "commands.function.instantiationFailure",
                        vec![Tag::String(f.id.clone()), why],
                    );
                    r.out.lines.extend(e.lines().into_iter().take(1));
                    flush(r);
                }
                continue;
            }
        };
        let src = r.src.clone();
        if let Some((_, v)) = call(r.ctx, r.tick, r.out, &mut r.admin, &src, &lines) {
            let with = vec![Tag::String(f.id.clone()), Tag::Int(v)];
            r.success(translate("commands.function.result", with), false);
            flush(r);
            total += v;
        }
    }
    Ok(total)
}

/// The function's lines with macro arguments filled in, or why not.
fn instantiate(
    ctx: &ServerContext,
    f: &Function,
    args: Option<&Compound>,
) -> std::result::Result<Vec<Vec<Context>>, Tag> {
    let id = || Tag::String(f.id.clone());
    if f.is_macro() {
        let Some(args) = args else {
            return Err(translate(
                "commands.function.error.missing_arguments",
                vec![id()],
            ));
        };
        if let Some(missing) = f.vars().into_iter().find(|v| args.get(v).is_none()) {
            return Err(translate(
                "commands.function.error.missing_argument",
                vec![id(), Tag::from(missing)],
            ));
        }
    }
    let mut out = Vec::new();
    for line in &f.lines {
        match line {
            Line::Parsed(chain) => out.push(chain.clone()),
            Line::Macro(parts) => {
                let mut text = String::new();
                for p in parts {
                    match p {
                        Part::Text(t) => text.push_str(t),
                        Part::Var(v) => {
                            if let Some(t) = args.and_then(|a| a.get(v)) {
                                text.push_str(&packs::macro_value(t));
                            }
                        }
                    }
                }
                match packs::parse_line(&text, &ctx.commands) {
                    Ok(chain) => out.push(chain),
                    Err(e) => {
                        return Err(translate(
                            "commands.function.error.parse",
                            vec![id(), Tag::String(text), Tag::String(e)],
                        ));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// `execute if function`: runs the function(s); whether one returned
/// something other than 0.
pub(super) fn test(
    ctx: &ServerContext,
    tick: &mut TickControl,
    out: &mut Output,
    admin: &mut Vec<Tag>,
    c: &Context,
    src: &Src,
) -> Result<bool> {
    let (id, tag) = match c.arg("name") {
        Some(Arg::IdOrTag { id, tag }) => (full_id(id), *tag),
        _ => (String::new(), false),
    };
    let functions: Vec<Arc<Function>> = {
        let packs = lock(&ctx.packs);
        if tag {
            packs.tag(&id).unwrap_or_default()
        } else {
            packs.function(&id).into_iter().collect()
        }
    };
    if functions.is_empty() {
        return Err(err("arguments.function.unknown", vec![Tag::String(id)]));
    }
    let mut pass = false;
    for f in functions {
        let Ok(lines) = instantiate(ctx, &f, None) else {
            continue;
        };
        if let Some((true, v)) = call(ctx, tick, out, admin, src, &lines) {
            pass |= v != 0;
        }
    }
    Ok(pass)
}

/// Runs a function's lines as `src`, without feedback; what it returned
/// with `return`, if it did.
fn call(
    ctx: &ServerContext,
    tick: &mut TickControl,
    out: &mut Output,
    admin: &mut Vec<Tag>,
    src: &Src,
    lines: &[Vec<Context>],
) -> Option<(bool, i32)> {
    if out.depth >= MAX_DEPTH {
        out.aborted = true;
        return None;
    }
    let outer = out.ret.take();
    out.depth += 1;
    let mut inner = src.clone();
    inner.silent = true;
    inner.stores.clear();
    for chain in lines {
        if out.aborted {
            break;
        }
        let _ = super::run_chain(ctx, tick, chain, inner.clone(), out, admin);
        if out.ret.is_some() {
            break;
        }
    }
    out.depth -= 1;
    std::mem::replace(&mut out.ret, outer)
}

/// Runs a function (or the functions of a `#tag`) as the server, as
/// `/schedule` and the `#tick` and `#load` tags do.
fn run_as_server(ctx: &ServerContext, tick: &mut TickControl, id: &str) -> Vec<Event> {
    let functions: Vec<Arc<Function>> = {
        let packs = lock(&ctx.packs);
        match id.strip_prefix('#') {
            Some(tag) => packs.tag(tag).unwrap_or_default(),
            None => packs.function(id).into_iter().collect(),
        }
    };
    let src = Src::of(ctx, &Source::Console);
    let mut out = Output::default();
    let mut admin = Vec::new();
    for f in functions {
        if let Ok(lines) = instantiate(ctx, &f, None) {
            out.steps = 0;
            out.forks = 0;
            out.aborted = false;
            call(ctx, tick, &mut out, &mut admin, &src, &lines);
        }
    }
    out.events
}

/// Each tick: `#minecraft:load` after a (re)load, functions whose
/// scheduled time came, then `#minecraft:tick`.
pub(crate) fn run_scheduled(ctx: &ServerContext, tick: &mut TickControl) -> Vec<Event> {
    let now = lock(&ctx.time).age;
    let (load, due) = {
        let mut packs = lock(&ctx.packs);
        let load = std::mem::take(&mut packs.load_pending);
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut packs.scheduled)
            .into_iter()
            .partition(|s| s.at <= now);
        packs.scheduled = later;
        (load, due)
    };
    let mut events = Vec::new();
    if load {
        events.extend(run_as_server(ctx, tick, "#minecraft:load"));
    }
    for s in due {
        events.extend(run_as_server(ctx, tick, &s.id));
    }
    events.extend(run_as_server(ctx, tick, "#minecraft:tick"));
    events
}

fn schedule(r: &mut Run, append: bool) -> Result<i32> {
    let (id, tag) = function_arg(r, "function");
    let ticks = r.int("time").unwrap_or(0);
    if ticks == 0 {
        return Err(err("commands.schedule.same_tick", Vec::new()));
    }
    if !tag {
        match lock(&r.ctx.packs).function(&id) {
            None => return Err(err("arguments.function.unknown", vec![Tag::String(id)])),
            Some(f) if f.is_macro() => return Err(err("commands.schedule.macro", Vec::new())),
            Some(_) => {}
        }
    }
    let at = lock(&r.ctx.time).age + i64::from(ticks);
    let key = if tag { format!("#{id}") } else { id.clone() };
    {
        let mut packs = lock(&r.ctx.packs);
        if !append {
            packs.scheduled.retain(|s| s.id != key);
        }
        packs.scheduled.push(packs::Scheduled { id: key, at });
    }
    let kind = if tag { "tag" } else { "function" };
    r.success(
        translate(
            &format!("commands.schedule.created.{kind}"),
            vec![Tag::String(id), Tag::Int(ticks), Tag::Int(at as i32)],
        ),
        true,
    );
    Ok(at as i32)
}

fn schedule_clear(r: &mut Run) -> Result<i32> {
    let id = r
        .string("function")
        .map(|s| full_id(&s))
        .unwrap_or_default();
    let removed = {
        let mut packs = lock(&r.ctx.packs);
        let before = packs.scheduled.len();
        packs.scheduled.retain(|s| s.id != id);
        before - packs.scheduled.len()
    };
    if removed == 0 {
        return Err(err(
            "commands.schedule.cleared.failure",
            vec![Tag::String(id)],
        ));
    }
    r.success(
        translate(
            "commands.schedule.cleared.success",
            vec![Tag::Int(removed as i32), Tag::String(id)],
        ),
        true,
    );
    Ok(removed as i32)
}

/// A string as a brigadier string argument: quoted unless plain.
fn quote_if_needed(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.+".contains(c))
    {
        return s.to_owned();
    }
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `[name (source)]` with the description on hover.
fn pack_text(p: &Pack) -> Tag {
    let mut hover_text = Compound::new();
    hover_text.insert("text", "");
    let (source, extra) = if p.dir.is_none() {
        (
            "pack.source.builtin",
            vec![
                translate("dataPack.vanilla.name", Vec::new()),
                Tag::from("\n"),
                translate("dataPack.vanilla.description", Vec::new()),
            ],
        )
    } else {
        (
            "pack.source.world",
            vec![
                Tag::String(p.title.clone()),
                Tag::from("\n"),
                p.description.clone(),
            ],
        )
    };
    hover_text.insert("extra", mc_protocol::text::list(extra));
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", Tag::Compound(hover_text));
    let mut name = translate(
        "pack.nameAndSource",
        vec![Tag::String(p.id.clone()), translate(source, Vec::new())],
    );
    style(&mut name, &[("color", "gray")]);
    let mut text = translate("chat.square_brackets", vec![name]);
    style(&mut text, &[("color", "green")]);
    if let Tag::Compound(c) = &mut text {
        c.insert("hover_event", Tag::Compound(hover));
        c.insert("insertion", Tag::String(quote_if_needed(&p.id)));
    }
    text
}

fn joined(packs: &[&Pack]) -> Tag {
    if let [one] = packs {
        return pack_text(one);
    }
    let mut parts = Vec::new();
    for (i, p) in packs.iter().enumerate() {
        if i > 0 {
            parts.push(colored(", ", "gray"));
        }
        parts.push(pack_text(p));
    }
    let mut c = Compound::new();
    c.insert("text", "");
    c.insert("extra", mc_protocol::text::list(parts));
    Tag::Compound(c)
}

fn datapack_list(r: &mut Run, which: Option<&str>) -> Result<i32> {
    let (enabled, available) = {
        let mut packs = lock(&r.ctx.packs);
        packs.scan();
        let enabled: Vec<Pack> = packs
            .enabled
            .iter()
            .filter_map(|id| packs.pack(id).cloned())
            .collect();
        let mut available: Vec<Pack> = packs
            .all
            .iter()
            .filter(|p| !packs.enabled.contains(&p.id))
            .cloned()
            .collect();
        available.sort_by(|a, b| a.id.cmp(&b.id));
        (enabled, available)
    };
    let mut count = 0;
    if which != Some("available") {
        count += enabled.len() as i32;
        let message = if enabled.is_empty() {
            translate("commands.datapack.list.enabled.none", Vec::new())
        } else {
            let list: Vec<&Pack> = enabled.iter().collect();
            translate(
                "commands.datapack.list.enabled.success",
                vec![Tag::Int(enabled.len() as i32), joined(&list)],
            )
        };
        r.success(message, false);
    }
    if which != Some("enabled") {
        count += available.len() as i32;
        let message = if available.is_empty() {
            translate("commands.datapack.list.available.none", Vec::new())
        } else {
            let list: Vec<&Pack> = available.iter().collect();
            translate(
                "commands.datapack.list.available.success",
                vec![Tag::Int(available.len() as i32), joined(&list)],
            )
        };
        r.success(message, false);
    }
    Ok(count)
}

/// The pack named `name`, or vanilla's error.
fn known_pack(r: &Run, name: &str) -> Result<Pack> {
    let packs = lock(&r.ctx.packs);
    if let Some(p) = packs.pack(name) {
        return Ok(p.clone());
    }
    if packs::FEATURE_PACKS.contains(&name) {
        return Err(err(
            "commands.datapack.enable.failed.no_flags",
            vec![Tag::from(name), Tag::String(format!("minecraft:{name}"))],
        ));
    }
    Err(err("commands.datapack.unknown", vec![Tag::from(name)]))
}

fn datapack_enable(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let name = r.string("name").unwrap_or_default();
    lock(&r.ctx.packs).scan();
    let pack = known_pack(r, &name)?;
    let existing = r.string("existing");
    {
        let mut packs = lock(&r.ctx.packs);
        if packs.enabled.contains(&name) {
            return Err(err(
                "commands.datapack.enable.failed",
                vec![Tag::String(name)],
            ));
        }
        let at = match rest {
            ["first"] => 0,
            [kind @ ("before" | "after"), ..] => {
                let other = existing.unwrap_or_default();
                let Some(i) = packs.enabled.iter().position(|p| *p == other) else {
                    return Err(err("commands.datapack.unknown", vec![Tag::String(other)]));
                };
                if *kind == "after" { i + 1 } else { i }
            }
            _ => packs.enabled.len(),
        };
        packs.enabled.insert(at, name);
        packs.reload(&r.ctx.commands);
    }
    r.success(
        translate("commands.datapack.modify.enable", vec![pack_text(&pack)]),
        true,
    );
    Ok(lock(&r.ctx.packs).enabled.len() as i32)
}

fn datapack_disable(r: &mut Run) -> Result<i32> {
    let name = r.string("name").unwrap_or_default();
    lock(&r.ctx.packs).scan();
    let pack = known_pack(r, &name)?;
    {
        let mut packs = lock(&r.ctx.packs);
        let Some(i) = packs.enabled.iter().position(|p| *p == name) else {
            return Err(err(
                "commands.datapack.disable.failed",
                vec![Tag::String(name)],
            ));
        };
        packs.enabled.remove(i);
        packs.reload(&r.ctx.commands);
    }
    r.success(
        translate("commands.datapack.modify.disable", vec![pack_text(&pack)]),
        true,
    );
    Ok(lock(&r.ctx.packs).enabled.len() as i32)
}

fn datapack_create(r: &mut Run) -> Result<i32> {
    let id = r.string("id").unwrap_or_default();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-.".contains(c))
    {
        return Err(err(
            "commands.datapack.create.invalid_name",
            vec![Tag::String(id)],
        ));
    }
    let Some(folder) = lock(&r.ctx.packs).folder().map(|f| f.join(&id)) else {
        return Err(err(
            "commands.datapack.create.io_failure",
            vec![Tag::String(id)],
        ));
    };
    if folder.exists() {
        return Err(err(
            "commands.datapack.create.already_exists",
            vec![Tag::String(id)],
        ));
    }
    let description = match r.nbt("description") {
        Some(Tag::String(s)) => serde_json::Value::String(s),
        Some(t) => mc_command::text::to_json(&t),
        None => serde_json::Value::String(String::new()),
    };
    let format = std::fs::read_to_string(r.ctx.data.root.join("version.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["pack_version"]["data_major"].as_i64())
        .unwrap_or(0);
    let meta = serde_json::json!({
        "pack": {"description": description, "min_format": format, "max_format": format}
    });
    let written = std::fs::create_dir_all(&folder).and_then(|()| {
        std::fs::write(
            folder.join("pack.mcmeta"),
            serde_json::to_string_pretty(&meta).unwrap_or_default(),
        )
    });
    if written.is_err() {
        return Err(err(
            "commands.datapack.create.io_failure",
            vec![Tag::String(id)],
        ));
    }
    lock(&r.ctx.packs).scan();
    r.success(
        translate("commands.datapack.create.success", vec![Tag::String(id)]),
        true,
    );
    Ok(1)
}

/// Runs a whole tick's worth of functions; the events go out at once.
pub(crate) fn tick(ctx: &ServerContext, tick: &mut TickControl) {
    for e in run_scheduled(ctx, tick) {
        let _ = ctx.events.send(e);
    }
}
