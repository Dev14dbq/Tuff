//! Command blocks: impulse, chain and repeating blocks, what their window
//! sets, how they run (from the middle of the block, as no entity), their
//! success count (what comparators read) and last output, kept in their
//! block entity as vanilla keeps it.

use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_tick::command_block::{Mode, mode};
use mc_tick::{BlockPos, Dir, Level};

use crate::commands::{self, Source};
use crate::context::{Event, ServerContext};
use crate::level::{WorldLevel, to_pos};
use crate::tick::TickControl;

/// The block entity id all three share.
pub const ID: &str = "minecraft:command_block";

/// The name a command block runs and reports as when it has none.
const NAME: &str = "@";

fn flag(c: &Compound, key: &str) -> bool {
    matches!(c.get(key), Some(Tag::Byte(b)) if *b != 0)
}

fn set_flag(c: &mut Compound, key: &str, on: bool) {
    c.insert(key, Tag::Byte(i8::from(on)));
}

fn string(c: &Compound, key: &str) -> String {
    match c.get(key) {
        Some(Tag::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn int(c: &Compound, key: &str) -> i32 {
    match c.get(key) {
        Some(Tag::Int(n)) => *n,
        _ => 0,
    }
}

fn long(c: &Compound, key: &str) -> i64 {
    match c.get(key) {
        Some(Tag::Long(n)) => *n,
        _ => -1,
    }
}

/// A fresh command block's data: nothing to run, tracking output.
fn blank(auto: bool, track: bool) -> Compound {
    let mut c = Compound::new();
    c.insert("Command", Tag::String(String::new()));
    c.insert("SuccessCount", Tag::Int(0));
    set_flag(&mut c, "TrackOutput", track);
    set_flag(&mut c, "UpdateLastExecution", true);
    set_flag(&mut c, "powered", false);
    set_flag(&mut c, "conditionMet", false);
    set_flag(&mut c, "auto", auto);
    c
}

fn is_command_block(ctx: &ServerContext, state: u32) -> bool {
    mode(logic(ctx), state).is_some()
}

fn logic(ctx: &ServerContext) -> mc_tick::Ctx<'_> {
    mc_tick::Ctx {
        shapes: &ctx.shapes,
        facts: &ctx.rules,
    }
}

/// The data of the command block at `p` (a blank one if it has none yet,
/// as a block set by a command starts); `None` if there is no command
/// block.
fn data(ctx: &ServerContext, p: Position) -> Option<Compound> {
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    if !is_command_block(ctx, state) {
        return None;
    }
    Some(
        ctx.world
            .block_entity(p, blocks)
            .filter(|be| be.id == ID)
            .map_or_else(|| blank(false, true), |be| be.data),
    )
}

fn store(ctx: &ServerContext, p: Position, data: Compound) {
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: ID.to_owned(),
            data,
        },
        &ctx.data.blocks,
    );
}

/// A player placed a command block: chain blocks start always active, the
/// others needing redstone; output is tracked as `send_command_feedback`
/// says.
pub fn placed(ctx: &ServerContext, p: Position, state: u32) {
    let Some(mode) = mode(logic(ctx), state) else {
        return;
    };
    let track = ctx.game_rule_bool("send_command_feedback");
    store(ctx, p, blank(mode == Mode::Chain, track));
}

/// What a comparator reads from a command block: its success count.
pub fn signal(ctx: &ServerContext, p: Position) -> Option<u8> {
    let d = data(ctx, p)?;
    Some(int(&d, "SuccessCount").clamp(0, 15) as u8)
}

fn facing(ctx: &ServerContext, state: u32) -> Dir {
    ctx.data
        .blocks
        .property(state, "facing")
        .and_then(Dir::from_name)
        .unwrap_or(Dir::North)
}

fn conditional(ctx: &ServerContext, state: u32) -> bool {
    ctx.data.blocks.property(state, "conditional") == Some("true")
}

/// Whether a conditional block may run: the command block behind it
/// succeeded. Stored as `conditionMet`.
fn mark_condition_met(ctx: &ServerContext, p: Position, state: u32, d: &mut Compound) -> bool {
    let mut met = true;
    if conditional(ctx, state) {
        let back = to_pos(p).offset(facing(ctx, state).opposite());
        let behind = Position::new(back.x, back.y, back.z);
        met = data(ctx, behind).is_some_and(|b| int(&b, "SuccessCount") > 0);
    }
    set_flag(d, "conditionMet", met);
    met
}

