//! Commands of the `rare` group (see `run::run` for the ones done
//! elsewhere): `/teammsg`, `/random reset`, `/swing`, `/transfer`,
//! `/dialog`, `/posteffect`, `/waypoint`, `/fetchprofile`. Each returns
//! `None` for paths it does not handle.

use std::collections::{BTreeSet, HashMap};

use mc_command::args::{Arg, full_id};
use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{ClearDialog, DisguisedChat, PostEffects, ShowDialog, Transfer};
use mc_protocol::text::{list, translate};

use super::run::{err, source_name};
use super::target::{self, Target, uuid_ints};
use super::{Result, Run, Who};
use crate::context::{Direct, Event};
use crate::tick::lock;

/// What these commands remember while the server runs.
#[derive(Debug, Default)]
pub struct State {
    /// Random sequences that exist (`/random reset`).
    sequences: BTreeSet<String>,
    /// Screen shaders per player.
    post_effects: HashMap<u128, Vec<String>>,
    /// `/attribute`: base values and modifiers set per player.
    pub attributes: HashMap<u128, HashMap<String, AttrState>>,
    /// `/advancement` and `/recipe`.
    pub book: Option<std::sync::Arc<super::progress::Book>>,
    pub progress: super::progress::Progress,
    /// Locator bar settings per entity.
    waypoints: HashMap<u128, Waypoint>,
}

/// What `/attribute` changed on one attribute of a player.
#[derive(Debug, Default, Clone)]
pub struct AttrState {
    pub base: Option<f64>,
    /// (id, amount, operation 0-2).
    pub modifiers: Vec<(String, f64, i32)>,
}

#[derive(Debug, Default, Clone)]
struct Waypoint {
    /// A chat colour name or `#RRGGBB`.
    color: Option<String>,
    style: Option<String>,
}

impl State {
    pub fn sequences(&self) -> Vec<String> {
        self.sequences.iter().cloned().collect()
    }

    /// A random sequence was used; it exists from now on.
    pub fn use_sequence(&mut self, id: &str) {
        self.sequences.insert(id.to_owned());
    }
}

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["teammsg", ..] => teammsg(r),
        ["random", "reset", rest @ ..] => random_reset(r, rest),
        ["swing", ..] => swing(r, path),
        ["transfer", ..] => transfer(r),
        ["dialog", "show", ..] => dialog_show(r),
        ["dialog", "clear", ..] => dialog_clear(r),
        ["posteffect", rest @ ..] => posteffect(r, rest),
        ["waypoint", "list"] => waypoint_list(r),
        ["waypoint", "modify", ..] => waypoint_modify(r, path),
        ["fetchprofile", kind, ..] => fetchprofile(r, kind),
        ["compute", ..] => compute(r, path),
        _ => return None,
    })
}

fn lit(s: &str) -> Tag {
    Tag::from(s)
}

fn n(v: usize) -> Tag {
    Tag::Int(v as i32)
}

/// The player entity id of a target.
fn entity_id(r: &Run, t: &Target) -> i32 {
    match t {
        Target::Player { uuid, .. } => lock(&r.ctx.players).get(uuid).map_or(0, |p| p.entity_id),
        Target::Entity { id, .. } => *id,
    }
}

