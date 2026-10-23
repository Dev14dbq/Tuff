//! Minecraft Java Edition 26.3 compatible server.
//!
//! Current scope (ROADMAP Ф1): server list status, online- and offline-mode
//! login, configuration with the vanilla data pack as a known pack, the player
//! list, and a superflat world players can walk and chat in. One tokio task per connection;
//! the tick thread from ARCHITECTURE.md §4 arrives with world simulation.

mod auth;
mod beacons;
mod blocks;
mod blockuse;
mod books;
mod border;
mod bossbars;
mod brewing;
mod combat;
mod command_blocks;
mod commands;
mod connection;
mod containers;
mod context;
mod crafting;
mod damage;
mod daylight;
mod decor;
mod dropped;
mod effects;
mod enchant_effects;
mod enchants;
mod explosion;
mod fastmap;
mod forced;
mod furnaces;
mod fx;
mod generation;
mod growth;
mod held;
mod hits;
mod hoppers;
mod interact;
mod items;
mod level;
mod lists;
pub mod logs;
mod loot;
mod maps;
mod menus;
mod mobs;
mod packs;
mod pipeline;
mod players;
mod portals;
mod precipitation;
mod projectiles;
pub mod properties;
mod random_ticks;
pub mod rules;
mod scoreboard;
mod storage;
mod summon;
mod tick;
mod trading;
mod useblocks;
mod util;
mod vehicles;
mod vitals;
mod weather;
mod world;
mod xp;

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use tokio::net::TcpListener;
use tracing::{debug, info};

pub use context::{DIMENSIONS, ServerContext};

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: IpAddr,
    pub port: u16,
    /// `cache/mcdata/<version>` from `cargo xtask extract-data`.
    pub data_dir: PathBuf,
    pub motd: String,
    pub max_players: usize,
    /// Server view distance in chunks; players get min(this, their own setting).
    pub view_distance: u8,
    /// Packets of at least this many bytes are compressed; `None` disables compression.
    pub compression_threshold: Option<usize>,
    /// Verify accounts with the session server and encrypt connections.
    pub online_mode: bool,
    /// Base URL of the session server (Mojang's by default).
    pub session_server: String,
    /// A world folder saved by vanilla 26.3; `None` for a superflat test world.
    /// A folder without `level.dat` becomes a new generated world.
    pub world: Option<PathBuf>,
    /// The level folder data packs are read from (also for the flat
    /// world, which is not saved).
    pub level_dir: Option<PathBuf>,
    /// Seed for a new world (random if `None`).
    pub seed: Option<i64>,
    /// How often changed chunks are written (vanilla: every 5 minutes).
    pub autosave: std::time::Duration,
    /// Read commands from standard input.
    pub console: bool,
    /// Players allowed to run commands (added to `ops.json`).
    pub ops: Vec<String>,
    /// The server's folder (`server.properties`, `ops.json`, the lists);
    /// `None` keeps the lists in memory (tests).
    pub server_dir: Option<PathBuf>,
    /// Only listed players and operators may join.
    pub white_list: bool,
    /// Game mode (0–3) and difficulty (0–3) of a new world.
    pub new_world_game_mode: i32,
    pub new_world_difficulty: i32,
    /// The level `/op` gives (`op-permission-level`).
    pub op_permission_level: i32,
}

impl Config {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            // A development server: stay local unless asked (--bind).
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 25565,
            data_dir,
            motd: "minecraft-rewrite".to_owned(),
            max_players: 20,
            view_distance: 10,
            compression_threshold: Some(256),
            online_mode: true,
            session_server: auth::MOJANG_SESSION_SERVER.to_owned(),
            world: None,
            level_dir: None,
            seed: None,
            autosave: std::time::Duration::from_secs(300),
            console: false,
            ops: Vec::new(),
            server_dir: None,
            white_list: false,
            new_world_game_mode: 0,
            new_world_difficulty: 2,
            op_permission_level: 4,
        }
    }
}

