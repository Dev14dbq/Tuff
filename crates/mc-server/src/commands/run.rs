//! What each command does, by the path of names to its executable node
//! (`time set time`), with the messages vanilla sends.

use mc_command::CommandError;
use mc_command::args::{Arg, Coord, Pos};
use mc_command::dispatch::Context;
use mc_command::text::{int, translate};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::{ItemStack, TickingState, TickingStep};

use super::target::{self, Target, player_name};
use super::world::{apply, in_world, put_block_data};
use super::{Result, Run, Src, Who};
use super::{bossbar, functions, living, places, rare, scoreboard, scores, team};
use crate::context::{Event, ServerContext};
use crate::rules;
use crate::tick::lock;

/// Most blocks one `fill` or `clone` may change (the
/// `max_block_modifications` default).
pub(super) fn block_limit(ctx: &ServerContext) -> i64 {
    i64::from(
        ctx.game_rules
            .lock()
            .expect("rules")
            .int("max_block_modifications"),
    )
}

pub(crate) fn err(key: &str, args: Vec<Tag>) -> CommandError {
    CommandError::new(key, args)
}

pub(super) fn str_tag(s: &str) -> Tag {
    Tag::from(s)
}

/// A number as vanilla's `%f`: six decimals.
pub(super) fn six(v: f64) -> Tag {
    Tag::String(format!("{v:.6}"))
}

pub(super) fn game_mode_id(name: &str) -> i32 {
    match name {
        "creative" => 1,
        "adventure" => 2,
        "spectator" => 3,
        _ => 0,
    }
}

pub(super) fn game_mode_key(mode: i32) -> String {
    let name = match mode {
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "survival",
    };
    format!("gameMode.{name}")
}

const DIFFICULTIES: [&str; 4] = ["peaceful", "easy", "normal", "hard"];

impl Run<'_> {
    pub(super) fn arg(&self, name: &str) -> Option<&Arg> {
        self.c.arg(name)
    }

    pub(super) fn int(&self, name: &str) -> Option<i32> {
        match self.arg(name)? {
            Arg::Int(v) | Arg::Time(v) => Some(*v),
            _ => None,
        }
    }

    pub(super) fn float(&self, name: &str) -> Option<f32> {
        match self.arg(name)? {
            Arg::Float(v) => Some(*v),
            _ => None,
        }
    }

    pub(super) fn boolean(&self, name: &str) -> Option<bool> {
        match self.arg(name)? {
            Arg::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub(super) fn string(&self, name: &str) -> Option<String> {
        match self.arg(name)? {
            Arg::Str(s) | Arg::Raw(s) | Arg::Id(s) => Some(s.clone()),
            Arg::IdOrTag { id, tag } => Some(if *tag { format!("#{id}") } else { id.clone() }),
            _ => None,
        }
    }

    pub(super) fn nbt(&self, name: &str) -> Option<Tag> {
        match self.arg(name)? {
            Arg::Nbt(t) => Some(t.clone()),
            _ => None,
        }
    }

    pub(super) fn targets(&self, name: &str, players: bool) -> Result<Vec<Target>> {
        match self.arg(name) {
            Some(Arg::Entity(s)) => target::entities(self.ctx, s, &self.src, players),
            _ => Err(err("argument.entity.notfound.entity", Vec::new())),
        }
    }

    /// The targets, or the source itself when the argument is left out.
    pub(super) fn targets_or_self(&self, name: &str, players: bool) -> Result<Vec<Target>> {
        if self.arg(name).is_some() {
            return self.targets(name, players);
        }
        let me = target::everyone(self.ctx)
            .into_iter()
            .filter(|t| match self.src.who {
                Who::Player(u) | Who::Entity(u) => t.uuid() == u,
                Who::Console => false,
            })
            .collect::<Vec<_>>();
        if me.is_empty() || (players && !me[0].is_player()) {
            let key = if players {
                "permissions.requires.player"
            } else {
                "permissions.requires.entity"
            };
            return Err(err(key, Vec::new()));
        }
        Ok(me)
    }

    pub(super) fn vec3(&self, name: &str) -> Option<[f64; 3]> {
        match self.arg(name)? {
            Arg::Pos(p) => Some(resolve(p, &self.src)),
            _ => None,
        }
    }

    /// A vec3 argument read as written, without moving whole numbers to
    /// the block middle (`particle`'s spread).
    pub(super) fn vec3_raw(&self, name: &str) -> Option<[f64; 3]> {
        match self.arg(name)? {
            Arg::Pos(Pos::World(c)) => {
                let f = |c: &Coord, base: f64| {
                    let v = if c.centered { c.value - 0.5 } else { c.value };
                    if c.relative { base + v } else { v }
                };
                Some([
                    f(&c[0], self.src.pos[0]),
                    f(&c[1], self.src.pos[1]),
                    f(&c[2], self.src.pos[2]),
                ])
            }
            Arg::Pos(p) => Some(resolve(p, &self.src)),
            _ => None,
        }
    }

    pub(super) fn block_pos(&self, name: &str) -> Option<Position> {
        let [x, y, z] = self.vec3(name)?;
        Some(Position::new(
            x.floor() as i32,
            y.floor() as i32,
            z.floor() as i32,
        ))
    }

    /// A block position that must be in the world and loaded.
    pub(super) fn loaded_pos(&self, name: &str) -> Result<Position> {
        let p = self
            .block_pos(name)
            .ok_or_else(|| err("argument.pos.unloaded", Vec::new()))?;
        if !in_world(self.ctx, p) {
            return Err(err("argument.pos.outofworld", Vec::new()));
        }
        if self.ctx.world.block(p, &self.ctx.data.blocks).is_none() {
            return Err(err("argument.pos.unloaded", Vec::new()));
        }
        Ok(p)
    }

    /// A block state with its block entity data.
    pub(super) fn block(&self, name: &str) -> Option<(u32, Option<Compound>)> {
        let Arg::Block(b) = self.arg(name)? else {
            return None;
        };
        let props: Vec<(&str, &str)> = b
            .properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let state = self.ctx.data.blocks.state_id(&b.id, &props)?;
        let nbt = match &b.nbt {
            Some(Tag::Compound(c)) => Some(c.clone()),
            _ => None,
        };
        Some((state, nbt))
    }
}

/// Coordinates for a source: `~` from its position, `^` along where it
/// looks.
pub fn resolve(p: &Pos, src: &Src) -> [f64; 3] {
    match p {
        Pos::World(c) => {
            let f = |c: &Coord, base: f64| if c.relative { base + c.value } else { c.value };
            [
                f(&c[0], src.pos[0]),
                f(&c[1], src.pos[1]),
                f(&c[2], src.pos[2]),
            ]
        }
        Pos::Local([left, up, forward]) => {
            let rad = std::f64::consts::PI / 180.0;
            let (yaw, pitch) = (f64::from(src.rot[0]), f64::from(src.rot[1]));
            let (f, g) = (((yaw + 90.0) * rad).cos(), ((yaw + 90.0) * rad).sin());
            let (h, i) = ((-pitch * rad).cos(), (-pitch * rad).sin());
            let (j, k) = (((-pitch + 90.0) * rad).cos(), ((-pitch + 90.0) * rad).sin());
            let fw = [f * h, i, g * h];
            let upv = [f * j, k, g * j];
            let lf = [
                -(fw[1] * upv[2] - fw[2] * upv[1]),
                -(fw[2] * upv[0] - fw[0] * upv[2]),
                -(fw[0] * upv[1] - fw[1] * upv[0]),
            ];
            let eye = if src.eyes { 1.62 } else { 0.0 };
            let base = [src.pos[0], src.pos[1] + eye, src.pos[2]];
            [
                base[0] + lf[0] * left + upv[0] * up + fw[0] * forward,
                base[1] + lf[1] * left + upv[1] * up + fw[1] * forward,
                base[2] + lf[2] * left + upv[2] * up + fw[2] * forward,
            ]
        }
    }
}

/// `execute` subcommands: the sources the rest of the line runs for.
pub fn modify(ctx: &ServerContext, c: &Context, s: &Src) -> Result<Vec<Src>> {
    let tree = &ctx.commands.tree;
    let mut path = c.path(tree);
    // After `execute ... <redirect>`, the path starts below `execute`.
    if path.first().is_some_and(|p| *p != "execute") && path.len() > 1 {
        path.insert(0, "execute");
    }
    let entities = |name: &str| -> Result<Vec<Target>> {
        match c.arg(name) {
            Some(Arg::Entity(sel)) => Ok(target::resolve(ctx, sel, s)),
            _ => Ok(Vec::new()),
        }
    };
    let one = |f: &dyn Fn(&mut Src)| {
        let mut n = s.clone();
        f(&mut n);
        Ok(vec![n])
    };
    match path.as_slice() {
        // Aliases and `run`: nothing changes.
        [_] | ["execute", "run"] => Ok(vec![s.clone()]),
        ["execute", "as", "targets"] => Ok(entities("targets")?
            .into_iter()
            .map(|t| {
                let mut n = s.clone();
                n.who = if t.is_player() {
                    Who::Player(t.uuid())
                } else {
                    Who::Entity(t.uuid())
                };
                if let Target::Player { name, .. } = &t {
                    n.name = name.clone();
                }
                n
            })
            .collect()),
        ["execute", "at", "targets"] => Ok(entities("targets")?
            .into_iter()
            .map(|t| {
                let mut n = s.clone();
                n.pos = t.pos();
                n.rot = t.rot();
                n
            })
            .collect()),
        ["execute", "positioned", "pos"] => {
            let Some(Arg::Pos(p)) = c.arg("pos") else {
                return Ok(vec![s.clone()]);
            };
            let at = resolve(p, s);
            one(&|n| {
                n.pos = at;
                n.eyes = false;
            })
        }
        ["execute", "positioned", "as", "targets"] => Ok(entities("targets")?
            .into_iter()
            .map(|t| {
                let mut n = s.clone();
                n.pos = t.pos();
                n
            })
            .collect()),
        ["execute", "rotated", "rot"] => {
            let Some(Arg::Pos2([yaw, pitch])) = c.arg("rot") else {
                return Ok(vec![s.clone()]);
            };
            let f = |c: &Coord, base: f32| {
                if c.relative {
                    base + c.value as f32
                } else {
                    c.value as f32
                }
            };
            let rot = [f(yaw, s.rot[0]), f(pitch, s.rot[1])];
            one(&|n| n.rot = rot)
        }
        ["execute", "rotated", "as", "targets"] => Ok(entities("targets")?
            .into_iter()
            .map(|t| {
                let mut n = s.clone();
                n.rot = t.rot();
                n
            })
            .collect()),
        ["execute", "anchored", "anchor"] => {
            let eyes = matches!(c.arg("anchor"), Some(Arg::Str(a)) if a == "eyes");
            one(&|n| n.eyes = eyes)
        }
        ["execute", "align", "axes"] => {
            let axes = match c.arg("axes") {
                Some(Arg::Str(a)) => a.clone(),
                _ => String::new(),
            };
            one(&|n| {
                for (i, axis) in ['x', 'y', 'z'].iter().enumerate() {
                    if axes.contains(*axis) {
                        n.pos[i] = n.pos[i].floor();
                    }
                }
            })
        }
        ["execute", "in", "dimension"] => Ok(vec![s.clone()]),
        ["execute", kind @ ("if" | "unless"), rest @ ..] => {
            let pass = condition(ctx, c, s, rest)?;
            if pass == (*kind == "if") {
                Ok(vec![s.clone()])
            } else {
                Ok(Vec::new())
            }
        }
        _ => functions::modify(ctx, c, s, &path)
            .or_else(|| scores::modify(ctx, c, s, &path))
            .or_else(|| bossbar::modify(ctx, c, s, &path))
            .or_else(|| scoreboard::modify(ctx, c, s, &path))
            .unwrap_or_else(|| {
                tracing::warn!("execute {} is not implemented yet", path.join(" "));
                Err(CommandError::new("command.failed", Vec::new()))
            }),
    }
}

/// `execute if ...`: whether the test passes.
pub(super) fn condition(ctx: &ServerContext, c: &Context, s: &Src, rest: &[&str]) -> Result<bool> {
    match rest {
        ["entity", "entities"] => Ok(match c.arg("entities") {
            Some(Arg::Entity(sel)) => !target::resolve(ctx, sel, s).is_empty(),
            _ => false,
        }),
        ["block", "pos", "block"] => {
            let (Some(Arg::Pos(p)), Some(Arg::Block(b))) = (c.arg("pos"), c.arg("block")) else {
                return Ok(false);
            };
            let [x, y, z] = resolve(p, s);
            let at = Position::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
            let Some(state) = ctx.world.block(at, &ctx.data.blocks) else {
                return Err(err("argument.pos.unloaded", Vec::new()));
            };
            let name = ctx.data.blocks.block_name(state).unwrap_or("");
            let id_ok = if b.tag {
                ctx.commands
                    .env
                    .tag("minecraft:block", &b.id)
                    .iter()
                    .any(|i| i == name)
            } else {
                b.id == name
            };
            Ok(id_ok
                && b.properties
                    .iter()
                    .all(|(k, v)| ctx.data.blocks.with_property(state, k, v) == Some(state)))
        }
        _ => scoreboard::condition(ctx, c, s, rest)
            .or_else(|| scores::condition(ctx, c, s, rest))
            .or_else(|| functions::condition(ctx, c, s, rest))
            .unwrap_or_else(|| Err(CommandError::new("command.failed", Vec::new()))),
    }
}

/// Runs the command at `path`; `None` when it is not done yet.
pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        // `execute if|unless ...` as the whole command: say whether it held.
        ["execute", kind @ ("if" | "unless"), rest @ ..] => {
            let pass = condition(r.ctx, r.c, &r.src, rest).map(|p| p == (*kind == "if"));
            // `if entity` counts what it found.
            let count = match (*kind, rest, r.arg("entities")) {
                ("if", ["entity", "entities"], Some(Arg::Entity(sel))) => {
                    Some(target::resolve(r.ctx, sel, &r.src).len() as i32)
                }
                _ => None,
            };
            match pass {
                Ok(true) if count.is_some() => {
                    let n = count.unwrap_or(1);
                    r.success(
                        translate("commands.execute.conditional.pass_count", vec![Tag::Int(n)]),
                        false,
                    );
                    Ok(n)
                }
                Ok(true) => {
                    r.success(
                        translate("commands.execute.conditional.pass", Vec::new()),
                        false,
                    );
                    Ok(1)
                }
                Ok(false) => Err(err("commands.execute.conditional.fail", Vec::new())),
                Err(e) => Err(e),
            }
        }
        ["help"] => help(r, None),
        ["help", "command"] => help(r, r.string("command")),
        ["list"] => list(r, false),
        ["list", "uuids"] => list(r, true),
        ["seed"] => seed(r),
        ["version"] => version(r),
        ["stop"] => {
            r.success(translate("commands.stop.stopping", Vec::new()), true);
            r.ctx.shutdown.notify_one();
            Ok(1)
        }
        ["save-all"] | ["save-all", "flush"] => {
            r.success(translate("commands.save.saving", Vec::new()), false);
            crate::save(r.ctx);
            r.success(translate("commands.save.success", Vec::new()), true);
            Ok(1)
        }
        ["save-on"] | ["save-off"] => save_toggle(r, path[0] == "save-on"),
        ["say", "message"] => say(r),
        ["me", "action"] => me(r),
        ["msg", "targets", "message"] => msg(r),
        ["tellraw", "targets", "message"] => tellraw(r),
        ["kick", "targets"] | ["kick", "targets", "reason"] => kick(r),
        ["op", "targets"] => op(r, true),
        ["deop", "targets"] => op(r, false),
        ["whitelist", sub @ ..] => whitelist(r, sub),
        ["ban", "targets"] | ["ban", "targets", "reason"] => ban(r),
        ["pardon", "targets"] => pardon(r),
        ["ban-ip", ..] => ban_ip(r),
        ["pardon-ip", "target"] => pardon_ip(r),
        ["banlist", rest @ ..] => banlist(r, rest.first().copied()),
        ["setidletimeout", "minutes"] => {
            let m = r.int("minutes").unwrap_or(0);
            let key = if m == 0 {
                "commands.setidletimeout.success.disabled"
            } else {
                "commands.setidletimeout.success"
            };
            let args = if m == 0 {
                Vec::new()
            } else {
                vec![Tag::Int(m)]
            };
            r.success(translate(key, args), true);
            Ok(m)
        }
        ["difficulty"] => {
            let d = r.ctx.difficulty();
            let name = format!(
                "options.difficulty.{}",
                DIFFICULTIES[d.clamp(0, 3) as usize]
            );
            r.success(
                translate(
                    "commands.difficulty.query",
                    vec![translate(&name, Vec::new())],
                ),
                false,
            );
            Ok(d)
        }
        ["difficulty", d] => difficulty(r, d),
        ["defaultgamemode", "gamemode"] => default_game_mode(r),
        ["gamemode", "gamemode"] | ["gamemode", "gamemode", "target"] => gamemode(r),
        ["gamerule", rule] => gamerule_query(r, rule.trim_start_matches("minecraft:")),
        ["gamerule", rule, "value"] => gamerule_set(r, rule.trim_start_matches("minecraft:")),
        ["time", rest @ ..] => time(r, rest),
        ["tick", rest @ ..] => tick(r, rest),
        ["teleport", ..] => teleport(r, path),
        ["give", ..] => give(r),
        ["clear", ..] => clear(r),
        ["kill"] | ["kill", "targets"] => kill(r),
        ["summon", ..] => summon(r),
        ["setblock", ..] => setblock(r, path.get(3).copied()),
        ["fill", ..] => fill(r, &path[4..]),
        [
            "item",
            "replace",
            kind @ ("entity" | "block"),
            "target",
            "slots",
            "with",
            "item",
            ..,
        ] => item_replace(r, kind),
        ["weather", kind, ..] => weather(r, kind),
        [
            "experience",
            op @ ("add" | "set"),
            "target",
            "amount",
            rest @ ..,
        ] => experience_change(r, op, rest.first() == Some(&"levels")),
        ["experience", "query", "target", what] => experience_query(r, *what == "levels"),
        ["worldborder", rest @ ..] => worldborder(r, rest),
        ["title", "targets", rest @ ..] => title(r, rest),
        ["playsound", "sound", ..] => playsound(r, path),
        ["stopsound", "targets", rest @ ..] => stopsound(r, rest),
        ["random", kind @ ("value" | "roll"), "range", ..] => random(r, *kind == "roll"),
        ["clone", ..] => clone(r, path),
        ["datapack", "list", rest @ ..] => datapack_list(r, rest.first().copied()),
        _ => {
            return scores::run(r, path)
                .or_else(|| places::run(r, path))
                .or_else(|| bossbar::run(r, path))
                .or_else(|| scoreboard::run(r, path))
                .or_else(|| team::run(r, path))
                .or_else(|| functions::run(r, path))
                .or_else(|| living::run(r, path))
                .or_else(|| rare::run(r, path));
        }
    })
}

