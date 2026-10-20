//! Commands from the server console and from players, parsed against the
//! tree vanilla registers (`mc_command`) and answered with vanilla's
//! messages (translation keys and arguments as a vanilla server sends
//! them, recorded by `cargo xtask observe-commands`). They run on the tick
//! thread, in order with everything else that changes the world.

mod bossbar;
#[cfg(test)]
mod coverage;
mod data;
mod execute;
mod devtools;
mod functions;
mod item;
mod living;
mod loot;
mod place;
mod places;
mod rare;
mod run;
mod scoreboard;
mod scores;
mod suggest;
mod target;
mod team;
mod world;

use mc_command::dispatch::{self, Context};
use mc_command::env::DataEnv;
use mc_command::lang::Lang;
use mc_command::{CommandError, Tree};
use mc_nbt::{Compound, Tag};
use tracing::{info, warn};

use crate::context::{Event, Pose, ServerContext};
use crate::tick::{TickControl, lock};
pub(crate) use functions::tick as run_functions;

pub use suggest::suggest;

/// The command tree with what its arguments are checked against, and the
/// English text for the console.
#[derive(Debug)]
pub struct Commands {
    pub tree: Tree,
    pub env: DataEnv,
    pub lang: Lang,
    /// The Commands packet for each permission level.
    packets: Vec<Vec<u8>>,
}

impl Commands {
    pub fn load(data: &std::path::Path) -> Self {
        let tree = Tree::load(data).unwrap_or_else(|e| {
            warn!("no command tree ({e}); run cargo xtask observe-commands");
            Tree::default()
        });
        let packets = if tree.nodes.is_empty() {
            Vec::new()
        } else {
            (0..=4).map(|l| tree.packet(l)).collect()
        };
        Self {
            env: DataEnv::load(data),
            lang: Lang::load(&data.join("assets/minecraft/lang/en_us.json")),
            tree,
            packets,
        }
    }

    /// The Commands packet body for someone with permission `level`.
    pub fn packet(&self, level: u8) -> Option<&[u8]> {
        self.packets
            .get(usize::from(level.min(4)))
            .map(Vec::as_slice)
    }
}

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

/// A player's permission level: their op level, 0 if not an op.
pub fn level(ctx: &ServerContext, uuid: u128, name: &str) -> u8 {
    lock(&ctx.lists)
        .op(uuid, name)
        .map_or(0, |o| o.level.clamp(0, 4) as u8)
}

/// What a command did.
#[derive(Debug, Default)]
pub struct Output {
    /// Lines for whoever ran it.
    pub lines: Vec<Tag>,
    pub events: Vec<Event>,
    /// `return`: the value (and whether it did not `fail`) the running
    /// function ends with.
    pub ret: Option<(bool, i32)>,
    /// How deep in functions calling functions.
    pub depth: u32,
    /// Commands run and contexts forked so far, against
    /// `max_command_sequence_length` and `max_command_forks`.
    pub steps: i64,
    pub forks: i64,
    /// A limit was hit: everything left is dropped.
    pub aborted: bool,
}

/// Where and as whom a command runs (`execute` changes it).
#[derive(Clone, Debug)]
pub struct Src {
    pub who: Who,
    pub name: String,
    pub pos: [f64; 3],
    /// Yaw and pitch.
    pub rot: [f32; 2],
    pub level: u8,
    /// `execute anchored eyes`.
    pub eyes: bool,
    /// `execute store`: where the command's result goes.
    pub stores: Vec<Store>,
    /// In a function: no feedback, errors dropped.
    pub silent: bool,
}

