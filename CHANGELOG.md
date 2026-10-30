# Changelog

All notable changes to Tuff are written down here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Until 1.0 any release may break worlds, configs or
behaviour; back up first.

## [Unreleased]

## [0.1.0] - 2026-10-29

First tagged alpha. A stock Minecraft: Java Edition 26.3 client joins it; everything a vanilla
26.3 dedicated server does is the target, and differences are bugs.

### Added

- Server: the `tuff` binary, started in the folder of a vanilla server. It reads and fills in
  `server.properties` with vanilla's defaults (except `server-ip`, which is `127.0.0.1`),
  keeps `ops.json`, `whitelist.json`, `banned-players.json`, `banned-ips.json` and
  `usercache.json`, and logs to `logs/latest.log`. `tuff --version` prints the version; SIGTERM
  saves and stops like `/stop`.
- No Java needed to run it: on the first start `tuff` downloads `server.jar` from Mojang
  (version manifest, SHA1 checked) into `cache/`, reads the vanilla data pack out of it, and
  takes the generated id tables from `tuff-reports-26.3.zip` in the release.
- Protocol: 26.3 handshake, status, login, configuration and play; compression and
  encryption; online mode with Mojang session checks and offline mode; signed chat; Velocity
  modern forwarding, BungeeCord forwarding and BungeeGuard tokens; server links, resource packs,
  server icon and the code of conduct screen.
- World and storage: Anvil region files read and written both ways with vanilla 26.3
  (`region`, `entities`, `poi`, `.mcc` oversized chunks); old `DIM-1`/`DIM1` and Paper's
  `world_nether`/`world_the_end` folders are read; `--forceUpgrade`; superflat worlds; sky and
  block light.
- World generation: 26.3 noise, density functions, biomes, surface rules, carvers, features and
  structures for the Overworld, the Nether and the End, with datapack worldgen overrides.
- Ticking: scheduled ticks, fluids, random ticks, redstone, pistons, rails and minecarts,
  hoppers, dispensers and droppers, furnaces, brewing, enchanting, crafting and the crafter,
  composters, beacons, conduits, sculk, campfires, jukeboxes, decorated pots and brushing.
- Entities: player movement, combat, damage and effects; dropped items and experience;
  projectiles; animals, monsters, aquatic mobs, villagers with trades, gossip and breeding,
  wandering traders, iron golems, raids, patrols and zombie sieges; the Ender Dragon, the Wither
  and the Warden; leads, mounts, item frames, paintings, armor stands and mannequins.
- Game systems: advancements and their triggers, the recipe book, statistics, scoreboards and
  teams, boss bars, the world border, weather, maps, chunk tickets as in vanilla.
- Commands: vanilla 26.3's command tree with its suggestions, permission levels and
  translated messages, `/execute`, functions, command blocks and command block minecarts.
- Datapacks: `world/datapacks` with `/reload` and `/datapack`, feature-flag checks.
- Administration: RCON, Query, the vanilla JSON-RPC management server, crash reports and a
  tick watchdog (`max-tick-time`), packet rate limiting (`rate-limit`).
- Beyond vanilla: `/backup` and scheduled backups (`backup-interval`, `backup-keep`),
  `/pregen` for chunk pregeneration that survives restarts, `/perf` reports with tick-time
  percentiles, `/chase`.
- Tooling: `cargo xtask` with data extraction from the user's own `server.jar`, parity and
  observation runs against vanilla, worldgen and chunk diffs, a protocol probe, load-test bots,
  benchmarks and a clean-room similarity check.
- Release: CI on Linux, macOS and Windows; release builds for Linux (x86_64 and aarch64,
  glibc and static musl), macOS (aarch64 and x86_64) and Windows (x86_64) with SHA-256
  checksums and the generated tables; `cargo deny` and `cargo audit` checks; a Docker image
  that runs as a non-root user.
- Admin docs in Russian: settings, commands, moving from vanilla and Paper, Docker.

[Unreleased]: https://github.com/Dev14dbq/tuff/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Dev14dbq/tuff/releases/tag/v0.1.0