fn teammsg(r: &mut Run) -> Result<i32> {
    let me = match r.src.who {
        Who::Player(u) | Who::Entity(u) => u,
        Who::Console => return Err(err("permissions.requires.entity", Vec::new())),
    };
    let sender = target::everyone(r.ctx)
        .into_iter()
        .find(|t| t.uuid() == me)
        .ok_or_else(|| err("permissions.requires.entity", Vec::new()))?;
    let text = r.string("message").unwrap_or_default();
    let (team, members) = {
        let board = lock(&r.ctx.scoreboard);
        let team = board
            .team_of(&sender.holder())
            .cloned()
            .ok_or_else(|| err("commands.teammsg.failed.noteam", Vec::new()))?;
        let members: Vec<String> = team.members.keys().into_iter().map(str::to_owned).collect();
        (team, members)
    };
    let name = source_name(r);
    let chat_type = |key: &str| {
        r.ctx
            .registries
            .iter()
            .find(|g| g.registry == "minecraft:chat_type")
            .and_then(|g| g.entries.iter().position(|e| e.id == key))
            .map_or(0, |i| i as i32)
    };
    let incoming = chat_type("minecraft:team_msg_command_incoming");
    let outgoing = chat_type("minecraft:team_msg_command_outgoing");
    let mut sends = Vec::new();
    for p in lock(&r.ctx.players).values() {
        if members.contains(&p.profile.name) {
            let kind = if p.profile.uuid == me {
                outgoing
            } else {
                incoming
            };
            sends.push((p.profile.uuid, kind));
        }
    }
    for (uuid, kind) in &sends {
        let packet = DisguisedChat {
            message: lit(&text),
            chat_type: *kind,
            name: name.clone(),
            target: Some(team.formatted()),
        };
        direct(r, *uuid, Direct::DisguisedChat(packet));
    }
    Ok(sends.len() as i32)
}

fn random_reset(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let mut state = lock(&r.ctx.rare);
    if rest.first() == Some(&"*") {
        let count = state.sequences.len();
        drop(state);
        r.success(
            translate("commands.random.reset.all.success", vec![n(count)]),
            true,
        );
        return Ok(count as i32);
    }
    let id = full_id(&r.string("sequence").unwrap_or_default());
    state.use_sequence(&id);
    drop(state);
    r.success(
        translate("commands.random.reset.success", vec![lit(&id)]),
        true,
    );
    Ok(1)
}

fn swing(r: &mut Run, path: &[&str]) -> Result<i32> {
    let targets = r.targets_or_self("targets", false)?;
    let living: Vec<&Target> = targets.iter().filter(|t| t.is_player()).collect();
    if living.is_empty() {
        return Err(err("commands.swing.failed.notliving", Vec::new()));
    }
    let hand = i32::from(path.contains(&"offhand"));
    let kind = match r.string("animation").as_deref() {
        Some("none") => 0,
        Some("stab") => 2,
        _ => 1,
    };
    let duration = r.int("duration").unwrap_or(6);
    for t in &living {
        let entity_id = entity_id(r, t);
        r.out.events.push(Event::Animate {
            uuid: t.uuid(),
            entity_id,
            hand,
            kind,
            duration,
        });
    }
    let msg = if living.len() == 1 {
        translate("commands.swing.success.single", vec![living[0].display()])
    } else {
        translate("commands.swing.success.multiple", vec![n(living.len())])
    };
    r.success(msg, true);
    Ok(living.len() as i32)
}

fn direct(r: &mut Run, uuid: u128, packet: Direct) {
    r.out.events.push(Event::Direct { uuid, packet });
}

fn transfer(r: &mut Run) -> Result<i32> {
    let host = r.string("hostname").unwrap_or_default();
    let port = r.int("port").unwrap_or(25565);
    let players = if r.arg("players").is_some() {
        r.targets("players", true)?
    } else {
        match r.targets_or_self("players", true) {
            Ok(t) => t,
            Err(_) => return Err(err("commands.transfer.error.no_players", Vec::new())),
        }
    };
    for p in &players {
        let packet = Transfer {
            host: host.clone(),
            port,
        };
        direct(r, p.uuid(), Direct::Transfer(packet));
    }
    let msg = if players.len() == 1 {
        translate(
            "commands.transfer.success.single",
            vec![players[0].display(), lit(&host), Tag::Int(port)],
        )
    } else {
        translate(
            "commands.transfer.success.multiple",
            vec![n(players.len()), lit(&host), Tag::Int(port)],
        )
    };
    r.success(msg, true);
    Ok(players.len() as i32)
}

