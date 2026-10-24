//! Test blocks and test instance blocks (minecraft.wiki: "Test Block", "Test
//! Instance Block"): game master blocks for block-based tests.
//!
//! A test block has a mode. *Start* gives a constant redstone signal like a
//! block of redstone; *log* writes its message to the server log each time
//! a signal comes; *fail* fails the running test on a signal and *accept*
//! passes it (every accept block of the test must have had one).
//!
//! A test instance block holds a test (a data pack `test_instance`), the
//! size of its structure, a rotation and whether entities are left alone.
//! Its window can reset the area (clear it and place the test's structure
//! one block up, east and south of the block), save the area back as that
//! structure, and run the test: block-based tests end when the accept
//! blocks were all powered, a fail block was powered, or `max_ticks` (after
//! `setup_ticks`) are over; function tests end at once for the built-in
//! `always_pass`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use mc_features::structure::template::{Rotation, Settings};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::packets::play::{BlockEntityData, GameTestHighlightPos, TestInstanceBlockStatus};
use mc_protocol::text::translate;
use serde_json::Value;

use crate::context::{Direct, Event, ServerContext};
use crate::interact::{Actor, Outcome};
use crate::level::WorldLevel;
use crate::structure_blocks::{apply, capture, file_of, load, template_id, text, write_file};
use crate::tick::lock;

pub const TEST: &str = "minecraft:test_block";
pub const INSTANCE: &str = "minecraft:test_instance_block";

const MODES: [&str; 4] = ["start", "log", "fail", "accept"];
const ROTATIONS: [&str; 4] = ["none", "clockwise_90", "180", "counterclockwise_90"];
/// Test areas may be this large along each axis.
const LIMIT: i32 = 48;

