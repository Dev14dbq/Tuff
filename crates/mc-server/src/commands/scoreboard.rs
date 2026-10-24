//! `/scoreboard`, `/trigger`, `execute if|unless score` and
//! `execute store ... score`, with vanilla's messages and results.

use mc_command::args::Arg;
use mc_command::dispatch::Context;
use mc_nbt::Tag;
use mc_protocol::packets::play::{NumberFormat, ResetScore};
use mc_protocol::text::translate;

use super::run::err;
use super::{Output, Result, Run, Src, Store, StoreTarget};
use crate::context::{Event, ServerContext};
use crate::scoreboard::{Objective, Packet, Scoreboard, read_only, slot_id, slot_name};
use crate::tick::lock;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        [
            "scoreboard",
            "objectives",
            "add",
            "objective",
            "criteria",
            ..,
        ] => objective_add(r),
        ["scoreboard", "objectives", "list"] => objective_list(r),
        ["scoreboard", "objectives", "remove", "objective"] => objective_remove(r),
        ["scoreboard", "objectives", "modify", "objective", rest @ ..] => objective_modify(r, rest),
        ["scoreboard", "objectives", "setdisplay", "slot", rest @ ..] => {
            set_display(r, !rest.is_empty())
        }
        ["scoreboard", "players", "set", ..] => set(r),
        ["scoreboard", "players", kind @ ("add" | "remove"), ..] => add(r, *kind == "remove"),
        ["scoreboard", "players", "get", ..] => get(r),
        ["scoreboard", "players", "list"] => list_holders(r),
        ["scoreboard", "players", "list", "target"] => list_scores(r),
        ["scoreboard", "players", "reset", "targets"] => reset(r, false),
        ["scoreboard", "players", "reset", "targets", "objective"] => reset(r, true),
        ["scoreboard", "players", "enable", ..] => enable(r),
        ["scoreboard", "players", "operation", ..] => operation(r),
        [
            "scoreboard",
            "players",
            "display",
            "name",
            "targets",
            "objective",
            rest @ ..,
        ] => display_name(r, !rest.is_empty()),
        [
            "scoreboard",
            "players",
            "display",
            "numberformat",
            "targets",
            "objective",
            rest @ ..,
        ] => display_format(r, rest),
        ["trigger", "objective", rest @ ..] => trigger(r, rest),
        _ => return None,
    })
}

/// A name scores are kept under, and how messages show it.
pub struct Holder {
    pub name: String,
    pub display: Tag,
}

/// The holders an argument names: entities by selector, `*` for every
/// holder with a score, or a name.
pub fn holders(ctx: &ServerContext, c: &Context, src: &Src, arg: &str) -> Result<Vec<Holder>> {
    let found: Vec<Holder> = match c.arg(arg) {
        Some(Arg::Entity(sel)) => super::target::resolve(ctx, sel, src)
            .iter()
            .map(|t| Holder {
                name: t.holder(),
                display: t.feedback(),
            })
            .collect(),
        Some(Arg::Str(s)) if s == "*" => lock(&ctx.scoreboard)
            .holders()
            .into_iter()
            .map(|name| Holder {
                display: Tag::from(name.as_str()),
                name,
            })
            .collect(),
        Some(Arg::Str(s)) => vec![named(ctx, s)],
        _ => Vec::new(),
    };
    if found.is_empty() {
        return Err(err("argument.scoreHolder.empty", Vec::new()));
    }
    Ok(found)
}

/// A holder given by name: an online player by that name, or the name.
fn named(ctx: &ServerContext, name: &str) -> Holder {
    let player = super::target::everyone(ctx)
        .into_iter()
        .find(|t| t.is_player() && t.holder() == name);
    Holder {
        name: name.to_owned(),
        display: player.map_or_else(|| Tag::from(name), |t| t.feedback()),
    }
}

/// The one holder an argument names (`*` names none).
fn holder(r: &Run, arg: &str) -> Result<Holder> {
    if matches!(r.arg(arg), Some(Arg::Str(s)) if s == "*") {
        return Err(err("argument.scoreHolder.empty", Vec::new()));
    }
    let mut all = holders(r.ctx, r.c, &r.src, arg)?;
    Ok(all.remove(0))
}

