//! Minecraft Java Edition 26.3 compatible server.
//!
//! Current scope (ROADMAP Ф1): server list status, online- and offline-mode
//! login, configuration with the vanilla data pack as a known pack, the player
//! list, and a superflat world players can walk and chat in. One tokio task per connection;
//! the tick thread from ARCHITECTURE.md §4 arrives with world simulation.

mod auth;
mod blocks;
mod commands;
mod connection;
mod context;
mod interact;
mod level;
mod tick;
mod util;
mod world;

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use tokio::net::TcpListener;
use tracing::{debug, info};

pub use context::ServerContext;

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
    pub world: Option<PathBuf>,
    /// How often changed chunks are written (vanilla: every 5 minutes).
    pub autosave: std::time::Duration,
    /// Read commands from standard input.
    pub console: bool,
    /// Players allowed to run commands.
    pub ops: Vec<String>,
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
            autosave: std::time::Duration::from_secs(300),
            console: false,
            ops: Vec::new(),
        }
    }
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
        let ctx = Arc::new(ServerContext::new(config, data)?);
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
        info!(
            "Minecraft {} (protocol {}) server on {}, {} mode",
            self.ctx.data.version.name,
            mc_protocol::PROTOCOL_VERSION,
            self.local_addr(),
            if self.ctx.config.online_mode {
                "online"
            } else {
                "offline"
            }
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
        info!("stopping");
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
        if let Some(outcome) = replies.blocking_recv() {
            for line in outcome.messages {
                info!("{line}");
            }
        }
    }
}

/// Writes changed chunks and the time, and reports it.
fn save(ctx: &ServerContext) {
    if !ctx.world.is_saved() {
        return;
    }
    let started = std::time::Instant::now();
    let time = ctx.time.lock().expect("time lock").clone();
    if let Err(err) = ctx.world.save_time(&time) {
        tracing::warn!("saving the time: {err:#}");
    }
    let saved = ctx.world.save(&ctx.data.blocks);
    if saved.chunks > 0 || saved.failed > 0 {
        info!(
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