fn dialog_show(r: &mut Run) -> Result<i32> {
    let players = r.targets("targets", true)?;
    let packet = match r.arg("dialog").cloned() {
        Some(Arg::Id(id)) => {
            let id = full_id(&id);
            let index = r
                .ctx
                .registries
                .iter()
                .find(|g| g.registry == "minecraft:dialog")
                .and_then(|g| g.entries.iter().position(|e| e.id == id))
                .ok_or_else(|| err("command.failed", Vec::new()))?;
            ShowDialog {
                registry_id: Some(index as i32),
                inline: None,
            }
        }
        Some(Arg::Nbt(Tag::Compound(c))) => {
            // The client gets what the codec writes: keys in order.
            let mut keys: Vec<String> = c.iter().map(|(k, _)| k.to_owned()).collect();
            keys.sort();
            let mut sorted = Compound::new();
            for k in keys {
                if let Some(v) = c.get(&k) {
                    sorted.insert(k.as_str(), v.clone());
                }
            }
            ShowDialog {
                registry_id: None,
                inline: Some(Tag::Compound(sorted)),
            }
        }
        _ => return Err(err("command.failed", Vec::new())),
    };
    for p in &players {
        direct(r, p.uuid(), Direct::ShowDialog(packet.clone()));
    }
    let msg = if players.len() == 1 {
        translate("commands.dialog.show.single", vec![players[0].display()])
    } else {
        translate("commands.dialog.show.multiple", vec![n(players.len())])
    };
    r.success(msg, true);
    Ok(players.len() as i32)
}

fn dialog_clear(r: &mut Run) -> Result<i32> {
    let players = r.targets("targets", true)?;
    for p in &players {
        direct(r, p.uuid(), Direct::ClearDialog(ClearDialog));
    }
    let msg = if players.len() == 1 {
        translate("commands.dialog.clear.single", vec![players[0].display()])
    } else {
        translate("commands.dialog.clear.multiple", vec![n(players.len())])
    };
    r.success(msg, true);
    Ok(players.len() as i32)
}

fn one_player(r: &Run, name: &str) -> Result<Target> {
    let mut found = r.targets(name, true)?;
    if found.len() > 1 {
        return Err(err("argument.player.toomany", Vec::new()));
    }
    Ok(found.remove(0))
}

fn posteffect(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let kind = rest[0];
    if kind == "list" {
        let p = one_player(r, "target")?;
        let effects = lock(&r.ctx.rare)
            .post_effects
            .get(&p.uuid())
            .cloned()
            .unwrap_or_default();
        let msg = if effects.is_empty() {
            translate("commands.posteffect.list.empty", vec![p.display()])
        } else {
            translate(
                "commands.posteffect.list.success",
                vec![p.display(), n(effects.len()), lit(&effects.join(", "))],
            )
        };
        r.success(msg, false);
        return Ok(effects.len() as i32);
    }
    let players = r.targets("targets", true)?;
    let id = r.string("posteffect").map(|s| full_id(&s));
    let mut changed = Vec::new();
    for p in &players {
        let mut state = lock(&r.ctx.rare);
        let effects = state.post_effects.entry(p.uuid()).or_default();
        let did = match (kind, &id) {
            ("add", Some(id)) if !effects.contains(id) => {
                effects.push(id.clone());
                true
            }
            ("remove", Some(id)) if effects.contains(id) => {
                effects.retain(|e| e != id);
                true
            }
            ("clear", _) if !effects.is_empty() => {
                effects.clear();
                true
            }
            _ => false,
        };
        if did {
            changed.push((p.clone(), effects.clone()));
        }
    }
    if changed.is_empty() {
        return Err(err(
            &format!("commands.posteffect.{kind}.failed"),
            Vec::new(),
        ));
    }
    for (p, ids) in &changed {
        direct(
            r,
            p.uuid(),
            Direct::PostEffects(PostEffects { ids: ids.clone() }),
        );
    }
    let who = if changed.len() == 1 {
        ("single", changed[0].0.display())
    } else {
        ("multiple", n(changed.len()))
    };
    let mut args = Vec::new();
    if let Some(id) = &id {
        args.push(lit(id));
    }
    args.push(who.1);
    r.success(
        translate(
            &format!("commands.posteffect.{kind}.success.{}", who.0),
            args,
        ),
        true,
    );
    Ok(changed.len() as i32)
}

