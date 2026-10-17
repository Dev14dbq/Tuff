//! Commands from the server console or from operators in chat. They run on
//! the tick thread, in order with everything else that changes the world.
//!
//! Syntax and effects follow the public command documentation
//! (minecraft.wiki, "Commands/<name>"); feedback is plain English for now.

use mc_protocol::Position;
use mc_protocol::packets::play::{TickingState, TickingStep};

use crate::context::{Event, Pose, ServerContext};
use crate::tick::{TickControl, lock};

/// Who runs a command.
#[derive(Clone, Debug)]
pub enum Source {
    Console,
    Player {
        uuid: u128,
        name: String,
        pose: Pose,
    },
}

impl Source {
    fn name(&self) -> &str {
        match self {
            Source::Console => "Server",
            Source::Player { name, .. } => name,
        }
    }

    /// Where `~` coordinates start.
    fn origin(&self, ctx: &ServerContext) -> (f64, f64, f64) {
        match self {
            Source::Console => ctx.world.spawn,
            Source::Player { pose, .. } => (pose.x, pose.y, pose.z),
        }
    }
}

/// What a command did.
#[derive(Debug, Default)]
pub struct Output {
    pub lines: Vec<String>,
    pub events: Vec<Event>,
}

/// Most blocks one `fill` may change (vanilla's default limit).
const FILL_LIMIT: i64 = 32768;

pub const COMMANDS: &[&str] = &[
    "fill", "gamerule", "help", "kick", "list", "save-all", "say", "setblock", "stop", "tick",
    "time", "tp", "teleport",
];

pub fn execute(ctx: &ServerContext, tick: &mut TickControl, source: &Source, line: &str) -> Output {
    let line = line.trim().trim_start_matches('/');
    let args: Vec<&str> = line.split_whitespace().collect();
    let mut out = Output::default();
    let Some((&name, args)) = args.split_first() else {
        return out;
    };
    let result = match name {
        "help" => Ok(format!("Commands: {}", COMMANDS.join(", "))),
        "say" => say(source, args, &mut out),
        "list" => Ok(list(ctx)),
        "stop" => {
            ctx.shutdown.notify_one();
            Ok("Stopping the server".into())
        }
        "save-all" => {
            crate::save(ctx);
            Ok("Saved the game".into())
        }
        "setblock" => setblock(ctx, source, args, &mut out),
        "fill" => fill(ctx, source, args, &mut out),
        "time" => time(ctx, args, &mut out),
        "gamerule" => gamerule(ctx, args, &mut out),
        "tick" => tick_command(tick, args, &mut out),
        "tp" | "teleport" => teleport(ctx, source, args, &mut out),
        "kick" => kick(ctx, args, &mut out),
        other => Err(format!("Unknown command: {other}")),
    };
    match result {
        Ok(line) if !line.is_empty() => out.lines.push(line),
        Ok(_) => {}
        Err(err) => out.lines.push(err),
    }
    out
}

type Result<T> = std::result::Result<T, String>;

fn usage(text: &str) -> String {
    format!("Usage: {text}")
}

fn say(source: &Source, args: &[&str], out: &mut Output) -> Result<String> {
    if args.is_empty() {
        return Err(usage("say <message>"));
    }
    let line = format!("[{}] {}", source.name(), args.join(" "));
    out.events
        .push(Event::Chat(mc_protocol::text::plain(&line)));
    Ok(String::new())
}

fn list(ctx: &ServerContext) -> String {
    let mut names: Vec<String> = lock(&ctx.players)
        .values()
        .map(|p| p.profile.name.clone())
        .collect();
    names.sort();
    format!(
        "There are {} of a max of {} players online: {}",
        names.len(),
        ctx.config.max_players,
        names.join(", ")
    )
}

fn coordinate(token: &str, base: f64) -> Result<f64> {
    let bad = || format!("Invalid coordinate: {token}");
    match token.strip_prefix('~') {
        Some("") => Ok(base),
        Some(offset) => offset.parse::<f64>().map(|o| base + o).map_err(|_| bad()),
        None => token.parse::<f64>().map_err(|_| bad()),
    }
}

fn position(tokens: &[&str], origin: (f64, f64, f64)) -> Result<(f64, f64, f64)> {
    let [x, y, z] = tokens else {
        return Err("Expected three coordinates".into());
    };
    Ok((
        coordinate(x, origin.0)?,
        coordinate(y, origin.1)?,
        coordinate(z, origin.2)?,
    ))
}

