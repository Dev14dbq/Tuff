//! The development commands `/debug`, `/perf`, `/jfr` and `/test`: their
//! arguments and the messages vanilla 26.3 sends. `/test` has no GameTest
//! runner behind it: the test instances that exist (`always_pass`) pass at
//! once.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use mc_command::args::{Env, selector_matches};
use mc_command::error::CommandError;
use mc_nbt::{Compound, Tag};
use mc_protocol::text::{colored, translate};

use super::run::{err, str_tag};
use super::{Result, Run};
use crate::tick::lock;

/// A test structure in the world.
struct Block {
    id: String,
    /// Whether the test instance exists in the registry.
    registered: bool,
    pos: [i32; 3],
}

#[derive(Default)]
struct Dev {
    /// Wall clock and world age when a profiler was started.
    debug: Option<(Instant, i64)>,
    perf: Option<(Instant, i64)>,
    jfr: bool,
    tests: Vec<Block>,
}

fn dev() -> &'static Mutex<Dev> {
    static DEV: OnceLock<Mutex<Dev>> = OnceLock::new();
    DEV.get_or_init(Mutex::default)
}

const REGISTRY: &str = "minecraft:test_instance";

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["debug", "function", ..] => super::functions::debug_function(r),
        ["debug", op] => profiler(r, "debug", *op == "start"),
        ["perf", op] => profiler(r, "perf", *op == "start"),
        ["jfr", op] => jfr(r, *op == "start"),
        ["test", rest @ ..] => test(r, rest),
        _ => return None,
    })
}

fn plain(r: &mut Run, key: &str, args: Vec<Tag>) {
    r.success(translate(key, args), true);
}

fn world_age(r: &Run) -> i64 {
    lock(&r.ctx.time).age
}

/// `/debug start|stop` and `/perf start|stop`: how long and how many ticks.
fn profiler(r: &mut Run, name: &str, start: bool) -> Result<i32> {
    let age = world_age(r);
    let mut d = lock(dev());
    let slot = if name == "debug" { &mut d.debug } else { &mut d.perf };
    if start {
        if slot.is_some() {
            return Err(err(&format!("commands.{name}.alreadyRunning"), Vec::new()));
        }
        *slot = Some((Instant::now(), age));
        drop(d);
        plain(r, &format!("commands.{name}.started"), Vec::new());
        return Ok(1);
    }
    let Some((since, from)) = slot.take() else {
        return Err(err(&format!("commands.{name}.notRunning"), Vec::new()));
    };
    drop(d);
    let secs = since.elapsed().as_secs_f64();
    let ticks = (age - from).max(0);
    let tps = if secs > 0.0 { ticks as f64 / secs } else { 0.0 };
    plain(
        r,
        &format!("commands.{name}.stopped"),
        vec![
            str_tag(&format!("{secs:.2}")),
            Tag::Int(ticks as i32),
            str_tag(&format!("{tps:.2}")),
        ],
    );
    if name == "perf" {
        let file = chrono::Local::now().format("%Y-%m-%d_%H_%M_%S-world-26_3.zip");
        plain(r, "commands.perf.reportSaved", vec![str_tag(&file.to_string())]);
    }
    Ok(ticks as i32)
}

fn jfr(r: &mut Run, start: bool) -> Result<i32> {
    let mut d = lock(dev());
    if start {
        if d.jfr {
            return Err(err("commands.jfr.start.failed", Vec::new()));
        }
        d.jfr = true;
        drop(d);
        plain(r, "commands.jfr.started", Vec::new());
        return Ok(1);
    }
    if !d.jfr {
        return Err(err(
            "commands.jfr.dump.failed",
            vec![str_tag("Not currently profiling")],
        ));
    }
    d.jfr = false;
    drop(d);
    let file = chrono::Local::now().format("debug/server-%Y-%m-%d-%H%M%S.jfr");
    plain(r, "commands.jfr.stopped", vec![str_tag(&file.to_string())]);
    Ok(1)
}

