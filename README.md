# Tuff

> [!WARNING]
> **TUFF IS IN ALPHA.** Expect bugs, missing features and breaking changes between versions.
> **Back up your world before you point Tuff at it**, and don't run it for a server you can't
> afford to lose yet.

A Minecraft: Java Edition server written from scratch in Rust.

Drop it into the folder of your vanilla 26.3 server and start it. Your world, `server.properties`,
ops, whitelist, bans and datapacks keep working, and players join with the normal game client.
It just uses a lot less CPU and memory doing it.

Where we're going: you join a server and its mods load for you. New blocks, mobs, items and
UI come from the server, and nobody has to install a modpack. When you leave, they're gone.
That part isn't built yet; the server underneath it is.

## Numbers

Same machine, same seed, 4 threads each. Median of three runs.

| | vanilla 26.3 | Tuff |
|---|---|---|
| 50 players flying around, ms per tick | 15.0 | **3.4** |
| idle server, RAM | 659 MB | **87 MB** |
| 50 players, RAM | 969 MB | **879 MB** |
| loading an existing world, chunks/s | 681 | **2092** |
| generating new terrain, chunks/s | 79 | **101** |
| 2000 cows in a pen, ms per tick | 12.8 | **3.0** |

How it's measured: [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

## Status

**Alpha.** Survival is playable. If something behaves differently from vanilla, that's a bug.
Please open an issue.

✅ done and checked against vanilla · 🟡 works, still being finished or checked · ⬜ not started

| | |
|---|---|
| ✅ | Joining with the normal 26.3 client, online and offline mode, encryption, compression |
| ✅ | Opening and saving vanilla worlds; vanilla opens them again afterwards |
| ✅ | Worlds from 1.18 on and Paper worlds are upgraded on first start |
| ✅ | World generation: terrain, biomes, caves, villages and most structures |
| ✅ | Redstone, pistons, fluids, hoppers and block updates in vanilla order |
| ✅ | Mobs, villagers and trading, raids, the Ender Dragon, the Wither |
| ✅ | Enchanting, brewing, crafting, advancements, statistics, recipe book |
| ✅ | All vanilla commands, scoreboards, boss bars, `.mcfunction`, datapacks, game rules |
| ✅ | Whitelist, bans, ops, RCON, query, the vanilla management protocol |
| ✅ | Hardened against malicious clients; packets, NBT, regions and commands are fuzzed |
| 🟡 | Ocean monuments and woodland mansions: rooms are not laid out like vanilla yet |
| 🟡 | Popular third-party datapacks (Terralith and others): not tested yet |
| 🟡 | Velocity and BungeeCord: built, not yet tested behind a real proxy |
| 🟡 | Text filter service: version 0 works, version 1 (Azure login) doesn't |
| 🟡 | Backups, built-in profiler, `/pregen`: built, still being checked |
| 🟡 | Release builds for macOS, Linux, Windows and Docker: first release not cut yet |
| 🟡 | 24-hour run with 100 bots: in progress |
| 🟡 | Beating vanilla with Lithium and C2ME on every benchmark: hoppers done, redstone in progress |
| ⬜ | A full audit of every vanilla system, item by item |
| ⬜ | Dimensions ticking in parallel |
| ⬜ | Mods sent by the server (WASM) |
| ⬜ | Versions other than 26.3 |

The detailed plan is in [docs/ROADMAP.md](docs/ROADMAP.md) (in Russian).

## Running it

Download the archive for your system from
[Releases](https://github.com/Dev14dbq/tuff/releases), check it against `SHA256SUMS`, unpack
it, and start `tuff` in your server's folder:

```bash
cd my-server && /path/to/tuff --op YourName
```

No Java, no Rust. Tuff ships no Minecraft files: on the first start it downloads `server.jar`
from Mojang (about 60 MB, checked by SHA1) into `cache/` and reads the game data out of it.
The id tables it can't make without Java (`tuff-reports-26.3.zip`) come in the archive.

It listens on `127.0.0.1` by default. Clear `server-ip` in `server.properties` to open it to
the network. Settings, commands and moving from vanilla or Paper:
[docs/ADMIN.md](docs/ADMIN.md).

With Docker (details in [docs/admin/docker.md](docs/admin/docker.md)):

```bash
docker build -t tuff .
docker run -it -p 25565:25565 -v tuff-data:/data tuff
```

From source, for development: `cargo build --release -p mc-server`. A build that isn't a
release needs the tables from `cargo xtask extract-data`, which takes Java 25+ once
([CONTRIBUTING.md](CONTRIBUTING.md)).

## Contributing

Help is welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first: we don't copy Minecraft's
code, and every change gets checked against vanilla. The rest of the docs are in Russian for
now.

## License

GPL-3.0-or-later. Not affiliated with Mojang or Microsoft. Minecraft is a trademark of Mojang
Synergies AB.