fn block_position(tokens: &[&str], origin: (f64, f64, f64)) -> Result<Position> {
    let (x, y, z) = position(tokens, origin)?;
    Ok(Position::new(
        x.floor() as i32,
        y.floor() as i32,
        z.floor() as i32,
    ))
}

/// `stone`, `minecraft:oak_stairs[facing=east,half=top]`.
pub fn parse_state(ctx: &ServerContext, text: &str) -> Result<u32> {
    let (name, properties) = match text.split_once('[') {
        Some((name, rest)) => {
            let inner = rest
                .strip_suffix(']')
                .ok_or_else(|| format!("Unclosed properties in {text}"))?;
            let pairs: Vec<(&str, &str)> = inner
                .split(',')
                .filter(|p| !p.is_empty())
                .map(|p| {
                    p.split_once('=')
                        .ok_or_else(|| format!("Expected property=value in {text}"))
                })
                .collect::<Result<_>>()?;
            (name, pairs)
        }
        None => (text, Vec::new()),
    };
    let full = if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    };
    if ctx.data.blocks.get(&full).is_none() {
        return Err(format!("Unknown block type '{full}'"));
    }
    ctx.data
        .blocks
        .state_id(&full, &properties)
        .ok_or_else(|| format!("Block {full} does not have those properties: {text}"))
}

fn in_world(ctx: &ServerContext, p: Position) -> bool {
    p.y >= ctx.world.min_y && p.y < ctx.world.min_y + ctx.world.height as i32
}

/// Applies changes and announces them to everyone (nobody predicted them).
fn apply(
    ctx: &ServerContext,
    changes: Vec<(Position, u32)>,
    destroy: bool,
    out: &mut Output,
) -> usize {
    let blocks = &ctx.data.blocks;
    let mut applied = Vec::with_capacity(changes.len());
    for (p, state) in changes {
        match ctx.world.set_block(p, state, blocks, &ctx.rules) {
            Some(old) if old != state => {
                if destroy && !blocks.is_air(old) {
                    out.events.push(Event::BlockBroken {
                        by: 0,
                        position: p,
                        state: old,
                    });
                }
                applied.push((p, state));
            }
            _ => {}
        }
    }
    let count = applied.len();
    if count > 0 {
        out.events.push(Event::BlocksChanged {
            by: 0,
            changes: applied,
        });
    }
    count
}

fn setblock(
    ctx: &ServerContext,
    source: &Source,
    args: &[&str],
    out: &mut Output,
) -> Result<String> {
    let help = || usage("setblock <x> <y> <z> <block> [destroy|keep|replace]");
    if args.len() < 4 {
        return Err(help());
    }
    let p = block_position(&args[..3], source.origin(ctx))?;
    let state = parse_state(ctx, args[3])?;
    let mode = args.get(4).copied().unwrap_or("replace");
    if !in_world(ctx, p) {
        return Err("That position is out of the world".into());
    }
    let current = ctx
        .world
        .block(p, &ctx.data.blocks)
        .ok_or("That position is not loaded")?;
    let allowed = match mode {
        "replace" | "destroy" => current != state,
        "keep" => ctx.data.blocks.is_air(current),
        _ => return Err(help()),
    };
    if !allowed || apply(ctx, vec![(p, state)], mode == "destroy", out) == 0 {
        return Err("Could not set the block".into());
    }
    Ok(format!("Changed the block at {}, {}, {}", p.x, p.y, p.z))
}

