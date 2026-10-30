# Contributing

Thanks for wanting to help. Issues can be in English or Russian. Code, comments and commit
messages are in English.

## Don't copy Minecraft's code

This matters more than anything else here. Minecraft is proprietary, and Tuff only stays
legal if none of its code ends up in this repo.

You can decompile the game locally to understand how something works. Write down what it does
in your own words (that's what `docs/specs/` is for), then close the decompiled source and
write the code. Don't paste or translate it line by line.

`cargo xtask similarity` compares our code against a local decompile and fails if something
looks too close. Never commit anything from `reference/`, `tools/` or `cache/`, or any `.jar`
or `.class` file. CI will reject it anyway.

## Getting set up

You need Rust and Java 25+. Tests and dev tools read game data from `cache/mcdata/`, made
from Mojang's `server.jar` (none of it is ever committed):

```bash
git clone https://github.com/Dev14dbq/tuff && cd tuff
cargo xtask fetch-server-jar       # reference/26.3/server.jar from Mojang, SHA1 checked
cargo xtask extract-data           # -> cache/mcdata/26.3 (runs Mojang's data generator)
cargo build --release -p mc-server
mkdir -p run && cd run && ../target/release/tuff --op YourName --offline
```

Some tables come from watching a running vanilla server (`cargo xtask observe-commands`,
`cargo xtask observe <fact>`); they need Mojang's EULA accepted in
`cache/vanilla-server/26.3/eula.txt`.

To decompile the server for reading:

```bash
unzip -p reference/26.3/server.jar META-INF/versions/26.3/server-26.3.jar > reference/26.3/server-26.3.jar
java -jar tools/vineflower.jar reference/26.3/server-26.3.jar reference/26.3/server-src/
```

(Get [Vineflower](https://github.com/Vineflower/vineflower/releases) into `tools/` first.)

## Before you open a PR

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The goal is to behave exactly like vanilla 26.3, down to chat messages and command errors. So
if you change game behaviour, show how you checked it against vanilla: a parity test
(`cargo xtask parity`, `observe`, `worldgen-diff`) or expected values taken from a real
vanilla server. If you made something faster, include `cargo xtask bench` numbers from before
and after.

Smaller PRs get reviewed faster. One subsystem at a time is ideal.

## License

Your contributions are licensed under GPL-3.0-or-later, same as the rest of the project.
