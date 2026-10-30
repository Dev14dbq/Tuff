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
Usage: tuff [options]

Run it in the server's folder (or point --universe at it). Settings come
from server.properties there, created with vanilla's defaults on the first
start; the world is the folder named by level-name.

Vanilla options:
  --universe <dir>       the server's folder (default: the current folder)
  --world <name>         world folder name (overrides level-name)
  --port <port>          overrides server-port
  --bind <address>       overrides server-ip for this run
  --initSettings         write server.properties and stop
  --nogui                accepted (there is no window)
  --forceUpgrade         upgrade and rewrite every stored chunk and player
                         before the server starts
  --eraseCache           with --forceUpgrade, drop stored light and heightmaps

Other options:
  --data <dir>           extracted game data (default: cache/mcdata/<version> found
                         upwards, else prepared in <server>/cache on the first start)
  --reports <zip>        generated tables for that (default: tuff-reports-<version>.zip
                         next to tuff or in the server folder, else downloaded from
                         this Tuff release)
  --seed <seed>          seed of a new world (overrides level-seed)
  --op <name>            add an operator to ops.json (repeatable)
  --offline              online-mode=false for this run
  --no-console           do not read commands from standard input
  -V, --version          print the version and stop

Logging: RUST_LOG=debug tuff
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

/// Game data: given, found above the server folder or the program, or
/// prepared in `<server>/cache` from Mojang's `server.jar` and the tables of
/// this release.
async fn data_dir(
    given: Option<PathBuf>,
    reports: Option<PathBuf>,
    server_dir: &Path,
) -> Result<PathBuf> {
    if let Some(d) = given {
        return Ok(d);
    }
    if let Ok(d) = find_data_dir(server_dir) {
        return Ok(d);
    }
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().unwrap_or(&exe).to_path_buf();
    if let Ok(d) = find_data_dir(&exe_dir) {
        return Ok(d);
    }
    let version = mc_protocol::VERSION_NAME;
    let name = mc_bootstrap::tables_name(version);
    let tables = match reports {
        Some(zip) => mc_bootstrap::Tables::File(zip),
        None => [exe_dir.join(&name), server_dir.join(&name)]
            .into_iter()
            .find(|p| p.is_file())
            .map_or_else(
                || mc_bootstrap::Tables::Release {
                    base: format!(
                        "{}/releases/download/v{}",
                        env!("CARGO_PKG_REPOSITORY"),
                        env!("CARGO_PKG_VERSION")
                    ),
                },
                mc_bootstrap::Tables::File,
            ),
    };
    let cache = server_dir.join("cache");
    tokio::task::spawn_blocking(move || {
        mc_bootstrap::prepare(&mc_bootstrap::Prepare {
            version,
            protocol: mc_protocol::PROTOCOL_VERSION,
            cache: &cache,
            tables,
        })
    })
    .await?
    .context("preparing game data (or pass --data <dir> made by `cargo xtask extract-data`)")
}