fn fill(ctx: &ServerContext, source: &Source, args: &[&str], out: &mut Output) -> Result<String> {
    let help =
        || usage("fill <from> <to> <block> [destroy|hollow|keep|outline|replace [<filter>]]");
    if args.len() < 7 {
        return Err(help());
    }
    let origin = source.origin(ctx);
    let (a, b) = (
        block_position(&args[..3], origin)?,
        block_position(&args[3..6], origin)?,
    );
    let state = parse_state(ctx, args[6])?;
    let mode = args.get(7).copied().unwrap_or("replace");
    let filter = match (mode, args.get(8)) {
        ("replace", Some(filter)) => Some(parse_state(ctx, filter)?),
        _ => None,
    };
    let (min, max) = (
        (a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        (a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    );
    let volume =
        i64::from(max.0 - min.0 + 1) * i64::from(max.1 - min.1 + 1) * i64::from(max.2 - min.2 + 1);
    if volume > FILL_LIMIT {
        return Err(format!(
            "Too many blocks in the specified area (maximum {FILL_LIMIT}, specified {volume})"
        ));
    }
    if !in_world(ctx, Position::new(min.0, min.1, min.2))
        || !in_world(ctx, Position::new(max.0, max.1, max.2))
    {
        return Err("That position is out of the world".into());
    }
    let blocks = &ctx.data.blocks;
    let air = ctx.rules.air;
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
                let Some(current) = ctx.world.block(p, blocks) else {
                    return Err("That position is not loaded".into());
                };
                let target = match mode {
                    "replace" | "destroy" => match filter {
                        Some(f) if current != f => continue,
                        _ => state,
                    },
                    "keep" if blocks.is_air(current) => state,
                    "keep" => continue,
                    "hollow" if edge => state,
                    "hollow" => air,
                    "outline" if edge => state,
                    "outline" => continue,
                    _ => return Err(help()),
                };
                if current != target {
                    changes.push((p, target));
                }
            }
        }
    }
    let count = apply(ctx, changes, mode == "destroy", out);
    if count == 0 {
        return Err("No blocks were filled".into());
    }
    Ok(format!("Successfully filled {count} block(s)"))
}

fn time(ctx: &ServerContext, args: &[&str], out: &mut Output) -> Result<String> {
    let help = || usage("time (set <time>|add <time>|query daytime|gametime|day)");
    let mut time = lock(&ctx.time);
    // The overworld's clock is the one `time` changes.
    let index = time
        .clocks
        .iter()
        .position(|c| c.name == "minecraft:overworld")
        .ok_or("This world has no overworld clock")?;
    let parse = |t: &str| -> Result<i64> {
        Ok(match t {
            "day" => 1000,
            "noon" => 6000,
            "night" => 13000,
            "midnight" => 18000,
            _ => {
                let (number, scale) = match t.chars().last() {
                    Some('d') => (&t[..t.len() - 1], 24000.0),
                    Some('s') => (&t[..t.len() - 1], 20.0),
                    Some('t') => (&t[..t.len() - 1], 1.0),
                    _ => (t, 1.0),
                };
                let value: f64 = number.parse().map_err(|_| format!("Invalid time: {t}"))?;
                (value * scale).round() as i64
            }
        })
    };
    match args {
        ["set", t] => {
            let value = parse(t)?;
            time.clocks[index].ticks = value;
        }
        ["add", t] => {
            let value = parse(t)?;
            time.clocks[index].ticks += value;
        }
        ["query", "daytime"] => {
            return Ok(format!(
                "The time is {}",
                time.clocks[index].ticks.rem_euclid(24000)
            ));
        }
        ["query", "gametime"] => return Ok(format!("The time is {}", time.age)),
        ["query", "day"] => {
            return Ok(format!(
                "The time is {}",
                time.clocks[index].ticks.div_euclid(24000)
            ));
        }
        _ => return Err(help()),
    }
    let mut packet = time.packet(true);
    packet.clocks.retain(|c| c.clock == time.clocks[index].id);
    out.events.push(Event::Time(packet));
    Ok(format!("Set the time to {}", time.clocks[index].ticks))
}

fn gamerule(ctx: &ServerContext, args: &[&str], out: &mut Output) -> Result<String> {
    match args {
        [rule] if rule.trim_start_matches("minecraft:") == "advance_time" => {
            let running = lock(&ctx.time).clocks.iter().any(|c| c.running);
            Ok(format!(
                "Gamerule advance_time is currently set to: {running}"
            ))
        }
        [rule, value] if rule.trim_start_matches("minecraft:") == "advance_time" => {
            let running = match *value {
                "true" => true,
                "false" => false,
                _ => return Err(format!("Invalid boolean: {value}")),
            };
            let mut time = lock(&ctx.time);
            for clock in &mut time.clocks {
                clock.running = running;
            }
            out.events.push(Event::Time(time.packet(true)));
            Ok(format!("Game rule advance_time is now set to {running}"))
        }
        [rule, ..] => Err(format!("Game rule {rule} is not supported yet")),
        [] => Err(usage("gamerule <rule> [<value>]")),
    }
}