/// Where `execute store result|success ...` puts what a command returned.
#[derive(Clone, Debug)]
pub struct Store {
    /// `success` (1 or 0) instead of the result.
    pub success: bool,
    pub target: StoreTarget,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum StoreTarget {
    /// `score <targets> <objective>` (holder names already resolved).
    Score {
        holders: Vec<String>,
        objective: String,
    },
    /// `block|entity|storage ... <path> <type> <scale>`.
    Data {
        target: DataTarget,
        path: mc_command::nbtpath::NbtPath,
        /// `byte`, `short`, `int`, `long`, `float`, `double`.
        kind: String,
        scale: f64,
    },
    /// `bossbar <id> value|max`.
    Bossbar { id: String, max: bool },
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum DataTarget {
    Block(mc_protocol::Position),
    Entity(u128),
    Storage(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Console,
    Player(u128),
    Entity(u128),
}

impl Src {
    fn of(ctx: &ServerContext, source: &Source) -> Self {
        match source {
            Source::Console => {
                let spawn = ctx.spawn.lock().expect("spawn lock").pos;
                let (x, y, z) = (f64::from(spawn.0), f64::from(spawn.1), f64::from(spawn.2));
                Src {
                    who: Who::Console,
                    name: "Server".into(),
                    pos: [x, y, z],
                    rot: [0.0, 0.0],
                    level: 4,
                    eyes: false,
                    stores: Vec::new(),
                    silent: false,
                }
            }
            Source::Player { uuid, name, pose } => Src {
                who: Who::Player(*uuid),
                name: name.clone(),
                pos: [pose.x, pose.y, pose.z],
                rot: [pose.yaw, pose.pitch],
                level: level(ctx, *uuid, name),
                eyes: false,
                stores: Vec::new(),
                silent: false,
            },
        }
    }

    pub fn player(&self) -> Option<u128> {
        match self.who {
            Who::Player(u) => Some(u),
            _ => None,
        }
    }
}

/// One command running for one source.
pub struct Run<'a> {
    pub ctx: &'a ServerContext,
    pub tick: &'a mut TickControl,
    pub src: Src,
    pub c: &'a Context,
    pub out: &'a mut Output,
    /// Feedback for the other operators and the console.
    admin: Vec<Tag>,
}

pub type Result<T> = std::result::Result<T, CommandError>;

impl Run<'_> {
    /// Tells the source; with `broadcast`, other operators and the console
    /// hear of it too (`[Name: ...]`).
    pub fn success(&mut self, message: Tag, broadcast: bool) {
        if self.src.silent {
            return;
        }
        if self.ctx.game_rule_bool("send_command_feedback") || self.src.who == Who::Console {
            self.out.lines.push(message.clone());
        }
        if broadcast {
            self.admin.push(message);
        }
    }
}

/// `chat.type.admin`: `[Name: message]`, gray and italic.
fn admin_line(name: &str, message: Tag) -> Tag {
    let mut c = Compound::new();
    c.insert("translate", "chat.type.admin");
    c.insert(
        "with",
        mc_protocol::text::list(vec![Tag::from(name), message]),
    );
    c.insert("color", "gray");
    c.insert("italic", Tag::Byte(1));
    Tag::Compound(c)
}

pub fn execute(ctx: &ServerContext, tick: &mut TickControl, source: &Source, line: &str) -> Output {
    let line = line.trim_end();
    let line = line.strip_prefix('/').unwrap_or(line);
    let commands = &ctx.commands;
    let src = Src::of(ctx, source);
    let mut out = Output::default();
    let parse = dispatch::parse(&commands.tree, line, src.level, &commands.env);
    if let Some(e) = parse
        .error(line)
        .or_else(|| execute::check_line(ctx, &parse.chain, line))
    {
        report(ctx, source, &e, &mut out);
        return out;
    }
    let mut admin = Vec::new();
    let result = run_chain(ctx, tick, &parse.chain, src.clone(), &mut out, &mut admin);
    if let Err(e) = result {
        report(ctx, source, &e, &mut out);
    }
    for message in admin {
        let text = commands.lang.render(&message);
        match source {
            Source::Console => {}
            Source::Player { name, .. } => info!("[{name}: {text}]"),
        }
        if ctx.game_rule_bool("log_admin_commands") {
            out.events.push(Event::AdminFeedback {
                from: src.player().unwrap_or(0),
                line: admin_line(&src.name, message),
            });
        }
    }
    if let Source::Console = source {
        for l in &out.lines {
            info!("{}", commands.lang.render(l));
        }
        out.lines.clear();
    }
    out
}

fn report(ctx: &ServerContext, source: &Source, e: &CommandError, out: &mut Output) {
    match source {
        Source::Console => warn!("{}", e.console(&ctx.commands.lang)),
        Source::Player { .. } => out.lines.extend(e.lines()),
    }
}

/// Runs the contexts of a parsed line: each but the last changes the
/// sources (`execute as`, `at`, `positioned`...), the last runs for each.
pub(crate) fn run_chain(
    ctx: &ServerContext,
    tick: &mut TickControl,
    chain: &[Context],
    src: Src,
    out: &mut Output,
    admin: &mut Vec<Tag>,
) -> Result<i32> {
    let tree = &ctx.commands.tree;
    let (max_steps, max_forks) = {
        let rules = lock(&ctx.game_rules);
        let limit = |name| match rules.get(name) {
            Some(_) => i64::from(rules.int(name)),
            None => i64::MAX,
        };
        (
            limit("max_command_sequence_length"),
            limit("max_command_forks"),
        )
    };
    let mut sources = vec![src];
    let mut returns = false;
    let (last, modifiers) = chain.split_last().expect("a context");
    for c in modifiers {
        if out.aborted {
            return Ok(0);
        }
        if c.path(tree).first() == Some(&"return") {
            returns = true;
            continue;
        }
        let mut path = c.path(tree);
        if path.first().is_some_and(|p| *p != "execute") && path.len() > 1 {
            path.insert(0, "execute");
        }
        let mut next = Vec::new();
        if let ["execute", kind @ ("if" | "unless"), "function", "name"] = path.as_slice() {
            // Runs the function, so it needs more than `run::modify` has.
            for s in &sources {
                out.steps += 1;
                if functions::test(ctx, tick, out, admin, c, s)? == (*kind == "if") {
                    next.push(s.clone());
                }
            }
        } else {
            for s in &sources {
                out.steps += 1;
                next.extend(run::modify(ctx, c, s)?);
            }
        }
        if out.steps > max_steps {
            out.aborted = true;
            return Ok(0);
        }
        out.forks += next.len() as i64;
        if out.forks >= max_forks {
            return Ok(0);
        }
        sources = next;
        if sources.is_empty() {
            return Ok(0);
        }
    }
    let Some(command) = last.command else {
        return Err(mc_command::error::builtin::unknown_command());
    };
    let names = tree.names(command);
    let path: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut total = 0;
    let many = sources.len() > 1;
    for s in sources {
        out.steps += 1;
        if out.aborted || out.steps > max_steps {
            out.aborted = true;
            return Ok(total);
        }
        let silent = s.silent;
        let mut r = Run {
            ctx,
            tick: &mut *tick,
            src: s,
            c: last,
            out: &mut *out,
            admin: Vec::new(),
        };
        let result = run::run(&mut r, &path);
        admin.append(&mut r.admin);
        let stores = std::mem::take(&mut r.src.stores);
        if !stores.is_empty() {
            // The reply comes before what storing the result changes.
            if let (Who::Player(uuid), Some(Ok(_))) = (r.src.who, &result) {
                let lines: Vec<Tag> = r.out.lines.drain(..).collect();
                for line in lines {
                    r.out.events.push(Event::Message {
                        uuid,
                        line,
                        overlay: false,
                    });
                }
            }
            let (ok, value) = match &result {
                Some(Ok(n)) => (true, *n),
                _ => (false, 0),
            };
            for store in &stores {
                let v = if store.success { i32::from(ok) } else { value };
                match &store.target {
                    StoreTarget::Score { .. } => scoreboard::store(ctx, store, v, r.out),
                    StoreTarget::Data { .. } => data::store(ctx, store, v, r.out),
                    StoreTarget::Bossbar { .. } => bossbar::store(ctx, store, v),
                }
            }
        }
        if returns && r.out.ret.is_none() {
            r.out.ret = Some(match &result {
                Some(Ok(n)) => (true, *n),
                _ => (false, 0),
            });
        }
        match result {
            Some(Ok(n)) => total += n,
            Some(Err(_)) if silent => {}
            Some(Err(e)) if many => r.out.lines.extend(e.lines()),
            Some(Err(e)) => return Err(e),
            None => {
                #[cfg(test)]
                coverage::missing(path.join(" "));
                warn!("/{} is not implemented yet", path.join(" "));
                return Err(CommandError::new("command.failed", Vec::new()));
            }
        }
        // `return` in one fork ends the function for all of them.
        if out.ret.is_some() {
            break;
        }
    }
    Ok(total)
}
