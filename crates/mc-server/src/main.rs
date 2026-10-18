//! Minecraft Java Edition 26.3 compatible server, laid out like vanilla's:
//! started in its folder, it reads (and fills in) `server.properties`, keeps
//! the world in `<level-name>/`, the player lists in `ops.json`,
//! `whitelist.json`, `banned-players.json`, `banned-ips.json` and
//! `usercache.json`, and logs to `logs/latest.log`.

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use mc_server::properties::Properties;
use mc_server::{Config, Server, find_data_dir};
use tracing::info;

const USAGE: &str = "\
Usage: mc-server [options]

Run it in the server's folder (or point --universe at it). Settings come
from server.properties there, created with vanilla's defaults on the first
start; the world is the folder named by level-name.

Vanilla options:
  --universe <dir>       the server's folder (default: the current folder)
  --world <name>         world folder name (overrides level-name)
  --port <port>          overrides server-port
  --initSettings         write server.properties and stop
  --nogui                accepted (there is no window)

Other options:
  --data <dir>           extracted game data (default: cache/mcdata/<version> found upwards)
  --seed <seed>          seed of a new world (overrides level-seed)
  --op <name>            add an operator to ops.json (repeatable)
  --offline              online-mode=false for this run
  --no-console           do not read commands from standard input

Logging: RUST_LOG=debug mc-server
";

fn game_mode(name: &str) -> i32 {
    match name.trim() {
        "creative" | "1" => 1,
        "adventure" | "2" => 2,
        "spectator" | "3" => 3,
        _ => 0,
    }
}

fn difficulty(name: &str) -> i32 {
    match name.trim() {
        "peaceful" | "0" => 0,
        "normal" | "2" => 2,
        "hard" | "3" => 3,
        _ => 1,
    }
}

/// A seed field as vanilla reads it: a number, or text hashed.
fn seed(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.parse::<i64>().unwrap_or_else(|_| {
        i64::from(
            text.encode_utf16()
                .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(c as i32)),
        )
    }))
}

#[tokio::main]
async fn main() -> ExitCode {
    mc_server::started();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err:#}");
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Game data: given, or found above the server folder or the program.
fn data_dir(given: Option<PathBuf>, server_dir: &Path) -> Result<PathBuf> {
    if let Some(d) = given {
        return Ok(d);
    }
    if let Ok(d) = find_data_dir(server_dir) {
        return Ok(d);
    }
    let exe = std::env::current_exe()?;
    find_data_dir(exe.parent().unwrap_or(&exe))
}

async fn run() -> Result<()> {
    let mut data: Option<PathBuf> = None;
    let mut universe: Option<PathBuf> = None;
    let mut world_name: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut seed_arg: Option<String> = None;
    let mut init_only = false;
    let mut offline = false;
    let mut console = true;
    let mut ops = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--nogui" | "nogui" | "--bonusChest" | "--forceUpgrade" | "--eraseCache"
            | "--safeMode" => {}
            "--initSettings" => init_only = true,
            "--universe" => universe = Some(value()?.into()),
            "--world" => world_name = Some(value()?),
            "--port" => port = Some(value()?.parse().context("--port")?),
            "--data" => data = Some(value()?.into()),
            "--seed" => seed_arg = Some(value()?),
            "--op" => ops.push(value()?),
            "--offline" => offline = true,
            "--no-console" => console = false,
            other => bail!("unknown option {other}\n\n{USAGE}"),
        }
    }
    let server_dir = match universe {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    std::fs::create_dir_all(&server_dir)?;
    mc_server::logs::init(Some(&server_dir));
    info!(
        "Starting minecraft server version {}",
        mc_protocol::VERSION_NAME
    );
    info!("Loading properties");
    let mut props = Properties::load(&server_dir.join("server.properties"))?;
    if init_only {
        info!(
            "Initialized '{}'",
            server_dir.join("server.properties").display()
        );
        return Ok(());
    }
    if let Some(name) = &world_name {
        props.set("level-name", name);
    }

    let mut config = Config::new(data_dir(data, &server_dir)?);
    config.server_dir = Some(server_dir.clone());
    let ip = props.get("server-ip").trim().to_owned();
    config.bind = if ip.is_empty() {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        ip.parse().with_context(|| format!("server-ip {ip}"))?
    };
    config.port = match port {
        Some(p) => p,
        None => props.int("server-port").unwrap_or(25565) as u16,
    };
    config.motd = props.get("motd").to_owned();
    config.max_players = props.int("max-players").unwrap_or(20).max(1) as usize;
    config.view_distance = props.int("view-distance").unwrap_or(10).clamp(2, 32) as u8;
    config.compression_threshold = match props.int("network-compression-threshold") {
        Some(t) if t >= 0 => Some(t as usize),
        Some(_) => None,
        None => Some(256),
    };
    config.online_mode = props.bool("online-mode") && !offline;
    config.white_list = props.bool("white-list");
    config.new_world_game_mode = game_mode(props.get("gamemode"));
    config.new_world_difficulty = difficulty(props.get("difficulty"));
    config.seed = seed(seed_arg.as_deref().unwrap_or(props.get("level-seed")));
    config.console = console;
    config.ops = ops;
    let level_name = props.get("level-name").to_owned();
    let flat = props.get("level-type").contains("flat");
    config.world = if flat {
        None
    } else {
        Some(server_dir.join(&level_name))
    };
    info!(
        "Default game type: {}",
        ["SURVIVAL", "CREATIVE", "ADVENTURE", "SPECTATOR"][config.new_world_game_mode as usize]
    );
    info!("Preparing level \"{level_name}\"");

    let server = Server::bind(config).await?;
    server
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