fn objective_named(board: &Scoreboard, name: &str) -> Result<Objective> {
    board
        .objectives
        .get(name)
        .cloned()
        .ok_or_else(|| err("arguments.objective.notFound", vec![Tag::from(name)]))
}

fn objective(r: &Run, arg: &str) -> Result<Objective> {
    let name = r.string(arg).unwrap_or_default();
    objective_named(&lock(&r.ctx.scoreboard), &name)
}

/// An objective commands may set scores in.
fn writable(r: &Run, arg: &str) -> Result<Objective> {
    let o = objective(r, arg)?;
    if read_only(&o.criteria) {
        return Err(err(
            "arguments.objective.readonly",
            vec![Tag::from(o.name.as_str())],
        ));
    }
    Ok(o)
}

fn send(out: &mut Output, packets: Vec<Packet>) {
    out.events
        .extend(packets.into_iter().map(Event::Scoreboard));
}

/// Sets a score with `change`; the packet if it showed and changed.
fn change(
    board: &mut Scoreboard,
    holder: &str,
    objective: &str,
    change: impl FnOnce(&mut crate::scoreboard::Score),
) -> Option<Packet> {
    let before = board.score(holder, objective).cloned();
    let s = board.score_mut(holder, objective);
    change(s);
    let after = s.clone();
    let same = before.is_some_and(|b| {
        b.value == after.value && b.display == after.display && b.format == after.format
    });
    if same {
        None
    } else {
        board.score_packet(holder, objective)
    }
}

fn single_or_many(n: usize, key: &str, single_args: Vec<Tag>, many: Vec<Tag>) -> Tag {
    if n == 1 {
        translate(&format!("{key}.single"), single_args)
    } else {
        translate(&format!("{key}.multiple"), many)
    }
}

fn number_format(r: &Run, rest: &[&str]) -> Option<NumberFormat> {
    match rest {
        ["blank"] => Some(NumberFormat::Blank),
        ["fixed", "contents"] => Some(NumberFormat::Fixed(
            r.nbt("contents").unwrap_or_else(|| Tag::from("")),
        )),
        ["styled", "style"] => Some(NumberFormat::Styled(
            r.nbt("style").unwrap_or(Tag::Compound(Default::default())),
        )),
        _ => None,
    }
}

// Objectives.

fn objective_add(r: &mut Run) -> Result<i32> {
    let name = r.string("objective").unwrap_or_default();
    let criteria = r.string("criteria").unwrap_or_default();
    let display = r
        .nbt("displayName")
        .unwrap_or_else(|| Tag::from(name.as_str()));
    let mut board = lock(&r.ctx.scoreboard);
    if board.objectives.contains_key(&name) {
        return Err(err(
            "commands.scoreboard.objectives.add.duplicate",
            Vec::new(),
        ));
    }
    let o = Objective {
        hearts: criteria == "health",
        name,
        criteria,
        display,
        auto_update: false,
        format: None,
    };
    let shown = o.formatted();
    board.add_objective(o);
    let n = board.objectives.len() as i32;
    drop(board);
    r.success(
        translate("commands.scoreboard.objectives.add.success", vec![shown]),
        true,
    );
    Ok(n)
}

fn objective_list(r: &mut Run) -> Result<i32> {
    let shown: Vec<Tag> = {
        let board = lock(&r.ctx.scoreboard);
        board
            .objective_names()
            .iter()
            .map(|n| board.objectives[n].formatted())
            .collect()
    };
    let n = shown.len() as i32;
    let msg = if shown.is_empty() {
        translate("commands.scoreboard.objectives.list.empty", Vec::new())
    } else {
        translate(
            "commands.scoreboard.objectives.list.success",
            vec![Tag::Int(n), super::scores::joined(shown)],
        )
    };
    r.success(msg, false);
    Ok(n)
}

fn objective_remove(r: &mut Run) -> Result<i32> {
    let o = objective(r, "objective")?;
    let packets = lock(&r.ctx.scoreboard).remove_objective(&o.name);
    let n = lock(&r.ctx.scoreboard).objectives.len() as i32;
    send(r.out, packets);
    r.success(
        translate(
            "commands.scoreboard.objectives.remove.success",
            vec![o.formatted()],
        ),
        true,
    );
    Ok(n)
}

