# AGENTS.md

Notes for AI coding agents (Claude Code, Codex, Cursor and others) working on Tuff. Humans
should read [CONTRIBUTING.md](CONTRIBUTING.md); everything there applies to you too.

## What this is

Tuff is a Minecraft: Java Edition 26.3 dedicated server written from scratch in Rust. The
goal is vanilla 26.3 behaviour, exactly, at a fraction of the cost. A stock 26.3 client must
join it and never notice a difference: same chat and error text, same command suggestions,
same files on disk, same world format both ways.

## Hard rules

1. **No Minecraft code in the repo.** You may read the decompiled source in `reference/26.3/`
   to learn what vanilla does. Never paste it, translate it line by line or keep its
   structure. Understand it, close it, write our own code. `cargo xtask similarity` checks
   this; run it when you've written a lot of game logic.
2. **Never commit** anything from `reference/`, `tools/`, `cache/` or `run/`, or any `.jar`
   or `.class`. They're gitignored. Don't force-add them.
3. **Vanilla is the spec.** Any difference a player or operator can see is a bug. Messages go
   through vanilla's translation keys with the same arguments, so the client localizes them.
4. **Behaviour changes are opt-in.** Speeding something up must not change order of updates,
   RNG use or timing. If it has to, put it behind a config flag that's off by default.
5. **Check against vanilla, not against your own understanding.** "Tests pass" isn't enough
   when the test encodes your guess. See "Checking your work".

## Layout

```
crates/
  mc-nbt        NBT
  mc-protocol   26.3 packets, framing, compression, encryption
  mc-data       block and registry tables, loaded from cache/mcdata/26.3
  mc-world      sections, palettes, heightmaps, stored light
  mc-worldgen   noise, biomes, surface
  mc-features   features, structures, datapack worldgen overlay
  mc-light      lighting
  mc-entity     entities, physics, pathfinding, mob AI
  mc-tick       tick phases, fluids, redstone, block logic
  mc-storage    Anvil region files
  mc-command    command tree
  mc-server     networking, the tick loop, commands, everything else; builds the `tuff` binary
xtask/          dev tools, run as `cargo xtask <name>`
docs/           design and specs, in Russian
```

`mc-server/src` is large and flat-ish: one file or folder per game system (`hoppers.rs`,
`brewing.rs`, `bosses/`, `mobs/`, `commands/`). Look for an existing file before adding one.

## Setup and commands

Game data isn't in the repo. It comes from Mojang's `server.jar` (README, "Running it"):

```bash
cargo xtask extract-data          # reference/26.3/server.jar -> cache/mcdata/26.3
```

What CI runs, and what must pass before you're done:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Running the server:

```bash
cargo build --release -p mc-server
mkdir -p run && cd run && ../target/release/tuff --op <name> --offline
```

Logs go to `run/logs/latest.log`, crash reports to `run/crash-reports/`.

## Checking your work

Useful `cargo xtask` tools (see `xtask/src/main.rs` for the full list):

| tool | what it does |
|---|---|
| `parity` | runs the same scenario on vanilla and Tuff, compares state |
| `observe`, `observe-commands`, `observe-structures` | records what vanilla does for a set of inputs, to compare with ours |
| `worldgen-diff`, `worldgen-compare`, `chunk-diff` | terrain and chunk contents against vanilla |
| `probe` | a protocol client that dumps raw packets from any server, ours or vanilla |
| `bots` | load test with fake players against a running `--offline` server |
| `bench` | vanilla vs Lithium/C2ME vs Tuff; see docs/BENCHMARKS.md for the rules |
| `similarity` | clean-room check against the local decompile |

When a bug only shows up with the real client and our end-to-end tests
(`crates/mc-server/tests/`) pass, the tests are probably wrong in the same way the code is.
Don't keep guessing. Either run vanilla on another port and diff `probe --dump` output byte by
byte, or add temporary logging of the raw packets and ask the user to reproduce once. Wiki
packet tables lag behind 26.3; the decompile and the real client are the truth.

Performance claims need `cargo xtask bench` numbers before and after, measured on a quiet
machine. Don't publish or commit numbers from a noisy run.

## Style

- Code, comments, identifiers, commit messages: English. Docs in `docs/`: Russian.
- Match the code around you. Module docs (`//!`) say what the module is for; doc comments say
  what a thing means in game terms, often in relation to vanilla. No comments that restate the
  code.
- No new dependencies without a reason. Keep hot paths free of allocation.
- The project name is written "Tuff" in prose; `tuff` only for the binary, commands and paths.
- The README stays short and shows only benchmark rows where Tuff is ahead of vanilla; the
  full table, losses included, lives in docs/BENCHMARKS.md. Keep the alpha warning on top
  until the owner says otherwise.
- Commit subjects describe the result in plain words, the way a player would notice it:
  "Piglins go for thrown gold; no fall damage after flying". No `feat:` prefixes.

## Working in parallel

Several agents often work at once, each in its own git worktree under `.claude/worktrees/`
on a `claude/<topic>` branch. Keep to one subsystem and its files so branches merge cleanly.
A new worktree has no `cache/` or `reference/` (they're gitignored), and tests that need game
data then print "skipped: no game data" and pass without testing anything. Link them first:
`ln -s "$(git rev-parse --path-format=absolute --git-common-dir)/../cache" cache`, the same
for `reference`, and check a data test doesn't print "skipped". After a branch is merged, remove its worktree and clear stale `target/` directories; they
grow fast.

When you split work across subagents, pick each one's model by
[docs/AGENT_MODELS.md](docs/AGENT_MODELS.md): the cheapest model that gets the task right,
not the strongest one everywhere.
