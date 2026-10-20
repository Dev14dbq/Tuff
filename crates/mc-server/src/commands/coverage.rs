//! Which commands of vanilla's command tree reach a handler here.
//! `COMMAND_LEAVES=<file> cargo test -p mc-server commands_reach_a_handler
//! -- --ignored --nocapture`, where the file has `path<TAB>line` per
//! executable branch (the path with `<argument>` names, the line with sample
//! values). Branches that fall through every handler are listed.

use std::sync::Mutex;

use super::Source;
use crate::context::ServerContext;
use crate::tick::TickControl;

static MISSING: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn missing(path: String) {
    MISSING.lock().expect("missing lock").push(path);
}

#[test]
#[ignore]
fn commands_reach_a_handler() {
    let Ok(file) = std::env::var("COMMAND_LEAVES") else {
        eprintln!("skipped: COMMAND_LEAVES not set");
        return;
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
    let Ok(data) = mc_data::McData::load(&dir) else {
        eprintln!("skipped: no game data");
        return;
    };
    let ctx = ServerContext::new(crate::Config::new(dir), data).expect("context");
    let mut tick = TickControl::default();
    let (mut unparsed, mut unhandled, mut total) = (Vec::new(), Vec::new(), 0);
    for row in std::fs::read_to_string(file).expect("leaves file").lines() {
        let Some((path, line)) = row.split_once('\t') else {
            continue;
        };
        total += 1;
        MISSING.lock().expect("missing lock").clear();
        let parse = super::dispatch::parse(&ctx.commands.tree, line, 4, &ctx.commands.env);
        if let Some(e) = parse.error(line) {
            unparsed.push(format!("{path}   [{}]", e.console(&ctx.commands.lang)));
            continue;
        }
        let _ = super::execute(&ctx, &mut tick, &Source::Console, line);
        let hit = MISSING.lock().expect("missing lock").len();
        if hit > 0 {
            unhandled.push(path.to_owned());
        }
    }
    eprintln!(
        "COVERAGE total {total}, unhandled {}, unparsed {}",
        unhandled.len(),
        unparsed.len()
    );
    for p in &unhandled {
        eprintln!("UNHANDLED {p}");
    }
    for p in &unparsed {
        eprintln!("UNPARSED {p}");
    }
}