static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// When the server process started (for "Done (…s)!").
pub fn started() -> std::time::Instant {
    *STARTED.get_or_init(std::time::Instant::now)
}

/// Finds extracted data for the protocol we implement: `cache/mcdata/<version>`
/// in `start` or any parent directory.
pub fn find_data_dir(start: &Path) -> Result<PathBuf> {
    for dir in start.ancestors() {
        let cache = dir.join("cache").join("mcdata");
        let Ok(entries) = std::fs::read_dir(&cache) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path();
            let Ok(text) = std::fs::read_to_string(candidate.join("version.json")) else {
                continue;
            };
            let protocol = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["protocol_version"].as_i64());
            if protocol == Some(i64::from(mc_protocol::PROTOCOL_VERSION)) {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!(
        "no extracted data for protocol {} under cache/mcdata/ (run `cargo xtask extract-data`)",
        mc_protocol::PROTOCOL_VERSION
    )
}

#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    ctx: Arc<ServerContext>,
}

impl Server {
    pub async fn bind(config: Config) -> Result<Self> {
        let data = mc_data::McData::load(&config.data_dir)
            .with_context(|| format!("loading game data from {}", config.data_dir.display()))?;
        ensure!(
            data.version.protocol_version == mc_protocol::PROTOCOL_VERSION,
            "data in {} is for protocol {}, this server implements {}",
            config.data_dir.display(),
            data.version.protocol_version,
            mc_protocol::PROTOCOL_VERSION
        );
        let addr = SocketAddr::new(config.bind, config.port);
        let ctx = ServerContext::new(config, data)?;
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot listen on {addr}"))?;
        Ok(Self { listener, ctx })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .expect("bound listener has an address")
    }

    pub fn context(&self) -> &Arc<ServerContext> {
        &self.ctx
    }

    /// Accepts connections until `shutdown` completes.
    pub async fn run(self, shutdown: impl Future<Output = ()>) -> Result<()> {
        info!("Starting Minecraft server on {}", self.local_addr());
        if !self.ctx.config.online_mode {
            tracing::warn!("**** SERVER IS RUNNING IN OFFLINE/INSECURE MODE!");
            tracing::warn!("The server will make no attempt to authenticate usernames. Beware.");
            tracing::warn!(
                "While this makes the game possible to play without internet access, it also opens up the ability for hackers to connect with any username they choose."
            );
            tracing::warn!(
                "To change this, set \"online-mode\" to \"true\" in the server.properties file."
            );
        }
        info!(
            "Done ({:.3}s)! For help, type \"help\"",
            started().elapsed().as_secs_f64()
        );
        tokio::pin!(shutdown);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticking = {
            let requests = self
                .ctx
                .action_queue
                .lock()
                .expect("action queue lock")
                .take()
                .context("server already ran")?;
            let (ctx, stop) = (self.ctx.clone(), stop.clone());
            std::thread::Builder::new()
                .name("tick".into())
                // Functions calling functions run deep.
                .stack_size(256 << 20)
                .spawn(move || tick::run(ctx, requests, stop))?
        };
        if self.ctx.config.console {
            let ctx = self.ctx.clone();
            std::thread::Builder::new()
                .name("console".into())
                .spawn(move || console(&ctx))?;
        }
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, peer) = accepted?;
                    stream.set_nodelay(true)?;
                    let ctx = self.ctx.clone();
                    tokio::spawn(async move {
                        if let Err(err) = connection::handle(stream, peer, ctx).await {
                            debug!("{peer}: {err:#}");
                        }
                    });
                }
                () = self.ctx.shutdown.notified() => break,
                () = &mut shutdown => break,
            }
        }
        info!("Stopping server");
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let ctx = self.ctx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = ticking.join();
            save(&ctx);
        })
        .await?;
        Ok(())
    }
}