fn help(r: &mut Run, command: Option<String>) -> Result<i32> {
    let tree = &r.ctx.commands.tree;
    let (node, prefix) = match &command {
        None => (mc_command::tree::ROOT, "/".to_owned()),
        Some(c) => {
            let p = mc_command::dispatch::parse(tree, c, r.src.level, &r.ctx.commands.env);
            if p.cursor < c.len() || !p.errors.is_empty() {
                return Err(err("commands.help.failed", Vec::new()));
            }
            let last = p.chain.last().and_then(|x| x.nodes.last()).map(|n| n.0);
            match last {
                Some(n) => (n, format!("/{c} ")),
                None => return Err(err("commands.help.failed", Vec::new())),
            }
        }
    };
    let usages = mc_command::usage::smart_usage(tree, node, r.src.level);
    if command.is_some() && usages.is_empty() {
        r.success(
            Tag::String(format!("/{}", command.unwrap_or_default())),
            false,
        );
        return Ok(1);
    }
    let n = usages.len() as i32;
    for (_, u) in usages {
        r.success(Tag::String(format!("{prefix}{u}")), false);
    }
    Ok(n)
}

fn list(r: &mut Run, uuids: bool) -> Result<i32> {
    let players: Vec<(String, u128)> = lock(&r.ctx.players)
        .values()
        .map(|p| (p.profile.name.clone(), p.profile.uuid))
        .collect();
    let names: Vec<Tag> = players
        .iter()
        .map(|(name, uuid)| {
            if uuids {
                translate(
                    "commands.list.nameAndId",
                    vec![str_tag(name), Tag::String(crate::util::uuid_string(*uuid))],
                )
            } else {
                player_name(name, *uuid)
            }
        })
        .collect();
    let joined = join(names);
    let count = players.len() as i32;
    r.success(
        translate(
            "commands.list.players",
            vec![
                Tag::Int(count),
                Tag::Int(r.ctx.config.max_players as i32),
                joined,
            ],
        ),
        false,
    );
    Ok(count)
}

