//! Development tasks, run as `cargo xtask <task>`.

mod bench;
mod blocks;
mod bots;
mod chunk_diff;
mod chunk_info;
mod chunk_speed;
mod extract_data;
mod flat;
mod gen_speed;
mod hash_check;
mod mcc;
mod mixins;
mod observe;
mod observe_commands;
mod observe_structures;
mod parity;
mod probe;
mod quiet;
mod registry_diff;
mod similarity;
mod tickets;
mod vanilla_locate;
mod watch;
mod worldgen_compare;
mod worldgen_diff;
mod worldgen_ref;
mod worldgen_seeds;

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
  bench [--only vanilla,modded,ours] [--scenarios a,b] [--rounds <n>] [--cores <n>]
        [--background <cores>] [--noisy] [--jfr] [--bots <n>] [--entities <n>] [--seconds <s>]
        | --summarize <file.jfr>
      Baseline table (docs/BENCHMARKS.md): world generation at view distance
      10 and 32, loading a stored world (16), n bots (50), hoppers, redstone
      clocks, n cows (2000) — on
      vanilla, vanilla + Lithium/C2ME and ours, each from a new world; MSPT
      from `tick query`. Runs only on a quiet machine (see quiet.rs), the
      sides taking turns over n rounds (3); --jfr adds a profiled round.
  blocks [--addr <host:port>] [--version <id>]
      Two players on an offline creative server with a superflat world: one
      places and breaks blocks, and what both receive is printed.
  registry-diff --a <host:port> --b <host:port> [--details <n>]
      Log into two offline-mode servers without known packs and compare every
      registry entry they send as NBT (e.g. vanilla vs ours).
  watch [--addr <host:port>] [--seconds <s>] [--packets <a,b,...>] [--name <player>]
      Join and print the chosen packets (hex) as they arrive, e.g. while
      running commands on a vanilla console.
  hash-check --lines <file>
      Check our item component hashes against a local vanilla server (one
      `item[components]` per line; `!` first spoils a hash on purpose).
  parity <scenario file> [--verbose]
      Run the same commands on a vanilla server and on ours (fresh superflat
      worlds, game frozen) and compare a region after every step. Scenarios
      live in xtask/parity/.
  observe attach [--first <state>] [--count <n>]
      Learn from vanilla which faces of every block state fences attach to
      (cache/mcdata/<version>/observed/fence_attach.json).
  observe-structures [--seed <n>] [--monuments <n>] [--mansions <n>]
      Record ocean monuments and woodland mansions as vanilla builds them
      (observed/monuments.json, observed/mansions.json).
  observe-commands [--lines <file>]
      Record the command tree vanilla sends an operator and its replies to
      the lines of xtask/commands.txt (observed/commands.json).
  worldgen-ref [--seed <n> | --seeds 1,2,3 | --seed-count <n>] [--radius <chunks>] [--dimension nether|end]
      Have a vanilla server generate reference worlds (cache/vanilla-server/26.3-worldgen/<seed>).
  worldgen-diff [--seed <n> | --seeds 1,2,3 | --seed-count <n>] [--radius <chunks>] [--dimension nether|end]
      Compare our generator with those worlds (biomes; blocks with WORLDGEN_BLOCKS=1).
  worldgen-compare <vanilla world> <our world> [--seeds 1,2,3 | --seed-count <n>] [--radius <chunks>]
      Compare two world folders; with several seeds the paths hold `{seed}`.
      Several seeds end in one table: seed, blocks equal %, biomes equal %.
  chunk-info <region dir> <chunk x> <chunk z> [--depth <n>] [--all]
      Print the NBT structure of one stored chunk.
  chunk-speed [--addr <host:port>] [--view-distance <n>] [--runs <n>] [--version <id>]
      Time how fast a joining player gets all chunks in view (offline mode).
  chunk-diff <dump a> <dump b>
      Compare chunk packets saved by `probe --dump` from two servers, decoded
      to blocks, biomes, heightmaps, light and block entities.
  flat
      A superflat world saved by vanilla, served and changed by us, and back:
      chunk packets identical (read and made), and vanilla finds our build.
  mcc
      Oversized chunks (external .mcc) both ways: vanilla writes one, we
      read it, change it and save, vanilla reads ours.
  mixins [--top <n>] [--show <n>]
      Rank vanilla classes/methods by how many top open-source Modrinth mods
      (26.x builds, into tools/modrinth/) patch them with Mixin; writes
      cache/mixins/{methods,classes}.tsv and ranking.md
  tickets [--only vanilla,ours] [--view <n>] [--sim <n>]
      Which chunks tick, seen from outside: a bot stands at spawn; per chunk
      around it and in a far force-loaded one, an item falls, a furnace
      smelts, water flows, wheat grows (or not).
  similarity [--reference <dir>] [--fail <n>] [--top <n>] [--self-test]
      Clean-room guard: compare our Rust sources with the decompiled vanilla
      sources (reference/26.3/server-src, or $MC_REFERENCE) by winnowing
      fingerprints; prints only paths, line ranges and scores. Skips when
      the sources are missing; fails when a file pair shares --fail (12) or
      more. --self-test checks that respelled vanilla pieces are caught.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("extract-data") => extract_data::run(&args[1..]),
        Some("probe") => probe::run(&args[1..]),
        Some("bench") => bench::run(&args[1..]),
        Some("bots") => bots::run(&args[1..]),
        Some("blocks") => blocks::run(&args[1..]),
        Some("registry-diff") => registry_diff::run(&args[1..]),
        Some("chunk-info") => chunk_info::run(&args[1..]),
        Some("chunk-diff") => chunk_diff::run(&args[1..]),
        Some("chunk-speed") => chunk_speed::run(&args[1..]),
        Some("flat") => flat::run(&args[1..]),
        Some("gen-speed") => gen_speed::run(&args[1..]),
        Some("mcc") => mcc::run(&args[1..]),
        Some("mixins") => mixins::run(&args[1..]),
        Some("tickets") => tickets::run(&args[1..]),
        Some("similarity") => similarity::run(&args[1..]),
        Some("watch") => watch::run(&args[1..]),
        Some("hash-check") => hash_check::run(&args[1..]),
        Some("parity") => parity::run(&args[1..]),
        Some("observe") => observe::run(&args[1..]),
        Some("observe-structures") => observe_structures::run(&args[1..]),
        Some("observe-commands") => observe_commands::run(&args[1..]),
        Some("worldgen-ref") => worldgen_ref::run(&args[1..]),
        Some("worldgen-diff") => worldgen_diff::run(&args[1..]),
        Some("worldgen-compare") => worldgen_compare::run(&args[1..]),
        Some("vanilla-locate") => vanilla_locate::run(&args[1..]),
        Some("worldgen-logs") => worldgen_compare::logs(&args[1..]),
        Some("worldgen-map") => worldgen_compare::map(&args[1..]),
        Some("worldgen-find") => worldgen_compare::find(&args[1..]),
        Some("worldgen-slice") => worldgen_compare::slice(&args[1..]),
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