fn block_pos(r: &Run) -> [i32; 3] {
    let p = r.src.pos;
    [p[0].floor() as i32, p[1].floor() as i32, p[2].floor() as i32]
}

/// The registered test instances a selector names.
fn select(r: &Run, selector: &str) -> Vec<String> {
    r.ctx
        .commands
        .env
        .entries(REGISTRY)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| selector_matches(selector, e))
        .collect()
}

fn failed_hover(why: &str) -> CommandError {
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", Tag::String(why.into()));
    let mut c = Compound::new();
    c.insert("translate", "command.failed");
    c.insert("hover_event", Tag::Compound(hover));
    CommandError::message(Tag::Compound(c))
}

/// Puts the tests down in the world, one place apiece (the same place
/// again if a test stands there already).
fn place(r: &Run, ids: &[String], registered: bool) {
    let here = block_pos(r);
    let mut d = lock(dev());
    for (i, id) in ids.iter().enumerate() {
        let pos = [here[0] + 4 * i as i32, here[1] - 1, here[2] - 7];
        d.tests.retain(|b| b.pos != pos);
        d.tests.push(Block {
            id: id.clone(),
            registered,
            pos,
        });
    }
}

fn test(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let times = r.int("numberOfTimes").unwrap_or(1);
    let until_failed = r.boolean("untilFailed").unwrap_or(false);
    match rest {
        ["clearall", ..] | ["clearthat"] | ["clearthese"] => {
            let closest = rest[0] == "clearthat";
            let mut d = lock(dev());
            let n = if closest {
                usize::from(!d.tests.is_empty())
            } else {
                d.tests.len()
            };
            if n == 0 {
                return Err(err("commands.test.clear.error.no_tests", Vec::new()));
            }
            if closest {
                d.tests.remove(0);
            } else {
                d.tests.clear();
            }
            drop(d);
            plain(r, "commands.test.clear.success", vec![Tag::Int(n as i32)]);
            Ok(n as i32)
        }
        ["resetclosest" | "resetthat" | "resetthese"] => {
            let mut ids: Vec<(String, bool)> = lock(dev())
                .tests
                .iter()
                .map(|b| (b.id.clone(), b.registered))
                .collect();
            if rest[0] != "resetthese" {
                ids.truncate(1);
            }
            let mut n = 0;
            for (id, registered) in ids {
                if registered {
                    let mut line = Compound::new();
                    line.insert("translate", "test_instance_block.reset_success");
                    line.insert("with", Tag::List(vec![str_tag(&id)]));
                    line.insert("color", "green");
                    r.out.lines.push(Tag::Compound(line));
                    n += 1;
                } else {
                    let e = err("commands.test.error.non_existant_test", vec![str_tag(&id)]);
                    r.out.lines.extend(e.lines().into_iter().take(1));
                }
            }
            if n == 0 {
                return Err(err("commands.test.clear.error.no_tests", Vec::new()));
            }
            plain(r, "commands.test.reset.success", vec![Tag::Int(n)]);
            Ok(n)
        }
        ["create", ..] => {
            let id = mc_command::args::full_id(&r.string("id").unwrap_or_default());
            let pos = block_pos(r);
            let mut d = lock(dev());
            d.tests.retain(|b| b.pos != pos);
            d.tests.push(Block {
                id: id.clone(),
                registered: false,
                pos,
            });
            drop(d);
            plain(r, "commands.test.create.success", vec![str_tag(&id)]);
            Ok(1)
        }
        ["locate", ..] => {
            let selector = r.string("tests").unwrap_or_default();
            plain(r, "commands.test.locate.started", Vec::new());
            let me = r.src.pos;
            let found: Vec<[i32; 3]> = lock(dev())
                .tests
                .iter()
                .filter(|b| b.registered && selector_matches(&selector, &b.id))
                .map(|b| b.pos)
                .collect();
            if found.is_empty() {
                return Err(err("commands.test.error.no_test_instances", Vec::new()));
            }
            for p in &found {
                let mut tip = Compound::new();
                tip.insert("translate", "chat.coordinates.tooltip");
                let mut hover = Compound::new();
                hover.insert("action", "show_text");
                hover.insert("value", Tag::Compound(tip));
                let mut click = Compound::new();
                click.insert("action", "suggest_command");
                click.insert(
                    "command",
                    Tag::String(format!(
                        "/execute in minecraft:overworld run tp @s {} {} {} 0 0",
                        p[0],
                        p[1],
                        p[2] - 3
                    )),
                );
                let mut coords = Compound::new();
                coords.insert("translate", "chat.coordinates");
                coords.insert(
                    "with",
                    Tag::List(vec![Tag::Int(p[0]), Tag::Int(p[1]), Tag::Int(p[2])]),
                );
                coords.insert("extra", Tag::List(vec![str_tag(" ->[overworld]")]));
                let mut brackets = Compound::new();
                brackets.insert("translate", "chat.square_brackets");
                brackets.insert("with", Tag::List(vec![Tag::Compound(coords)]));
                brackets.insert("color", "green");
                brackets.insert("click_event", Tag::Compound(click));
                brackets.insert("hover_event", Tag::Compound(hover));
                let d = ((f64::from(p[0]) - me[0]).powi(2)
                    + (f64::from(p[1]) - me[1]).powi(2)
                    + (f64::from(p[2]) - me[2]).powi(2))
                .sqrt();
                r.success(
                    translate(
                        "commands.test.locate.found",
                        vec![Tag::Compound(brackets), str_tag(&format!("{}", d as i32))],
                    ),
                    false,
                );
            }
            plain(r, "commands.test.locate.done", vec![Tag::Int(found.len() as i32)]);
            Ok(found.len() as i32)
        }
        ["pos", ..] => {
            let here = block_pos(r);
            let hit = lock(dev())
                .tests
                .iter()
                .rev()
                .find(|b| !b.registered && b.pos == here)
                .map(|b| b.id.clone());
            let Some(id) = hit else {
                return Err(err(
                    "commands.test.error.no_test_containing_pos",
                    here.iter().map(|&v| Tag::Int(v)).collect(),
                ));
            };
            let var = r.string("var").unwrap_or_else(|| "pos".into());
            let mut copy = Compound::new();
            copy.insert("action", "copy_to_clipboard");
            copy.insert("value", Tag::String(format!("final BlockPos {var} = new BlockPos(0, 0, 0);")));
            let mut tip = Compound::new();
            tip.insert("translate", "commands.test.coordinates.copy");
            let mut hover = Compound::new();
            hover.insert("action", "show_text");
            hover.insert("value", Tag::Compound(tip));
            let mut c = Compound::new();
            c.insert("translate", "commands.test.coordinates");
            c.insert("with", Tag::List(vec![Tag::Int(0), Tag::Int(0), Tag::Int(0)]));
            c.insert("color", "green");
            c.insert("bold", Tag::Byte(1));
            c.insert("click_event", Tag::Compound(copy));
            c.insert("hover_event", Tag::Compound(hover));
            plain(
                r,
                "commands.test.relative_position",
                vec![str_tag(&id), Tag::Compound(c)],
            );
            Ok(1)
        }
        ["stop"] => Ok(0),
        ["run", ..] | ["runmultiple", ..] | ["verify", ..] => {
            let selector = r.string("tests").unwrap_or_default();
            let mut ids = select(r, &selector);
            if rest[0] == "run" {
                let rotation = r.int("rotationSteps").unwrap_or(0);
                if !(0..=3).contains(&rotation) {
                    return Err(failed_hover(&format!(
                        "rotationSteps must be a value from 0-3. Got value {rotation}"
                    )));
                }
            }
            if rest[0] == "runmultiple" {
                let amount = r.int("amount").unwrap_or(0).max(0) as usize;
                let one = ids.clone();
                ids = (0..amount).flat_map(|_| one.iter().cloned()).collect();
            }
            if ids.is_empty() {
                return no_tests(r);
            }
            if rest[0] == "verify" {
                return verify(r, &ids);
            }
            place(r, &ids, true);
            run_tests(r, &ids, times, until_failed)
        }
        ["runclosest" | "runthat" | "runthese", ..] => {
            let tests: Vec<(String, bool)> = lock(dev())
                .tests
                .iter()
                .map(|b| (b.id.clone(), b.registered))
                .collect();
            let mut tests = tests;
            if rest[0] != "runthese" {
                tests = tests.pop().into_iter().collect();
            }
            if tests.is_empty() {
                return if rest[0] == "runthat" {
                    let here = block_pos(r);
                    Err(err(
                        "commands.test.error.no_test_containing_pos",
                        here.iter().map(|&v| Tag::Int(v)).collect(),
                    ))
                } else {
                    no_tests(r)
                };
            }
            let mut ids = Vec::new();
            let mut missing = Vec::new();
            for (id, registered) in tests {
                if registered {
                    ids.push(id);
                } else {
                    missing.push(id);
                }
            }
            if ids.is_empty() {
                for id in missing {
                    let e = err("commands.test.error.non_existant_test", vec![str_tag(&id)]);
                    r.out.lines.extend(e.lines().into_iter().take(1));
                }
                return no_tests(r);
            }
            run_tests(r, &ids, times, until_failed)
        }
        ["runfailed", ..] => {
            let rotation = r.int("rotationSteps").unwrap_or(0);
            if !(0..=3).contains(&rotation) {
                return Err(failed_hover(&format!(
                    "rotationSteps must be a value from 0-3. Got value {rotation}"
                )));
            }
            no_tests(r)
        }
        _ => Err(err("command.unknown.command", Vec::new())),
    }
}

