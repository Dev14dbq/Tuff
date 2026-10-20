//! `/team`, with vanilla's messages, results and packets.

use mc_nbt::Tag;
use mc_protocol::text::translate;

use super::run::err;
use super::scoreboard::{Holder, holders};
use super::{Result, Run};
use crate::context::Event;
use crate::scoreboard::{COLLISION, COLORS, Packet, Team, VISIBILITY};
use crate::tick::lock;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["team", "add", "team", ..] => add(r),
        ["team", "remove", "team"] => remove(r),
        ["team", "empty", "team"] => empty(r),
        ["team", "list"] => list(r),
        ["team", "list", "team"] => members(r),
        ["team", "join", "team", ..] => join(r),
        ["team", "leave", "members"] => leave(r),
        ["team", "modify", "team", rest @ ..] => modify(r, rest),
        _ => return None,
    })
}

fn team(r: &Run) -> Result<Team> {
    let name = r.string("team").unwrap_or_default();
    lock(&r.ctx.scoreboard)
        .teams
        .get(&name)
        .cloned()
        .ok_or_else(|| err("team.notFound", vec![Tag::from(name.as_str())]))
}

fn send(r: &mut Run, packets: Vec<Packet>) {
    r.out
        .events
        .extend(packets.into_iter().map(Event::Scoreboard));
}

fn add(r: &mut Run) -> Result<i32> {
    let name = r.string("team").unwrap_or_default();
    let display = r.nbt("displayName");
    let mut board = lock(&r.ctx.scoreboard);
    if board.teams.contains_key(&name) {
        return Err(err("commands.team.add.duplicate", Vec::new()));
    }
    let mut team = Team::new(&name);
    let mut packets = vec![Packet::Team(team.packet(0, Vec::new()))];
    if let Some(d) = display {
        team.display = d;
    }
    packets.push(Packet::Team(team.packet(2, Vec::new())));
    let shown = team.formatted();
    board.add_team(team);
    let n = board.teams.len() as i32;
    drop(board);
    send(r, packets);
    r.success(translate("commands.team.add.success", vec![shown]), true);
    Ok(n)
}

fn remove(r: &mut Run) -> Result<i32> {
    let t = team(r)?;
    let mut board = lock(&r.ctx.scoreboard);
    board.remove_team(&t.name);
    let n = board.teams.len() as i32;
    drop(board);
    send(r, vec![Packet::Team(t.packet(1, Vec::new()))]);
    r.success(
        translate("commands.team.remove.success", vec![t.formatted()]),
        true,
    );
    Ok(n)
}

fn empty(r: &mut Run) -> Result<i32> {
    let t = team(r)?;
    let members: Vec<String> = t.members.keys().into_iter().map(str::to_owned).collect();
    if members.is_empty() {
        return Err(err("commands.team.empty.unchanged", Vec::new()));
    }
    {
        let mut board = lock(&r.ctx.scoreboard);
        for m in &members {
            board.leave(m);
        }
    }
    let n = members.len() as i32;
    let packets = members
        .into_iter()
        .map(|m| Packet::Team(t.packet(4, vec![m])))
        .collect();
    send(r, packets);
    r.success(
        translate(
            "commands.team.empty.success",
            vec![Tag::Int(n), t.formatted()],
        ),
        true,
    );
    Ok(n)
}

fn list(r: &mut Run) -> Result<i32> {
    let shown: Vec<Tag> = {
        let board = lock(&r.ctx.scoreboard);
        board
            .team_names()
            .iter()
            .map(|t| board.teams[t].formatted())
            .collect()
    };
    let n = shown.len() as i32;
    let msg = if shown.is_empty() {
        translate("commands.team.list.teams.empty", Vec::new())
    } else {
        translate(
            "commands.team.list.teams.success",
            vec![Tag::Int(n), super::scores::joined(shown)],
        )
    };
    r.success(msg, false);
    Ok(n)
}

fn members(r: &mut Run) -> Result<i32> {
    let t = team(r)?;
    let mut names: Vec<String> = t.members.keys().into_iter().map(str::to_owned).collect();
    names.sort();
    let n = names.len() as i32;
    let msg = if names.is_empty() {
        translate("commands.team.list.members.empty", vec![t.formatted()])
    } else {
        translate(
            "commands.team.list.members.success",
            vec![
                t.formatted(),
                Tag::Int(n),
                super::scores::green_list(&names),
            ],
        )
    };
    r.success(msg, false);
    Ok(n)
}

/// The members named, or the source.
fn members_arg(r: &Run) -> Result<Vec<Holder>> {
    if r.arg("members").is_some() {
        return holders(r.ctx, r.c, &r.src, "members");
    }
    let name = r.src.name.clone();
    let me = super::target::everyone(r.ctx)
        .into_iter()
        .find(|t| t.is_player() && t.holder() == name);
    Ok(vec![Holder {
        display: me.map_or_else(|| Tag::from(name.as_str()), |t| t.feedback()),
        name,
    }])
}