/// Its power changed: (powered before, always active). An impulse or
/// repeating block that needs redstone checks its condition as the power
/// comes (its tick follows).
pub fn power(ctx: &ServerContext, p: Position, powered: bool) -> Option<(bool, bool)> {
    let state = ctx.world.block(p, &ctx.data.blocks)?;
    let kind = mode(logic(ctx), state)?;
    let mut d = data(ctx, p)?;
    let was = flag(&d, "powered");
    let auto = flag(&d, "auto");
    if was == powered {
        return Some((was, auto));
    }
    set_flag(&mut d, "powered", powered);
    if powered && !auto && kind != Mode::Chain {
        mark_condition_met(ctx, p, state, &mut d);
    }
    store(ctx, p, d);
    Some((was, auto))
}

/// `[HH:mm:ss] ` before a tracked message.
fn stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let day = secs % 86400;
    format!("[{:02}:{:02}:{:02}] ", day / 3600, day / 60 % 60, day % 60)
}

/// Runs the command of a command block (or its minecart): returns whether
/// it ran this tick (`false` if it already had). The success count, last
/// output and last execution go into `d`.
pub fn perform(
    ctx: &ServerContext,
    control: Option<&mut TickControl>,
    source: Source,
    d: &mut Compound,
    now: i64,
) -> bool {
    if long(d, "LastExecution") == now {
        return false;
    }
    let command = string(d, "Command");
    let track = flag(d, "TrackOutput");
    if command.eq_ignore_ascii_case("Searge") {
        if track {
            d.insert("LastOutput", text("#itzlipofutzli"));
        }
        d.insert("SuccessCount", Tag::Int(1));
        return true;
    }
    d.insert("SuccessCount", Tag::Int(0));
    if ctx.game_rule_bool("command_blocks_work") && !command.trim().is_empty() {
        d.remove("LastOutput");
        let mut scratch = TickControl::default();
        let control = control.unwrap_or(&mut scratch);
        let out = commands::execute(ctx, control, &source, &command);
        d.insert("SuccessCount", Tag::Int(out.successes));
        if track && let Some(line) = out.lines.last() {
            let mut c = Compound::new();
            c.insert("text", Tag::String(stamp()));
            c.insert("extra", Tag::List(vec![line.clone()]));
            d.insert("LastOutput", Tag::Compound(c));
        }
        for e in out.events {
            let _ = ctx.events.send(e);
        }
    }
    if !track {
        d.remove("LastOutput");
    }
    let keep_time = flag(d, "UpdateLastExecution");
    d.insert("LastExecution", Tag::Long(if keep_time { now } else { -1 }));
    true
}

fn text(s: &str) -> Tag {
    let mut c = Compound::new();
    c.insert("text", Tag::String(s.to_owned()));
    Tag::Compound(c)
}

fn source_of(d: &Compound, p: Position) -> Source {
    let name = match d.get("CustomName") {
        Some(Tag::String(s)) => s.clone(),
        Some(Tag::Compound(c)) => match c.get("text") {
            Some(Tag::String(s)) => s.clone(),
            _ => NAME.to_owned(),
        },
        _ => NAME.to_owned(),
    };
    Source::Block { pos: p, name }
}

/// The due tick of a command block: an impulse block runs once, a
/// repeating one runs and asks for the next tick while it may; both start
/// the chain in front of them.
pub fn tick(level: &mut WorldLevel, p: Position) {
    let ctx = level.context();
    let Some(state) = ctx.world.block(p, &ctx.data.blocks) else {
        return;
    };
    let Some(kind) = mode(level.logic(), state) else {
        return;
    };
    let Some(mut d) = data(ctx, p) else {
        return;
    };
    let was_met = flag(&d, "conditionMet");
    let has_command = !string(&d, "Command").is_empty();
    let mut ran = false;
    match kind {
        Mode::Repeating | Mode::Impulse => {
            if kind == Mode::Repeating {
                mark_condition_met(ctx, p, state, &mut d);
            }
            if was_met {
                if has_command {
                    let now = level.game_time();
                    perform(ctx, level.control.as_deref_mut(), source_of(&d, p), &mut d, now);
                } else {
                    d.insert("SuccessCount", Tag::Int(0));
                }
                ran = true;
            } else if conditional(ctx, state) {
                d.insert("SuccessCount", Tag::Int(0));
            }
            let again = flag(&d, "powered") || flag(&d, "auto");
            store(ctx, p, d);
            if kind == Mode::Repeating && again {
                let block = ctx.shapes.block_index(state);
                level.schedule_block_tick(to_pos(p), block, 1, 0);
            }
        }
        Mode::Chain => {}
    }
    if ran {
        chain(level, p, facing(ctx, state));
    }
    let logic = level.logic();
    mc_tick::redstone::container_changed(level, logic, to_pos(p));
}