/// The colour of a chat colour name as `#RRGGBB`.
fn color_rgb(name: &str) -> String {
    let rgb = match name {
        "black" => "000000",
        "dark_blue" => "0000AA",
        "dark_green" => "00AA00",
        "dark_aqua" => "00AAAA",
        "dark_red" => "AA0000",
        "dark_purple" => "AA00AA",
        "gold" => "FFAA00",
        "gray" => "AAAAAA",
        "dark_gray" => "555555",
        "blue" => "5555FF",
        "green" => "55FF55",
        "aqua" => "55FFFF",
        "red" => "FF5555",
        "light_purple" => "FF55FF",
        "yellow" => "FFFF55",
        _ => "FFFFFF",
    };
    format!("#{rgb}")
}

fn waypoint_list(r: &mut Run) -> Result<i32> {
    let dimension = "minecraft:overworld";
    let mut entries = Vec::new();
    {
        let state = lock(&r.ctx.rare);
        for t in target::everyone(r.ctx).iter().filter(|t| t.is_player()) {
            let color = state
                .waypoints
                .get(&t.uuid())
                .and_then(|w| w.color.as_deref())
                .map_or_else(
                    || "#FFFFFF".to_owned(),
                    |c| {
                        if c.starts_with('#') {
                            c.to_owned()
                        } else {
                            color_rgb(c)
                        }
                    },
                );
            let [x, y, z] = t.pos().map(|v| v.floor() as i32);
            let mut click = Compound::new();
            click.insert("action", "suggest_command");
            click.insert(
                "command",
                Tag::String(format!("/execute in {dimension} run tp @s {x} {y} {z}")),
            );
            let mut hover = Compound::new();
            hover.insert("action", "show_text");
            hover.insert("value", translate("chat.coordinates.tooltip", Vec::new()));
            let mut c = Compound::new();
            c.insert("text", Tag::String(t.holder()));
            c.insert("color", Tag::String(color));
            c.insert("click_event", Tag::Compound(click));
            c.insert("hover_event", Tag::Compound(hover));
            c.insert("insertion", Tag::String(t.holder()));
            entries.push(Tag::Compound(c));
        }
    }
    let count = entries.len();
    let msg = if count == 0 {
        translate("commands.waypoint.list.empty", vec![lit(dimension)])
    } else {
        let shown = if count == 1 {
            entries.remove(0)
        } else {
            let mut parts = Vec::new();
            for (i, e) in entries.into_iter().enumerate() {
                if i > 0 {
                    let mut sep = Compound::new();
                    sep.insert("text", ", ");
                    sep.insert("color", "gray");
                    parts.push(Tag::Compound(sep));
                }
                parts.push(e);
            }
            let mut c = Compound::new();
            c.insert("text", "");
            c.insert("extra", list(parts));
            Tag::Compound(c)
        };
        translate(
            "commands.waypoint.list.success",
            vec![n(count), lit(dimension), shown],
        )
    };
    r.success(msg, false);
    Ok(count as i32)
}

fn waypoint_modify(r: &mut Run, path: &[&str]) -> Result<i32> {
    let mut found = r.targets("waypoint", false)?;
    if found.len() > 1 {
        return Err(err("argument.entity.toomany", Vec::new()));
    }
    let t = found.remove(0);
    if !t.is_player() {
        return Err(err("argument.waypoint.invalid", Vec::new()));
    }
    let mut state = lock(&r.ctx.rare);
    let way = state.waypoints.entry(t.uuid()).or_default();
    let msg = match path {
        ["waypoint", "modify", _, "color", "reset"] => {
            way.color = None;
            translate("commands.waypoint.modify.color.reset", Vec::new())
        }
        ["waypoint", "modify", _, "color", "hex", _] => {
            let hex = r.string("color").unwrap_or_default();
            way.color = Some(format!("#{hex}"));
            let mut c = Compound::new();
            c.insert("color", Tag::String(format!("#{hex}")));
            c.insert("text", hex.as_str());
            translate("commands.waypoint.modify.color", vec![Tag::Compound(c)])
        }
        ["waypoint", "modify", _, "color", _] => {
            let name = r.string("color").unwrap_or_default();
            way.color = Some(name.clone());
            let mut c = Compound::new();
            c.insert("color", name.as_str());
            c.insert("text", name.as_str());
            translate("commands.waypoint.modify.color", vec![Tag::Compound(c)])
        }
        ["waypoint", "modify", _, "style", "set", _] => {
            way.style = r.string("style");
            translate("commands.waypoint.modify.style", Vec::new())
        }
        _ => {
            way.style = None;
            translate("commands.waypoint.modify.style", Vec::new())
        }
    };
    drop(state);
    r.success(msg, true);
    Ok(1)
}

