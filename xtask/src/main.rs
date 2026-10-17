//! Development tasks, run as `cargo xtask <task>`.

mod bots;
mod extract_data;
mod probe;
mod registry_diff;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo xtask <task> [options]

Tasks:
  extract-data [--version <id>]
      Extract vanilla game data from reference/<id>/server.jar into cache/mcdata/<id>/.
      Without --version, the newest release found in reference/ is used.
  probe [--addr <host:port>] [--name <player>] [--version <id>] [--dump <dir>]
      Connect to a server as a client (status, login, configuration, spawn) and
      report what it sent, checking our packet layouts against it. --dump writes
      the raw play packets to <dir>.
  bots [--addr <host:port>] [--count <n>] [--seconds <s>] [--version <id>]
      Load test: n bots (default 100) log in, get their chunks and walk around
      for s seconds (default 30). The server must run with --offline.
  registry-diff --a <host:port> --b <host:port> [--details <n>]
      Log into two offline-mode servers without known packs and compare every
      registry entry they send as NBT (e.g. vanilla vs ours).
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("extract-data") => extract_data::run(&args[1..]),
        Some("probe") => probe::run(&args[1..]),
        Some("bots") => bots::run(&args[1..]),
        Some("registry-diff") => registry_diff::run(&args[1..]),
        None | Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(anyhow::anyhow!("unknown task `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Root of the cargo workspace (the parent directory of this crate).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
        .to_path_buf()
}