fn objective_modify(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let mut o = objective(r, "objective")?;
    let name = Tag::from(o.name.as_str());
    let msg = match rest {
        ["displayname", "displayName"] => {
            o.display = r.nbt("displayName").unwrap_or_else(|| Tag::from(""));
            translate(
                "commands.scoreboard.objectives.modify.displayname",
                vec![name, o.formatted()],
            )
        }
        ["rendertype", kind] => {
            o.hearts = *kind == "hearts";
            translate(
                "commands.scoreboard.objectives.modify.rendertype",
                vec![o.formatted()],
            )
        }
        ["displayautoupdate", "value"] => {
            o.auto_update = r.boolean("value").unwrap_or(false);
            let how = if o.auto_update { "enable" } else { "disable" };
            translate(
                &format!("commands.scoreboard.objectives.modify.displayAutoUpdate.{how}"),
                vec![name, o.formatted()],
            )
        }
        ["numberformat", format @ ..] => {
            o.format = number_format(r, format);
            let how = if o.format.is_some() { "set" } else { "clear" };
            translate(
                &format!("commands.scoreboard.objectives.modify.objectiveFormat.{how}"),
                vec![name],
            )
        }
        _ => return Err(err("command.failed", Vec::new())),
    };
    let mut board = lock(&r.ctx.scoreboard);
    let shown = board.displayed(&o.name);
    let packet = o.packet(2);
    board.objectives.insert(o.name.clone(), o);
    drop(board);
    if shown {
        send(r.out, vec![Packet::Objective(packet)]);
    }
    r.success(msg, true);
    Ok(0)
}

fn set_display(r: &mut Run, with_objective: bool) -> Result<i32> {
    let slot_text = r.string("slot").unwrap_or_default();
    let slot = slot_id(&slot_text).unwrap_or(1);
    let mut board = lock(&r.ctx.scoreboard);
    let msg = if with_objective {
        let name = r.string("objective").unwrap_or_default();
        let o = objective_named(&board, &name)?;
        if board.slots[slot].as_deref() == Some(name.as_str()) {
            return Err(err(
                "commands.scoreboard.objectives.display.alreadySet",
                Vec::new(),
            ));
        }
        let packets = board.set_slot(slot, Some(name));
        drop(board);
        send(r.out, packets);
        translate(
            "commands.scoreboard.objectives.display.set",
            vec![Tag::from(slot_name(slot).as_str()), o.display],
        )
    } else {
        if board.slots[slot].is_none() {
            return Err(err(
                "commands.scoreboard.objectives.display.alreadyEmpty",
                Vec::new(),
            ));
        }
        let packets = board.set_slot(slot, None);
        drop(board);
        send(r.out, packets);
        translate(
            "commands.scoreboard.objectives.display.cleared",
            vec![Tag::from(slot_name(slot).as_str())],
        )
    };
    r.success(msg, true);
    Ok(0)
}

// Scores.

fn set(r: &mut Run) -> Result<i32> {
    let holders = holders(r.ctx, r.c, &r.src, "targets")?;
    let o = writable(r, "objective")?;
    let value = r.int("score").unwrap_or(0);
    let mut packets = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for h in &holders {
            packets.extend(change(&mut board, &h.name, &o.name, |s| s.value = value));
        }
    }
    send(r.out, packets);
    let count = if value == 0 { 0 } else { holders.len() };
    let msg = if count == 1 {
        translate(
            "commands.scoreboard.players.set.success.single",
            vec![o.formatted(), holders[0].display.clone(), Tag::Int(value)],
        )
    } else {
        translate(
            "commands.scoreboard.players.set.success.multiple",
            vec![o.formatted(), Tag::Int(count as i32), Tag::Int(value)],
        )
    };
    r.success(msg, true);
    Ok(value.wrapping_mul(holders.len() as i32))
}

