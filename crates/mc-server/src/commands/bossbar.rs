//! `/bossbar` and `execute store ... bossbar`, with vanilla's messages and
//! results.

use mc_command::dispatch::Context;
use mc_nbt::Tag;
use mc_protocol::packets::play::{BossAction, BossEvent};
use mc_protocol::text::translate;

use super::run::err;
use super::target::player_name;
use super::{Result, Run, Src, Store, StoreTarget};
use crate::bossbars::{Bar, COLORS, STYLES};
use crate::context::{Direct, Event, ServerContext};

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["bossbar", "add", "id", "name"] => add(r),
        ["bossbar", "remove", "id"] => remove(r),
        ["bossbar", "list"] => list(r),
        ["bossbar", "get", "id", what] => get(r, what),
        ["bossbar", "set", "id", rest @ ..] => set(r, rest),
        _ => return None,
    })
}

/// `execute store result|success bossbar <id> value|max`.
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
        "bossbar",
        "id",
        which,
    ] = path
    else {
        return None;
    };
    let id = id_of(c.arg("id"));
    if ctx
        .bossbars
        .lock()
        .expect("bossbars lock")
        .get(&id)
        .is_none()
    {
        return Some(Err(unknown(&id)));
    }
    let mut n = s.clone();
    n.stores.push(Store {
        success: *kind == "success",
        target: StoreTarget::Bossbar {
            id,
            max: *which == "max",
        },
    });
    Some(Ok(vec![n]))
}

/// Puts a stored result into a boss bar.
pub fn store(ctx: &ServerContext, store: &Store, value: i32) {
    let StoreTarget::Bossbar { id, max } = &store.target else {
        return;
    };
    let mut bars = ctx.bossbars.lock().expect("bossbars lock");
    let Some(bar) = bars.get_mut(id) else {
        return;
    };
    let before = bar.progress();
    if *max {
        bar.max = value;
    } else {
        bar.value = value;
    }
    if bar.progress().to_bits() != before.to_bits() {
        let packet = bar.packet(BossAction::Progress(bar.progress()));
        for uuid in seen_by(ctx, bar) {
            let _ = ctx.events.send(Event::Direct {
                uuid,
                packet: Direct::Boss(packet.clone()),
            });
        }
    }
}

