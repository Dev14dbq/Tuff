//! Development tasks, run as `cargo xtask <task>`.

mod blocks;
mod bots;
mod chunk_diff;
mod chunk_info;
mod chunk_speed;
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
  probe [--addr <host:port>] [--name <player>] [--version <id>] [--dump <dir>] [--mover] [--batches <n>]
      Connect to a server as a client (status, login, configuration, spawn) and
      report what it sent, checking our packet layouts against it. --dump writes
      the raw play packets to <dir>.
  bots [--addr <host:port>] [--count <n>] [--seconds <s>] [--view-distance <n>] [--version <id>]
      Load test: n bots (default 100) log in, get their chunks and walk around
      for s seconds (default 30). The server must run with --offline.
  blocks [--addr <host:port>] [--version <id>]
      Two players on an offline creative server with a superflat world: one
      places and breaks blocks, and what both receive is printed.
  registry-diff --a <host:port> --b <host:port> [--details <n>]
      Log into two offline-mode servers without known packs and compare every
      registry entry they send as NBT (e.g. vanilla vs ours).
  chunk-info <region dir> <chunk x> <chunk z> [--depth <n>] [--all]
      Print the NBT structure of one stored chunk.
  chunk-speed [--addr <host:port>] [--view-distance <n>] [--runs <n>] [--version <id>]
      Time how fast a joining player gets all chunks in view (offline mode).
  chunk-diff <dump a> <dump b>
      Compare chunk packets saved by `probe --dump` from two servers, decoded
      to blocks, biomes, heightmaps, light and block entities.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("extract-data") => extract_data::run(&args[1..]),
        Some("probe") => probe::run(&args[1..]),
        Some("bots") => bots::run(&args[1..]),
        Some("blocks") => blocks::run(&args[1..]),
        Some("registry-diff") => registry_diff::run(&args[1..]),
        Some("chunk-info") => chunk_info::run(&args[1..]),
        Some("chunk-diff") => chunk_diff::run(&args[1..]),
        Some("chunk-speed") => chunk_speed::run(&args[1..]),
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