fn add(r: &mut Run, remove: bool) -> Result<i32> {
    let holders = holders(r.ctx, r.c, &r.src, "targets")?;
    let o = writable(r, "objective")?;
    let amount = r.int("score").unwrap_or(0);
    let delta = if remove {
        amount.wrapping_neg()
    } else {
        amount
    };
    let mut packets = Vec::new();
    let mut total = 0i32;
    let mut last = 0;
    {
        let mut board = lock(&r.ctx.scoreboard);
        for h in &holders {
            packets.extend(change(&mut board, &h.name, &o.name, |s| {
                s.value = s.value.wrapping_add(delta);
                last = s.value;
            }));
            total = total.wrapping_add(last);
        }
    }
    send(r.out, packets);
    let key = if remove { "remove" } else { "add" };
    let msg = single_or_many(
        holders.len(),
        &format!("commands.scoreboard.players.{key}.success"),
        vec![
            Tag::Int(amount),
            o.formatted(),
            holders[0].display.clone(),
            Tag::Int(last),
        ],
        vec![
            Tag::Int(amount),
            o.formatted(),
            Tag::Int(holders.len() as i32),
        ],
    );
    r.success(msg, true);
    Ok(total)
}

fn get(r: &mut Run) -> Result<i32> {
    let h = holder(r, "target")?;
    let o = objective(r, "objective")?;
    let score = lock(&r.ctx.scoreboard).score(&h.name, &o.name).cloned();
    let Some(score) = score else {
        return Err(err(
            "commands.scoreboard.players.get.null",
            vec![Tag::from(o.name.as_str()), h.display],
        ));
    };
    r.success(
        translate(
            "commands.scoreboard.players.get.success",
            vec![h.display, Tag::Int(score.value), o.formatted()],
        ),
        false,
    );
    Ok(score.value)
}

fn list_holders(r: &mut Run) -> Result<i32> {
    let names = lock(&r.ctx.scoreboard).holders();
    let n = names.len() as i32;
    let msg = if names.is_empty() {
        translate("commands.scoreboard.players.list.empty", Vec::new())
    } else {
        let shown = names.iter().map(|n| Tag::from(n.as_str())).collect();
        translate(
            "commands.scoreboard.players.list.success",
            vec![Tag::Int(n), super::scores::joined(shown)],
        )
    };
    r.success(msg, false);
    Ok(n)
}

fn list_scores(r: &mut Run) -> Result<i32> {
    let h = holder(r, "target")?;
    let (scores, objectives) = {
        let board = lock(&r.ctx.scoreboard);
        let scores = board.scores_of(&h.name);
        let objectives: Vec<Objective> = scores
            .iter()
            .filter_map(|(o, _)| board.objectives.get(o).cloned())
            .collect();
        (scores, objectives)
    };
    if scores.is_empty() {
        r.success(
            translate(
                "commands.scoreboard.players.list.entity.empty",
                vec![h.display],
            ),
            false,
        );
        return Ok(0);
    }
    r.success(
        translate(
            "commands.scoreboard.players.list.entity.success",
            vec![h.display, Tag::Int(scores.len() as i32)],
        ),
        false,
    );
    for ((_, s), o) in scores.iter().zip(&objectives) {
        r.success(
            translate(
                "commands.scoreboard.players.list.entity.entry",
                vec![o.formatted(), Tag::Int(s.value)],
            ),
            false,
        );
    }
    Ok(scores.len() as i32)
}

fn reset(r: &mut Run, specific: bool) -> Result<i32> {
    let holders = holders(r.ctx, r.c, &r.src, "targets")?;
    let o = if specific {
        Some(objective(r, "objective")?)
    } else {
        None
    };
    let mut packets = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for h in &holders {
            match &o {
                Some(o) => {
                    let shown = board.displayed(&o.name);
                    if board.remove_score(&h.name, &o.name) {
                        // A holder's last score gone, they are forgotten.
                        if board.scores_of(&h.name).is_empty() {
                            packets.push(Packet::Reset(ResetScore {
                                holder: h.name.clone(),
                                objective: None,
                            }));
                        } else if shown {
                            packets.push(Packet::Reset(ResetScore {
                                holder: h.name.clone(),
                                objective: Some(o.name.clone()),
                            }));
                        }
                    }
                }
                None => {
                    if !board.scores_of(&h.name).is_empty() {
                        packets.push(Packet::Reset(ResetScore {
                            holder: h.name.clone(),
                            objective: None,
                        }));
                    }
                    board.reset(&h.name);
                }
            }
        }
    }
    send(r.out, packets);
    let n = holders.len();
    let msg = match &o {
        Some(o) => single_or_many(
            n,
            "commands.scoreboard.players.reset.specific",
            vec![o.formatted(), holders[0].display.clone()],
            vec![o.formatted(), Tag::Int(n as i32)],
        ),
        None => single_or_many(
            n,
            "commands.scoreboard.players.reset.all",
            vec![holders[0].display.clone()],
            vec![Tag::Int(n as i32)],
        ),
    };
    r.success(msg, true);
    Ok(n as i32)
}