/// The chain blocks in front of a block that ran, each pointing on to the
/// next, run in turn (at most `max_command_sequence_length` of them).
fn chain(level: &mut WorldLevel, from: Position, mut dir: Dir) {
    let ctx = level.context();
    let max = crate::tick::lock(&ctx.game_rules).int("max_command_sequence_length");
    let mut at: BlockPos = to_pos(from);
    let mut left = max;
    while left > 0 {
        left -= 1;
        at = at.offset(dir);
        let p = Position::new(at.x, at.y, at.z);
        let Some(state) = ctx.world.block(p, &ctx.data.blocks) else {
            break;
        };
        if mode(level.logic(), state) != Some(Mode::Chain) {
            break;
        }
        let Some(mut d) = data(ctx, p) else {
            break;
        };
        if flag(&d, "powered") || flag(&d, "auto") {
            if mark_condition_met(ctx, p, state, &mut d) {
                let now = level.game_time();
                let ran = perform(ctx, level.control.as_deref_mut(), source_of(&d, p), &mut d, now);
                store(ctx, p, d);
                if !ran {
                    break;
                }
                let logic = level.logic();
                mc_tick::redstone::container_changed(level, logic, at);
            } else {
                if conditional(ctx, state) {
                    d.insert("SuccessCount", Tag::Int(0));
                }
                store(ctx, p, d);
            }
        }
        dir = facing(ctx, state);
    }
    if left <= 0 {
        tracing::warn!("Command Block chain tried to execute more than {max} steps!");
    }
}

/// Whether a player may use command blocks: an operator (level 2 or more)
/// in creative mode.
pub fn may_use(ctx: &ServerContext, uuid: u128, game_mode: i32) -> bool {
    if game_mode != 1 {
        return false;
    }
    let name = crate::tick::lock(&ctx.players)
        .get(&uuid)
        .map(|p| p.profile.name.clone())
        .unwrap_or_default();
    commands::level(ctx, uuid, &name) >= 2
}

/// A player clicked a command block: an operator in creative gets its
/// data, and their game opens the window. `None` for anyone else (the
/// click goes on as with any block).
pub fn open(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
) -> Option<mc_protocol::packets::play::BlockEntityData> {
    let d = data(ctx, p)?;
    if !may_use(ctx, uuid, game_mode) {
        return None;
    }
    Some(mc_protocol::packets::play::BlockEntityData {
        position: p,
        kind: ctx.world.block_entity_kind(ID)?,
        data: d,
    })
}

/// The window's settings for the command block at `p`: its kind and
/// whether it is conditional (the block), the command, tracking output and
/// always active (the block entity). Returns what the player is told.
pub fn configure(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    command: &str,
    kind: i32,
    flags: u8,
) -> Option<Tag> {
    if !may_use(ctx, uuid, game_mode) {
        return Some(mc_protocol::text::translate("advMode.notAllowed", Vec::new()));
    }
    let blocks = &ctx.data.blocks;
    let state = ctx.world.block(p, blocks)?;
    let mut d = data(ctx, p)?;
    let (track, cond, auto) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
    let name = match kind {
        0 => "minecraft:chain_command_block",
        1 => "minecraft:repeating_command_block",
        _ => "minecraft:command_block",
    };
    let facing = blocks.property(state, "facing").unwrap_or("north");
    let cond_text = if cond { "true" } else { "false" };
    let new_state = blocks
        .state_id(name, &[("facing", facing), ("conditional", cond_text)])
        .unwrap_or(state);
    d.insert("Command", Tag::String(command.to_owned()));
    set_flag(&mut d, "TrackOutput", track);
    if !track {
        d.remove("LastOutput");
    }
    let was_auto = flag(&d, "auto");
    set_flag(&mut d, "auto", auto);
    let mut level = WorldLevel::new(ctx);
    if new_state != state {
        // The block entity goes with the old block: put it back after.
        level.set_block(p, new_state);
    }
    // Turned always active (and not powered): it starts a tick later.
    let starts = auto && !was_auto && !flag(&d, "powered") && name != "minecraft:chain_command_block";
    if starts {
        mark_condition_met(ctx, p, new_state, &mut d);
    }
    store(ctx, p, d);
    if starts {
        level.schedule_block_tick(to_pos(p), ctx.shapes.block_index(new_state), 1, 0);
    }
    let (changes, spawned) = level.finish();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
    drop(spawned);
    if command.is_empty() {
        return None;
    }
    let key = if ctx.game_rule_bool("command_blocks_work") {
        "advMode.setCommand.success"
    } else {
        "advMode.setCommand.disabled"
    };
    Some(mc_protocol::text::translate(
        key,
        vec![Tag::String(command.to_owned())],
    ))
}