async fn run() -> Result<()> {
    let mut data: Option<PathBuf> = None;
    let mut reports: Option<PathBuf> = None;
    let mut universe: Option<PathBuf> = None;
    let mut world_name: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut bind: Option<IpAddr> = None;
    let mut seed_arg: Option<String> = None;
    let mut init_only = false;
    let mut force_upgrade = false;
    let mut erase_cache = false;
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
            "-V" | "--version" => {
                println!(
                    "tuff {} (Minecraft {}, protocol {})",
                    env!("CARGO_PKG_VERSION"),
                    mc_protocol::VERSION_NAME,
                    mc_protocol::PROTOCOL_VERSION
                );
                return Ok(());
            }
            "--nogui" | "nogui" | "--bonusChest" | "--safeMode" => {}
            "--eraseCache" => erase_cache = true,
            "--forceUpgrade" => force_upgrade = true,
            "--initSettings" => init_only = true,
            "--universe" => universe = Some(value()?.into()),
            "--world" => world_name = Some(value()?),
            "--port" => port = Some(value()?.parse().context("--port")?),
            "--bind" => bind = Some(value()?.parse().context("--bind")?),
            "--data" => data = Some(value()?.into()),
            "--reports" => reports = Some(value()?.into()),
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
    info!("Starting tuff for Minecraft {}", mc_protocol::VERSION_NAME);
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

    let mut config = Config::new(data_dir(data, reports, &server_dir).await?);
    config.server_dir = Some(server_dir.clone());
    let ip = props.get("server-ip").trim().to_owned();
    config.bind = if let Some(addr) = bind {
        addr
    } else if ip.is_empty() {
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
    config.simulation_distance = props.int("simulation-distance").unwrap_or(10).clamp(2, 32) as u8;
    config.compression_threshold = match props.int("network-compression-threshold") {
        Some(t) if t >= 0 => Some(t as usize),
        Some(_) => None,
        None => Some(256),
    };
    config.online_mode = props.bool("online-mode") && !offline;
    config.enforce_secure_profile = props.bool("enforce-secure-profile");
    config.status_heartbeat_interval =
        props.int("status-heartbeat-interval").unwrap_or(0).max(0) as u64;
    config.code_of_conduct = mc_server::serverlist::CodeOfConduct::load(
        props.bool("enable-code-of-conduct"),
        Some(&server_dir),
    )
    .map_err(anyhow::Error::msg)?
    .map(std::sync::Arc::new);
    config.text_filter = mc_server::textfilter::TextFilter::from_properties(
        props.int("text-filtering-version").unwrap_or(0),
        props.get("text-filtering-config"),
    );
    config.white_list = props.bool("white-list");
    config.op_permission_level = props.int("op-permission-level").unwrap_or(4).clamp(0, 4) as i32;
    config.spawn_protection = props.int("spawn-protection").unwrap_or(16).max(0) as i32;
    {
        use mc_server::oldlists::{
            MojangResolver, OfflineResolver, RETRY_DELAY, Resolver, convert,
        };
        let mut resolver: Box<dyn Resolver + Send> = if config.online_mode {
            Box::new(MojangResolver)
        } else {
            Box::new(OfflineResolver)
        };
        let (dir, level) = (server_dir.clone(), config.op_permission_level);
        tokio::task::spawn_blocking(move || convert(&dir, level, resolver.as_mut(), RETRY_DELAY))
            .await?
            .map_err(anyhow::Error::msg)?;
    }
    config.new_world_game_mode = game_mode(props.get("gamemode"));
    config.new_world_difficulty = difficulty(props.get("difficulty"));
    config.seed = seed(seed_arg.as_deref().unwrap_or(props.get("level-seed")));
    config.console = console;
    config.ops = ops;
    config.enable_status = props.bool("enable-status");
    config.hide_online_players = props.bool("hide-online-players");
    config.icon = mc_server::serverlist::icon(&server_dir.join("server-icon.png"));
    config.resource_pack = mc_server::serverlist::ResourcePack::from_properties(&props);
    if props.bool("enable-rcon") {
        let password = props.get("rcon.password").to_owned();
        if password.is_empty() {
            tracing::warn!("No rcon password set in server.properties, rcon disabled!");
        } else {
            config.rcon = Some((props.int("rcon.port").unwrap_or(25575) as u16, password));
        }
    }
    config.broadcast_rcon_to_ops = props.bool("broadcast-rcon-to-ops");
    if props.bool("enable-query") {
        config.query_port = Some(props.int("query.port").unwrap_or(25565) as u16);
    }
    config.allow_flight = props.bool("allow-flight");
    config.rate_limit = props.int("rate-limit").unwrap_or(0).max(0) as u32;
    config.connection_throttle = std::time::Duration::from_millis(
        props.int("connection-throttle").unwrap_or(0).max(0) as u64,
    );
    config.max_pending_per_ip = props
        .int("max-pending-connections-per-ip")
        .unwrap_or(0)
        .max(0) as u32;
    let seconds = |key: &str| {
        let s = props.int(key).unwrap_or(10);
        s.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    config.chat_spam_seconds = seconds("chat-spam-threshold-seconds");
    config.command_spam_seconds = seconds("command-spam-threshold-seconds");
    config.max_tick_time = props
        .int("max-tick-time")
        .filter(|&ms| ms > 0)
        .map(|ms| std::time::Duration::from_millis(ms as u64));
    config.backup_minutes = props.int("backup-interval").unwrap_or(0).max(0) as u32;
    config.backup_keep = props.int("backup-keep").unwrap_or(5).max(1) as usize;
    config.set_proxy(
        props.get("proxy"),
        props.get("velocity-secret"),
        props.get("bungee-tokens"),
    );
    let management_host = props.get("management-server-host").trim();
    config.management = mc_server::management::ManagementConfig {
        enabled: props.bool("management-server-enabled"),
        host: if management_host.is_empty() {
            "localhost".to_owned()
        } else {
            management_host.to_owned()
        },
        port: props
            .int("management-server-port")
            .unwrap_or(0)
            .clamp(0, 65535) as u16,
        secret: props.get("management-server-secret").to_owned(),
        tls: props.bool("management-server-tls-enabled"),
    };
    let level_name = props.get("level-name").to_owned();
    let flat = props.get("level-type").contains("flat");
    config.level_dir = Some(server_dir.join(&level_name));
    // `level-type` only decides what a new world is; a folder that is there
    // keeps the generator its settings name.
    config.new_world_flat = flat;
    config.world = Some(server_dir.join(&level_name));
    info!(
        "Default game type: {}",
        ["SURVIVAL", "CREATIVE", "ADVENTURE", "SPECTATOR"][config.new_world_game_mode as usize]
    );
    info!("Preparing level \"{level_name}\"");
    let world = server_dir.join(&level_name);
    if world.is_dir() {
        let version = mc_data::McData::read_version(&config.data_dir)?;
        mc_server::prepare_world(&world, version.world_version, &version.name)?;
        if force_upgrade {
            info!("Forcing world upgrade");
            let report = mc_server::upgrade_world(&world, version.world_version, erase_cache)?;
            info!(
                "Upgraded {} chunks in {} region files and {} players; {} current, {} left as stored",
                report.chunks_rewritten,
                report.regions,
                report.players,
                report.chunks_current,
                report.chunks_left
            );
        }
    }

    let server = Server::bind(config).await?;
    // The Nether and the End load their generators now, not when the first
    // player steps through a portal.
    let ctx = server.context().clone();
    std::thread::spawn(move || {
        for index in 1..mc_server::DIMENSIONS.len() {
            ctx.dimension(index);
        }
    });
    server.run(stop_signal()).await
}

/// Ctrl+C, or SIGTERM from `kill`, systemd or `docker stop`: vanilla saves
/// and stops on either.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