fn enable(r: &mut Run) -> Result<i32> {
    let holders = holders(r.ctx, r.c, &r.src, "targets")?;
    let o = objective(r, "objective")?;
    if o.criteria != "trigger" {
        return Err(err(
            "commands.scoreboard.players.enable.invalid",
            Vec::new(),
        ));
    }
    let mut count = 0;
    let mut packets = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for h in &holders {
            let fresh = board.score(&h.name, &o.name).is_none();
            let s = board.score_mut(&h.name, &o.name);
            if s.locked {
                s.locked = false;
                count += 1;
            }
            if fresh {
                packets.extend(board.score_packet(&h.name, &o.name));
            }
        }
    }
    send(r.out, packets);
    if count == 0 {
        return Err(err("commands.scoreboard.players.enable.failed", Vec::new()));
    }
    let msg = single_or_many(
        holders.len(),
        "commands.scoreboard.players.enable.success",
        vec![o.formatted(), holders[0].display.clone()],
        vec![o.formatted(), Tag::Int(holders.len() as i32)],
    );
    r.success(msg, true);
    Ok(count)
}

fn apply(op: &str, a: i32, b: i32) -> Result<(i32, i32)> {
    Ok(match op {
        "+=" => (a.wrapping_add(b), b),
        "-=" => (a.wrapping_sub(b), b),
        "*=" => (a.wrapping_mul(b), b),
        "/=" | "%=" if b == 0 => return Err(err("arguments.operation.div0", Vec::new())),
        "/=" => {
            let q = a.wrapping_div(b);
            let floor = a.wrapping_rem(b) != 0 && (a < 0) != (b < 0);
            (q - i32::from(floor), b)
        }
        "%=" => {
            let m = a.wrapping_rem(b);
            (
                if m != 0 && (m < 0) != (b < 0) {
                    m + b
                } else {
                    m
                },
                b,
            )
        }
        "=" => (b, b),
        "<" => (a.min(b), b),
        ">" => (a.max(b), b),
        "><" => (b, a),
        _ => return Err(err("arguments.operation.invalid", Vec::new())),
    })
}

fn operation(r: &mut Run) -> Result<i32> {
    let targets = holders(r.ctx, r.c, &r.src, "targets")?;
    let to = writable(r, "targetObjective")?;
    let op = r.string("operation").unwrap_or_default();
    let sources = holders(r.ctx, r.c, &r.src, "source")?;
    let from = objective(r, "sourceObjective")?;
    let mut packets = Vec::new();
    let mut total = 0i32;
    let mut last = 0;
    {
        let mut board = lock(&r.ctx.scoreboard);
        for t in &targets {
            for s in &sources {
                // Reading the source makes its score, as in vanilla.
                board.score_mut(&s.name, &from.name);
                let a = board.score(&t.name, &to.name).map_or(0, |x| x.value);
                let b = board.score(&s.name, &from.name).map_or(0, |x| x.value);
                let (na, nb) = apply(&op, a, b)?;
                packets.extend(change(&mut board, &t.name, &to.name, |x| x.value = na));
                if op == "><" && (s.name != t.name || from.name != to.name) {
                    packets.extend(change(&mut board, &s.name, &from.name, |x| x.value = nb));
                }
                last = na;
            }
            total = total.wrapping_add(last);
        }
    }
    send(r.out, packets);
    let msg = single_or_many(
        targets.len(),
        "commands.scoreboard.players.operation.success",
        vec![to.formatted(), targets[0].display.clone(), Tag::Int(last)],
        vec![to.formatted(), Tag::Int(targets.len() as i32)],
    );
    r.success(msg, true);
    Ok(total)
}