/// Writes one online player's file (world folders only).
pub(crate) fn save_player(ctx: &ServerContext, uuid: u128) {
    let Some(store) = &ctx.player_files else {
        return;
    };
    let Some(saved) = saved_of(ctx, uuid) else {
        return;
    };
    if let Err(err) = store.save(uuid, &saved, ctx.data.version.world_version) {
        tracing::warn!("saving {}: {err:#}", crate::util::uuid_string(uuid));
    }
    commands::save_advancements(ctx, uuid);
}

/// Reads commands from standard input until it closes; each runs on the
/// tick thread and its feedback is printed.
fn console(ctx: &Arc<ServerContext>) {
    use std::io::BufRead as _;
    let (reply, mut replies) = tokio::sync::mpsc::unbounded_channel();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request = tick::Request {
            dim: 0,
            actor: interact::Actor::console(),
            action: tick::Action::Command {
                line,
                source: commands::Source::Console,
            },
            reply: reply.clone(),
        };
        if ctx.actions.send(request).is_err() {
            break;
        }
        // The feedback is logged by the command itself.
        let _ = replies.blocking_recv();
    }
}

/// Writes changed chunks and the time.
fn save(ctx: &ServerContext) {
    if !ctx.world.is_saved() {
        return;
    }
    tracing::debug!("Saving players");
    let started = std::time::Instant::now();
    let time = ctx.time.lock().expect("time lock").clone();
    ctx.weather.lock().expect("weather lock").save();
    ctx.border.lock().expect("border lock").save();
    ctx.bossbars.lock().expect("bossbars lock").save();
    ctx.scoreboard.lock().expect("scoreboard lock").save();
    ctx.storage.lock().expect("storage lock").save();
    if let Err(err) = ctx.world.save_time(&time) {
        tracing::warn!("saving the time: {err:#}");
    }
    let online: Vec<u128> = ctx
        .players
        .lock()
        .expect("players lock")
        .keys()
        .copied()
        .collect();
    for uuid in online {
        save_player(ctx, uuid);
    }
    tracing::debug!("Saving worlds");
    ctx.world.set_ticks_to_save(tick::ticks_by_chunk(ctx));
    dropped::save(ctx);
    let mut saved = ctx.world.save(&ctx.data.blocks);
    for other in ctx.open_dimensions().iter().filter(|d| d.dim != ctx.dim) {
        other.border.lock().expect("border lock").save();
        other.world.set_ticks_to_save(tick::ticks_by_chunk(other));
        dropped::save(other);
        let more = other.world.save(&other.data.blocks);
        saved.chunks += more.chunks;
        saved.failed += more.failed;
    }
    if saved.chunks > 0 || saved.failed > 0 {
        tracing::debug!(
            "saved {} chunks in {} ms{}",
            saved.chunks,
            started.elapsed().as_millis(),
            if saved.failed > 0 {
                format!(", {} failed", saved.failed)
            } else {
                String::new()
            }
        );
    }
}

/// An online player's state as it would be saved.
pub(crate) fn saved_of(ctx: &ServerContext, uuid: u128) -> Option<players::Saved> {
    let player = ctx
        .players
        .lock()
        .expect("players lock")
        .get(&uuid)
        .cloned();
    let hands = ctx
        .inventories
        .lock()
        .expect("inventories lock")
        .get(&uuid)
        .cloned();
    let (Some(player), Some(hands)) = (player, hands) else {
        return None;
    };
    let pose = player.pose?;
    let vitals = ctx
        .vitals
        .lock()
        .expect("vitals lock")
        .get(&uuid)
        .cloned()
        .unwrap_or_default();
    Some(players::Saved {
        pose,
        dimension: player.dimension.clone(),
        game_mode: player.game_mode,
        flying: player.flying,
        hands,
        vitals,
        extra: players::Extra {
            recipes: commands::known_recipes(ctx, uuid),
            ..player.extra.clone()
        },
    })
}