fn join(r: &mut Run) -> Result<i32> {
    let t = team(r)?;
    let members = members_arg(r)?;
    let mut packets = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for m in &members {
            packets.extend(board.join(&t.name, &m.name));
        }
    }
    send(r, packets);
    // Names show as the team now shows them.
    let first = members_arg(r)
        .ok()
        .and_then(|m| m.into_iter().next())
        .filter(|_| members.len() == 1);
    let n = members.len() as i32;
    let msg = match (members.len(), first) {
        (1, Some(m)) => translate(
            "commands.team.join.success.single",
            vec![redisplay(r, &m), t.formatted()],
        ),
        _ => translate(
            "commands.team.join.success.multiple",
            vec![Tag::Int(n), t.formatted()],
        ),
    };
    r.success(msg, true);
    Ok(n)
}

/// A holder's name shown again, now that their team changed.
fn redisplay(r: &Run, h: &Holder) -> Tag {
    match super::target::everyone(r.ctx)
        .into_iter()
        .find(|t| t.holder() == h.name)
    {
        Some(t) => t.feedback(),
        None => h.display.clone(),
    }
}

fn leave(r: &mut Run) -> Result<i32> {
    let members = holders(r.ctx, r.c, &r.src, "members")?;
    let mut packets = Vec::new();
    let mut left = Vec::new();
    {
        let mut board = lock(&r.ctx.scoreboard);
        for m in &members {
            if let Some(p) = board.leave(&m.name) {
                packets.push(p);
                left.push(m);
            }
        }
    }
    send(r, packets);
    let msg = if left.len() == 1 {
        // Selected players show as they do now, out of the team.
        let shown = match &left[0].display {
            Tag::String(_) => left[0].display.clone(),
            _ => redisplay(r, left[0]),
        };
        translate("commands.team.leave.success.single", vec![shown])
    } else {
        translate(
            "commands.team.leave.success.multiple",
            vec![Tag::Int(left.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(left.len() as i32)
}

fn modify(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let old = team(r)?;
    let mut t = old.clone();
    let unchanged = |what: &str| {
        Err(err(
            &format!("commands.team.option.{what}.unchanged"),
            Vec::new(),
        ))
    };
    let flag = |r: &Run, now: bool, what: &str, key: &str, t: &Team| -> Result<(bool, Tag)> {
        let want = r.boolean("allowed").unwrap_or(true);
        if want == now {
            let how = if want {
                "alreadyEnabled"
            } else {
                "alreadyDisabled"
            };
            return Err(err(
                &format!("commands.team.option.{key}.{how}"),
                Vec::new(),
            ));
        }
        let how = if want { "enabled" } else { "disabled" };
        let _ = what;
        Ok((
            want,
            translate(
                &format!("commands.team.option.{key}.{how}"),
                vec![t.formatted()],
            ),
        ))
    };
    let msg = match rest {
        ["color", "reset"] => {
            if t.color.is_none() {
                return unchanged("color");
            }
            t.color = None;
            translate(
                "commands.team.option.color.clear.success",
                vec![t.formatted()],
            )
        }
        ["color", "value"] => {
            let name = r.string("value").unwrap_or_default();
            let color = COLORS.iter().position(|c| *c == name);
            if t.color == color {
                return unchanged("color");
            }
            t.color = color;
            translate(
                "commands.team.option.color.success",
                vec![t.formatted(), Tag::from(name.as_str())],
            )
        }
        ["displayName", "displayName"] => {
            t.display = r.nbt("displayName").unwrap_or_else(|| Tag::from(""));
            if t.display == old.display {
                return unchanged("name");
            }
            translate("commands.team.option.name.success", vec![t.formatted()])
        }
        ["friendlyFire", "allowed"] => {
            let (v, msg) = flag(r, t.friendly_fire, "", "friendlyfire", &t)?;
            t.friendly_fire = v;
            msg
        }
        ["seeFriendlyInvisibles", "allowed"] => {
            let (v, msg) = flag(r, t.see_invisible, "", "seeFriendlyInvisibles", &t)?;
            t.see_invisible = v;
            msg
        }
        [
            rule @ ("nametagVisibility" | "deathMessageVisibility" | "collisionRule"),
            value,
        ] => {
            let (list, prefix) = if *rule == "collisionRule" {
                (&COLLISION, "team.collision")
            } else {
                (&VISIBILITY, "team.visibility")
            };
            let v = list.iter().position(|n| n == value).unwrap_or(0);
            let slot = match *rule {
                "nametagVisibility" => &mut t.nametag,
                "deathMessageVisibility" => &mut t.death_messages,
                _ => &mut t.collision,
            };
            if *slot == v {
                return unchanged(rule);
            }
            *slot = v;
            translate(
                &format!("commands.team.option.{rule}.success"),
                vec![
                    t.formatted(),
                    translate(&format!("{prefix}.{value}"), Vec::new()),
                ],
            )
        }
        [which @ ("prefix" | "suffix"), _] => {
            let v = r.nbt(which).unwrap_or_else(|| Tag::from(""));
            if *which == "prefix" {
                t.prefix = v.clone();
            } else {
                t.suffix = v.clone();
            }
            translate(&format!("commands.team.option.{which}.success"), vec![v])
        }
        _ => return Err(err("command.failed", Vec::new())),
    };
    let packet = t.packet(2, Vec::new());
    lock(&r.ctx.scoreboard).teams.insert(t.name.clone(), t);
    send(r, vec![Packet::Team(packet)]);
    r.success(msg, true);
    Ok(0)
}