fn display_name(r: &mut Run, set: bool) -> Result<i32> {
    let holders = holders(r.ctx, r.c, &r.src, "targets")?;
    let o = objective(r, "objective")?;
    let name = if set { r.nbt("name") } else { None };
    let mut packets = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for h in &holders {
            packets.extend(change(&mut board, &h.name, &o.name, |s| {
                s.display = name.clone()
            }));
        }
    }
    send(r.out, packets);
    let n = holders.len();
    let msg = match name {
        Some(name) => single_or_many(
            n,
            "commands.scoreboard.players.display.name.set.success",
            vec![name.clone(), holders[0].display.clone(), o.formatted()],
            vec![name, Tag::Int(n as i32), o.formatted()],
        ),
        None => single_or_many(
            n,
            "commands.scoreboard.players.display.name.clear.success",
            vec![holders[0].display.clone(), o.formatted()],
            vec![Tag::Int(n as i32), o.formatted()],
        ),
    };
    r.success(msg, true);
    Ok(n as i32)
}

fn display_format(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let holders = holders(r.ctx, r.c, &r.src, "targets")?;
    let o = objective(r, "objective")?;
    let format = number_format(r, rest);
    let mut packets = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for h in &holders {
            board.score_mut(&h.name, &o.name).format = format.clone();
            packets.extend(board.score_packet(&h.name, &o.name));
        }
    }
    send(r.out, packets);
    let n = holders.len();
    let how = if format.is_some() { "set" } else { "clear" };
    let msg = single_or_many(
        n,
        &format!("commands.scoreboard.players.display.numberFormat.{how}.success"),
        vec![holders[0].display.clone(), o.formatted()],
        vec![Tag::Int(n as i32), o.formatted()],
    );
    r.success(msg, true);
    Ok(n as i32)
}

fn trigger(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let o = objective(r, "objective")?;
    if o.criteria != "trigger" {
        return Err(err("commands.trigger.failed.invalid", Vec::new()));
    }
    let name = r.src.name.clone();
    let value = r.int("value").unwrap_or(1);
    let mut board = lock(&r.ctx.scoreboard);
    if board.score(&name, &o.name).is_none_or(|s| s.locked) {
        return Err(err("commands.trigger.failed.unprimed", Vec::new()));
    }
    let (key, args) = match rest {
        ["set", ..] => ("set", vec![o.formatted(), Tag::Int(value)]),
        ["add", ..] => ("add", vec![o.formatted(), Tag::Int(value)]),
        _ => ("simple", vec![o.formatted()]),
    };
    let packet = change(&mut board, &name, &o.name, |s| {
        s.value = if key == "set" {
            value
        } else {
            s.value.wrapping_add(value)
        };
        s.locked = true;
    });
    let now = board.score(&name, &o.name).map_or(0, |s| s.value);
    drop(board);
    send(r.out, packet.into_iter().collect());
    r.success(
        translate(&format!("commands.trigger.{key}.success"), args),
        false,
    );
    Ok(now)
}

// Execute.

/// `execute if|unless score ...`.
pub fn condition(ctx: &ServerContext, c: &Context, s: &Src, rest: &[&str]) -> Option<Result<bool>> {
    let ["score", "target", "targetObjective", test, ..] = rest else {
        return None;
    };
    Some((|| {
        let target = single(ctx, c, s, "target")?;
        let board_objective = |arg: &str| -> Result<Objective> {
            let name = match c.arg(arg) {
                Some(Arg::Str(n)) => n.clone(),
                _ => String::new(),
            };
            objective_named(&lock(&ctx.scoreboard), &name)
        };
        let to = board_objective("targetObjective")?;
        if *test == "matches" {
            let Some(Arg::Range(range)) = c.arg("range") else {
                return Ok(false);
            };
            let board = lock(&ctx.scoreboard);
            return Ok(board
                .score(&target.name, &to.name)
                .is_some_and(|sc| range.contains(f64::from(sc.value))));
        }
        let source = single(ctx, c, s, "source")?;
        let from = board_objective("sourceObjective")?;
        let board = lock(&ctx.scoreboard);
        let (Some(a), Some(b)) = (
            board.score(&target.name, &to.name),
            board.score(&source.name, &from.name),
        ) else {
            return Ok(false);
        };
        let (a, b) = (a.value, b.value);
        Ok(match *test {
            "<" => a < b,
            "<=" => a <= b,
            "=" => a == b,
            ">" => a > b,
            _ => a >= b,
        })
    })())
}