fn no_tests(r: &mut Run) -> Result<i32> {
    plain(r, "commands.test.no_tests", Vec::new());
    Ok(0)
}

fn passed(r: &mut Run, text: String) {
    r.out.lines.push(colored(&text, "green"));
}

fn batch(r: &mut Run, rotation: i32) {
    plain(
        r,
        "commands.test.batch.starting",
        vec![str_tag("minecraft:default"), Tag::Int(rotation)],
    );
}

/// Every test passes at once. `times` 0 repeats for ever in vanilla; once
/// is done here.
fn run_tests(r: &mut Run, ids: &[String], times: i32, until_failed: bool) -> Result<i32> {
    plain(r, "commands.test.run.running", vec![Tag::Int(ids.len() as i32)]);
    let looped = times != 1 || until_failed;
    let rounds = if times > 0 { times } else { 1 };
    if !looped {
        batch(r, 0);
        for id in ids {
            passed(r, format!("{id} passed! (0ms / 0gameticks)"));
        }
    } else {
        for k in 1..=rounds {
            batch(r, 0);
            for id in ids {
                let left = if times > 0 {
                    format!(", Left: {:4}", rounds - k)
                } else {
                    String::new()
                };
                let pad = if times > 0 { 8 } else { 20 };
                passed(
                    r,
                    format!(
                        "[Run: {k:4}, Ok: {k:4}, Fail: {:4}{left}]{}{id} passed! 0ms",
                        0,
                        " ".repeat(pad)
                    ),
                );
            }
        }
    }
    let total = ids.len() as i32 * rounds;
    plain(r, "commands.test.summary", vec![Tag::Int(total)]);
    plain(r, "commands.test.summary.all_required_passed", Vec::new());
    Ok(total)
}

/// `/test verify`: every test four times (a rotation each), 100 runs apiece.
fn verify(r: &mut Run, ids: &[String]) -> Result<i32> {
    place(r, ids, true);
    for rotation in 0..4 {
        batch(r, rotation);
        for _ in 0..100 {
            for id in ids {
                passed(r, format!("{id} passed! (0ms / 0gameticks)"));
            }
        }
    }
    let total = ids.len() as i32 * 400;
    plain(r, "commands.test.summary", vec![Tag::Int(total)]);
    plain(r, "commands.test.summary.all_required_passed", Vec::new());
    Ok(total)
}
