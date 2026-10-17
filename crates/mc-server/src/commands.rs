//! Commands from the server console or from operators in chat. They run on
//! the tick thread, in order with everything else that changes the world.
//!
//! Syntax and effects follow the public command documentation
//! (minecraft.wiki, "Commands/<name>"); feedback is plain English for now.

use mc_protocol::Position;
use mc_protocol::packets::play::{ItemStack, TickingState, TickingStep};

use crate::context::{Event, Pose, ServerContext};
use crate::level::WorldLevel;
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
    "clear", "fill", "gamemode", "gamerule", "give", "help", "item", "kick", "kill", "list",
    "save-all", "say", "setblock", "stop", "summon", "tick", "time", "tp", "teleport",
];

/// Splits a command line at spaces, but not inside `{}`/`[]` or quotes, so
/// SNBT and block states with spaces stay one argument.
fn tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None::<char>, None::<usize>);
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                c if c.is_whitespace() && depth <= 0 => {
                    if let Some(s) = start.take() {
                        out.push(&line[s..i]);
                    }
                    continue;
                }
                _ => {}
            },
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    out
}

pub fn execute(ctx: &ServerContext, tick: &mut TickControl, source: &Source, line: &str) -> Output {
    let line = line.trim().trim_start_matches('/');
    let args: Vec<&str> = tokens(line);
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
        "gamemode" => gamemode(ctx, source, args, &mut out),
        "give" => give(ctx, args, &mut out),
        "kill" => kill(ctx, source, args),
        "clear" => clear(ctx, source, args, &mut out),
        "summon" => summon(ctx, source, args, &mut out),
        "item" => item_command(ctx, source, args, &mut out),
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

/// A block with optional block entity data: `chest[facing=east]{Items:[...]}`.
fn parse_block(ctx: &ServerContext, text: &str) -> Result<(u32, Option<mc_nbt::Compound>)> {
    // Properties come before the data: `name[...]{...}`.
    let first_brace = text.find('{').unwrap_or(text.len());
    let close = match text.find('[') {
        Some(open) if open < first_brace => text[open..].find(']').map_or(0, |i| open + i + 1),
        _ => 0,
    };
    let brace = text[close..].find('{').map(|i| i + close);
    match brace {
        Some(b) => {
            let state = parse_state(ctx, &text[..b])?;
            match mc_nbt::parse_snbt(&text[b..]) {
                Ok(mc_nbt::Tag::Compound(c)) => Ok((state, Some(c))),
                Ok(_) => Err("Expected a compound".into()),
                Err(e) => Err(format!("Invalid NBT: {e}")),
            }
        }
        None => Ok((parse_state(ctx, text)?, None)),
    }
}

/// Block entity id of a block (most are named like the block).
fn block_entity_id(name: &str) -> String {
    let short = name.trim_start_matches("minecraft:");
    let id = if short.ends_with("_hanging_sign") {
        "hanging_sign"
    } else if short.ends_with("_sign") {
        "sign"
    } else if short.ends_with("_bed") {
        "bed"
    } else if short.ends_with("_banner") {
        "banner"
    } else if short.ends_with("shulker_box") {
        "shulker_box"
    } else if short.ends_with("_skull") || short.ends_with("_head") {
        "skull"
    } else {
        short
    };
    format!("minecraft:{id}")
}

/// Merges command data into the block entity at `p`.
fn put_block_data(ctx: &ServerContext, p: Position, state: u32, data: mc_nbt::Compound) {
    let blocks = &ctx.data.blocks;
    let name = blocks.block_name(state).unwrap_or("");
    let mut be = ctx
        .world
        .block_entity(p, blocks)
        .unwrap_or(mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: block_entity_id(name),
            data: mc_nbt::Compound::new(),
        });
    for (key, value) in data.iter() {
        be.data.insert(key, value.clone());
    }
    ctx.world.set_block_entity(be, blocks);
    ctx.containers
        .lock()
        .expect("containers lock")
        .remove(&(p.x, p.y, p.z));
    ctx.container_updates
        .lock()
        .expect("container updates lock")
        .push(p);
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

/// Applies changes with block updates and announces everything that changed
/// to everyone (nobody predicted it). Returns how many of `changes` took.
fn apply(
    ctx: &ServerContext,
    changes: Vec<(Position, u32)>,
    destroy: bool,
    out: &mut Output,
) -> usize {
    let blocks = &ctx.data.blocks;
    let mut level = WorldLevel::new(ctx);
    let mut count = 0;
    for (p, state) in changes {
        let old = ctx.world.block(p, blocks);
        let state = mc_tick::placement_state(&level, level.logic(), crate::level::to_pos(p), state);
        if level.set_block(p, state) {
            crate::containers::placed(ctx, p, state);
            count += 1;
            if destroy && let Some(old) = old.filter(|&o| !blocks.is_air(o)) {
                out.events.push(Event::BlockBroken {
                    by: 0,
                    position: p,
                    state: old,
                });
            }
        }
    }
    let changed = level.into_changes();
    if !changed.is_empty() {
        out.events.push(Event::BlocksChanged {
            by: 0,
            changes: changed,
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
    let (state, data) = parse_block(ctx, args[3])?;
    let mode = args.get(4).copied().unwrap_or("replace");
    if !in_world(ctx, p) {
        return Err("That position is out of the world".into());
    }
    let current = ctx
        .world
        .block(p, &ctx.data.blocks)
        .ok_or("That position is not loaded")?;
    let allowed = match mode {
        "replace" | "destroy" => current != state || data.is_some(),
        "keep" => ctx.data.blocks.is_air(current),
        _ => return Err(help()),
    };
    if !allowed {
        return Err("Could not set the block".into());
    }
    if current != state && apply(ctx, vec![(p, state)], mode == "destroy", out) == 0 {
        return Err("Could not set the block".into());
    }
    if let Some(data) = data {
        put_block_data(ctx, p, state, data);
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
    let (state, data) = parse_block(ctx, args[6])?;
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
    let placed: Vec<Position> = changes
        .iter()
        .filter(|(_, s)| *s == state)
        .map(|(p, _)| *p)
        .collect();
    let count = apply(ctx, changes, mode == "destroy", out);
    if count == 0 {
        return Err("No blocks were filled".into());
    }
    if let Some(data) = data {
        for p in placed {
            put_block_data(ctx, p, state, data.clone());
        }
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

fn gamemode(
    ctx: &ServerContext,
    source: &Source,
    args: &[&str],
    out: &mut Output,
) -> Result<String> {
    let help = || usage("gamemode (survival|creative|adventure|spectator) [<player>]");
    let Some(mode_name) = args.first() else {
        return Err(help());
    };
    let mode = match *mode_name {
        "survival" => 0,
        "creative" => 1,
        "adventure" => 2,
        "spectator" => 3,
        _ => return Err(help()),
    };
    let (uuid, name) = match (args.get(1), source) {
        (Some(who), _) => (find_player(ctx, who)?.0, (*who).to_owned()),
        (None, Source::Player { uuid, name, .. }) => (*uuid, name.clone()),
        (None, Source::Console) => return Err(help()),
    };
    if let Some(player) = lock(&ctx.players).get_mut(&uuid) {
        player.game_mode = mode;
    }
    out.events.push(Event::GameMode { uuid, mode });
    Ok(format!("Set {name}'s game mode to {mode_name}"))
}

fn give(ctx: &ServerContext, args: &[&str], out: &mut Output) -> Result<String> {
    let help = || usage("give <player> <item> [<count>]");
    let (Some(who), Some(item)) = (args.first(), args.get(1)) else {
        return Err(help());
    };
    let count: i32 = match args.get(2) {
        Some(c) => c.parse().map_err(|_| format!("Invalid count: {c}"))?,
        None => 1,
    };
    let name = if item.contains(':') {
        (*item).to_owned()
    } else {
        format!("minecraft:{item}")
    };
    let id = ctx
        .data
        .builtin
        .id("minecraft:item", &name)
        .ok_or_else(|| format!("Unknown item '{name}'"))?;
    let (uuid, _) = find_player(ctx, who)?;
    let max = ctx.items.max_stack(id);
    let mut inventories = lock(&ctx.inventories);
    let hands = inventories
        .get_mut(&uuid)
        .ok_or("That player has no inventory")?;
    let (given, changed) = hands.insert(id, count, max);
    for slot in changed {
        out.events.push(Event::InventorySlot {
            uuid,
            slot: slot as i16,
            item: hands.slots[slot].clone(),
        });
    }
    Ok(format!("Gave {given} [{name}] to {who}"))
}

fn kill(ctx: &ServerContext, source: &Source, args: &[&str]) -> Result<String> {
    let (uuid, name) = match (args.first(), source) {
        (Some(who), _) => (find_player(ctx, who)?.0, (*who).to_owned()),
        (None, Source::Player { uuid, name, .. }) => (*uuid, name.clone()),
        (None, Source::Console) => return Err(usage("kill <player>")),
    };
    // Bypasses game mode and immunity, like vanilla's /kill.
    crate::vitals::kill(
        ctx,
        uuid,
        "minecraft:generic_kill",
        "death.attack.genericKill",
    );
    Ok(format!("Killed {name}"))
}

fn clear(ctx: &ServerContext, source: &Source, args: &[&str], out: &mut Output) -> Result<String> {
    let (uuid, name) = match (args.first(), source) {
        (Some(who), _) => (find_player(ctx, who)?.0, (*who).to_owned()),
        (None, Source::Player { uuid, name, .. }) => (*uuid, name.clone()),
        (None, Source::Console) => return Err(usage("clear <player>")),
    };
    let mut inventories = lock(&ctx.inventories);
    let hands = inventories
        .get_mut(&uuid)
        .ok_or("That player has no inventory")?;
    let mut removed = 0;
    for slot in 0..hands.slots.len() {
        if let Some(stack) = hands.slots[slot].take() {
            removed += stack.count;
            out.events.push(Event::InventorySlot {
                uuid,
                slot: slot as i16,
                item: None,
            });
        }
    }
    hands.components.clear();
    Ok(format!("Removed {removed} item(s) from player {name}"))
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

fn tag_f64(tag: Option<&mc_nbt::Tag>) -> Option<f64> {
    match tag? {
        mc_nbt::Tag::Double(v) => Some(*v),
        mc_nbt::Tag::Float(v) => Some(f64::from(*v)),
        mc_nbt::Tag::Int(v) => Some(f64::from(*v)),
        _ => None,
    }
}

fn tag_i32(tag: Option<&mc_nbt::Tag>) -> Option<i32> {
    match tag? {
        mc_nbt::Tag::Byte(v) => Some(i32::from(*v)),
        mc_nbt::Tag::Short(v) => Some(i32::from(*v)),
        mc_nbt::Tag::Int(v) => Some(*v),
        _ => None,
    }
}

/// `summon <entity> [<x> <y> <z>] [<nbt>]`: dropped items (`Item:{id,
/// count}`, `Motion`, `PickupDelay`) and falling blocks (`BlockState:{Name,
/// Properties}`) for now.
fn summon(ctx: &ServerContext, source: &Source, args: &[&str], out: &mut Output) -> Result<String> {
    let help = || usage("summon <entity> [<x> <y> <z>] [<nbt>]");
    let Some(kind) = args.first() else {
        return Err(help());
    };
    let kind = kind.trim_start_matches("minecraft:");
    let origin = source.origin(ctx);
    let at = if args.len() >= 4 {
        position(&args[1..4], origin)?
    } else {
        origin
    };
    let nbt = match args.get(4) {
        Some(text) => match mc_nbt::parse_snbt(text) {
            Ok(mc_nbt::Tag::Compound(c)) => c,
            Ok(_) => return Err("Expected a compound".into()),
            Err(e) => return Err(format!("Invalid NBT: {e}")),
        },
        None => mc_nbt::Compound::new(),
    };
    let vel = match nbt.get("Motion") {
        Some(mc_nbt::Tag::List(m)) if m.len() == 3 => [
            tag_f64(m.first()).unwrap_or(0.0),
            tag_f64(m.get(1)).unwrap_or(0.0),
            tag_f64(m.get(2)).unwrap_or(0.0),
        ],
        _ => [0.0; 3],
    };
    let entity_kind = match kind {
        "item" => {
            let Some(mc_nbt::Tag::Compound(item)) = nbt.get("Item") else {
                return Err("Unable to summon entity: an item needs Item:{id,count}".into());
            };
            let Some(mc_nbt::Tag::String(id)) = item.get("id") else {
                return Err("Item needs an id".into());
            };
            let name = if id.contains(':') {
                id.clone()
            } else {
                format!("minecraft:{id}")
            };
            let item_id = ctx
                .items
                .id(&name)
                .ok_or_else(|| format!("Unknown item '{name}'"))?;
            mc_entity::Kind::Item {
                item: item_id,
                count: tag_i32(item.get("count")).unwrap_or(1),
                pickup_delay: tag_i32(nbt.get("PickupDelay")).unwrap_or(0),
            }
        }
        "falling_block" => {
            let state = match nbt.get("BlockState") {
                Some(mc_nbt::Tag::Compound(bs)) => {
                    let Some(mc_nbt::Tag::String(name)) = bs.get("Name") else {
                        return Err("BlockState needs a Name".into());
                    };
                    let props = match bs.get("Properties") {
                        Some(mc_nbt::Tag::Compound(p)) => p
                            .iter()
                            .filter_map(|(k, v)| match v {
                                mc_nbt::Tag::String(v) => Some(format!("{k}={v}")),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(","),
                        _ => String::new(),
                    };
                    parse_state(ctx, &format!("{name}[{props}]"))?
                }
                _ => ctx.data.blocks.default_state("minecraft:sand").unwrap_or(0),
            };
            mc_entity::Kind::FallingBlock { state }
        }
        other => {
            return Err(format!(
                "Unable to summon entity: {other} is not supported yet"
            ));
        }
    };
    let entity = mc_entity::Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: entity_kind,
        pos: [at.0, at.1, at.2],
        vel,
        on_ground: false,
        age: 0,
        removed: false,
    };
    out.events.push(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
    Ok(format!("Summoned new {kind}"))
}

/// `item replace block <x> <y> <z> container.<n> with <item> [<count>]`
/// and `item replace entity <player> <slot> with <item> [<count>]` (slots
/// `hotbar.<n>`, `inventory.<n>`, `weapon.mainhand`, `weapon.offhand`,
/// `armor.head|chest|legs|feet`).
fn item_command(
    ctx: &ServerContext,
    source: &Source,
    args: &[&str],
    out: &mut Output,
) -> Result<String> {
    let help = || usage("item replace (block <pos>|entity <player>) <slot> with <item> [<count>]");
    if args.first() != Some(&"replace") {
        return Err(help());
    }
    let (target, rest) = match args.get(1) {
        Some(&"block") if args.len() >= 5 => (None, &args[5..]),
        Some(&"entity") if args.len() >= 3 => (Some(args[2]), &args[3..]),
        _ => return Err(help()),
    };
    let [slot, with, item, more @ ..] = rest else {
        return Err(help());
    };
    if *with != "with" {
        return Err(help());
    }
    let name = if item.contains(':') {
        (*item).to_owned()
    } else {
        format!("minecraft:{item}")
    };
    let id = ctx
        .items
        .id(&name)
        .ok_or_else(|| format!("Unknown item '{name}'"))?;
    let count: i32 = match more.first() {
        Some(c) => c.parse().map_err(|_| format!("Invalid count: {c}"))?,
        None => 1,
    };
    let stack = (count > 0 && name != "minecraft:air").then_some(ItemStack {
        count,
        item: id,
        has_component_changes: false,
    });
    match target {
        None => {
            let p = block_position(&args[2..5], source.origin(ctx))?;
            let index: usize = slot
                .strip_prefix("container.")
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("Unknown slot '{slot}'"))?;
            let state = ctx
                .world
                .block(p, &ctx.data.blocks)
                .ok_or("That position is not loaded")?;
            let block = ctx.data.blocks.block_name(state).unwrap_or("");
            let kind =
                crate::containers::kind_of(block).ok_or("Target block is not a container")?;
            if index >= kind.slots {
                return Err(format!("Unknown slot '{slot}'"));
            }
            crate::containers::set_slot(ctx, p, kind, index, stack);
            Ok(format!(
                "Replaced a slot at {}, {}, {} with {count} [{name}]",
                p.x, p.y, p.z
            ))
        }
        Some(who) => {
            let (uuid, _) = find_player(ctx, who)?;
            let index = match *slot {
                "weapon.mainhand" | "weapon" => lock(&ctx.inventories)
                    .get(&uuid)
                    .map_or(36, |h| h.hand_slot(0)),
                "weapon.offhand" => 45,
                "armor.head" => 5,
                "armor.chest" => 6,
                "armor.legs" => 7,
                "armor.feet" => 8,
                s => match s.split_once('.') {
                    Some(("hotbar", n)) => {
                        n.parse::<usize>().ok().filter(|&n| n < 9).map(|n| 36 + n)
                    }
                    Some(("inventory", n)) => {
                        n.parse::<usize>().ok().filter(|&n| n < 27).map(|n| 9 + n)
                    }
                    _ => None,
                }
                .ok_or_else(|| format!("Unknown slot '{slot}'"))?,
            };
            let mut inventories = lock(&ctx.inventories);
            let hands = inventories
                .get_mut(&uuid)
                .ok_or("That player has no inventory")?;
            hands.slots[index] = stack.clone();
            hands.components.remove(&index);
            out.events.push(Event::InventorySlot {
                uuid,
                slot: index as i16,
                item: stack,
            });
            Ok(format!("Replaced a slot on {who} with {count} [{name}]"))
        }
    }
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
