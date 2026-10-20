//! Commands of the `scores` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

#![allow(dead_code)]

use mc_command::dispatch::Context;
use mc_nbt::{Compound, Tag};
use mc_protocol::text::translate;

use super::run::err;
use super::target::Target;
use super::{Result, Run, Src};
use crate::context::ServerContext;
use crate::tick::lock;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["tag", "targets", "add", "name"] => tag_change(r, true),
        ["tag", "targets", "remove", "name"] => tag_change(r, false),
        ["tag", "targets", "list"] => tag_list(r),
        _ => return None,
    })
}

/// An entity holds at most this many tags.
const MAX_TAGS: usize = 1024;

/// Changes the tags of a player or entity; whether they changed.
fn change_tags(
    ctx: &ServerContext,
    t: &Target,
    change: impl FnOnce(&mut Vec<String>) -> bool,
) -> bool {
    if t.is_player() {
        lock(&ctx.players)
            .get_mut(&t.uuid())
            .is_some_and(|p| change(&mut p.extra.tags))
    } else {
        lock(&ctx.entities)
            .iter_mut()
            .find(|e| e.uuid == t.uuid())
            .is_some_and(|e| change(&mut e.tags))
    }
}

fn tag_change(r: &mut Run, add: bool) -> Result<i32> {
    let targets = r.targets("targets", false)?;
    let name = r.string("name").unwrap_or_default();
    let mut done = 0;
    for t in &targets {
        let changed = change_tags(r.ctx, t, |tags| {
            if add {
                if tags.len() >= MAX_TAGS || tags.contains(&name) {
                    return false;
                }
                tags.push(name.clone());
                true
            } else {
                let before = tags.len();
                tags.retain(|g| g != &name);
                tags.len() != before
            }
        });
        if changed {
            done += 1;
        }
    }
    let op = if add { "add" } else { "remove" };
    if done == 0 {
        return Err(err(&format!("commands.tag.{op}.failed"), Vec::new()));
    }
    let msg = if targets.len() == 1 {
        translate(
            &format!("commands.tag.{op}.success.single"),
            vec![Tag::String(name), targets[0].display()],
        )
    } else {
        translate(
            &format!("commands.tag.{op}.success.multiple"),
            vec![Tag::String(name), Tag::Int(targets.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(done)
}

/// Names in green, separated by gray commas, as vanilla lists them.
pub(super) fn green_list(names: &[String]) -> Tag {
    let green = |n: &String| {
        let mut c = Compound::new();
        c.insert("color", "green");
        c.insert("text", n.as_str());
        Tag::Compound(c)
    };
    joined(names.iter().map(green).collect())
}

/// Components separated by gray commas, as vanilla lists them.
pub(super) fn joined(items: Vec<Tag>) -> Tag {
    let mut parts = Vec::new();
    for (i, n) in items.into_iter().enumerate() {
        if i > 0 {
            let mut sep = Compound::new();
            sep.insert("color", "gray");
            sep.insert("text", ", ");
            parts.push(Tag::Compound(sep));
        }
        parts.push(n);
    }
    match parts.len() {
        0 => Tag::from(""),
        1 => parts.remove(0),
        _ => {
            let mut c = Compound::new();
            c.insert("text", "");
            c.insert("extra", mc_protocol::text::list(parts));
            Tag::Compound(c)
        }
    }
}

fn tag_list(r: &mut Run) -> Result<i32> {
    let targets = r.targets("targets", false)?;
    let mut all: Vec<String> = Vec::new();
    for t in &targets {
        for g in t.tags() {
            if !all.contains(g) {
                all.push(g.clone());
            }
        }
    }
    all.sort();
    let n = all.len() as i32;
    // Vanilla says "multiple.empty" with the (zero) count of tags even for
    // one target.
    let msg = match (targets.len(), all.is_empty()) {
        (_, true) => translate("commands.tag.list.multiple.empty", vec![Tag::Int(0)]),
        (1, false) => translate(
            "commands.tag.list.single.success",
            vec![targets[0].display(), Tag::Int(n), green_list(&all)],
        ),
        (count, false) => translate(
            "commands.tag.list.multiple.success",
            vec![Tag::Int(count as i32), Tag::Int(n), green_list(&all)],
        ),
    };
    r.success(msg, false);
    Ok(n)
}

/// `execute if|unless score ...`; `None` for other tests.
pub fn condition(
    _ctx: &ServerContext,
    _c: &Context,
    _s: &Src,
    _rest: &[&str],
) -> Option<Result<bool>> {
    None
}

/// `execute` subcommands of this group; `None` for others.
pub fn modify(
    _ctx: &ServerContext,
    _c: &Context,
    _s: &Src,
    _path: &[&str],
) -> Option<Result<Vec<Src>>> {
    None
}