/// A clickable `[label]` that copies or runs something.
fn button(label: &str, action: &str, key: &str, value: &str, with: Option<Tag>) -> Tag {
    let mut click = Compound::new();
    click.insert("action", action);
    click.insert(key, value);
    let mut c = Compound::new();
    c.insert("translate", label);
    c.insert("color", "green");
    c.insert("click_event", Tag::Compound(click));
    if let Some(w) = with {
        c.insert("with", list(vec![w]));
    }
    translate("chat.square_brackets", vec![Tag::Compound(c)])
}

fn profile_text(uuid: u128, name: &str) -> (String, Tag) {
    let ints = uuid_ints(uuid);
    let id = format!("[I;{},{},{},{}]", ints[0], ints[1], ints[2], ints[3]);
    let snbt = format!("{{id:{id},name:\"{name}\"}}");
    let mut player = Compound::new();
    player.insert("id", Tag::IntArray(ints));
    player.insert("name", name);
    let mut shown = Compound::new();
    shown.insert("color", "white");
    shown.insert("player", Tag::Compound(player));
    (snbt, Tag::Compound(shown))
}

/// The line of buttons after a resolved profile.
fn profile_buttons(uuid: u128, name: &str) -> Tag {
    let (snbt, shown) = profile_text(uuid, name);
    let space = |s: &str| {
        let mut c = Compound::new();
        c.insert("", s);
        Tag::Compound(c)
    };
    let parts = vec![
        button(
            "commands.fetchprofile.copy_component",
            "copy_to_clipboard",
            "value",
            &snbt,
            None,
        ),
        space(" "),
        button(
            "commands.fetchprofile.give_item",
            "run_command",
            "command",
            &format!("give @s minecraft:player_head[profile={snbt}]"),
            None,
        ),
        space(" "),
        button(
            "commands.fetchprofile.summon_mannequin",
            "run_command",
            "command",
            &format!("summon minecraft:mannequin ~ ~ ~ {{profile:{snbt}}}"),
            None,
        ),
        space(" "),
        button(
            "commands.fetchprofile.copy_text",
            "copy_to_clipboard",
            "value",
            &format!("{{player:{snbt}}}"),
            Some(shown),
        ),
    ];
    let mut c = Compound::new();
    c.insert("text", "");
    c.insert("extra", list(parts));
    Tag::Compound(c)
}

fn fetchprofile(r: &mut Run, kind: &str) -> Result<i32> {
    match kind {
        "entity" => {
            let mut found = r.targets("entity", false)?;
            if found.len() > 1 {
                return Err(err("argument.entity.toomany", Vec::new()));
            }
            let t = found.remove(0);
            if !t.is_player() {
                return Err(err("commands.fetchprofile.no_profile", vec![t.display()]));
            }
            let msg = translate(
                "commands.fetchprofile.entity.success",
                vec![t.display(), profile_buttons(t.uuid(), &t.holder())],
            );
            r.success(msg, false);
            Ok(1)
        }
        _ => {
            // There is no profile service to ask: only players who are
            // online resolve.
            let (key, query) = if kind == "id" {
                ("id", r.string("id").unwrap_or_default())
            } else {
                ("name", r.string("name").unwrap_or_default())
            };
            let found = lock(&r.ctx.players)
                .values()
                .find(|p| {
                    if key == "id" {
                        crate::util::uuid_string(p.profile.uuid) == query.to_lowercase()
                    } else {
                        p.profile.name.eq_ignore_ascii_case(&query)
                    }
                })
                .map(|p| (p.profile.uuid, p.profile.name.clone()));
            let Some((uuid, name)) = found else {
                return Err(err(
                    &format!("commands.fetchprofile.{key}.failure"),
                    vec![lit(&query)],
                ));
            };
            let msg = translate(
                &format!("commands.fetchprofile.{key}.success"),
                vec![lit(&query), profile_buttons(uuid, &name)],
            );
            r.success(msg, false);
            Ok(1)
        }
    }
}