fn single(ctx: &ServerContext, c: &Context, s: &Src, arg: &str) -> Result<Holder> {
    if matches!(c.arg(arg), Some(Arg::Str(n)) if n == "*") {
        return Err(err("argument.scoreHolder.empty", Vec::new()));
    }
    Ok(holders(ctx, c, s, arg)?.remove(0))
}

/// `execute store result|success score <targets> <objective>`.
pub fn modify(
    ctx: &ServerContext,
    c: &Context,
    s: &Src,
    path: &[&str],
) -> Option<Result<Vec<Src>>> {
    let [
        "execute",
        "store",
        kind @ ("result" | "success"),
        "score",
        "targets",
        "objective",
    ] = path
    else {
        return None;
    };
    Some((|| {
        let holders = holders(ctx, c, s, "targets")?;
        let name = match c.arg("objective") {
            Some(Arg::Str(n)) => n.clone(),
            _ => String::new(),
        };
        let o = objective_named(&lock(&ctx.scoreboard), &name)?;
        if read_only(&o.criteria) {
            return Err(err(
                "arguments.objective.readonly",
                vec![Tag::from(name.as_str())],
            ));
        }
        let mut n = s.clone();
        n.stores.push(Store {
            success: *kind == "success",
            target: StoreTarget::Score {
                holders: holders.into_iter().map(|h| h.name).collect(),
                objective: name,
            },
        });
        Ok(vec![n])
    })())
}

/// Sets a holder's scores in every objective counting one of `criteria`
/// (as the game does when what they count changes).
pub fn criteria(ctx: &ServerContext, holder: &str, values: &[(&str, i32)], out: &mut Output) {
    let mut packets = Vec::new();
    {
        let mut board = lock(&ctx.scoreboard);
        for (criteria, value) in values {
            let names: Vec<String> = board
                .objectives
                .values()
                .filter(|o| o.criteria == *criteria)
                .map(|o| o.name.clone())
                .collect();
            for name in names {
                packets.extend(change(&mut board, holder, &name, |s| s.value = *value));
            }
        }
    }
    send(out, packets);
}

/// A player's statistic went up by `n`: the score of each objective
/// counting it (`minecraft.mined:minecraft.stone`, `deathCount`...) goes up
/// with it, for everyone to see.
pub fn stat_scores(ctx: &ServerContext, uuid: u128, kind: &str, id: &str, n: i32) {
    if lock(&ctx.scoreboard).objectives.is_empty() {
        return;
    }
    let dots = |s: &str| s.replacen(':', ".", 1);
    let mut criteria = vec![format!("{}:{}", dots(kind), dots(id))];
    if kind == crate::stats::CUSTOM {
        match id {
            "minecraft:deaths" => criteria.push("deathCount".into()),
            "minecraft:player_kills" => {
                criteria.push("playerKillCount".into());
                criteria.push("totalKillCount".into());
            }
            "minecraft:mob_kills" => criteria.push("totalKillCount".into()),
            _ => {}
        }
    }
    let Some(holder) = lock(&ctx.players)
        .get(&uuid)
        .map(|p| p.profile.name.clone())
    else {
        return;
    };
    let mut packets = Vec::new();
    {
        let mut board = lock(&ctx.scoreboard);
        let names: Vec<String> = board
            .objectives
            .values()
            .filter(|o| criteria.contains(&o.criteria))
            .map(|o| o.name.clone())
            .collect();
        for name in names {
            packets.extend(change(&mut board, &holder, &name, |s| {
                s.value = s.value.saturating_add(n);
            }));
        }
    }
    for p in packets {
        let _ = ctx.events.send(Event::Scoreboard(p));
    }
}

/// Puts a stored result into scores.
pub fn store(ctx: &ServerContext, store: &Store, value: i32, out: &mut Output) {
    let StoreTarget::Score { holders, objective } = &store.target else {
        return;
    };
    let mut packets = Vec::new();
    {
        let mut board = lock(&ctx.scoreboard);
        if !board.objectives.contains_key(objective) {
            return;
        }
        for h in holders {
            packets.extend(change(&mut board, h, objective, |s| s.value = value));
        }
    }
    send(out, packets);
}