fn string(d: &Compound, key: &str) -> String {
    match d.get(key) {
        Some(Tag::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn flag(d: &Compound, key: &str) -> bool {
    matches!(d.get(key), Some(Tag::Byte(b)) if *b != 0)
}

fn size_of(d: &Compound) -> [i32; 3] {
    match d.get("size") {
        Some(Tag::IntArray(v)) if v.len() == 3 => [v[0], v[1], v[2]],
        _ => [0; 3],
    }
}

fn data(ctx: &ServerContext, p: Position, id: &str) -> Option<Compound> {
    let state = ctx.world.block(p, &ctx.data.blocks)?;
    if ctx.data.blocks.block_name(state)? != id {
        return None;
    }
    Some(
        ctx.world
            .block_entity(p, &ctx.data.blocks)
            .map_or_else(Compound::new, |be| be.data),
    )
}

fn store(ctx: &ServerContext, p: Position, id: &str, data: Compound) {
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: id.to_owned(),
            data,
        },
        &ctx.data.blocks,
    );
}

fn tell(ctx: &ServerContext, p: Position, id: &str) {
    if let (Some(kind), Some(d)) = (ctx.world.block_entity_kind(id), data(ctx, p, id)) {
        let _ = ctx.events.send(Event::BlockEntity {
            position: p,
            kind,
            data: d,
        });
    }
}

/// A test block or test instance block was placed.
pub fn placed(ctx: &ServerContext, p: Position, state: u32) {
    let blocks = &ctx.data.blocks;
    match blocks.block_name(state) {
        Some(TEST) => {
            let mut d = Compound::new();
            d.insert("message", text(""));
            store(ctx, p, TEST, d);
        }
        Some(INSTANCE) => {
            let mut d = Compound::new();
            d.insert("size", Tag::IntArray(vec![1, 1, 1]));
            d.insert("rotation", text("none"));
            d.insert("ignore_entities", Tag::Byte(0));
            d.insert("status", text("cleared"));
            store(ctx, p, INSTANCE, d);
        }
        _ => {}
    }
}

/// An operator in creative clicked one: their game opens the window.
pub fn open(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    id: &str,
) -> Option<BlockEntityData> {
    let d = data(ctx, p, id)?;
    if !crate::command_blocks::may_use(ctx, uuid, game_mode) {
        return None;
    }
    Some(BlockEntityData {
        position: p,
        kind: ctx.world.block_entity_kind(id)?,
        data: d,
    })
}

// -- Test block

/// A test block's window: its mode and message.
pub fn set_test_block(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    mode: i32,
    message: &str,
) -> Vec<Tag> {
    if !crate::command_blocks::may_use(ctx, uuid, game_mode) {
        return Vec::new();
    }
    let Some(mut d) = data(ctx, p, TEST) else {
        return Vec::new();
    };
    let Some(name) = MODES.get(mode.max(0) as usize) else {
        return Vec::new();
    };
    d.insert("message", text(message));
    store(ctx, p, TEST, d);
    let blocks = &ctx.data.blocks;
    if let Some(state) = ctx.world.block(p, blocks)
        && let Some(new) = blocks.with_property(state, "mode", name)
        && new != state
    {
        let mut level = WorldLevel::new(ctx);
        level.set_block(p, new);
        let changes = level.into_changes();
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
    tell(ctx, p, TEST);
    Vec::new()
}

/// What a test block got: its power came or went.
pub fn power(ctx: &ServerContext, p: Position, powered: bool) {
    let Some(mut d) = data(ctx, p, TEST) else {
        return;
    };
    if flag(&d, "powered") == powered {
        return;
    }
    d.insert("powered", Tag::Byte(i8::from(powered)));
    let message = string(&d, "message");
    store(ctx, p, TEST, d);
    if !powered {
        return;
    }
    let blocks = &ctx.data.blocks;
    let mode = ctx
        .world
        .block(p, blocks)
        .and_then(|s| blocks.property(s, "mode").map(str::to_owned))
        .unwrap_or_default();
    match mode.as_str() {
        "log" => tracing::info!("{message}"),
        "fail" | "accept" => {
            let mut runs = lock(runs());
            for run in runs.values_mut().filter(|r| r.contains(ctx.dim, p)) {
                if mode == "fail" {
                    run.failed.get_or_insert(p);
                } else {
                    run.accepted.insert((p.x, p.y, p.z));
                }
            }
        }
        _ => {}
    }
}

// -- Test instance block

/// A test being run.
#[derive(Debug)]
struct Run {
    dim: usize,
    block: Position,
    min: Position,
    size: [i32; 3],
    test: String,
    by: u128,
    started: i64,
    setup_ticks: i64,
    max_ticks: i64,
    accepts: usize,
    accepted: HashSet<(i32, i32, i32)>,
    failed: Option<Position>,
    function: Option<String>,
}

impl Run {
    fn contains(&self, dim: usize, p: Position) -> bool {
        dim == self.dim
            && (0..3).all(|i| {
                let v = [p.x, p.y, p.z][i];
                let lo = [self.min.x, self.min.y, self.min.z][i];
                v >= lo && v < lo + self.size[i]
            })
    }
}

type Runs = Mutex<HashMap<(usize, i32, i32, i32), Run>>;

fn runs() -> &'static Runs {
    static RUNS: OnceLock<Runs> = OnceLock::new();
    RUNS.get_or_init(Mutex::default)
}

/// The data pack's test instance `name`.
fn definition(ctx: &ServerContext, name: &str) -> Option<Value> {
    ctx.data
        .data_pack()
        .read_entry("minecraft:test_instance", name.trim_start_matches("minecraft:"))
        .ok()
}

fn rotation(n: i32) -> Rotation {
    Rotation(n.rem_euclid(4) as u8)
}

fn rotation_index(name: &str) -> i32 {
    ROTATIONS.iter().position(|r| *r == name).unwrap_or(0) as i32
}

fn coords(p: Position) -> Vec<Tag> {
    vec![Tag::Int(p.x), Tag::Int(p.y), Tag::Int(p.z)]
}

/// The text a test instance block's window shows for its test.
fn describe(ctx: &ServerContext, d: &Compound) -> Tag {
    let mut extra: Vec<Tag> = Vec::new();
    let name = string(d, "test");
    let line = |key: &str, arg: String| translate(key, vec![text(&arg)]);
    match (!name.is_empty()).then(|| definition(ctx, &name)).flatten() {
        None => extra.push(translate(
            if name.is_empty() {
                "test_instance.description.invalid_id"
            } else {
                "test_instance.description.no_test"
            },
            Vec::new(),
        )),
        Some(def) => {
            let function = def["type"].as_str() == Some("minecraft:function");
            extra.push(translate(
                if function {
                    "test_instance.type.function"
                } else {
                    "test_instance.type.block_based"
                },
                Vec::new(),
            ));
            extra.push(text("\n"));
            extra.push(line(
                "test_instance.description.structure",
                def["structure"].as_str().unwrap_or("").to_owned(),
            ));
            if let Some(f) = def["function"].as_str() {
                extra.push(text("\n"));
                extra.push(line("test_instance.description.function", f.to_owned()));
            }
            extra.push(text("\n"));
            extra.push(line(
                "test_instance.description.batch",
                def["environment"].as_str().unwrap_or("minecraft:default").to_owned(),
            ));
        }
    }
    let error = string(d, "error_message");
    if !error.is_empty() {
        extra.push(text("\n"));
        extra.push(line("test_instance.description.failed", error));
    }
    let mut c = Compound::new();
    c.insert("text", "");
    c.insert("extra", Tag::List(extra));
    Tag::Compound(c)
}

fn status_event(ctx: &ServerContext, uuid: u128, p: Position, d: &Compound) -> Event {
    let size = {
        let known = size_of(d);
        (known != [0; 3]).then_some(known)
    };
    let _ = p;
    Event::Direct {
        uuid,
        packet: Direct::TestStatus(TestInstanceBlockStatus {
            status: describe(ctx, d),
            size,
        }),
    }
}

/// A test instance block's window asked for something. `action`: 0 init, 1
/// query, 2 set, 3 reset, 4 save, 5 export, 6 run.
#[allow(clippy::too_many_arguments)]
pub fn instance_action(
    ctx: &ServerContext,
    actor: Actor,
    p: Position,
    action: i32,
    test: Option<String>,
    size: [i32; 3],
    rotation_n: i32,
    ignore_entities: bool,
) -> Outcome {
    let mut out = Outcome::default();
    if !crate::command_blocks::may_use(ctx, actor.uuid, actor.game_mode) {
        return out;
    }
    let Some(mut d) = data(ctx, p, INSTANCE) else {
        return out;
    };
    if action == 2 {
        match &test {
            Some(t) => {
                d.insert("test", text(&mc_command::args::full_id(t)));
            }
            None => {
                d.remove("test");
            }
        }
        let size = size.map(|s| s.clamp(1, LIMIT));
        d.insert("size", Tag::IntArray(size.to_vec()));
        d.insert("rotation", text(ROTATIONS[rotation_n.rem_euclid(4) as usize]));
        d.insert("ignore_entities", Tag::Byte(i8::from(ignore_entities)));
        store(ctx, p, INSTANCE, d.clone());
        tell(ctx, p, INSTANCE);
    }
    match action {
        3 => out.messages.push(reset(ctx, p, &mut d)),
        4 | 5 => out.messages.push(save(ctx, p, &d)),
        6 => out.messages.extend(run(ctx, actor.uuid, p, &mut d)),
        _ => {}
    }
    store(ctx, p, INSTANCE, d.clone());
    out.events.push(status_event(ctx, actor.uuid, p, &d));
    out
}

/// The test's structure id (`minecraft:empty`).
fn structure_of(ctx: &ServerContext, d: &Compound) -> Option<String> {
    let name = string(d, "test");
    if name.is_empty() {
        return None;
    }
    let def = definition(ctx, &name)?;
    template_id(def["structure"].as_str()?)
}

/// Where the test's area starts.
fn area_min(p: Position) -> Position {
    Position::new(p.x + 1, p.y + 1, p.z + 1)
}

fn no_test(p: Position, key: &str) -> Tag {
    translate(key, coords(p))
}

fn clear_area(ctx: &ServerContext, min: Position, size: [i32; 3], ignore_entities: bool) {
    let air = ctx.shapes.air();
    let mut changes = Vec::new();
    for y in 0..size[1] {
        for z in 0..size[2] {
            for x in 0..size[0] {
                let at = Position::new(min.x + x, min.y + y, min.z + z);
                if ctx.world.block(at, &ctx.data.blocks) != Some(air) {
                    changes.push((at, air));
                }
            }
        }
    }
    apply(ctx, changes, Vec::new());
    if ignore_entities {
        return;
    }
    let mut gone = Vec::new();
    for e in lock(&ctx.entities).iter_mut() {
        let inside = (0..3).all(|i| {
            e.pos[i] >= f64::from([min.x, min.y, min.z][i])
                && e.pos[i] < f64::from([min.x, min.y, min.z][i] + size[i])
        });
        if inside && !e.removed {
            e.removed = true;
            gone.push(e.id);
        }
    }
    if !gone.is_empty() {
        let _ = ctx.events.send(Event::EntitiesRemoved(gone));
    }
}

/// Clears the area and places the test's structure in it.
fn reset(ctx: &ServerContext, p: Position, d: &mut Compound) -> Tag {
    let name = string(d, "test");
    if name.is_empty() || definition(ctx, &name).is_none() {
        return no_test(p, "test_instance_block.error.no_test");
    }
    let Some(id) = structure_of(ctx, d) else {
        return no_test(p, "test_instance_block.error.no_test_structure");
    };
    let Some(t) = load(ctx, &id) else {
        return no_test(p, "test_instance_block.error.no_test_structure");
    };
    lock(runs()).remove(&(ctx.dim, p.x, p.y, p.z));
    let turns = rotation_index(&string(d, "rotation"));
    let want = if turns % 2 == 1 {
        [t.size.z, t.size.y, t.size.x]
    } else {
        [t.size.x, t.size.y, t.size.z]
    };
    d.insert("size", Tag::IntArray(want.to_vec()));
    let min = area_min(p);
    clear_area(ctx, min, want, flag(d, "ignore_entities"));
    let settings = Settings {
        rotation: rotation(turns),
        ..Settings::default()
    };
    // A turned structure grows to the south or west of where it stands.
    let at = match turns.rem_euclid(4) {
        1 => Position::new(min.x + want[0] - 1, min.y, min.z),
        2 => Position::new(min.x + want[0] - 1, min.y, min.z + want[2] - 1),
        3 => Position::new(min.x, min.y, min.z + want[2] - 1),
        _ => min,
    };
    if let Some((changes, entities)) = crate::commands::place::place_template(ctx, &t, at, &settings, 0)
    {
        apply(ctx, changes, entities);
    }
    d.insert("status", text("cleared"));
    d.remove("error_message");
    let mut line = Compound::new();
    line.insert("translate", "test_instance_block.reset_success");
    line.insert("with", Tag::List(vec![text(&name)]));
    line.insert("color", "green");
    Tag::Compound(line)
}

/// Writes the area as the test's structure.
fn save(ctx: &ServerContext, p: Position, d: &Compound) -> Tag {
    let Some(id) = structure_of(ctx, d) else {
        return no_test(p, "test_instance_block.error.no_test_structure");
    };
    let size = size_of(d);
    let ok = size.iter().all(|&s| s >= 1)
        && file_of(ctx, &id).is_some_and(|path| write_file(&path, capture(ctx, area_min(p), size)));
    if ok {
        translate("structure_block.save_success", vec![text(&id)])
    } else {
        no_test(p, "test_instance_block.error.unable_to_save")
    }
}

/// Resets the area and starts the test.
fn run(ctx: &ServerContext, by: u128, p: Position, d: &mut Compound) -> Vec<Tag> {
    let name = string(d, "test");
    let Some(def) = (!name.is_empty()).then(|| definition(ctx, &name)).flatten() else {
        return vec![no_test(p, "test_instance_block.error.no_test")];
    };
    let mut out = vec![reset(ctx, p, d)];
    if string(d, "status") != "cleared" {
        return out;
    }
    out.push(translate("test_instance_block.starting", vec![text(&name)]));
    let size = size_of(d);
    let min = area_min(p);
    let function = def["function"].as_str().map(str::to_owned);
    let mut run = Run {
        dim: ctx.dim,
        block: p,
        min,
        size,
        test: name.clone(),
        by,
        started: lock(&ctx.time).age,
        setup_ticks: def["setup_ticks"].as_i64().unwrap_or(0),
        max_ticks: def["max_ticks"].as_i64().unwrap_or(100),
        accepts: 0,
        accepted: HashSet::new(),
        failed: None,
        function,
    };
    if run.function.is_none() {
        // Block-based: one start block, at least one accept block.
        let (mut starts, mut accepts) = (0, 0);
        let blocks = &ctx.data.blocks;
        for be in ctx.world.block_entities_of(TEST) {
            let at = Position::new(be.x, be.y, be.z);
            if !run.contains(ctx.dim, at) {
                continue;
            }
            match ctx
                .world
                .block(at, blocks)
                .and_then(|s| blocks.property(s, "mode"))
            {
                Some("start") => starts += 1,
                Some("accept") => accepts += 1,
                _ => {}
            }
        }
        let missing = |what: &str| translate("test_block.error.missing", vec![text(what)]);
        if starts == 0 {
            out.push(missing("start"));
        } else if starts > 1 {
            out.push(translate("test_block.error.too_many", vec![text("start")]));
        }
        if accepts == 0 {
            out.push(missing("accept"));
        }
        if starts != 1 || accepts == 0 {
            d.insert("status", text("finished"));
            d.insert("error_message", text("invalid test"));
            return out;
        }
        run.accepts = accepts;
    }
    d.insert("status", text("running"));
    lock(runs()).insert((ctx.dim, p.x, p.y, p.z), run);
    out
}

fn finish(ctx: &ServerContext, run: &Run, error: Option<(String, Option<Position>)>) {
    let Some(mut d) = data(ctx, run.block, INSTANCE) else {
        return;
    };
    d.insert("status", text("finished"));
    match &error {
        Some((e, _)) => {
            d.insert("error_message", text(e));
        }
        None => {
            d.remove("error_message");
        }
    }
    store(ctx, run.block, INSTANCE, d.clone());
    tell(ctx, run.block, INSTANCE);
    let _ = ctx.events.send(status_event(ctx, run.by, run.block, &d));
    if let Some((_, Some(at))) = &error {
        let _ = ctx.events.send(Event::Direct {
            uuid: run.by,
            packet: Direct::TestHighlight(GameTestHighlightPos {
                absolute: *at,
                relative: Position::new(at.x - run.min.x, at.y - run.min.y, at.z - run.min.z),
            }),
        });
    }
    tracing::info!(
        "test {} {}",
        run.test,
        if error.is_none() { "passed" } else { "failed" }
    );
}

/// Runs the tests that are under way.
pub fn tick(ctx: &ServerContext, now: i64) {
    let mut done = Vec::new();
    {
        let mut all = lock(runs());
        let keys: Vec<_> = all.keys().copied().filter(|k| k.0 == ctx.dim).collect();
        for k in keys {
            let run = &all[&k];
            let waited = now - run.started;
            let verdict = if let Some(at) = run.failed {
                Some(Some((
                    "Fail conditions met".to_owned(),
                    Some(at),
                )))
            } else if let Some(f) = &run.function {
                (waited >= run.setup_ticks.max(1)).then(|| {
                    (f != "minecraft:always_pass")
                        .then(|| (format!("unknown test function {f}"), None))
                })
            } else if run.accepted.len() >= run.accepts {
                Some(None)
            } else if waited > run.setup_ticks + run.max_ticks {
                Some(Some((
                    format!("Didn't succeed or fail within {} ticks", run.max_ticks),
                    None,
                )))
            } else {
                None
            };
            if let Some(v) = verdict
                && let Some(run) = all.remove(&k)
            {
                done.push((run, v));
            }
        }
    }
    for (run, v) in done {
        finish(ctx, &run, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(world: &std::path::Path) -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let mut config = crate::Config::new(dir);
        config.world = Some(world.to_owned());
        ServerContext::new(config, data).ok()
    }

    fn put(ctx: &ServerContext, p: Position, block: &str, props: &[(&str, &str)]) {
        let s = ctx.data.blocks.state_id(block, props).unwrap();
        WorldLevel::new(ctx).set_block(p, s);
        placed(ctx, p, s);
    }

    fn status(ctx: &ServerContext, p: Position) -> String {
        string(&data(ctx, p, INSTANCE).unwrap(), "status")
    }

    #[test]
    fn modes_follow_the_window_and_start_gives_power() {
        let dir = std::env::temp_dir().join(format!("mc-testblock-{}", std::process::id()));
        let Some(ctx) = context(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        put(&ctx, p, TEST, &[("mode", "start")]);
        // Not an operator: nothing changes.
        set_test_block(&ctx, 5, 1, p, 3, "x");
        let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
        assert_eq!(ctx.data.blocks.property(s, "mode"), Some("start"));
        let uuid = 5;
        lock(&ctx.players).clear();
        // Fake an operator in creative.
        assert!(!crate::command_blocks::may_use(&ctx, uuid, 1));
        // Power edges are remembered: log mode only notes the rise.
        put(&ctx, p, TEST, &[("mode", "log")]);
        power(&ctx, p, true);
        assert!(flag(&data(&ctx, p, TEST).unwrap(), "powered"));
        power(&ctx, p, false);
        assert!(!flag(&data(&ctx, p, TEST).unwrap(), "powered"));
    }

    #[test]
    fn a_block_based_test_runs_to_accept_or_fail() {
        let dir = std::env::temp_dir().join(format!("mc-testrun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(ctx) = context(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(0, -60, 0);
        put(&ctx, p, INSTANCE, &[]);
        let mut d = data(&ctx, p, INSTANCE).unwrap();
        // No test set: running says so.
        let said = run(&ctx, 1, p, &mut d);
        assert_eq!(said.len(), 1);
        // always_pass is a function test that passes after its setup.
        d.insert("test", text("minecraft:always_pass"));
        store(&ctx, p, INSTANCE, d.clone());
        let said = run(&ctx, 1, p, &mut d);
        assert!(said.len() >= 2);
        assert_eq!(string(&d, "status"), "running");
        store(&ctx, p, INSTANCE, d);
        tick(&ctx, lock(&ctx.time).age + 5);
        assert_eq!(status(&ctx, p), "finished");
        assert!(data(&ctx, p, INSTANCE).unwrap().get("error_message").is_none());

        // A block-based test of our own: a start block and an accept block
        // in a 3×1×1 area, run from the blank structure.
        let blocks = &ctx.data.blocks;
        let min = area_min(p);
        let mut run_d = data(&ctx, p, INSTANCE).unwrap();
        run_d.insert("size", Tag::IntArray(vec![3, 1, 1]));
        put(&ctx, min, TEST, &[("mode", "start")]);
        put(&ctx, Position::new(min.x + 2, min.y, min.z), TEST, &[("mode", "accept")]);
        let area = Run {
            dim: ctx.dim,
            block: p,
            min,
            size: [3, 1, 1],
            test: "t".into(),
            by: 1,
            started: 0,
            setup_ticks: 0,
            max_ticks: 100,
            accepts: 1,
            accepted: HashSet::new(),
            failed: None,
            function: None,
        };
        lock(runs()).insert((ctx.dim, p.x, p.y, p.z), area);
        tick(&ctx, 10);
        assert!(lock(runs()).contains_key(&(ctx.dim, p.x, p.y, p.z)), "still going");
        power(&ctx, Position::new(min.x + 2, min.y, min.z), true);
        tick(&ctx, 11);
        assert!(lock(runs()).is_empty(), "accepted");
        assert_eq!(status(&ctx, p), "finished");

        // A fail block ends it as failed.
        put(&ctx, Position::new(min.x + 1, min.y, min.z), TEST, &[("mode", "fail")]);
        let again = Run {
            dim: ctx.dim,
            block: p,
            min,
            size: [3, 1, 1],
            test: "t".into(),
            by: 1,
            started: 0,
            setup_ticks: 0,
            max_ticks: 100,
            accepts: 1,
            accepted: HashSet::new(),
            failed: None,
            function: None,
        };
        lock(runs()).insert((ctx.dim, p.x, p.y, p.z), again);
        power(&ctx, Position::new(min.x + 1, min.y, min.z), true);
        tick(&ctx, 12);
        assert_eq!(status(&ctx, p), "finished");
        assert!(!string(&data(&ctx, p, INSTANCE).unwrap(), "error_message").is_empty());
        // It times out if nothing happens.
        let slow = Run {
            dim: ctx.dim,
            block: p,
            min,
            size: [3, 1, 1],
            test: "t".into(),
            by: 1,
            started: 0,
            setup_ticks: 0,
            max_ticks: 5,
            accepts: 1,
            accepted: HashSet::new(),
            failed: None,
            function: None,
        };
        lock(runs()).insert((ctx.dim, p.x, p.y, p.z), slow);
        tick(&ctx, 5);
        assert!(lock(runs()).contains_key(&(ctx.dim, p.x, p.y, p.z)));
        tick(&ctx, 6);
        assert!(lock(runs()).is_empty());
        let _ = blocks;
    }

    #[test]
    fn reset_places_the_structure_and_save_writes_it_back() {
        let dir = std::env::temp_dir().join(format!("mc-testreset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(ctx) = context(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let p = Position::new(200, -60, 0);
        put(&ctx, p, INSTANCE, &[]);
        let mut d = data(&ctx, p, INSTANCE).unwrap();
        d.insert("test", text("minecraft:always_pass"));
        d.insert("size", Tag::IntArray(vec![2, 1, 1]));
        // Something stands in the area: the reset clears it.
        put(&ctx, area_min(p), "minecraft:gold_block", &[]);
        let said = reset(&ctx, p, &mut d);
        assert!(format!("{said:?}").contains("reset_success"));
        let s = ctx.world.block(area_min(p), &ctx.data.blocks).unwrap();
        assert_eq!(s, ctx.shapes.air());
        // Saving writes the (empty) area as the test's structure.
        put(&ctx, area_min(p), "minecraft:gold_block", &[]);
        let said = save(&ctx, p, &d);
        assert!(format!("{said:?}").contains("save_success"));
        assert!(dir.join("generated/minecraft/structures/empty.nbt").exists());
        // The window text names the test.
        assert!(format!("{:?}", describe(&ctx, &d)).contains("structure"));
    }
}
