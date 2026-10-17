//! Minecraft Java Edition 26.3 compatible server.

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use mc_server::{Config, Server, find_data_dir};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
Usage: mc-server [options]

Options:
  --data <dir>           extracted game data (default: cache/mcdata/<version> found upwards)
  --world <dir>          world folder saved by vanilla 26.3 (default: a superflat test world);
                         changes are saved there every --autosave seconds and on Ctrl-C
  --autosave <seconds>   how often changed chunks are written (default 300)
  --bind <ip>            address to listen on (default 127.0.0.1)
  --port <port>          port (default 25565)
  --view-distance <n>    server view distance in chunks, 2-32 (default 10)
  --max-players <n>      (default 20)
  --motd <text>          server list description
  --no-compression       disable packet compression
  --offline              offline mode: no account check, no encryption (any name can join)
  --op <name>            let this player run commands (repeatable)
  --no-console           do not read commands from standard input

Commands (console, or chat for operators): help, setblock, fill, time, gamerule
advance_time, tick, tp, list, say, kick, save-all, stop.

Logging: RUST_LOG=debug mc-server
";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let mut data_dir: Option<PathBuf> = None;
    let mut world: Option<PathBuf> = None;
    let mut overrides: Vec<(String, String)> = Vec::new();
    let mut no_compression = false;
    let mut offline = false;
    let mut console = true;
    let mut ops = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--no-compression" => no_compression = true,
            "--offline" => offline = true,
            "--no-console" => console = false,
            "--op" => ops.push(args.next().context("--op needs a name")?),
            "--data" => data_dir = Some(args.next().context("--data needs a value")?.into()),
            "--world" => world = Some(args.next().context("--world needs a value")?.into()),
            "--bind" | "--port" | "--view-distance" | "--max-players" | "--motd" | "--autosave" => {
                let value = args
                    .next()
                    .with_context(|| format!("{arg} needs a value"))?;
                overrides.push((arg, value));
            }
            other => bail!("unknown option {other}\n\n{USAGE}"),
        }
    }

    let data_dir = match data_dir {
        Some(dir) => dir,
        None => find_data_dir(&std::env::current_dir()?)?,
    };
    let mut config = Config::new(data_dir);
    for (option, value) in overrides {
        match option.as_str() {
            "--bind" => config.bind = value.parse::<IpAddr>().context("--bind")?,
            "--port" => config.port = value.parse().context("--port")?,
            "--view-distance" => {
                config.view_distance = value.parse().context("--view-distance")?;
                if !(2..=32).contains(&config.view_distance) {
                    bail!("--view-distance must be 2-32");
                }
            }
            "--max-players" => config.max_players = value.parse().context("--max-players")?,
            "--motd" => config.motd = value,
            "--autosave" => {
                let seconds: u64 = value.parse().context("--autosave")?;
                if seconds == 0 {
                    bail!("--autosave must be at least 1 second");
                }
                config.autosave = std::time::Duration::from_secs(seconds);
            }
            _ => unreachable!("only known options are collected"),
        }
    }
    if no_compression {
        config.compression_threshold = None;
    }
    config.online_mode = !offline;
    config.world = world;
    config.console = console;
    config.ops = ops;

    let server = Server::bind(config).await?;
    server
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