/// Components joined with `, ` as vanilla lists them.
pub(super) fn join(items: Vec<Tag>) -> Tag {
    let mut parts = Vec::new();
    for (i, t) in items.into_iter().enumerate() {
        if i > 0 {
            parts.push(Tag::from(", "));
        }
        parts.push(t);
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

fn seed(r: &mut Run) -> Result<i32> {
    let seed = r
        .ctx
        .world
        .folder()
        .and_then(crate::generation::seed_of)
        // The flat test world: the configured seed.
        .or(r.ctx.config.seed)
        .unwrap_or(0);
    let text = seed.to_string();
    let mut click = Compound::new();
    click.insert("action", "copy_to_clipboard");
    click.insert("value", text.as_str());
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", translate("chat.copy.click", Vec::new()));
    let mut c = Compound::new();
    c.insert("text", text.as_str());
    c.insert("color", "green");
    c.insert("click_event", Tag::Compound(click));
    c.insert("hover_event", Tag::Compound(hover));
    c.insert("insertion", text.as_str());
    let bracketed = translate("chat.square_brackets", vec![Tag::Compound(c)]);
    r.success(translate("commands.seed.success", vec![bracketed]), false);
    Ok(seed as i32)
}

/// `2026-09-15T11:20:48+00:00` as Java prints a date: `Tue Sep 15
/// 11:20:48 UTC 2026`.
fn java_date(iso: &str) -> String {
    let n = |a: usize, b: usize| {
        iso.get(a..b)
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let (y, m, d) = (n(0, 4), n(5, 7), n(8, 10));
    // Days since 1970-01-01 (civil from days, inverted).
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(m.clamp(1, 12) - 1) as usize];
    format!(
        "{weekday} {month} {d:02} {} UTC {y}",
        iso.get(11..19).unwrap_or("00:00:00")
    )
}

fn version(r: &mut Run) -> Result<i32> {
    let v: serde_json::Value = std::fs::read_to_string(r.ctx.config.data_dir.join("version.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let protocol = v["protocol_version"].as_i64().unwrap_or(0) as i32;
    let pack = |kind: &str| {
        format!(
            "{}.{}",
            v["pack_version"][format!("{kind}_major")]
                .as_i64()
                .unwrap_or(0),
            v["pack_version"][format!("{kind}_minor")]
                .as_i64()
                .unwrap_or(0)
        )
    };
    let lines = vec![
        translate("commands.version.header", Vec::new()),
        translate("commands.version.id", vec![Tag::String(s("id"))]),
        translate("commands.version.name", vec![Tag::String(s("name"))]),
        translate(
            "commands.version.data",
            vec![Tag::Int(v["world_version"].as_i64().unwrap_or(0) as i32)],
        ),
        translate("commands.version.series", vec![Tag::String(s("series_id"))]),
        translate(
            "commands.version.protocol",
            vec![Tag::Int(protocol), Tag::String(format!("0x{protocol:x}"))],
        ),
        translate(
            "commands.version.build_time",
            vec![Tag::String(java_date(&s("build_time")))],
        ),
        translate(
            "commands.version.pack.resource",
            vec![Tag::String(pack("resource"))],
        ),
        translate(
            "commands.version.pack.data",
            vec![Tag::String(pack("data"))],
        ),
        translate(
            if v["stable"].as_bool().unwrap_or(false) {
                "commands.version.stable.yes"
            } else {
                "commands.version.stable.no"
            },
            Vec::new(),
        ),
    ];
    for l in lines {
        r.success(l, false);
    }
    Ok(1)
}

fn save_toggle(r: &mut Run, on: bool) -> Result<i32> {
    let was = r.ctx.world.autosave(on);
    if was == on {
        let key = if on {
            "commands.save.alreadyOn"
        } else {
            "commands.save.alreadyOff"
        };
        return Err(err(key, Vec::new()));
    }
    let key = if on {
        "commands.save.enabled"
    } else {
        "commands.save.disabled"
    };
    r.success(translate(key, Vec::new()), true);
    Ok(1)
}

/// The source's name as messages show it.
pub(super) fn source_name(r: &Run) -> Tag {
    match r.src.who {
        Who::Player(u) => player_name(&r.src.name, u),
        _ => str_tag(&r.src.name),
    }
}

fn say(r: &mut Run) -> Result<i32> {
    let text = r.string("message").unwrap_or_default();
    let line = translate(
        "chat.type.announcement",
        vec![source_name(r), Tag::String(text)],
    );
    r.out.events.push(Event::Chat(line));
    Ok(1)
}

fn me(r: &mut Run) -> Result<i32> {
    let text = r.string("action").unwrap_or_default();
    let line = translate("chat.type.emote", vec![source_name(r), Tag::String(text)]);
    r.out.events.push(Event::Chat(line));
    Ok(1)
}

fn msg(r: &mut Run) -> Result<i32> {
    let targets = r.targets("targets", true)?;
    let text = r.string("message").unwrap_or_default();
    let from = source_name(r);
    for t in &targets {
        let mut incoming = translate(
            "commands.message.display.incoming",
            vec![from.clone(), Tag::String(text.clone())],
        );
        style(&mut incoming, &[("color", "gray"), ("italic", "")]);
        r.out.events.push(Event::Message {
            uuid: t.uuid(),
            line: incoming,
            overlay: false,
        });
        let mut outgoing = translate(
            "commands.message.display.outgoing",
            vec![t.display(), Tag::String(text.clone())],
        );
        style(&mut outgoing, &[("color", "gray"), ("italic", "")]);
        r.out.lines.push(outgoing);
    }
    Ok(targets.len() as i32)
}

/// Adds styles (`italic` with an empty value means true).
pub(super) fn style(t: &mut Tag, styles: &[(&str, &str)]) {
    if let Tag::Compound(c) = t {
        for (k, v) in styles {
            if v.is_empty() {
                c.insert(*k, Tag::Byte(1));
            } else {
                c.insert(*k, *v);
            }
        }
    }
}

fn tellraw(r: &mut Run) -> Result<i32> {
    let targets = r.targets("targets", true)?;
    let message = r.nbt("message").unwrap_or_else(|| Tag::from(""));
    for t in &targets {
        r.out.events.push(Event::Message {
            uuid: t.uuid(),
            line: message.clone(),
            overlay: false,
        });
    }
    Ok(targets.len() as i32)
}

fn kick(r: &mut Run) -> Result<i32> {
    let targets = r.targets("targets", true)?;
    let reason = match r.string("reason") {
        Some(s) => Tag::String(s),
        None => translate("multiplayer.disconnect.kicked", Vec::new()),
    };
    for t in &targets {
        r.out.events.push(Event::Kick {
            uuid: t.uuid(),
            reason: reason.clone(),
        });
        r.success(
            translate("commands.kick.success", vec![t.display(), reason.clone()]),
            true,
        );
    }
    Ok(targets.len() as i32)
}

/// Profiles named by a `game_profile` argument: players online by name or
/// selector, else known or offline-mode profiles.
fn profiles(r: &Run, name: &str) -> Result<Vec<(String, u128)>> {
    let Some(Arg::Entity(sel)) = r.arg(name) else {
        return Err(err("argument.player.unknown", Vec::new()));
    };
    if let Some(n) = &sel.player {
        if let Some(p) = lock(&r.ctx.players)
            .values()
            .find(|p| p.profile.name.eq_ignore_ascii_case(n))
        {
            return Ok(vec![(p.profile.name.clone(), p.profile.uuid)]);
        }
        let known = lock(&r.ctx.lists)
            .usercache
            .iter()
            .find(|(p, _)| p.name.eq_ignore_ascii_case(n))
            .and_then(|(p, _)| Some((p.name.clone(), p.uuid?)));
        if let Some(k) = known {
            return Ok(vec![k]);
        }
        if r.ctx.config.online_mode {
            return Err(err("argument.player.unknown", Vec::new()));
        }
        return Ok(vec![(n.clone(), crate::util::offline_uuid(n))]);
    }
    let found: Vec<(String, u128)> = target::resolve(r.ctx, sel, &r.src)
        .into_iter()
        .filter_map(|t| match t {
            Target::Player { name, uuid, .. } => Some((name, uuid)),
            Target::Entity { .. } => None,
        })
        .collect();
    if found.is_empty() {
        return Err(err("argument.player.unknown", Vec::new()));
    }
    Ok(found)
}

fn op(r: &mut Run, make: bool) -> Result<i32> {
    let profiles = profiles(r, "targets")?;
    let mut changed = 0;
    for (name, uuid) in profiles {
        let mut lists = lock(&r.ctx.lists);
        let is_op = lists.op(uuid, &name).is_some();
        if make == is_op {
            continue;
        }
        if make {
            lists.ops.push(crate::lists::Op {
                uuid: Some(uuid),
                name: name.clone(),
                level: r.ctx.config.op_permission_level,
                bypasses_player_limit: false,
            });
        } else {
            lists
                .ops
                .retain(|o| o.uuid != Some(uuid) && !o.name.eq_ignore_ascii_case(&name));
        }
        lists.save_ops();
        drop(lists);
        changed += 1;
        r.out.events.push(Event::Permissions { uuid });
        let key = if make {
            "commands.op.success"
        } else {
            "commands.deop.success"
        };
        r.success(translate(key, vec![str_tag(&name)]), true);
    }
    if changed == 0 {
        let key = if make {
            "commands.op.failed"
        } else {
            "commands.deop.failed"
        };
        return Err(err(key, Vec::new()));
    }
    Ok(changed)
}

fn whitelist(r: &mut Run, sub: &[&str]) -> Result<i32> {
    use std::sync::atomic::Ordering;
    match sub {
        ["on"] | ["off"] => {
            let on = sub[0] == "on";
            if r.ctx.white_list() == on {
                let key = if on {
                    "commands.whitelist.alreadyOn"
                } else {
                    "commands.whitelist.alreadyOff"
                };
                return Err(err(key, Vec::new()));
            }
            r.ctx.white_list.store(on, Ordering::Relaxed);
            if let Some(dir) = &r.ctx.config.server_dir {
                crate::properties::set(dir, "white-list", if on { "true" } else { "false" });
            }
            let key = if on {
                "commands.whitelist.enabled"
            } else {
                "commands.whitelist.disabled"
            };
            r.success(translate(key, Vec::new()), true);
            Ok(1)
        }
        ["list"] => {
            let names: Vec<String> = lock(&r.ctx.lists)
                .whitelist
                .iter()
                .map(|n| n.name.clone())
                .collect();
            if names.is_empty() {
                r.success(translate("commands.whitelist.none", Vec::new()), false);
            } else {
                r.success(
                    translate(
                        "commands.whitelist.list",
                        vec![Tag::Int(names.len() as i32), Tag::String(names.join(", "))],
                    ),
                    false,
                );
            }
            Ok(names.len() as i32)
        }
        ["reload"] => {
            if let Some(dir) = &r.ctx.config.server_dir {
                let fresh = crate::lists::Lists::load(dir);
                lock(&r.ctx.lists).whitelist = fresh.whitelist;
            }
            r.success(translate("commands.whitelist.reloaded", Vec::new()), true);
            Ok(1)
        }
        ["add", "targets"] | ["remove", "targets"] => {
            let add = sub[0] == "add";
            let mut changed = 0;
            for (name, uuid) in profiles(r, "targets")? {
                let mut lists = lock(&r.ctx.lists);
                let listed = lists.whitelisted(uuid, &name);
                if listed == add {
                    continue;
                }
                if add {
                    lists.whitelist.push(crate::lists::Named {
                        uuid: Some(uuid),
                        name: name.clone(),
                    });
                } else {
                    lists
                        .whitelist
                        .retain(|n| n.uuid != Some(uuid) && !n.name.eq_ignore_ascii_case(&name));
                }
                lists.save_whitelist();
                drop(lists);
                changed += 1;
                let key = if add {
                    "commands.whitelist.add.success"
                } else {
                    "commands.whitelist.remove.success"
                };
                r.success(translate(key, vec![str_tag(&name)]), true);
            }
            if changed == 0 {
                let key = if add {
                    "commands.whitelist.add.failed"
                } else {
                    "commands.whitelist.remove.failed"
                };
                return Err(err(key, Vec::new()));
            }
            Ok(changed)
        }
        _ => Err(err("command.failed", Vec::new())),
    }
}

fn ban_reason(r: &Run) -> String {
    r.string("reason").unwrap_or_default()
}

fn reason_tag(reason: &str) -> Tag {
    if reason.is_empty() {
        translate("multiplayer.disconnect.banned.reason.default", Vec::new())
    } else {
        Tag::String(reason.to_owned())
    }
}

fn ban(r: &mut Run) -> Result<i32> {
    let reason = ban_reason(r);
    let mut changed = 0;
    for (name, uuid) in profiles(r, "targets")? {
        let mut lists = lock(&r.ctx.lists);
        if lists.player_ban(uuid, &name).is_some() {
            continue;
        }
        lists.banned_players.push(crate::lists::Ban {
            uuid: Some(uuid),
            name: name.clone(),
            created: crate::util::now_text(),
            source: r.src.name.clone(),
            expires: "forever".into(),
            reason: if reason.is_empty() {
                "Banned by an operator.".into()
            } else {
                reason.clone()
            },
        });
        lists.save_bans();
        drop(lists);
        changed += 1;
        r.success(
            translate(
                "commands.ban.success",
                vec![str_tag(&name), reason_tag(&reason)],
            ),
            true,
        );
        r.out.events.push(Event::Kick {
            uuid,
            reason: translate("multiplayer.disconnect.banned", Vec::new()),
        });
    }
    if changed == 0 {
        return Err(err("commands.ban.failed", Vec::new()));
    }
    Ok(changed)
}

fn pardon(r: &mut Run) -> Result<i32> {
    let mut changed = 0;
    for (name, uuid) in profiles(r, "targets")? {
        let mut lists = lock(&r.ctx.lists);
        let before = lists.banned_players.len();
        lists
            .banned_players
            .retain(|b| b.uuid != Some(uuid) && !b.name.eq_ignore_ascii_case(&name));
        if lists.banned_players.len() == before {
            continue;
        }
        lists.save_bans();
        drop(lists);
        changed += 1;
        r.success(
            translate("commands.pardon.success", vec![str_tag(&name)]),
            true,
        );
    }
    if changed == 0 {
        return Err(err("commands.pardon.failed", Vec::new()));
    }
    Ok(changed)
}

fn is_ip(s: &str) -> bool {
    s.parse::<std::net::Ipv4Addr>().is_ok()
}

fn ban_ip(r: &mut Run) -> Result<i32> {
    let target = r.string("target").unwrap_or_default();
    let ip = if is_ip(&target) {
        target
    } else {
        let addr = lock(&r.ctx.players)
            .values()
            .find(|p| p.profile.name.eq_ignore_ascii_case(&target))
            .and_then(|p| p.address.clone());
        addr.ok_or_else(|| err("commands.banip.invalid", Vec::new()))?
    };
    let reason = ban_reason(r);
    let mut lists = lock(&r.ctx.lists);
    if lists.ip_ban(&ip).is_some() {
        return Err(err("commands.banip.failed", Vec::new()));
    }
    lists.banned_ips.push(crate::lists::Ban {
        uuid: None,
        name: ip.clone(),
        created: crate::util::now_text(),
        source: r.src.name.clone(),
        expires: "forever".into(),
        reason: if reason.is_empty() {
            "Banned by an operator.".into()
        } else {
            reason.clone()
        },
    });
    lists.save_bans();
    drop(lists);
    r.success(
        translate(
            "commands.banip.success",
            vec![str_tag(&ip), reason_tag(&reason)],
        ),
        true,
    );
    let hit: Vec<(u128, String)> = lock(&r.ctx.players)
        .values()
        .filter(|p| p.address.as_deref() == Some(ip.as_str()))
        .map(|p| (p.profile.uuid, p.profile.name.clone()))
        .collect();
    if !hit.is_empty() {
        let names: Vec<String> = hit.iter().map(|(_, n)| n.clone()).collect();
        r.success(
            translate(
                "commands.banip.info",
                vec![Tag::Int(hit.len() as i32), Tag::String(names.join(", "))],
            ),
            true,
        );
        for (uuid, _) in hit {
            r.out.events.push(Event::Kick {
                uuid,
                reason: translate("multiplayer.disconnect.ip_banned", Vec::new()),
            });
        }
    }
    Ok(1)
}

fn pardon_ip(r: &mut Run) -> Result<i32> {
    let ip = r.string("target").unwrap_or_default();
    if !is_ip(&ip) {
        return Err(err("commands.pardonip.invalid", Vec::new()));
    }
    let mut lists = lock(&r.ctx.lists);
    let before = lists.banned_ips.len();
    lists.banned_ips.retain(|b| b.name != ip);
    if lists.banned_ips.len() == before {
        return Err(err("commands.pardonip.failed", Vec::new()));
    }
    lists.save_bans();
    drop(lists);
    r.success(
        translate("commands.pardonip.success", vec![str_tag(&ip)]),
        true,
    );
    Ok(1)
}

fn banlist(r: &mut Run, which: Option<&str>) -> Result<i32> {
    let lists = lock(&r.ctx.lists);
    let mut bans: Vec<crate::lists::Ban> = Vec::new();
    if which != Some("ips") {
        bans.extend(lists.banned_players.iter().cloned());
    }
    if which != Some("players") {
        bans.extend(lists.banned_ips.iter().cloned());
    }
    drop(lists);
    if bans.is_empty() {
        r.success(translate("commands.banlist.none", Vec::new()), false);
        return Ok(0);
    }
    r.success(
        translate("commands.banlist.list", vec![Tag::Int(bans.len() as i32)]),
        false,
    );
    for b in &bans {
        r.success(
            translate(
                "commands.banlist.entry",
                vec![str_tag(&b.name), str_tag(&b.source), str_tag(&b.reason)],
            ),
            false,
        );
    }
    Ok(bans.len() as i32)
}

fn difficulty(r: &mut Run, name: &str) -> Result<i32> {
    let d = DIFFICULTIES.iter().position(|x| *x == name).unwrap_or(1) as i32;
    let key = format!("options.difficulty.{name}");
    if r.ctx.difficulty() == d {
        return Err(err("commands.difficulty.failure", vec![str_tag(name)]));
    }
    r.ctx
        .difficulty
        .store(d, std::sync::atomic::Ordering::Relaxed);
    let _ = r.ctx.world.update_level(|data| {
        let mut settings = match data.get("difficulty_settings") {
            Some(Tag::Compound(c)) => c.clone(),
            _ => Compound::new(),
        };
        settings.insert("difficulty", name);
        data.insert("difficulty_settings", Tag::Compound(settings));
    });
    r.out.events.push(Event::Difficulty(d));
    r.success(
        translate(
            "commands.difficulty.success",
            vec![translate(&key, Vec::new())],
        ),
        true,
    );
    Ok(0)
}

fn default_game_mode(r: &mut Run) -> Result<i32> {
    let name = r.string("gamemode").unwrap_or_default();
    let mode = game_mode_id(&name);
    r.ctx
        .default_game_mode
        .store(mode, std::sync::atomic::Ordering::Relaxed);
    let _ = r.ctx.world.update_level(|data| {
        data.insert("GameType", Tag::Int(mode));
    });
    r.success(
        translate(
            "commands.defaultgamemode.success",
            vec![translate(&game_mode_key(mode), Vec::new())],
        ),
        true,
    );
    Ok(0)
}

fn set_game_mode(r: &mut Run, uuid: u128, mode: i32) -> bool {
    let mut players = lock(&r.ctx.players);
    let Some(p) = players.get_mut(&uuid) else {
        return false;
    };
    if p.game_mode == mode {
        return false;
    }
    p.game_mode = mode;
    drop(players);
    r.out.events.push(Event::GameMode { uuid, mode });
    true
}

fn gamemode(r: &mut Run) -> Result<i32> {
    let mode = game_mode_id(&r.string("gamemode").unwrap_or_default());
    let mode_name = translate(&game_mode_key(mode), Vec::new());
    let targets = r.targets_or_self("target", true)?;
    let mut changed = 0;
    for t in &targets {
        if !set_game_mode(r, t.uuid(), mode) {
            continue;
        }
        changed += 1;
        if Some(t.uuid()) == r.src.player() {
            r.success(
                translate("commands.gamemode.success.self", vec![mode_name.clone()]),
                true,
            );
        } else {
            r.out.events.push(Event::Message {
                uuid: t.uuid(),
                line: translate("gameMode.changed", vec![mode_name.clone()]),
                overlay: false,
            });
            r.success(
                translate(
                    "commands.gamemode.success.other",
                    vec![t.display(), mode_name.clone()],
                ),
                true,
            );
        }
    }
    Ok(changed)
}

fn gamerule_query(r: &mut Run, name: &str) -> Result<i32> {
    let value = r.ctx.game_rules.lock().expect("rules").get(name);
    let Some(v) = value else {
        return Err(err("command.failed", Vec::new()));
    };
    r.success(
        translate(
            "commands.gamerule.query",
            vec![str_tag(name), str_tag(&v.text())],
        ),
        false,
    );
    Ok(match v {
        rules::Value::Bool(b) => i32::from(b),
        rules::Value::Int(n) => n,
    })
}

fn gamerule_set(r: &mut Run, name: &str) -> Result<i32> {
    let current = r.ctx.game_rules.lock().expect("rules").get(name);
    let value = match current {
        Some(rules::Value::Bool(_)) => rules::Value::Bool(r.boolean("value").unwrap_or(false)),
        Some(rules::Value::Int(_)) => rules::Value::Int(r.int("value").unwrap_or(0)),
        None => return Err(err("command.failed", Vec::new())),
    };
    if current == Some(value) {
        return Err(err(
            "commands.gamerule.not_set",
            vec![str_tag(name), str_tag(&value.text())],
        ));
    }
    r.ctx.game_rules.lock().expect("rules").set(name, value);
    if name == "advance_time" {
        let running = value == rules::Value::Bool(true);
        let mut time = lock(&r.ctx.time);
        for clock in &mut time.clocks {
            clock.running = running;
        }
        r.out.events.push(Event::Time(time.packet(true)));
    }
    r.success(
        translate(
            "commands.gamerule.set",
            vec![str_tag(name), str_tag(&value.text())],
        ),
        true,
    );
    Ok(match value {
        rules::Value::Bool(b) => i32::from(b),
        rules::Value::Int(n) => n,
    })
}

/// A timeline's definition (`data/minecraft/timeline/<name>.json`).
fn timeline(ctx: &ServerContext, id: &str) -> Option<serde_json::Value> {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let file = ctx
        .config
        .data_dir
        .join("data")
        .join(ns)
        .join("timeline")
        .join(format!("{path}.json"));
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// Where a time marker of `clock` falls: (period, ticks).
fn time_marker(ctx: &ServerContext, clock: &str, marker: &str) -> Option<(i64, i64)> {
    let dir = ctx.config.data_dir.join("data/minecraft/timeline");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(v) =
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(e.path()).ok()?)
        else {
            continue;
        };
        if v["clock"].as_str() != Some(clock) {
            continue;
        }
        let m = &v["time_markers"][marker];
        let ticks = m.as_i64().or_else(|| m["ticks"].as_i64());
        if let Some(t) = ticks {
            return Some((v["period_ticks"].as_i64().unwrap_or(0), t));
        }
    }
    None
}

fn time(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let (clock_name, rest) = match rest {
        ["of", "clock", rest @ ..] => (r.string("clock").unwrap_or_default(), rest),
        _ => ("minecraft:overworld".to_owned(), rest),
    };
    let mut time = lock(&r.ctx.time);
    let Some(index) = time.clocks.iter().position(|c| c.name == clock_name) else {
        return Err(err(
            "commands.time.no_default_clock",
            vec![str_tag("minecraft:overworld")],
        ));
    };
    let clock = str_tag(&clock_name);
    let mut changed = false;
    let result = match rest {
        ["set", "time"] => {
            let t = i64::from(r.int("time").unwrap_or(0));
            if time.clocks[index].ticks == t {
                return Err(err(
                    "commands.time.set.already_at_time",
                    vec![clock, int(t)],
                ));
            }
            time.clocks[index].ticks = t;
            changed = true;
            r.success(
                translate("commands.time.set.absolute", vec![clock, int(t)]),
                true,
            );
            t as i32
        }
        ["set", "timemarker"] => {
            let marker = r.string("timemarker").unwrap_or_default();
            let Some((period, at)) = time_marker(r.ctx, &clock_name, &marker) else {
                return Err(err(
                    "commands.time.no_time_marker_found",
                    vec![str_tag(&marker), clock],
                ));
            };
            let now = time.clocks[index].ticks;
            if period > 0 && now.rem_euclid(period) == at {
                return Err(err(
                    "commands.time.set.already_at_time_marker",
                    vec![clock, str_tag(&marker)],
                ));
            }
            let next = if period > 0 {
                let base = now - now.rem_euclid(period) + at;
                if base > now { base } else { base + period }
            } else {
                at
            };
            time.clocks[index].ticks = next;
            changed = true;
            r.success(
                translate(
                    "commands.time.set.time_marker",
                    vec![clock, str_tag(&marker)],
                ),
                true,
            );
            next as i32
        }
        ["add", "time"] => {
            let t = time.clocks[index].ticks + i64::from(r.int("time").unwrap_or(0));
            time.clocks[index].ticks = t;
            changed = true;
            r.success(
                translate("commands.time.set.absolute", vec![clock, int(t)]),
                true,
            );
            t as i32
        }
        ["pause"] | ["resume"] => {
            let run = rest[0] == "resume";
            if time.clocks[index].running == run {
                let key = if run {
                    "commands.time.pause.already_running"
                } else {
                    "commands.time.pause.already_paused"
                };
                return Err(err(key, vec![clock]));
            }
            time.clocks[index].running = run;
            changed = true;
            let key = if run {
                "commands.time.resume"
            } else {
                "commands.time.pause"
            };
            r.success(translate(key, vec![clock]), true);
            1
        }
        ["rate", "rate"] => {
            let rate = r.float("rate").unwrap_or(1.0);
            let shown = Tag::String(format!("{rate}"));
            if time.clocks[index].rate == rate {
                return Err(err("commands.time.rate.already_same", vec![clock, shown]));
            }
            time.clocks[index].rate = rate;
            changed = true;
            r.success(translate("commands.time.rate", vec![clock, shown]), true);
            1
        }
        ["query", "time"] => {
            let t = time.clocks[index].ticks;
            r.success(
                translate("commands.time.query.absolute", vec![clock, int(t)]),
                false,
            );
            t as i32
        }
        ["query", "gametime"] => {
            let t = time.age;
            r.success(
                translate("commands.time.query.gametime", vec![int(t)]),
                false,
            );
            (t % i64::from(i32::MAX)) as i32
        }
        ["query", "timeline"] | ["query", "timeline", "repetition"] => {
            let id = r.string("timeline").unwrap_or_default();
            let Some(def) = timeline(r.ctx, &id) else {
                return Err(err(
                    "argument.resource.not_found",
                    vec![str_tag(&id), str_tag("minecraft:timeline")],
                ));
            };
            if def["clock"].as_str() != Some(clock_name.as_str()) {
                return Err(err(
                    "commands.time.wrong_timeline_for_clock",
                    vec![str_tag(&id), clock],
                ));
            }
            let period = def["period_ticks"].as_i64().unwrap_or(0).max(1);
            let t = time.clocks[index].ticks;
            if rest.len() == 3 {
                let n = t.div_euclid(period);
                r.success(
                    translate(
                        "commands.time.query.timeline.repetitions",
                        vec![str_tag(&id), int(n)],
                    ),
                    false,
                );
                n as i32
            } else {
                let n = t.rem_euclid(period);
                r.success(
                    translate("commands.time.query.timeline", vec![str_tag(&id), int(n)]),
                    false,
                );
                n as i32
            }
        }
        _ => return Err(err("command.failed", Vec::new())),
    };
    if changed {
        let mut packet = time.packet(true);
        let id = time.clocks[index].id;
        packet.clocks.retain(|c| c.clock == id);
        r.out.events.push(Event::Time(packet));
    }
    Ok(result)
}

fn tick(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let state = |t: &crate::tick::TickControl| {
        Event::TickingState(TickingState {
            tick_rate: t.rate,
            frozen: t.frozen,
        })
    };
    match rest {
        ["query"] => {
            let mspt = 1000.0 / r.tick.rate;
            let average = r.tick.smoothed_ms;
            let status = if r.tick.frozen {
                "commands.tick.status.frozen"
            } else if average > mspt {
                "commands.tick.status.lagging"
            } else {
                "commands.tick.status.running"
            };
            r.success(translate(status, Vec::new()), false);
            r.success(
                translate(
                    "commands.tick.query.rate.running",
                    vec![
                        Tag::String(format!("{:.1}", r.tick.rate)),
                        Tag::String(format!("{average:.1}")),
                        Tag::String(format!("{mspt:.1}")),
                    ],
                ),
                false,
            );
            let mut times = r.tick.times;
            times.sort_unstable();
            let ms = |i: usize| Tag::String(format!("{:.1}", times[i] as f64 / 1.0e6));
            let n = times.len();
            r.success(
                translate(
                    "commands.tick.query.percentiles",
                    vec![
                        ms(n / 2),
                        ms((n as f64 * 0.95) as usize),
                        ms((n as f64 * 0.99) as usize),
                        Tag::Int(n as i32),
                    ],
                ),
                false,
            );
            Ok(r.tick.rate as i32)
        }
        ["rate", "rate"] => {
            let rate = r.float("rate").unwrap_or(20.0);
            r.tick.rate = rate;
            r.out.events.push(state(r.tick));
            r.success(
                translate(
                    "commands.tick.rate.success",
                    vec![Tag::String(format!("{rate:.1}"))],
                ),
                true,
            );
            Ok(rate as i32)
        }
        ["freeze"] | ["unfreeze"] => {
            let freeze = rest[0] == "freeze";
            r.tick.frozen = freeze;
            if !freeze {
                r.tick.steps = 0;
            }
            r.out.events.push(state(r.tick));
            let key = if freeze {
                "commands.tick.status.frozen"
            } else {
                "commands.tick.status.running"
            };
            r.success(translate(key, Vec::new()), true);
            Ok(1)
        }
        ["step"] | ["step", "time"] => {
            if !r.tick.frozen {
                return Err(err("commands.tick.step.fail", Vec::new()));
            }
            let steps = r.int("time").unwrap_or(1).max(1) as u32;
            r.tick.steps += steps;
            r.out.events.push(Event::TickingStep(TickingStep {
                steps: steps as i32,
            }));
            r.success(
                translate("commands.tick.step.success", vec![Tag::Int(steps as i32)]),
                true,
            );
            Ok(1)
        }
        ["step", "stop"] => {
            if r.tick.steps == 0 {
                return Err(err("commands.tick.step.stop.fail", Vec::new()));
            }
            r.tick.steps = 0;
            r.success(
                translate("commands.tick.step.stop.success", Vec::new()),
                true,
            );
            Ok(1)
        }
        _ => Err(err("command.failed", Vec::new())),
    }
}

fn teleport(r: &mut Run, path: &[&str]) -> Result<i32> {
    let targets = if path.get(1) == Some(&"targets") {
        r.targets("targets", false)?
    } else {
        r.targets_or_self("__self", false)?
    };
    let (dest, entity_dest) = if path.contains(&"destination") {
        let to = r.targets("destination", false)?;
        if to.len() != 1 {
            return Err(err("argument.entity.toomany", Vec::new()));
        }
        (to[0].pos(), Some(to[0].clone()))
    } else {
        (r.vec3("location").unwrap_or(r.src.pos), None)
    };
    let rot = match r.arg("rotation") {
        Some(Arg::Pos2([yaw, pitch])) => {
            let f = |c: &Coord, b: f32| {
                if c.relative {
                    b + c.value as f32
                } else {
                    c.value as f32
                }
            };
            Some((f(yaw, r.src.rot[0]), f(pitch, r.src.rot[1])))
        }
        _ => None,
    };
    let facing = if path.contains(&"facing") {
        Some(facing_point(r)?)
    } else {
        None
    };
    for t in &targets {
        if let Target::Player { uuid, pose, .. } = t {
            let mut pose = *pose;
            (pose.x, pose.y, pose.z) = (dest[0], dest[1], dest[2]);
            if let Some((yaw, pitch)) = rot {
                (pose.yaw, pose.pitch) = (yaw, pitch);
            } else if let Some(Target::Player { pose: p, .. }) = &entity_dest {
                (pose.yaw, pose.pitch) = (p.yaw, p.pitch);
            }
            if let Some(at) = facing {
                (pose.yaw, pose.pitch) = look_at([pose.x, pose.y + EYE_HEIGHT, pose.z], at);
            }
            r.out.events.push(Event::Teleport { uuid: *uuid, pose });
        } else {
            let mut entities = lock(&r.ctx.entities);
            if let Some(e) = entities.iter_mut().find(|e| e.uuid == t.uuid()) {
                e.pos = dest;
            }
        }
    }
    let n = targets.len();
    let msg = match (&entity_dest, n) {
        (Some(d), 1) => translate(
            "commands.teleport.success.entity.single",
            vec![targets[0].display(), d.display()],
        ),
        (Some(d), _) => translate(
            "commands.teleport.success.entity.multiple",
            vec![Tag::Int(n as i32), d.display()],
        ),
        (None, 1) => translate(
            "commands.teleport.success.location.single",
            vec![
                targets[0].display(),
                six(dest[0]),
                six(dest[1]),
                six(dest[2]),
            ],
        ),
        (None, _) => translate(
            "commands.teleport.success.location.multiple",
            vec![Tag::Int(n as i32), six(dest[0]), six(dest[1]), six(dest[2])],
        ),
    };
    r.success(msg, true);
    Ok(n as i32)
}

/// How high a standing player's eyes are.
pub(super) const EYE_HEIGHT: f64 = 1.62;

/// The yaw and pitch to look from `from` at `to`, as vanilla turns.
pub(super) fn look_at(from: [f64; 3], to: [f64; 3]) -> (f32, f32) {
    let (dx, dy, dz) = (to[0] - from[0], to[1] - from[1], to[2] - from[2]);
    let flat = (dx * dx + dz * dz).sqrt();
    let wrap = |d: f32| {
        let mut d = d % 360.0;
        if d >= 180.0 {
            d -= 360.0;
        }
        if d < -180.0 {
            d += 360.0;
        }
        d
    };
    let pitch = wrap(-(dy.atan2(flat).to_degrees() as f32));
    let yaw = wrap(dz.atan2(dx).to_degrees() as f32 - 90.0);
    (yaw, pitch)
}

/// The point of a `facing` clause: `facingLocation`, or `facingEntity` at
/// its feet or eyes (`facingAnchor`).
pub(super) fn facing_point(r: &Run) -> Result<[f64; 3]> {
    if let Some(at) = r.vec3("facingLocation") {
        return Ok(at);
    }
    let to = r.targets("facingEntity", false)?;
    let Some(t) = to.first() else {
        return Err(err("argument.entity.notfound.entity", Vec::new()));
    };
    let mut at = t.pos();
    if r.string("facingAnchor").as_deref() == Some("eyes") && t.is_player() {
        at[1] += EYE_HEIGHT;
    }
    Ok(at)
}

/// An item as messages show it: `[Diamond]` with a hover card.
pub(super) fn item_display(ctx: &ServerContext, name: &str) -> Tag {
    let short = name.trim_start_matches("minecraft:");
    let key = if ctx.data.blocks.get(name).is_some() {
        format!("block.minecraft.{short}")
    } else {
        format!("item.minecraft.{short}")
    };
    let mut inner = Compound::new();
    inner.insert("text", "");
    inner.insert("extra", Tag::List(vec![translate(&key, Vec::new())]));
    let mut hover = Compound::new();
    hover.insert("action", "show_item");
    hover.insert("id", name);
    let mut c = Compound::new();
    c.insert("translate", "chat.square_brackets");
    c.insert("with", Tag::List(vec![Tag::Compound(inner)]));
    c.insert("color", "white");
    c.insert("hover_event", Tag::Compound(hover));
    Tag::Compound(c)
}

fn give(r: &mut Run) -> Result<i32> {
    let Some(Arg::Item(item)) = r.arg("item").cloned() else {
        return Err(err("command.failed", Vec::new()));
    };
    let count = r.int("count").unwrap_or(1);
    let id = r
        .ctx
        .items
        .id(&item.id)
        .ok_or_else(|| err("argument.item.id.invalid", vec![str_tag(&item.id)]))?;
    let max = r.ctx.items.max_stack(id);
    if count > max * 100 {
        return Err(err(
            "commands.give.failed.toomanyitems",
            vec![Tag::Int(max * 100), item_display(r.ctx, &item.id)],
        ));
    }
    let targets = r.targets("targets", true)?;
    for t in &targets {
        let uuid = t.uuid();
        let mut inventories = lock(&r.ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            continue;
        };
        let (_, changed) = hands.insert(id, count, max);
        for slot in changed {
            r.out.events.push(Event::InventorySlot {
                uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
    }
    let shown = item_display(r.ctx, &item.id);
    let msg = if targets.len() == 1 {
        translate(
            "commands.give.success.single",
            vec![Tag::Int(count), shown, targets[0].display()],
        )
    } else {
        translate(
            "commands.give.success.multiple",
            vec![Tag::Int(count), shown, Tag::Int(targets.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(targets.len() as i32)
}

fn clear(r: &mut Run) -> Result<i32> {
    let targets = r.targets_or_self("targets", true)?;
    let filter = match r.arg("item") {
        Some(Arg::Item(i)) => Some(i.clone()),
        _ => None,
    };
    let max_count = r.int("maxCount");
    let wanted: Option<Vec<i32>> = filter.as_ref().map(|f| {
        if f.id == "*" {
            Vec::new()
        } else if f.tag {
            r.ctx
                .commands
                .env
                .tag("minecraft:item", &f.id)
                .iter()
                .filter_map(|n| r.ctx.items.id(n))
                .collect()
        } else {
            r.ctx.items.id(&f.id).into_iter().collect()
        }
    });
    let any = filter.as_ref().is_none_or(|f| f.id == "*");
    let mut total = 0;
    for t in &targets {
        let uuid = t.uuid();
        let mut inventories = lock(&r.ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            continue;
        };
        let mut left = max_count.unwrap_or(i32::MAX);
        for slot in 0..hands.slots.len() {
            let Some(stack) = hands.slots[slot].clone() else {
                continue;
            };
            if !any && !wanted.as_ref().is_some_and(|w| w.contains(&stack.item)) {
                continue;
            }
            if max_count == Some(0) {
                total += stack.count;
                continue;
            }
            let take = stack.count.min(left);
            if take == 0 {
                break;
            }
            left -= take;
            total += take;
            hands.slots[slot] = if take == stack.count {
                hands.components.remove(&slot);
                None
            } else {
                Some(ItemStack {
                    count: stack.count - take,
                    ..stack
                })
            };
            r.out.events.push(Event::InventorySlot {
                uuid,
                slot: slot as i16,
                item: hands.slots[slot].clone(),
            });
        }
    }
    let single = targets.len() == 1;
    if total == 0 {
        return Err(if single {
            err("clear.failed.single", vec![targets[0].display()])
        } else {
            err(
                "clear.failed.multiple",
                vec![Tag::Int(targets.len() as i32)],
            )
        });
    }
    let test = max_count == Some(0);
    let key = match (test, single) {
        (true, true) => "commands.clear.test.single",
        (true, false) => "commands.clear.test.multiple",
        (false, true) => "commands.clear.success.single",
        (false, false) => "commands.clear.success.multiple",
    };
    let who = if single {
        targets[0].display()
    } else {
        Tag::Int(targets.len() as i32)
    };
    r.success(translate(key, vec![Tag::Int(total), who]), true);
    Ok(total)
}

fn kill(r: &mut Run) -> Result<i32> {
    let targets = r.targets_or_self("targets", false)?;
    let mut gone = Vec::new();
    for t in &targets {
        match t {
            Target::Player { uuid, .. } => {
                crate::vitals::kill(
                    r.ctx,
                    *uuid,
                    "minecraft:generic_kill",
                    "death.attack.genericKill",
                );
            }
            Target::Entity { id, uuid, .. } => {
                let mut entities = lock(&r.ctx.entities);
                if let Some(e) = entities.iter_mut().find(|e| e.uuid == *uuid) {
                    e.removed = true;
                }
                entities.retain(|e| !e.removed);
                drop(entities);
                let packets = lock(&r.ctx.scoreboard).forget(&crate::util::uuid_string(*uuid));
                r.out
                    .events
                    .extend(packets.into_iter().map(Event::Scoreboard));
                gone.push(*id);
            }
        }
    }
    if !gone.is_empty() {
        r.out.events.push(Event::EntitiesRemoved(gone));
    }
    let msg = if targets.len() == 1 {
        translate("commands.kill.success.single", vec![targets[0].display()])
    } else {
        translate(
            "commands.kill.success.multiple",
            vec![Tag::Int(targets.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(targets.len() as i32)
}

pub(super) fn tag_f64(tag: Option<&Tag>) -> Option<f64> {
    match tag? {
        Tag::Double(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v)),
        Tag::Int(v) => Some(f64::from(*v)),
        _ => None,
    }
}

pub(super) fn tag_i32(tag: Option<&Tag>) -> Option<i32> {
    match tag? {
        Tag::Byte(v) => Some(i32::from(*v)),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Int(v) => Some(*v),
        _ => None,
    }
}

fn summon(r: &mut Run) -> Result<i32> {
    let kind = r.string("entity").unwrap_or_default();
    let at = r.vec3("pos").unwrap_or(r.src.pos);
    let p = Position::new(
        at[0].floor() as i32,
        at[1].floor() as i32,
        at[2].floor() as i32,
    );
    if !in_world(r.ctx, p) && (at[1] < -30_000_000.0 || at[1] > 30_000_000.0) {
        return Err(err("commands.summon.invalidPosition", Vec::new()));
    }
    let nbt = match r.nbt("nbt") {
        Some(Tag::Compound(c)) => c,
        _ => Compound::new(),
    };
    let vel = match nbt.get("Motion") {
        Some(Tag::List(m)) if m.len() == 3 => [
            tag_f64(m.first()).unwrap_or(0.0),
            tag_f64(m.get(1)).unwrap_or(0.0),
            tag_f64(m.get(2)).unwrap_or(0.0),
        ],
        _ => [0.0; 3],
    };
    let entity_kind = match kind.as_str() {
        "minecraft:item" => {
            let Some(Tag::Compound(item)) = nbt.get("Item") else {
                return Err(err("commands.summon.failed", Vec::new()));
            };
            let Some(Tag::String(id)) = item.get("id") else {
                return Err(err("commands.summon.failed", Vec::new()));
            };
            let item_id = r
                .ctx
                .items
                .id(&mc_command::args::full_id(id))
                .ok_or_else(|| err("commands.summon.failed", Vec::new()))?;
            mc_entity::Kind::Item {
                item: item_id,
                count: tag_i32(item.get("count")).unwrap_or(1),
                pickup_delay: tag_i32(nbt.get("PickupDelay")).unwrap_or(0),
            }
        }
        "minecraft:falling_block" => {
            let state = match nbt.get("BlockState") {
                Some(Tag::Compound(bs)) => {
                    let name = match bs.get("Name") {
                        Some(Tag::String(n)) => mc_command::args::full_id(n),
                        _ => "minecraft:sand".into(),
                    };
                    let props: Vec<(String, String)> = match bs.get("Properties") {
                        Some(Tag::Compound(p)) => p
                            .iter()
                            .filter_map(|(k, v)| match v {
                                Tag::String(v) => Some((k.to_owned(), v.clone())),
                                _ => None,
                            })
                            .collect(),
                        _ => Vec::new(),
                    };
                    let props: Vec<(&str, &str)> = props
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_str()))
                        .collect();
                    r.ctx
                        .data
                        .blocks
                        .state_id(&name, &props)
                        .ok_or_else(|| err("commands.summon.failed", Vec::new()))?
                }
                _ => r
                    .ctx
                    .data
                    .blocks
                    .default_state("minecraft:sand")
                    .unwrap_or(0),
            };
            mc_entity::Kind::FallingBlock { state }
        }
        _ => return Err(err("commands.summon.failed", Vec::new())),
    };
    let entity = mc_entity::Entity {
        id: r.ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: entity_kind,
        pos: at,
        vel,
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    };
    r.out.events.push(Event::EntitySpawned(entity.clone()));
    lock(&r.ctx.entities).push(entity.clone());
    let shown = target::everyone(r.ctx)
        .into_iter()
        .find(|t| t.uuid() == entity.uuid)
        .map(|t| t.display())
        .unwrap_or_else(|| str_tag(&kind));
    r.success(translate("commands.summon.success", vec![shown]), true);
    Ok(1)
}

fn setblock(r: &mut Run, mode: Option<&str>) -> Result<i32> {
    let p = r.loaded_pos("pos")?;
    let (state, data) = r
        .block("block")
        .ok_or_else(|| err("command.failed", Vec::new()))?;
    let current = r.ctx.world.block(p, &r.ctx.data.blocks).unwrap_or(0);
    let allowed = match mode.unwrap_or("replace") {
        "keep" => r.ctx.data.blocks.is_air(current),
        _ => current != state || data.is_some(),
    };
    if !allowed {
        return Err(err("commands.setblock.failed", Vec::new()));
    }
    if current != state && apply(r.ctx, vec![(p, state)], mode == Some("destroy"), r.out) == 0 {
        return Err(err("commands.setblock.failed", Vec::new()));
    }
    if let Some(data) = data {
        put_block_data(r.ctx, p, state, data);
    }
    r.success(
        translate(
            "commands.setblock.success",
            vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)],
        ),
        true,
    );
    Ok(1)
}

fn fill(r: &mut Run, mode: &[&str]) -> Result<i32> {
    let a = r.loaded_pos("from")?;
    let b = r.loaded_pos("to")?;
    let (state, data) = r
        .block("block")
        .ok_or_else(|| err("command.failed", Vec::new()))?;
    let filter = match r.arg("filter") {
        Some(Arg::Block(f)) => Some(f.clone()),
        _ => None,
    };
    let mode = mode
        .iter()
        .copied()
        .find(|m| *m != "replace" && *m != "filter")
        .unwrap_or("replace");
    let (min, max) = (
        (a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        (a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    );
    let volume =
        i64::from(max.0 - min.0 + 1) * i64::from(max.1 - min.1 + 1) * i64::from(max.2 - min.2 + 1);
    let limit = block_limit(r.ctx);
    if volume > limit {
        return Err(err("commands.fill.toobig", vec![int(limit), int(volume)]));
    }
    let blocks = &r.ctx.data.blocks;
    let air = r.ctx.rules.air;
    let matches_filter = |s: u32| -> bool {
        let Some(f) = &filter else { return true };
        let name = blocks.block_name(s).unwrap_or("");
        let id_ok = if f.tag {
            r.ctx
                .commands
                .env
                .tag("minecraft:block", &f.id)
                .iter()
                .any(|i| i == name)
        } else {
            f.id == name
        };
        id_ok
            && f.properties
                .iter()
                .all(|(k, v)| blocks.with_property(s, k, v) == Some(s))
    };
    let mut changes = Vec::new();
    for y in min.1..=max.1 {
        for z in min.2..=max.2 {
            for x in min.0..=max.0 {
                let p = Position::new(x, y, z);
                let edge = x == min.0
                    || x == max.0
                    || y == min.1
                    || y == max.1
                    || z == min.2
                    || z == max.2;
                let Some(current) = r.ctx.world.block(p, blocks) else {
                    return Err(err("argument.pos.unloaded", Vec::new()));
                };
                if !matches_filter(current) {
                    continue;
                }
                let target = match mode {
                    "keep" if blocks.is_air(current) => state,
                    "keep" => continue,
                    "hollow" if edge => state,
                    "hollow" => air,
                    "outline" if edge => state,
                    "outline" => continue,
                    _ => state,
                };
                if current != target || data.is_some() {
                    changes.push((p, target));
                }
            }
        }
    }
    let placed: Vec<Position> = changes
        .iter()
        .filter(|(_, s)| *s == state)
        .map(|(p, _)| *p)
        .collect();
    let changes: Vec<(Position, u32)> = changes
        .into_iter()
        .filter(|(p, s)| r.ctx.world.block(*p, blocks) != Some(*s))
        .collect();
    let mut count = apply(r.ctx, changes, mode == "destroy", r.out);
    if let Some(data) = data {
        for p in &placed {
            put_block_data(r.ctx, *p, state, data.clone());
        }
        count = count.max(placed.len());
    }
    if count == 0 {
        return Err(err("commands.fill.failed", Vec::new()));
    }
    r.success(
        translate("commands.fill.success", vec![Tag::Int(count as i32)]),
        true,
    );
    Ok(count as i32)
}

/// `item replace (entity|block) ... with <item> [<count>]`.
fn item_replace(r: &mut Run, kind: &str) -> Result<i32> {
    let Some(Arg::Item(item)) = r.arg("item").cloned() else {
        return Err(err("command.failed", Vec::new()));
    };
    let count = r.int("count").unwrap_or(1);
    let slot = r.string("slots").unwrap_or_default();
    let id = r.ctx.items.id(&item.id).unwrap_or(0);
    let stack = (count > 0 && item.id != "minecraft:air").then_some(ItemStack {
        count,
        item: id,
        has_component_changes: false,
    });
    let shown = item_display(r.ctx, &item.id);
    if kind == "block" {
        let p = r.loaded_pos("target")?;
        let index: usize = slot
            .strip_prefix("container.")
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| err("commands.item.target.no_such_slot", vec![str_tag(&slot)]))?;
        let state = r.ctx.world.block(p, &r.ctx.data.blocks).unwrap_or(0);
        let block = r.ctx.data.blocks.block_name(state).unwrap_or("");
        let pos_args = vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)];
        let kind = crate::containers::kind_of(block)
            .ok_or_else(|| err("commands.item.target.not_a_container", pos_args.clone()))?;
        if index >= kind.slots {
            return Err(err(
                "commands.item.target.no_such_slot",
                vec![str_tag(&slot)],
            ));
        }
        crate::containers::set_slot(r.ctx, p, kind, index, stack);
        let mut args = pos_args;
        args.push(shown);
        r.success(translate("commands.item.block.set.success", args), true);
        return Ok(1);
    }
    let targets = r.targets("target", false)?;
    let mut done = 0;
    for t in &targets {
        let uuid = t.uuid();
        let index = match slot.as_str() {
            "weapon.mainhand" | "weapon" => {
                lock(&r.ctx.inventories).get(&uuid).map(|h| h.hand_slot(0))
            }
            "weapon.offhand" => Some(45),
            "armor.head" => Some(5),
            "armor.chest" => Some(6),
            "armor.legs" => Some(7),
            "armor.feet" => Some(8),
            s => match s.split_once('.') {
                Some(("hotbar", n)) => n.parse::<usize>().ok().filter(|&n| n < 9).map(|n| 36 + n),
                Some(("inventory", n)) => {
                    n.parse::<usize>().ok().filter(|&n| n < 27).map(|n| 9 + n)
                }
                _ => None,
            },
        };
        let Some(index) = index else { continue };
        let mut inventories = lock(&r.ctx.inventories);
        let Some(hands) = inventories.get_mut(&uuid) else {
            continue;
        };
        hands.slots[index] = stack.clone();
        hands.components.remove(&index);
        r.out.events.push(Event::InventorySlot {
            uuid,
            slot: index as i16,
            item: stack.clone(),
        });
        done += 1;
    }
    if done == 0 {
        return Err(err(
            "commands.item.target.no_changed.known_item",
            vec![shown, str_tag(&slot)],
        ));
    }
    let msg = if targets.len() == 1 {
        translate(
            "commands.item.entity.set.success.single",
            vec![targets[0].display(), shown],
        )
    } else {
        translate(
            "commands.item.entity.set.success.multiple",
            vec![Tag::Int(targets.len() as i32), shown],
        )
    };
    r.success(msg, true);
    Ok(done)
}

fn weather(r: &mut Run, kind: &str) -> Result<i32> {
    use crate::weather as w;
    let given = r.int("duration");
    let mut weather = lock(&r.ctx.weather);
    let (key, duration) = match kind {
        "clear" => {
            let d = given.unwrap_or_else(w::rain_delay);
            weather.set(d, 0, false, false);
            ("commands.weather.set.clear", d)
        }
        "rain" => {
            let d = given.unwrap_or_else(w::rain_duration);
            weather.set(0, d, true, false);
            ("commands.weather.set.rain", d)
        }
        _ => {
            let d = given.unwrap_or_else(w::thunder_duration);
            weather.set(0, d, true, true);
            ("commands.weather.set.thunder", d)
        }
    };
    drop(weather);
    r.success(translate(key, Vec::new()), true);
    Ok(duration)
}

fn worldborder(r: &mut Run, rest: &[&str]) -> Result<i32> {
    use crate::border::MAX_SIZE;
    let mut b = lock(&r.ctx.border);
    let result = match rest {
        ["get"] => {
            let size = b.size;
            drop(b);
            r.success(
                translate(
                    "commands.worldborder.get",
                    vec![Tag::String(format!("{size:.0}"))],
                ),
                false,
            );
            return Ok((size + 0.5).floor() as i32);
        }
        [op @ ("set" | "add"), "distance", ..] => {
            let d = match r.arg("distance") {
                Some(Arg::Double(v)) => *v,
                _ => 0.0,
            };
            let target = if *op == "add" { b.size + d } else { d };
            let ticks = r.int("time").unwrap_or(0);
            if target == b.size && ticks == 0 {
                return Err(err("commands.worldborder.set.failed.nochange", Vec::new()));
            }
            if target < 1.0 {
                return Err(err("commands.worldborder.set.failed.small", Vec::new()));
            }
            if target > MAX_SIZE {
                return Err(err(
                    "commands.worldborder.set.failed.big",
                    vec![Tag::String(format!("{MAX_SIZE:.0}"))],
                ));
            }
            let size = Tag::String(format!("{target:.1}"));
            let moved = (target - b.size).abs() as i32;
            let msg = if ticks > 0 {
                let key = if target > b.size {
                    "commands.worldborder.set.grow"
                } else {
                    "commands.worldborder.set.shrink"
                };
                b.lerp_target = target;
                b.lerp_time = i64::from(ticks) * 50;
                translate(key, vec![size, Tag::Long(i64::from(ticks / 20))])
            } else {
                b.size = target;
                b.lerp_target = target;
                b.lerp_time = 0;
                translate("commands.worldborder.set.immediate", vec![size])
            };
            (msg, moved)
        }
        ["center", "pos"] => {
            let (x, z) = match r.arg("pos") {
                Some(Arg::Pos2([x, z])) => (
                    if x.relative {
                        r.src.pos[0] + x.value
                    } else {
                        x.value
                    },
                    if z.relative {
                        r.src.pos[2] + z.value
                    } else {
                        z.value
                    },
                ),
                _ => (0.0, 0.0),
            };
            if b.center_x == x && b.center_z == z {
                return Err(err("commands.worldborder.center.failed", Vec::new()));
            }
            b.center_x = x;
            b.center_z = z;
            let args = vec![
                Tag::String(format!("{x:.2}")),
                Tag::String(format!("{z:.2}")),
            ];
            (translate("commands.worldborder.center.success", args), 0)
        }
        ["damage", "amount", "damagePerBlock"] => {
            let v = f64::from(r.float("damagePerBlock").unwrap_or(0.0));
            if b.damage_per_block == v {
                return Err(err("commands.worldborder.damage.amount.failed", Vec::new()));
            }
            b.damage_per_block = v;
            let args = vec![Tag::String(format!("{v:.2}"))];
            (
                translate("commands.worldborder.damage.amount.success", args),
                v as i32,
            )
        }
        ["damage", "buffer", "distance"] => {
            let v = f64::from(r.float("distance").unwrap_or(0.0));
            if b.safe_zone == v {
                return Err(err("commands.worldborder.damage.buffer.failed", Vec::new()));
            }
            b.safe_zone = v;
            let args = vec![Tag::String(format!("{v:.2}"))];
            (
                translate("commands.worldborder.damage.buffer.success", args),
                v as i32,
            )
        }
        ["warning", "distance", "distance"] => {
            let v = r.int("distance").unwrap_or(0);
            if b.warning_blocks == v {
                return Err(err(
                    "commands.worldborder.warning.distance.failed",
                    Vec::new(),
                ));
            }
            b.warning_blocks = v;
            (
                translate(
                    "commands.worldborder.warning.distance.success",
                    vec![Tag::Int(v)],
                ),
                v,
            )
        }
        ["warning", "time", "time"] => {
            let v = r.int("time").unwrap_or(0) / 20;
            if b.warning_time == v {
                return Err(err("commands.worldborder.warning.time.failed", Vec::new()));
            }
            b.warning_time = v;
            (
                translate(
                    "commands.worldborder.warning.time.success",
                    vec![Tag::Int(v)],
                ),
                v,
            )
        }
        _ => return Err(err("command.failed", Vec::new())),
    };
    b.save();
    drop(b);
    r.out.events.push(Event::Border);
    r.success(result.0, true);
    Ok(result.1)
}

pub(super) fn direct(r: &mut Run, uuid: u128, packet: crate::context::Direct) {
    r.out.events.push(Event::Direct { uuid, packet });
}

fn title(r: &mut Run, rest: &[&str]) -> Result<i32> {
    use crate::context::Direct;
    use mc_protocol::packets::play as p;
    let targets = r.targets("targets", true)?;
    let text = r.nbt("title").unwrap_or_else(|| Tag::from(""));
    let (key, packet) = match rest {
        ["clear"] => ("cleared", Direct::Clear(p::ClearTitles { reset: false })),
        ["reset"] => ("reset", Direct::Clear(p::ClearTitles { reset: true })),
        ["title", "title"] => ("show.title", Direct::Title(p::SetTitleText { text })),
        ["subtitle", "title"] => (
            "show.subtitle",
            Direct::Subtitle(p::SetSubtitleText { text }),
        ),
        ["actionbar", "title"] => (
            "show.actionbar",
            Direct::ActionBar(p::SetActionBarText { text }),
        ),
        ["times", ..] => (
            "times",
            Direct::Times(p::TitlesAnimation {
                fade_in: r.int("fadeIn").unwrap_or(10),
                stay: r.int("stay").unwrap_or(70),
                fade_out: r.int("fadeOut").unwrap_or(20),
            }),
        ),
        _ => return Err(err("command.failed", Vec::new())),
    };
    for t in &targets {
        direct(r, t.uuid(), packet.clone());
    }
    let msg = if targets.len() == 1 {
        translate(
            &format!("commands.title.{key}.single"),
            vec![targets[0].display()],
        )
    } else {
        translate(
            &format!("commands.title.{key}.multiple"),
            vec![Tag::Int(targets.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(targets.len() as i32)
}

const SOUND_SOURCES: [&str; 11] = [
    "master", "music", "record", "weather", "block", "hostile", "neutral", "player", "ambient",
    "voice", "ui",
];

fn playsound(r: &mut Run, path: &[&str]) -> Result<i32> {
    let sound = r.string("sound").unwrap_or_default();
    let category = path
        .get(2)
        .and_then(|c| SOUND_SOURCES.iter().position(|s| s == c))
        .unwrap_or(0) as i32;
    let targets = r.targets_or_self("targets", true)?;
    let at = r.vec3("pos").unwrap_or(r.src.pos);
    let volume = r.float("volume").unwrap_or(1.0);
    let pitch = r.float("pitch").unwrap_or(1.0);
    let id = r.ctx.data.builtin.id("minecraft:sound_event", &sound);
    for t in &targets {
        let packet = mc_protocol::packets::play::Sound {
            id,
            name: sound.clone(),
            category,
            x: at[0],
            y: at[1],
            z: at[2],
            volume,
            pitch,
            seed: crate::util::random_u64() as i64,
        };
        direct(r, t.uuid(), crate::context::Direct::Sound(packet));
    }
    let msg = if targets.len() == 1 {
        translate(
            "commands.playsound.success.single",
            vec![str_tag(&sound), targets[0].display()],
        )
    } else {
        translate(
            "commands.playsound.success.multiple",
            vec![str_tag(&sound), Tag::Int(targets.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(targets.len() as i32)
}

fn stopsound(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let targets = r.targets("targets", true)?;
    let source = rest.first().filter(|s| **s != "*").copied();
    let category = source
        .and_then(|c| SOUND_SOURCES.iter().position(|s| *s == c))
        .map(|c| c as i32);
    let sound = r.string("sound");
    for t in &targets {
        let packet = mc_protocol::packets::play::StopSound {
            category,
            name: sound.clone(),
        };
        direct(r, t.uuid(), crate::context::Direct::StopSound(packet));
    }
    let msg = match (source, &sound) {
        (None, None) => translate("commands.stopsound.success.sourceless.any", Vec::new()),
        (Some(src), None) => translate("commands.stopsound.success.source.any", vec![str_tag(src)]),
        (None, Some(s)) => translate(
            "commands.stopsound.success.sourceless.sound",
            vec![str_tag(s)],
        ),
        (Some(src), Some(s)) => translate(
            "commands.stopsound.success.source.sound",
            vec![str_tag(s), str_tag(src)],
        ),
    };
    r.success(msg, true);
    Ok(targets.len() as i32)
}

fn random(r: &mut Run, roll: bool) -> Result<i32> {
    let Some(Arg::Range(range)) = r.arg("range").cloned() else {
        return Err(err("command.failed", Vec::new()));
    };
    let min = range.min.map_or(i64::from(i32::MIN), |v| v as i64);
    let max = range.max.map_or(i64::from(i32::MAX), |v| v as i64);
    let span = max - min + 1;
    if span < 2 {
        return Err(err("commands.random.error.range_too_small", Vec::new()));
    }
    if span > i64::from(i32::MAX) {
        return Err(err("commands.random.error.range_too_large", Vec::new()));
    }
    let value = min + (crate::util::random_u64() % span as u64) as i64;
    if roll {
        let line = translate(
            "commands.random.roll",
            vec![source_name(r), int(value), int(min), int(max)],
        );
        r.out.events.push(Event::Chat(line));
    } else {
        r.success(
            translate("commands.random.sample.success", vec![int(value)]),
            false,
        );
    }
    Ok(value as i32)
}

fn clone(r: &mut Run, path: &[&str]) -> Result<i32> {
    let a = r.loaded_pos("begin")?;
    let b = r.loaded_pos("end")?;
    let dest = r.loaded_pos("destination")?;
    let masked = path.contains(&"masked");
    let filtered = path.contains(&"filtered");
    let force = path.contains(&"force");
    let moving = path.contains(&"move");
    let (min, max) = (
        (a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        (a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    );
    let size = (max.0 - min.0, max.1 - min.1, max.2 - min.2);
    let volume = i64::from(size.0 + 1) * i64::from(size.1 + 1) * i64::from(size.2 + 1);
    let limit = block_limit(r.ctx);
    if volume > limit {
        return Err(err("commands.clone.toobig", vec![int(limit), int(volume)]));
    }
    let overlaps = dest.x <= max.0
        && dest.x + size.0 >= min.0
        && dest.y <= max.1
        && dest.y + size.1 >= min.1
        && dest.z <= max.2
        && dest.z + size.2 >= min.2;
    if overlaps && !force {
        return Err(err("commands.clone.overlap", Vec::new()));
    }
    let far = Position::new(dest.x + size.0, dest.y + size.1, dest.z + size.2);
    if !in_world(r.ctx, far) {
        return Err(err("argument.pos.outofworld", Vec::new()));
    }
    let blocks = &r.ctx.data.blocks;
    let filter = match r.arg("filter") {
        Some(Arg::Block(f)) if filtered => Some(f.clone()),
        _ => None,
    };
    let mut changes = Vec::new();
    let mut cleared = Vec::new();
    for y in min.1..=max.1 {
        for z in min.2..=max.2 {
            for x in min.0..=max.0 {
                let from = Position::new(x, y, z);
                let Some(state) = r.ctx.world.block(from, blocks) else {
                    return Err(err("argument.pos.unloaded", Vec::new()));
                };
                if masked && blocks.is_air(state) {
                    continue;
                }
                if let Some(f) = &filter {
                    let name = blocks.block_name(state).unwrap_or("");
                    let ok = if f.tag {
                        r.ctx
                            .commands
                            .env
                            .tag("minecraft:block", &f.id)
                            .iter()
                            .any(|i| i == name)
                    } else {
                        f.id == name
                    };
                    if !ok {
                        continue;
                    }
                }
                let to = Position::new(dest.x + x - min.0, dest.y + y - min.1, dest.z + z - min.2);
                changes.push((to, state));
                if moving {
                    cleared.push((from, r.ctx.rules.air));
                }
            }
        }
    }
    let count = changes.len();
    if count == 0 {
        return Err(err("commands.clone.failed", Vec::new()));
    }
    if moving {
        apply(r.ctx, cleared, false, r.out);
    }
    apply(r.ctx, changes, false, r.out);
    r.success(
        translate("commands.clone.success", vec![Tag::Int(count as i32)]),
        true,
    );
    Ok(count as i32)
}

fn datapack_list(r: &mut Run, which: Option<&str>) -> Result<i32> {
    let mut hover_text = Compound::new();
    hover_text.insert("text", "");
    hover_text.insert(
        "extra",
        mc_protocol::text::list(vec![
            translate("dataPack.vanilla.name", Vec::new()),
            Tag::from("\n"),
            translate("dataPack.vanilla.description", Vec::new()),
        ]),
    );
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", Tag::Compound(hover_text));
    let source = vec![
        Tag::from("vanilla"),
        translate("pack.source.builtin", Vec::new()),
    ];
    let mut name = translate("pack.nameAndSource", source);
    style(&mut name, &[("color", "gray")]);
    let mut pack = translate("chat.square_brackets", vec![name]);
    style(&mut pack, &[("color", "green")]);
    if let Tag::Compound(c) = &mut pack {
        c.insert("hover_event", Tag::Compound(hover));
        c.insert("insertion", "vanilla");
    }
    if which != Some("available") {
        r.success(
            translate(
                "commands.datapack.list.enabled.success",
                vec![Tag::Int(1), pack],
            ),
            false,
        );
    }
    if which != Some("enabled") {
        r.success(
            translate("commands.datapack.list.available.none", Vec::new()),
            false,
        );
    }
    Ok(1)
}

fn experience_change(r: &mut Run, op: &str, levels: bool) -> Result<i32> {
    let targets = r.targets("target", true)?;
    let amount = r.int("amount").unwrap_or(0);
    let mut done = 0;
    for t in &targets {
        let mut all = lock(&r.ctx.vitals);
        let Some(v) = all.get_mut(&t.uuid()) else {
            continue;
        };
        let before = (v.xp_total, v.xp_level);
        match (op, levels) {
            ("add", false) => v.add_xp_points(amount),
            ("add", true) => v.add_xp_levels(amount),
            ("set", true) => {
                v.xp_level = amount.max(0);
            }
            _ => {
                let need = crate::vitals::xp_needed(v.xp_level);
                if amount > need {
                    if targets.len() == 1 {
                        return Err(err("commands.experience.set.points.invalid", Vec::new()));
                    }
                    continue;
                }
                v.xp_progress = amount as f32 / need as f32;
            }
        }
        let packet = v.experience_packet();
        let after = (v.xp_total, v.xp_level);
        drop(all);
        direct(r, t.uuid(), crate::context::Direct::Experience(packet));
        let mut changed = Vec::new();
        if after.0 != before.0 {
            changed.push(("xp", after.0));
        }
        if after.1 != before.1 {
            changed.push(("level", after.1));
        }
        super::scoreboard::criteria(r.ctx, &t.holder(), &changed, r.out);
        done += 1;
    }
    let kind = if levels { "levels" } else { "points" };
    let msg = if targets.len() == 1 {
        translate(
            &format!("commands.experience.{op}.{kind}.success.single"),
            vec![Tag::Int(amount), targets[0].display()],
        )
    } else {
        translate(
            &format!("commands.experience.{op}.{kind}.success.multiple"),
            vec![Tag::Int(amount), Tag::Int(targets.len() as i32)],
        )
    };
    r.success(msg, true);
    Ok(done)
}

fn experience_query(r: &mut Run, levels: bool) -> Result<i32> {
    let targets = r.targets("target", true)?;
    let Some(t) = targets.first() else {
        return Err(err("argument.entity.notfound.player", Vec::new()));
    };
    let value = lock(&r.ctx.vitals)
        .get(&t.uuid())
        .map_or(0, |v| if levels { v.xp_level } else { v.xp_points() });
    let key = if levels {
        "commands.experience.query.levels"
    } else {
        "commands.experience.query.points"
    };
    r.success(translate(key, vec![t.display(), Tag::Int(value)]), false);
    Ok(value)
}