fn id_of(arg: Option<&mc_command::args::Arg>) -> String {
    let id = match arg {
        Some(mc_command::args::Arg::Id(s) | mc_command::args::Arg::Str(s)) => s.as_str(),
        _ => "",
    };
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn unknown(id: &str) -> mc_command::CommandError {
    err("commands.bossbar.unknown", vec![Tag::from(id)])
}

/// The online players who see the bar (none while it is hidden).
fn seen_by(ctx: &ServerContext, bar: &Bar) -> Vec<u128> {
    if !bar.visible {
        return Vec::new();
    }
    online(ctx, bar)
}

fn online(ctx: &ServerContext, bar: &Bar) -> Vec<u128> {
    let players = ctx.players.lock().expect("players lock");
    bar.players
        .iter()
        .copied()
        .filter(|u| players.contains_key(u))
        .collect()
}

fn send(r: &mut Run, to: &[u128], packet: &BossEvent) {
    for &uuid in to {
        super::run::direct(r, uuid, Direct::Boss(packet.clone()));
    }
}

fn add(r: &mut Run) -> Result<i32> {
    let id = id_of(r.arg("id"));
    let name = r.nbt("name").unwrap_or_else(|| Tag::from(""));
    let mut bars = r.ctx.bossbars.lock().expect("bossbars lock");
    if bars.get(&id).is_some() {
        return Err(err("commands.bossbar.create.failed", vec![Tag::String(id)]));
    }
    let bar = bars.create(&id, name);
    let shown = bar.display();
    bars.insert(bar);
    let count = bars.list().len() as i32;
    drop(bars);
    r.success(
        translate("commands.bossbar.create.success", vec![shown]),
        true,
    );
    Ok(count)
}

fn remove(r: &mut Run) -> Result<i32> {
    let id = id_of(r.arg("id"));
    let mut bars = r.ctx.bossbars.lock().expect("bossbars lock");
    let Some(bar) = bars.remove(&id) else {
        return Err(unknown(&id));
    };
    let count = bars.list().len() as i32;
    drop(bars);
    let to = seen_by(r.ctx, &bar);
    send(r, &to, &bar.packet(BossAction::Remove));
    r.success(
        translate("commands.bossbar.remove.success", vec![bar.display()]),
        true,
    );
    Ok(count)
}

fn list(r: &mut Run) -> Result<i32> {
    let shown: Vec<Tag> = {
        let bars = r.ctx.bossbars.lock().expect("bossbars lock");
        bars.list().iter().map(|b| b.display()).collect()
    };
    let n = shown.len() as i32;
    let msg = if shown.is_empty() {
        translate("commands.bossbar.list.bars.none", Vec::new())
    } else {
        translate(
            "commands.bossbar.list.bars.some",
            vec![Tag::Int(n), super::scores::joined(shown)],
        )
    };
    r.success(msg, false);
    Ok(n)
}

/// The online players of a bar as commands list them, and how many.
fn player_list(r: &Run, bar: &Bar) -> (i32, Tag) {
    let players = r.ctx.players.lock().expect("players lock");
    let names: Vec<Tag> = bar
        .players
        .iter()
        .filter_map(|u| players.get(u).map(|p| player_name(&p.profile.name, *u)))
        .collect();
    (names.len() as i32, super::scores::joined(names))
}

fn get(r: &mut Run, what: &str) -> Result<i32> {
    let id = id_of(r.arg("id"));
    let Some(bar) = r
        .ctx
        .bossbars
        .lock()
        .expect("bossbars lock")
        .get(&id)
        .cloned()
    else {
        return Err(unknown(&id));
    };
    let shown = bar.display();
    let (msg, result) = match what {
        "value" => (
            translate(
                "commands.bossbar.get.value",
                vec![shown, Tag::Int(bar.value)],
            ),
            bar.value,
        ),
        "max" => (
            translate("commands.bossbar.get.max", vec![shown, Tag::Int(bar.max)]),
            bar.max,
        ),
        "visible" => {
            let key = if bar.visible { "visible" } else { "hidden" };
            (
                translate(&format!("commands.bossbar.get.visible.{key}"), vec![shown]),
                i32::from(bar.visible),
            )
        }
        _ => {
            let (n, names) = player_list(r, &bar);
            let msg = if n == 0 {
                translate("commands.bossbar.get.players.none", vec![shown])
            } else {
                translate(
                    "commands.bossbar.get.players.some",
                    vec![shown, Tag::Int(n), names],
                )
            };
            (msg, n)
        }
    };
    r.success(msg, false);
    Ok(result)
}

fn set(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let id = id_of(r.arg("id"));
    let Some(old) = r
        .ctx
        .bossbars
        .lock()
        .expect("bossbars lock")
        .get(&id)
        .cloned()
    else {
        return Err(unknown(&id));
    };
    let unchanged = |what: &str| {
        Err(err(
            &format!("commands.bossbar.set.{what}.unchanged"),
            Vec::new(),
        ))
    };
    let mut bar = old.clone();
    let mut packets = Vec::new();
    let mut result = 0;
    let key;
    let mut args = Vec::new();
    match rest {
        ["value", "value"] | ["max", "max"] => {
            let v = r.int(rest[1]).unwrap_or(0);
            let max = rest[0] == "max";
            let slot = if max { &mut bar.max } else { &mut bar.value };
            if *slot == v {
                return unchanged(rest[0]);
            }
            *slot = v;
            if bar.progress().to_bits() != old.progress().to_bits() {
                packets.push(BossAction::Progress(bar.progress()));
            }
            key = format!("{}.success", rest[0]);
            args.push(Tag::Int(v));
            result = v;
        }
        ["color", color] => {
            bar.color = COLORS.iter().position(|c| c.0 == *color).unwrap_or(6);
            if bar.color == old.color {
                return unchanged("color");
            }
            packets.push(style(&bar));
            key = "color.success".into();
        }
        ["style", s] => {
            bar.style = STYLES.iter().position(|n| n == s).unwrap_or(0);
            if bar.style == old.style {
                return unchanged("style");
            }
            packets.push(style(&bar));
            key = "style.success".into();
        }
        ["name", "name"] => {
            bar.name = r.nbt("name").unwrap_or_else(|| Tag::from(""));
            if bar.name == old.name {
                return unchanged("name");
            }
            packets.push(BossAction::Name(bar.name.clone()));
            key = "name.success".into();
        }
        ["visible", "visible"] => {
            bar.visible = r.boolean("visible").unwrap_or(true);
            let how = if bar.visible { "visible" } else { "hidden" };
            if bar.visible == old.visible {
                return Err(err(
                    &format!("commands.bossbar.set.visibility.unchanged.{how}"),
                    Vec::new(),
                ));
            }
            key = format!("visible.success.{how}");
        }
        ["players"] | ["players", "targets"] => {
            let targets = if rest.len() == 2 {
                r.targets("targets", true)?
            } else {
                Vec::new()
            };
            let mut wanted: Vec<u128> = Vec::new();
            for t in &targets {
                if !wanted.contains(&t.uuid()) {
                    wanted.push(t.uuid());
                }
            }
            let now = online(r.ctx, &old);
            if now.len() == wanted.len() && now.iter().all(|u| wanted.contains(u)) {
                return unchanged("players");
            }
            if old.visible {
                let gone: Vec<u128> = now
                    .iter()
                    .copied()
                    .filter(|u| !wanted.contains(u))
                    .collect();
                send(r, &gone, &old.packet(BossAction::Remove));
                let new: Vec<u128> = wanted
                    .iter()
                    .copied()
                    .filter(|u| !now.contains(u))
                    .collect();
                send(r, &new, &old.add_packet());
            }
            bar.players = wanted;
            let (n, names) = player_list(r, &bar);
            if n == 0 {
                key = "players.success.none".into();
            } else {
                key = "players.success.some".into();
                args.push(Tag::Int(n));
                args.push(names);
            }
            result = n;
        }
        _ => return Err(err("command.failed", Vec::new())),
    }
    if bar.visible != old.visible {
        let to = online(r.ctx, &bar);
        let packet = if bar.visible {
            bar.add_packet()
        } else {
            bar.packet(BossAction::Remove)
        };
        send(r, &to, &packet);
    } else {
        let to = seen_by(r.ctx, &bar);
        for p in packets {
            send(r, &to, &bar.packet(p));
        }
    }
    args.insert(0, bar.display());
    if let Some(b) = r.ctx.bossbars.lock().expect("bossbars lock").get_mut(&id) {
        *b = bar;
    }
    r.success(
        translate(&format!("commands.bossbar.set.{key}"), args),
        true,
    );
    Ok(result)
}

fn style(bar: &Bar) -> BossAction {
    BossAction::Style {
        color: bar.color as i32,
        overlay: bar.style as i32,
    }
}
