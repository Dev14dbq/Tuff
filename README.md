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

**Alpha.** Survival is playable: world generation, mobs, villagers, raids, the Ender Dragon
and the Wither, redstone, enchanting, brewing, advancements, commands and datapacks. Online
and offline mode, Velocity and BungeeCord, the vanilla management protocol.

Not yet: plugins and mods, versions other than 26.3. If something behaves differently from
vanilla, that's a bug. Please open an issue.

## Running it

You need Rust and, once, Java 25+. Tuff doesn't ship any Minecraft files, so the first step
pulls game data out of Mojang's own `server.jar`.

```bash
git clone https://github.com/Dev14dbq/tuff && cd tuff

mkdir -p reference/26.3
curl -o reference/26.3/26.3.json https://piston-meta.mojang.com/v1/packages/11f1aefe473eefba0af62e3286c33b692807b2eb/26.3.json
curl -o reference/26.3/server.jar https://piston-data.mojang.com/v1/objects/33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c/server.jar
cargo xtask extract-data

cargo build --release -p mc-server
mkdir -p run && cd run && ../target/release/tuff --op YourName
```

It listens on `127.0.0.1` by default. Clear `server-ip` in `server.properties` to open it to
the network. More in [docs/ADMIN.md](docs/ADMIN.md).

With Docker:

```bash
docker build -t tuff .
docker run --rm -p 25565:25565 \
  -v "$PWD/cache/mcdata:/data/cache/mcdata:ro" \
  -v tuff-data:/data \
  tuff
```

## Contributing

Help is welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first: we don't copy Minecraft's
code, and every change gets checked against vanilla. The rest of the docs are in Russian for
now.

## License

GPL-3.0-or-later. Not affiliated with Mojang or Microsoft. Minecraft is a trademark of Mojang
Synergies AB.