fn tick_command(tick: &mut TickControl, args: &[&str], out: &mut Output) -> Result<String> {
    let state = |tick: &TickControl| {
        Event::TickingState(TickingState {
            tick_rate: tick.rate,
            frozen: tick.frozen,
        })
    };
    match args {
        ["query"] => Ok(format!(
            "The game is {}; target tick rate {} per second",
            if tick.frozen {
                "frozen"
            } else {
                "running normally"
            },
            tick.rate
        )),
        ["rate", rate] => {
            let rate: f32 = rate
                .parse()
                .map_err(|_| format!("Invalid number: {rate}"))?;
            if !(1.0..=10000.0).contains(&rate) {
                return Err("The tick rate must be between 1 and 10000".into());
            }
            tick.rate = rate;
            out.events.push(state(tick));
            Ok(format!("Set the target tick rate to {rate} per second"))
        }
        ["freeze"] => {
            tick.frozen = true;
            out.events.push(state(tick));
            Ok("The game is frozen".into())
        }
        ["unfreeze"] => {
            tick.frozen = false;
            tick.steps = 0;
            out.events.push(state(tick));
            Ok("The game is running".into())
        }
        ["step", rest @ ..] => {
            if !tick.frozen {
                return Err("The game must be frozen first to step".into());
            }
            let steps: u32 = match rest {
                [] => 1,
                [n] => n.parse().map_err(|_| format!("Invalid number: {n}"))?,
                _ => return Err(usage("tick step [<time>]")),
            };
            tick.steps += steps;
            out.events.push(Event::TickingStep(TickingStep {
                steps: steps as i32,
            }));
            Ok(format!("Stepping {steps} tick(s)"))
        }
        _ => Err(usage(
            "tick (query|rate <rate>|freeze|unfreeze|step [<time>])",
        )),
    }
}

fn find_player(ctx: &ServerContext, name: &str) -> Result<(u128, Pose)> {
    lock(&ctx.players)
        .values()
        .find(|p| p.profile.name.eq_ignore_ascii_case(name))
        .and_then(|p| p.pose.map(|pose| (p.profile.uuid, pose)))
        .ok_or_else(|| format!("No player was found: {name}"))
}

fn teleport(
    ctx: &ServerContext,
    source: &Source,
    args: &[&str],
    out: &mut Output,
) -> Result<String> {
    let (target, destination) = match (args, source) {
        ([x, y, z], Source::Player { uuid, pose, name }) => {
            let (dx, dy, dz) = position(&[x, y, z], (pose.x, pose.y, pose.z))?;
            ((*uuid, *pose, name.clone()), (dx, dy, dz))
        }
        ([who, x, y, z], _) => {
            let (uuid, pose) = find_player(ctx, who)?;
            let destination = position(&[x, y, z], (pose.x, pose.y, pose.z))?;
            ((uuid, pose, (*who).to_owned()), destination)
        }
        ([who, to], _) => {
            let (uuid, pose) = find_player(ctx, who)?;
            let (_, there) = find_player(ctx, to)?;
            ((uuid, pose, (*who).to_owned()), (there.x, there.y, there.z))
        }
        _ => return Err(usage("tp [<player>] <x> <y> <z> | tp <player> <player>")),
    };
    let (uuid, pose, name) = target;
    let pose = Pose {
        x: destination.0,
        y: destination.1,
        z: destination.2,
        ..pose
    };
    out.events.push(Event::Teleport { uuid, pose });
    Ok(format!(
        "Teleported {name} to {:.2}, {:.2}, {:.2}",
        pose.x, pose.y, pose.z
    ))
}

fn kick(ctx: &ServerContext, args: &[&str], out: &mut Output) -> Result<String> {
    let Some((who, reason)) = args.split_first() else {
        return Err(usage("kick <player> [<reason>]"));
    };
    let (uuid, _) = find_player(ctx, who)?;
    let reason = if reason.is_empty() {
        "Kicked by an operator".to_owned()
    } else {
        reason.join(" ")
    };
    out.events.push(Event::Kick {
        uuid,
        reason: reason.clone(),
    });
    Ok(format!("Kicked {who}: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates() {
        assert_eq!(coordinate("~", 5.5), Ok(5.5));
        assert_eq!(coordinate("~-2", 5.5), Ok(3.5));
        assert_eq!(coordinate("-12", 5.5), Ok(-12.0));
        assert!(coordinate("x", 0.0).is_err());
        let p = block_position(&["~1", "64", "-0.5"], (0.5, 0.0, 0.0)).unwrap();
        assert_eq!((p.x, p.y, p.z), (1, 64, -1));
    }
}