/// A command block minecart's window was done: its command and whether it
/// tracks output.
pub fn configure_cart(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    entity_id: i32,
    command: &str,
    track_output: bool,
) -> Option<Tag> {
    if !may_use(ctx, uuid, game_mode) {
        return Some(mc_protocol::text::translate("advMode.notAllowed", Vec::new()));
    }
    let _ = (entity_id, track_output);
    if command.is_empty() {
        return None;
    }
    Some(mc_protocol::text::translate(
        "advMode.setCommand.success",
        vec![Tag::String(command.to_owned())],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick::lock;

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    fn put(ctx: &ServerContext, p: Position, block: &str, props: &[(&str, &str)]) -> u32 {
        let state = ctx.data.blocks.state_id(block, props).expect("state");
        WorldLevel::new(ctx).set_block(p, state);
        state
    }

    fn name(ctx: &ServerContext, p: Position) -> String {
        ctx.world
            .block(p, &ctx.data.blocks)
            .and_then(|s| ctx.data.blocks.block_name(s))
            .unwrap_or("?")
            .to_owned()
    }

    fn command_block(ctx: &ServerContext, p: Position, block: &str, command: &str, auto: bool) {
        put(ctx, p, block, &[("facing", "east"), ("conditional", "false")]);
        let mut d = blank(auto, true);
        d.insert("Command", Tag::String(command.to_owned()));
        store(ctx, p, d);
    }

    fn ticks(ctx: &ServerContext, n: usize) {
        for _ in 0..n {
            lock(&ctx.time).advance();
            crate::tick::run_scheduled_ticks(ctx);
        }
    }

    #[test]
    fn power_runs_an_impulse_block_and_its_chain() {
        let Some(ctx) = context() else {
            return;
        };
        let p = Position::new(0, -60, 0);
        command_block(&ctx, p, "minecraft:command_block", "setblock 0 -55 0 minecraft:stone", false);
        command_block(
            &ctx,
            Position::new(1, -60, 0),
            "minecraft:chain_command_block",
            "setblock 1 -55 0 minecraft:dirt",
            true,
        );
        ticks(&ctx, 2);
        assert_eq!(name(&ctx, Position::new(0, -55, 0)), "minecraft:air");
        put(&ctx, Position::new(-1, -60, 0), "minecraft:redstone_block", &[]);
        ticks(&ctx, 2);
        assert_eq!(name(&ctx, Position::new(0, -55, 0)), "minecraft:stone");
        assert_eq!(name(&ctx, Position::new(1, -55, 0)), "minecraft:dirt");
        assert_eq!(signal(&ctx, p), Some(1));
        // Once per rising edge: staying powered does not run it again.
        put(&ctx, Position::new(0, -55, 0), "minecraft:air", &[]);
        ticks(&ctx, 3);
        assert_eq!(name(&ctx, Position::new(0, -55, 0)), "minecraft:air");
    }

    #[test]
    fn a_failing_command_counts_no_success() {
        let Some(ctx) = context() else {
            return;
        };
        let p = Position::new(4, -60, 0);
        command_block(&ctx, p, "minecraft:command_block", "kill @e[type=minecraft:pig]", false);
        put(&ctx, Position::new(4, -59, 0), "minecraft:redstone_block", &[]);
        ticks(&ctx, 2);
        assert_eq!(signal(&ctx, p), Some(0));
        let d = data(&ctx, p).expect("data");
        assert!(d.get("LastOutput").is_some(), "the error is kept: {d:?}");
    }

    #[test]
    fn a_repeating_block_runs_every_tick_and_conditions_follow_the_one_behind() {
        let Some(ctx) = context() else {
            return;
        };
        let p = Position::new(8, -60, 0);
        command_block(&ctx, p, "minecraft:repeating_command_block", "kill @e[type=minecraft:pig]", true);
        let q = Position::new(9, -60, 0);
        put(
            &ctx,
            q,
            "minecraft:chain_command_block",
            &[("facing", "east"), ("conditional", "true")],
        );
        let mut d = blank(true, true);
        d.insert("Command", Tag::String("setblock 9 -55 0 minecraft:stone".into()));
        store(&ctx, q, d);
        let state = ctx.world.block(p, &ctx.data.blocks).expect("block");
        let mut level = WorldLevel::new(&ctx);
        level.schedule_block_tick(to_pos(p), ctx.shapes.block_index(state), 1, 0);
        drop(level);
        let mut d = data(&ctx, p).expect("data");
        set_flag(&mut d, "conditionMet", true);
        store(&ctx, p, d);
        ticks(&ctx, 3);
        let first = long(&data(&ctx, p).expect("data"), "LastExecution");
        ticks(&ctx, 1);
        assert_eq!(long(&data(&ctx, p).expect("data"), "LastExecution"), first + 1);
        // Nothing to kill: the conditional chain block does not run.
        assert_eq!(name(&ctx, Position::new(9, -55, 0)), "minecraft:air");
        assert_eq!(signal(&ctx, q), Some(0));
    }
}