/// `/compute`: what a provider of the data pack comes to.
fn compute(r: &mut Run, path: &[&str]) -> Result<i32> {
    let block = match path[1] {
        "block" => {
            let p = r.loaded_pos("computePos")?;
            let state = r.ctx.world.block(p, &r.ctx.data.blocks).unwrap_or(0);
            r.ctx.data.blocks.block_name(state).unwrap_or("").to_owned()
        }
        "entity" => {
            r.targets("computeTarget", false)?;
            String::new()
        }
        _ => String::new(),
    };
    let float = path.contains(&"float");
    let registry = if float {
        "minecraft:context_float_provider"
    } else {
        "minecraft:context_int_provider"
    };
    let (value, named) = match r.arg("provider") {
        Some(Arg::Id(id)) => (serde_json::Value::String(full_id(id)), Some(full_id(id))),
        Some(Arg::Nbt(t)) => (mc_command::text::to_json(t), None),
        _ => return Err(err("command.failed", Vec::new())),
    };
    let kind = if named.is_some() { "named" } else { "unnamed" };
    let evaluated = match super::provider::eval(&resolve_named(r, registry, &value, 0), float) {
        Ok(super::provider::Num::Int(i)) => Some(f64::from(i)),
        Ok(super::provider::Num::Float(f)) => Some(f64::from(f)),
        Err(super::provider::Failure::Invalid(why)) => {
            return Err(err(
                &format!("command.compute.result.{kind}.invalid"),
                vec![Tag::from(why.as_str())],
            ));
        }
        Err(super::provider::Failure::Unknown) => {
            crate::furnaces::provider(&r.ctx.data, registry, &value, &block, 0)
        }
    };
    let Some(v) = evaluated else {
        return Err(err(
            &format!("command.compute.result.{kind}.invalid"),
            vec![Tag::from("unsupported provider")],
        ));
    };
    let scale = match r.arg("scale") {
        Some(Arg::Double(s)) => *s,
        Some(Arg::Float(s)) => f64::from(*s),
        _ => 1.0,
    };
    let (shown, result) = if float {
        let f = f64::from(v as f32);
        (f, (f * scale).trunc() as i32)
    } else {
        (v, v as i32)
    };
    let mut args = Vec::new();
    if let Some(id) = &named {
        args.push(Tag::from(id.as_str()));
    }
    let exact = shown.fract() == 0.0;
    let key = if exact { "exact" } else { "rounded" };
    if exact && float {
        args.push(Tag::Int(result));
    } else if exact {
        args.push(Tag::Int(shown as i32));
    } else {
        args.push(Tag::Double(shown));
        args.push(Tag::Int(result));
    }
    r.success(
        translate(&format!("command.compute.result.{kind}.{key}"), args),
        false,
    );
    Ok(result)
}

/// A provider with the data pack entries it names written in.
fn resolve_named(r: &Run, registry: &str, v: &serde_json::Value, depth: u32) -> serde_json::Value {
    use serde_json::Value;
    if depth > 16 {
        return v.clone();
    }
    match v {
        Value::String(id) => match r.ctx.data.data_pack().read_entry(registry, id) {
            Ok(entry) => resolve_named(r, registry, &entry, depth + 1),
            Err(_) => v.clone(),
        },
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, e)| (k.clone(), resolve_named(r, registry, e, depth + 1)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|e| resolve_named(r, registry, e, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}
