//! The management server (minecraft.wiki, "Minecraft Server Management Protocol"):
//! JSON-RPC 2.0 for a dedicated server. Vanilla carries those calls on a
//! WebSocket and checks `management-server-secret` before the upgrade, in
//! `Authorization: Bearer <secret>` or `Sec-WebSocket-Protocol: minecraft-v1,<secret>`.
//! There is no TLS stack here, so the same calls are one JSON value per line
//! on TCP. The first call on a connection is [`METHOD_AUTHENTICATE`] with that
//! secret. Until it succeeds, every other call is an error and the connection
//! stays open, unauthenticated.
//!
//! TLS is not implemented and is not pretended. When `tls` is set, plaintext
//! is served only for a loopback host. Any other host is refused, so the
//! process does not open an unauthenticated public port.
//!
//! After authentication, [`METHOD_SERVER_STATUS`] and [`METHOD_PLAYERS`] are
//! still answered from [`ManagementHooks`]. Any other method is dispatched to
//! [`ManagementActions`] when [`ManagementHooks::with_actions`] installed one.
//! [`MemoryActions`] keeps those lists in memory for tests and for a server
//! that has not wired a live world into the management API.

use std::collections::BTreeMap;
use std::io::{Error, ErrorKind};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// First call on a TCP connection. Vanilla has no JSON-RPC method for this
/// (it checks the WebSocket handshake); the name is the call this transport
/// requires before [`METHOD_SERVER_STATUS`] or [`METHOD_PLAYERS`].
pub const METHOD_AUTHENTICATE: &str = "minecraft:server/authenticate";

/// Wiki method `minecraft:server/status` (Server → `/status`).
pub const METHOD_SERVER_STATUS: &str = "minecraft:server/status";

/// Wiki method `minecraft:players` (Players → `/`).
pub const METHOD_PLAYERS: &str = "minecraft:players";

/// `version.name` in a status result. This server's brand, not vanilla's.
pub const SERVER_BRAND: &str = crate::BRAND;

/// Protocol number of the 26.3 game this server speaks (`mc_protocol::PROTOCOL_VERSION`).
const GAME_PROTOCOL: i64 = 777;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const SERVER_ERROR: i64 = -32000;

/// One line, including its newline, may be this long. Longer input drops the connection.
const MAX_LINE: usize = 1024 * 1024;

/// Where to listen, and whether to. `start` does nothing when `enabled` is false.
/// An empty `secret` is refused: the listener is not opened.
#[derive(Clone)]
pub struct ManagementConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub secret: String,
    pub tls: bool,
}

impl std::fmt::Debug for ManagementConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagementConfig")
            .field("enabled", &self.enabled)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("secret", &"<redacted>")
            .field("tls", &self.tls)
            .finish()
    }
}

/// How the listener reads the game without depending on the server context.
/// The callbacks may be called from a connection task, on several connections at once.
#[derive(Clone)]
pub struct ManagementHooks {
    pub player_names: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub player_count: Arc<dyn Fn() -> usize + Send + Sync>,
    /// Handlers for methods other than status and the player list.
    /// `None` answers those calls with "Method not found".
    pub actions: Option<Arc<dyn ManagementActions>>,
    /// Notifications written to an authenticated connection.
    notes: crate::notify::Bus,
}

impl std::fmt::Debug for ManagementHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagementHooks").finish_non_exhaustive()
    }
}

impl ManagementHooks {
    pub fn new<N, C>(player_names: N, player_count: C) -> Self
    where
        N: Fn() -> Vec<String> + Send + Sync + 'static,
        C: Fn() -> usize + Send + Sync + 'static,
    {
        Self {
            player_names: Arc::new(player_names),
            player_count: Arc::new(player_count),
            actions: None,
            notes: crate::notify::Bus::new(),
        }
    }

    /// Uses `bus` for notifications instead of a private one.
    pub fn with_notes(mut self, bus: crate::notify::Bus) -> Self {
        self.notes = bus;
        self
    }

    /// Installs handlers for methods other than status and the player list.
    pub fn with_actions(mut self, actions: impl ManagementActions) -> Self {
        self.actions = Some(Arc::new(actions));
        self
    }
}

/// JSON-RPC methods other than [`METHOD_SERVER_STATUS`] and [`METHOD_PLAYERS`].
///
/// `call` returns `None` when this server does not implement `method`.
/// `Some(Ok(value))` is the JSON-RPC result value. `Some(Err((code, message)))`
/// is a JSON-RPC error.
pub trait ManagementActions: Send + Sync + 'static {
    fn call(&self, method: &str, params: Option<&Value>) -> Option<Result<Value, (i64, String)>>;
}

/// Binds the management listener and returns its address.
///
/// Returns immediately with `None` when `enabled` is false, when `secret` is
/// empty, when `tls` is requested for a host that is not loopback, or when
/// the socket cannot be bound (that failure is logged, not panicked).
/// Otherwise the accept loop runs in the background and this returns the
/// bound address — port `0` is an ephemeral port, so the returned address is
/// the one clients should use.
///
/// Must be called on a tokio runtime.
pub async fn start(config: ManagementConfig, hooks: ManagementHooks) -> Option<SocketAddr> {
    if !config.enabled {
        return None;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        tracing::warn!("management server was started outside a tokio runtime");
        return None;
    }
    if config.secret.is_empty() {
        tracing::warn!("management-server-secret is empty, management server not started");
        return None;
    }
    if config.tls && !is_loopback_host(&config.host) {
        tracing::warn!(
            "management server TLS is not implemented; not binding {}:{} because that host is not loopback and plaintext would be reachable beyond this machine",
            config.host,
            config.port
        );
        return None;
    }

    let addr = match resolve(&config.host, config.port) {
        Ok(addr) => addr,
        Err(err) => {
            tracing::warn!(
                "management server failed to resolve {}:{}: {err}",
                config.host,
                config.port
            );
            return None;
        }
    };
    if config.tls && !addr.ip().is_loopback() {
        tracing::warn!(
            "management server TLS is not implemented; not binding {addr} because that address is not loopback and plaintext would be reachable beyond this machine"
        );
        return None;
    }

    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::warn!("management server failed to bind {addr}: {err}");
            return None;
        }
    };
    let bound = match listener.local_addr() {
        Ok(bound) => bound,
        Err(err) => {
            tracing::warn!("management server has no local address: {err}");
            return None;
        }
    };
    if config.tls {
        tracing::warn!(
            "management server TLS is not implemented; serving plaintext JSON-RPC on {bound} because the host is loopback"
        );
    }

    let secret = config.secret;
    tokio::spawn(async move {
        if let Err(err) = accept_loop(listener, secret, hooks).await {
            tracing::warn!("management server on {bound} stopped: {err:#}");
        }
    });
    tracing::info!("Management server running on {bound}");
    Some(bound)
}

fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    parse_host_ip(host).is_some_and(|ip| ip.is_loopback())
}

fn parse_host_ip(host: &str) -> Option<IpAddr> {
    let host = host.trim();
    let bare = if host.starts_with('[') && host.ends_with(']') && host.len() >= 2 {
        &host[1..host.len() - 1]
    } else {
        host
    };
    bare.parse().ok()
}

fn resolve(host: &str, port: u16) -> std::io::Result<SocketAddr> {
    let host = host.trim();
    if host.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "empty host"));
    }
    if let Some(ip) = parse_host_ip(host) {
        return Ok(SocketAddr::new(ip, port));
    }
    let mut addrs = (host, port).to_socket_addrs()?;
    addrs
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no addresses"))
}

async fn accept_loop(listener: TcpListener, secret: String, hooks: ManagementHooks) -> Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::info!("Management connection from {peer}");
        let secret = secret.clone();
        let hooks = hooks.clone();
        tokio::spawn(async move {
            if let Err(err) = client(stream, peer, secret, hooks).await {
                tracing::debug!("management client {peer}: {err:#}");
            }
        });
    }
}

async fn client(
    stream: TcpStream,
    peer: SocketAddr,
    secret: String,
    hooks: ManagementHooks,
) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let mut read = tokio::io::BufReader::new(read);
    let (incoming_tx, mut incoming_rx) = tokio::sync::mpsc::channel(16);
    tokio::spawn(async move {
        loop {
            match next_line(&mut read).await {
                Ok(Some(line)) => {
                    if incoming_tx.send(Some(line)).await.is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = incoming_tx.send(None).await;
                    break;
                }
            }
        }
    });
    let mut notes = hooks.notes.subscribe();
    let mut authed = false;
    loop {
        tokio::select! {
            incoming = incoming_rx.recv() => {
                let Some(Some(line)) = incoming else {
                    break;
                };
                if line.is_empty() {
                    continue;
                }
                let was = authed;
                let reply = dispatch(&line, &mut authed, &secret, &hooks);
                if !was && authed {
                    tracing::debug!("{peer} authenticated");
                }
                let mut bytes = reply.into_bytes();
                bytes.push(b'\n');
                write.write_all(&bytes).await?;
                write.flush().await?;
            }
            note = notes.recv(), if authed => {
                if let Ok(text) = note {
                    write.write_all(text.as_bytes()).await?;
                    write.flush().await?;
                }
            }
        }
    }
    Ok(())
}

async fn next_line<R>(reader: &mut R) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
            if buf.len().saturating_add(pos) > MAX_LINE {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "management line too long",
                ));
            }
            buf.extend_from_slice(&available[..=pos]);
            let consume = pos + 1;
            reader.consume(consume);
            break;
        }
        if buf.len().saturating_add(available.len()) > MAX_LINE {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "management line too long",
            ));
        }
        let n = available.len();
        buf.extend_from_slice(available);
        reader.consume(n);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let text = String::from_utf8(buf).map_err(|err| Error::new(ErrorKind::InvalidData, err))?;
    Ok(Some(text))
}

fn dispatch(line: &str, authed: &mut bool, secret: &str, hooks: &ManagementHooks) -> String {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return error_line(&Value::Null, PARSE_ERROR, "Parse error"),
    };
    let Some(obj) = value.as_object() else {
        return error_line(&Value::Null, INVALID_REQUEST, "Invalid Request");
    };
    if let Some(id) = obj.get("id")
        && !matches!(id, Value::Null | Value::String(_) | Value::Number(_))
    {
        return error_line(&Value::Null, INVALID_REQUEST, "Invalid Request");
    }
    let id = request_id(obj);
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return error_line(&id, INVALID_REQUEST, "Invalid Request");
    }
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return error_line(&id, INVALID_REQUEST, "Invalid Request");
    };
    if method == METHOD_AUTHENTICATE {
        return authenticate(&id, obj.get("params"), authed, secret);
    }
    if !*authed {
        return error_line(&id, SERVER_ERROR, "Not authenticated");
    }
    // Status and the player list stay on the fn hooks. Their result shape does
    // not go through `ManagementActions`.
    match method {
        METHOD_SERVER_STATUS => result_line(&id, status_result(hooks)),
        METHOD_PLAYERS => result_line(&id, players_result(hooks)),
        _ => action_line(&id, method, obj.get("params"), hooks),
    }
}

fn action_line(
    id: &Value,
    method: &str,
    params: Option<&Value>,
    hooks: &ManagementHooks,
) -> String {
    if let Some(actions) = &hooks.actions
        && let Some(outcome) = actions.call(method, params)
    {
        return match outcome {
            Ok(result) => result_line(id, result),
            Err((code, message)) => error_line(id, code, &message),
        };
    }
    error_line(id, METHOD_NOT_FOUND, "Method not found")
}

fn authenticate(id: &Value, params: Option<&Value>, authed: &mut bool, secret: &str) -> String {
    let Some(given) = secret_param(params) else {
        return error_line(id, INVALID_PARAMS, "Invalid params");
    };
    if secret.is_empty() || !secrets_match(secret, &given) {
        return error_line(id, SERVER_ERROR, "Invalid secret");
    }
    *authed = true;
    result_line(id, Value::Bool(true))
}

/// Object `{"secret":"..."}`, a one-string array, or a bare string.
fn secret_param(params: Option<&Value>) -> Option<String> {
    match params? {
        Value::Object(map) => map.get("secret")?.as_str().map(str::to_owned),
        Value::Array(items) => items.first()?.as_str().map(str::to_owned),
        Value::String(text) => Some(text.clone()),
        _ => None,
    }
}

/// Compares every byte. A mismatch does not return on the first difference.
fn secrets_match(expected: &str, given: &str) -> bool {
    let expected = expected.as_bytes();
    let given = given.as_bytes();
    let mut diff = expected.len() ^ given.len();
    for i in 0..expected.len().max(given.len()) {
        let left = expected.get(i).copied().unwrap_or(0);
        let right = given.get(i).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

fn request_id(obj: &Map<String, Value>) -> Value {
    match obj.get("id") {
        Some(id @ (Value::Null | Value::String(_) | Value::Number(_))) => id.clone(),
        _ => Value::Null,
    }
}

/// Wiki "Server State": `started`, `players` (the first `player_count()` names),
/// `version`. The player count a client reads is `players.len()`, which equals
/// `ManagementHooks::player_count` when `ManagementHooks::player_names` returned
/// at least that many names. Names we do not have are not invented.
fn status_result(hooks: &ManagementHooks) -> Value {
    let count = (hooks.player_count)();
    let names = (hooks.player_names)();
    let players: Vec<Value> = names
        .into_iter()
        .take(count)
        .map(|name| json!({ "name": name }))
        .collect();
    json!({
        "started": true,
        "players": players,
        "version": {
            "name": SERVER_BRAND,
            "protocol": GAME_PROTOCOL,
        },
    })
}

/// Wiki result of `minecraft:players`: the callback's names, in order.
fn players_result(hooks: &ManagementHooks) -> Value {
    let names = (hooks.player_names)();
    Value::Array(
        names
            .into_iter()
            .map(|name| json!({ "name": name }))
            .collect(),
    )
}

fn result_line(id: &Value, result: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .unwrap_or_else(|_| error_line(&Value::Null, INTERNAL_ERROR, "Internal error"))
}

fn error_line(id: &Value, code: i64, message: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"Internal error"}}"#
            .to_owned()
    })
}

const DEFAULT_MOTD: &str = "A Minecraft Server";
const DEFAULT_BAN_SOURCE: &str = "Management server";
const DEFAULT_MAX_PLAYERS: i64 = 20;
const DEFAULT_OP_LEVEL: i64 = 4;

type RpcError = (i64, String);

fn invalid_params() -> RpcError {
    (INVALID_PARAMS, "Invalid params".to_owned())
}

/// In-memory allowlist, bans, operators, gamerules, and the few server
/// settings implemented here. It does not touch sockets or a world: kick,
/// save, and stop are recorded and answered.
#[derive(Clone)]
pub struct MemoryActions {
    inner: Arc<Mutex<MemoryState>>,
}

#[derive(Clone)]
struct MemoryState {
    allowlist: Vec<Player>,
    bans: Vec<UserBan>,
    ip_bans: Vec<IpBan>,
    operators: Vec<Operator>,
    gamerules: BTreeMap<String, Rule>,
    motd: String,
    use_allowlist: bool,
    max_players: i64,
    view_distance: i64,
    kicks: Vec<Kick>,
    saves: Vec<bool>,
    system_messages: Vec<Value>,
    stop_requested: bool,
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            allowlist: Vec::new(),
            bans: Vec::new(),
            ip_bans: Vec::new(),
            operators: Vec::new(),
            gamerules: BTreeMap::new(),
            motd: DEFAULT_MOTD.to_owned(),
            use_allowlist: false,
            max_players: DEFAULT_MAX_PLAYERS,
            view_distance: 10,
            kicks: Vec::new(),
            saves: Vec::new(),
            system_messages: Vec::new(),
            stop_requested: false,
        }
    }
}

#[derive(Clone)]
struct Player {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Clone)]
struct UserBan {
    player: Player,
    reason: Option<String>,
    source: Option<String>,
    expires: Option<String>,
}

#[derive(Clone)]
struct IpBan {
    ip: IpAddr,
    reason: Option<String>,
    source: Option<String>,
    expires: Option<String>,
}

#[derive(Clone)]
struct Operator {
    player: Player,
    permission_level: i64,
    bypasses_player_limit: bool,
}

#[derive(Clone)]
struct Kick {
    player: Player,
    message: Option<String>,
}

#[derive(Clone, Copy)]
enum RuleKind {
    Boolean,
    Integer,
}

#[derive(Clone, Copy)]
struct Rule {
    kind: RuleKind,
    boolean: bool,
    integer: i64,
}

impl std::fmt::Debug for MemoryActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("MemoryActions")
            .field("allowlist", &state.allowlist.len())
            .field("bans", &state.bans.len())
            .field("ip_bans", &state.ip_bans.len())
            .field("operators", &state.operators.len())
            .field("gamerules", &state.gamerules.len())
            .field("motd", &state.motd)
            .field("use_allowlist", &state.use_allowlist)
            .field("max_players", &state.max_players)
            .field("kicks", &state.kicks.len())
            .field("saves", &state.saves)
            .field("system_messages", &state.system_messages.len())
            .field("stop_requested", &state.stop_requested)
            .finish()
    }
}

impl Default for MemoryActions {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryActions {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryState::default())),
        }
    }

    /// Whether `minecraft:server/stop` has been called.
    pub fn stop_requested(&self) -> bool {
        self.lock().stop_requested
    }

    /// Kick requests, in order. Each value is the recorded player and optional message.
    pub fn kicks(&self) -> Vec<Value> {
        self.lock().kicks.iter().map(Kick::to_json).collect()
    }

    /// `flush` flags from `minecraft:server/save`, in call order.
    pub fn saves(&self) -> Vec<bool> {
        self.lock().saves.clone()
    }

    /// System-message objects accepted by `minecraft:server/system_message`.
    pub fn system_messages(&self) -> Vec<Value> {
        self.lock().system_messages.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn allowlist(&self) -> Result<Value, RpcError> {
        let state = self.lock();
        Ok(list_json(&state.allowlist, Player::to_json))
    }

    fn allowlist_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let players = parse_players(param_array(params, "players")?)?;
        let mut state = self.lock();
        state.allowlist = players;
        Ok(list_json(&state.allowlist, Player::to_json))
    }

    fn allowlist_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_players(param_array(params, "add")?)?;
        let mut state = self.lock();
        for player in incoming {
            if !state
                .allowlist
                .iter()
                .any(|existing| same_player(existing, &player))
            {
                state.allowlist.push(player);
            }
        }
        Ok(list_json(&state.allowlist, Player::to_json))
    }

    fn allowlist_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let gone = parse_players(param_array(params, "remove")?)?;
        let mut state = self.lock();
        state
            .allowlist
            .retain(|existing| !gone.iter().any(|player| same_player(existing, player)));
        Ok(list_json(&state.allowlist, Player::to_json))
    }

    fn allowlist_clear(&self) -> Result<Value, RpcError> {
        self.lock().allowlist.clear();
        Ok(Value::Array(Vec::new()))
    }

    fn bans(&self) -> Result<Value, RpcError> {
        let state = self.lock();
        Ok(list_json(&state.bans, UserBan::to_json))
    }

    fn bans_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let bans = parse_all(param_array(params, "bans")?, |value| {
            parse_user_ban(value, false)
        })?;
        let mut state = self.lock();
        state.bans = bans;
        Ok(list_json(&state.bans, UserBan::to_json))
    }

    fn bans_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_all(param_array(params, "add")?, |value| {
            parse_user_ban(value, true)
        })?;
        let mut state = self.lock();
        for ban in incoming {
            upsert_user(&mut state.bans, ban);
        }
        Ok(list_json(&state.bans, UserBan::to_json))
    }

    fn bans_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let gone = parse_players(param_array(params, "remove")?)?;
        let mut state = self.lock();
        state
            .bans
            .retain(|ban| !gone.iter().any(|player| same_player(&ban.player, player)));
        Ok(list_json(&state.bans, UserBan::to_json))
    }

    fn bans_clear(&self) -> Result<Value, RpcError> {
        self.lock().bans.clear();
        Ok(Value::Array(Vec::new()))
    }

    fn ip_bans(&self) -> Result<Value, RpcError> {
        let state = self.lock();
        Ok(list_json(&state.ip_bans, IpBan::to_json))
    }

    fn ip_bans_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let bans = parse_all(param_array(params, "banlist")?, |value| {
            parse_ip_ban(value, false)
        })?;
        let mut state = self.lock();
        state.ip_bans = bans;
        Ok(list_json(&state.ip_bans, IpBan::to_json))
    }

    fn ip_bans_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_all(param_array(params, "add")?, |value| {
            parse_ip_ban(value, true)
        })?;
        let mut state = self.lock();
        for ban in incoming {
            upsert_ip(&mut state.ip_bans, ban);
        }
        Ok(list_json(&state.ip_bans, IpBan::to_json))
    }

    fn ip_bans_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let ips = parse_ip_list(param_array(params, "ip")?)?;
        let mut state = self.lock();
        state.ip_bans.retain(|ban| !ips.contains(&ban.ip));
        Ok(list_json(&state.ip_bans, IpBan::to_json))
    }

    fn ip_bans_clear(&self) -> Result<Value, RpcError> {
        self.lock().ip_bans.clear();
        Ok(Value::Array(Vec::new()))
    }

    fn operators(&self) -> Result<Value, RpcError> {
        let state = self.lock();
        Ok(list_json(&state.operators, Operator::to_json))
    }

    fn operators_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let operators = parse_all(param_array(params, "operators")?, parse_operator)?;
        let mut state = self.lock();
        state.operators = operators;
        Ok(list_json(&state.operators, Operator::to_json))
    }

    fn operators_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_all(param_array(params, "add")?, parse_operator)?;
        let mut state = self.lock();
        for operator in incoming {
            upsert_operator(&mut state.operators, operator);
        }
        Ok(list_json(&state.operators, Operator::to_json))
    }

    fn operators_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let gone = parse_players(param_array(params, "remove")?)?;
        let mut state = self.lock();
        state.operators.retain(|operator| {
            !gone
                .iter()
                .any(|player| same_player(&operator.player, player))
        });
        Ok(list_json(&state.operators, Operator::to_json))
    }

    fn operators_clear(&self) -> Result<Value, RpcError> {
        self.lock().operators.clear();
        Ok(Value::Array(Vec::new()))
    }

    fn gamerules(&self) -> Result<Value, RpcError> {
        let state = self.lock();
        Ok(Value::Array(
            state
                .gamerules
                .iter()
                .map(|(short, rule)| rule.to_json(short))
                .collect(),
        ))
    }

    fn gamerules_update(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let spec = lookup(params, "gamerule", true).ok_or_else(invalid_params)?;
        let obj = spec.as_object().ok_or_else(invalid_params)?;
        let raw = obj
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(invalid_params)?;
        let short = short_key(raw)?;
        let value = obj.get("value").ok_or_else(invalid_params)?;
        let mut state = self.lock();
        let kind = if let Some(kind) = known_kind(&short) {
            kind
        } else if let Some(existing) = state.gamerules.get(&short) {
            existing.kind
        } else {
            infer_kind(value)?
        };
        let rule = coerce(kind, value)?;
        let json = rule.to_json(&short);
        state.gamerules.insert(short, rule);
        Ok(json)
    }

    fn motd(&self) -> Result<Value, RpcError> {
        Ok(json!({ "message": self.lock().motd }))
    }

    fn motd_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let message = param_string(params, "message")?;
        self.lock().motd = message.clone();
        Ok(json!({ "message": message }))
    }

    fn use_allowlist(&self) -> Result<Value, RpcError> {
        Ok(json!({ "used": self.lock().use_allowlist }))
    }

    fn use_allowlist_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let used = param_bool(params, "use", None)?;
        self.lock().use_allowlist = used;
        Ok(json!({ "used": used }))
    }

    fn max_players(&self) -> Result<Value, RpcError> {
        Ok(json!({ "max": self.lock().max_players }))
    }

    fn max_players_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let max = param_i64(params, "max")?;
        if max < 1 {
            return Err(invalid_params());
        }
        self.lock().max_players = max;
        Ok(json!({ "max": max }))
    }

    fn view_distance(&self) -> Result<Value, RpcError> {
        Ok(json!({ "view_distance": self.lock().view_distance }))
    }

    fn view_distance_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let distance = param_i64(params, "view_distance")?;
        if !(2..=32).contains(&distance) {
            return Err(invalid_params());
        }
        self.lock().view_distance = distance;
        Ok(json!({ "view_distance": distance }))
    }

    fn kick(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let kicks = parse_all(param_array(params, "kick")?, parse_kick)?;
        let result = list_json(&kicks, |kick| kick.player.to_json());
        self.lock().kicks.extend(kicks);
        Ok(result)
    }

    fn save(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let flush = param_bool(params, "flush", Some(false))?;
        self.lock().saves.push(flush);
        Ok(Value::Bool(true))
    }

    fn stop(&self) -> Result<Value, RpcError> {
        self.lock().stop_requested = true;
        Ok(Value::Bool(true))
    }

    fn system_message(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let message = notification(params)?.clone();
        self.lock().system_messages.push(message);
        Ok(Value::Bool(true))
    }
}

impl ManagementActions for MemoryActions {
    fn call(&self, method: &str, params: Option<&Value>) -> Option<Result<Value, (i64, String)>> {
        Some(match method {
            "minecraft:allowlist" => self.allowlist(),
            "minecraft:allowlist/set" => self.allowlist_set(params),
            "minecraft:allowlist/add" => self.allowlist_add(params),
            "minecraft:allowlist/remove" => self.allowlist_remove(params),
            "minecraft:allowlist/clear" => self.allowlist_clear(),
            "minecraft:bans" => self.bans(),
            "minecraft:bans/set" => self.bans_set(params),
            "minecraft:bans/add" => self.bans_add(params),
            "minecraft:bans/remove" => self.bans_remove(params),
            "minecraft:bans/clear" => self.bans_clear(),
            "minecraft:ip_bans" => self.ip_bans(),
            "minecraft:ip_bans/set" => self.ip_bans_set(params),
            "minecraft:ip_bans/add" => self.ip_bans_add(params),
            "minecraft:ip_bans/remove" => self.ip_bans_remove(params),
            "minecraft:ip_bans/clear" => self.ip_bans_clear(),
            "minecraft:operators" => self.operators(),
            "minecraft:operators/set" => self.operators_set(params),
            "minecraft:operators/add" => self.operators_add(params),
            "minecraft:operators/remove" => self.operators_remove(params),
            "minecraft:operators/clear" => self.operators_clear(),
            "minecraft:gamerules" => self.gamerules(),
            "minecraft:gamerules/update" => self.gamerules_update(params),
            "minecraft:serversettings/motd" => self.motd(),
            "minecraft:serversettings/motd/set" => self.motd_set(params),
            "minecraft:serversettings/use_allowlist" => self.use_allowlist(),
            "minecraft:serversettings/use_allowlist/set" => self.use_allowlist_set(params),
            "minecraft:serversettings/max_players" => self.max_players(),
            "minecraft:serversettings/max_players/set" => self.max_players_set(params),
            "minecraft:serversettings/view_distance" => self.view_distance(),
            "minecraft:serversettings/view_distance/set" => self.view_distance_set(params),
            "minecraft:players/kick" => self.kick(params),
            "minecraft:server/save" => self.save(params),
            "minecraft:server/stop" => self.stop(),
            "minecraft:server/system_message" => self.system_message(params),
            _ => return None,
        })
    }
}

impl Player {
    fn parse(value: &Value) -> Result<Self, RpcError> {
        let obj = value.as_object().ok_or_else(invalid_params)?;
        let id = match obj.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(normalize_uuid(text).ok_or_else(invalid_params)?),
            Some(_) => return Err(invalid_params()),
        };
        let name = match obj.get("name") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => return Err(invalid_params()),
        };
        if id.is_none() && name.is_none() {
            return Err(invalid_params());
        }
        Ok(Self { id, name })
    }

    fn to_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(id) = &self.id {
            map.insert("id".to_owned(), Value::String(id.clone()));
        }
        if let Some(name) = &self.name {
            map.insert("name".to_owned(), Value::String(name.clone()));
        }
        Value::Object(map)
    }
}

impl UserBan {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("player".to_owned(), self.player.to_json());
        push_string(&mut map, "reason", &self.reason);
        push_string(&mut map, "source", &self.source);
        push_string(&mut map, "expires", &self.expires);
        Value::Object(map)
    }
}

impl IpBan {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("ip".to_owned(), Value::String(self.ip.to_string()));
        push_string(&mut map, "reason", &self.reason);
        push_string(&mut map, "source", &self.source);
        push_string(&mut map, "expires", &self.expires);
        Value::Object(map)
    }
}

impl Operator {
    fn to_json(&self) -> Value {
        json!({
            "player": self.player.to_json(),
            "permissionLevel": self.permission_level,
            "bypassesPlayerLimit": self.bypasses_player_limit,
        })
    }
}

impl Kick {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("player".to_owned(), self.player.to_json());
        push_string(&mut map, "message", &self.message);
        Value::Object(map)
    }
}

impl Rule {
    fn to_json(self, short: &str) -> Value {
        let key = format!("minecraft:{short}");
        match self.kind {
            RuleKind::Boolean => json!({
                "key": key,
                "type": "boolean",
                "value": self.boolean,
            }),
            RuleKind::Integer => json!({
                "key": key,
                "type": "integer",
                "value": self.integer,
            }),
        }
    }
}

fn push_string(map: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), Value::String(value.clone()));
    }
}

fn list_json<T>(items: &[T], to_json: impl Fn(&T) -> Value) -> Value {
    Value::Array(items.iter().map(to_json).collect())
}

fn parse_all<T>(
    items: &[Value],
    parse: impl Fn(&Value) -> Result<T, RpcError>,
) -> Result<Vec<T>, RpcError> {
    items.iter().map(parse).collect()
}

fn parse_players(items: &[Value]) -> Result<Vec<Player>, RpcError> {
    parse_all(items, Player::parse)
}

/// Both have an id: the ids must match. Otherwise the names must match.
fn same_player(left: &Player, right: &Player) -> bool {
    match (&left.id, &right.id) {
        (Some(left_id), Some(right_id)) => left_id == right_id,
        _ => match (&left.name, &right.name) {
            (Some(left_name), Some(right_name)) => left_name == right_name,
            _ => false,
        },
    }
}

fn normalize_uuid(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 {
        return None;
    }
    let mut out = String::with_capacity(36);
    for (index, byte) in bytes.iter().copied().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return None;
            }
            out.push('-');
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return None;
        }
        out.push(char::from(byte.to_ascii_lowercase()));
    }
    Some(out)
}

fn optional_string(obj: &Map<String, Value>, name: &str) -> Result<Option<String>, RpcError> {
    match obj.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(invalid_params()),
    }
}

fn source_field(
    obj: &Map<String, Value>,
    default_source: bool,
) -> Result<Option<String>, RpcError> {
    match optional_string(obj, "source")? {
        Some(source) => Ok(Some(source)),
        None if default_source => Ok(Some(DEFAULT_BAN_SOURCE.to_owned())),
        None => Ok(None),
    }
}

fn parse_user_ban(value: &Value, default_source: bool) -> Result<UserBan, RpcError> {
    let obj = value.as_object().ok_or_else(invalid_params)?;
    Ok(UserBan {
        player: Player::parse(obj.get("player").ok_or_else(invalid_params)?)?,
        reason: optional_string(obj, "reason")?,
        source: source_field(obj, default_source)?,
        expires: optional_string(obj, "expires")?,
    })
}

fn parse_ip_ban(value: &Value, default_source: bool) -> Result<IpBan, RpcError> {
    let obj = value.as_object().ok_or_else(invalid_params)?;
    Ok(IpBan {
        ip: ip_of(obj)?,
        reason: optional_string(obj, "reason")?,
        source: source_field(obj, default_source)?,
        expires: optional_string(obj, "expires")?,
    })
}

fn ip_of(obj: &Map<String, Value>) -> Result<IpAddr, RpcError> {
    if let Some(ip) = obj.get("ip").and_then(json_ip) {
        return Ok(ip);
    }
    let name = obj
        .get("player")
        .and_then(Value::as_object)
        .and_then(|player| player.get("name"))
        .and_then(Value::as_str);
    name.and_then(|name| name.parse().ok())
        .ok_or_else(invalid_params)
}

fn json_ip(value: &Value) -> Option<IpAddr> {
    value.as_str()?.parse().ok()
}

fn parse_ip_list(items: &[Value]) -> Result<Vec<IpAddr>, RpcError> {
    items
        .iter()
        .map(|item| {
            item.as_str()
                .and_then(|text| text.parse().ok())
                .ok_or_else(invalid_params)
        })
        .collect()
}

fn parse_operator(value: &Value) -> Result<Operator, RpcError> {
    let obj = value.as_object().ok_or_else(invalid_params)?;
    let permission_level = match obj.get("permissionLevel") {
        None | Some(Value::Null) => DEFAULT_OP_LEVEL,
        Some(Value::Number(number)) => number.as_i64().ok_or_else(invalid_params)?,
        Some(_) => return Err(invalid_params()),
    };
    let bypasses_player_limit = match obj.get("bypassesPlayerLimit") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err(invalid_params()),
    };
    Ok(Operator {
        player: Player::parse(obj.get("player").ok_or_else(invalid_params)?)?,
        permission_level,
        bypasses_player_limit,
    })
}

fn parse_kick(value: &Value) -> Result<Kick, RpcError> {
    let obj = value.as_object().ok_or_else(invalid_params)?;
    Ok(Kick {
        player: Player::parse(obj.get("player").ok_or_else(invalid_params)?)?,
        message: optional_string(obj, "message")?,
    })
}

fn upsert_user(list: &mut Vec<UserBan>, ban: UserBan) {
    if let Some(existing) = list
        .iter_mut()
        .find(|existing| same_player(&existing.player, &ban.player))
    {
        *existing = ban;
    } else {
        list.push(ban);
    }
}

fn upsert_ip(list: &mut Vec<IpBan>, ban: IpBan) {
    if let Some(existing) = list.iter_mut().find(|existing| existing.ip == ban.ip) {
        *existing = ban;
    } else {
        list.push(ban);
    }
}

fn upsert_operator(list: &mut Vec<Operator>, operator: Operator) {
    if let Some(existing) = list
        .iter_mut()
        .find(|existing| same_player(&existing.player, &operator.player))
    {
        *existing = operator;
    } else {
        list.push(operator);
    }
}

fn known_kind(short: &str) -> Option<RuleKind> {
    match short {
        "locator_bar" => Some(RuleKind::Boolean),
        "random_tick_speed" => Some(RuleKind::Integer),
        _ => None,
    }
}

fn short_key(raw: &str) -> Result<String, RpcError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(invalid_params());
    }
    let short = raw.strip_prefix("minecraft:").unwrap_or(raw);
    if short.is_empty() {
        return Err(invalid_params());
    }
    Ok(short.to_owned())
}

fn parse_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn infer_kind(value: &Value) -> Result<RuleKind, RpcError> {
    if matches!(value, Value::Bool(_)) {
        return Ok(RuleKind::Boolean);
    }
    if parse_integer(value).is_some() {
        return Ok(RuleKind::Integer);
    }
    Err(invalid_params())
}

fn coerce(kind: RuleKind, value: &Value) -> Result<Rule, RpcError> {
    match kind {
        RuleKind::Boolean => match value {
            Value::Bool(boolean) => Ok(Rule {
                kind,
                boolean: *boolean,
                integer: 0,
            }),
            _ => Err(invalid_params()),
        },
        RuleKind::Integer => Ok(Rule {
            kind,
            boolean: false,
            integer: parse_integer(value).ok_or_else(invalid_params)?,
        }),
    }
}

/// Wiki params are a named object. A one-element array carries that object,
/// or the list / payload itself as the first element.
fn lookup<'a>(params: Option<&'a Value>, name: &str, unwrapped_object: bool) -> Option<&'a Value> {
    match params? {
        Value::Object(map) => map.get(name),
        Value::Array(items) => {
            let first = items.first()?;
            match first {
                Value::Object(map) => {
                    if let Some(value) = map.get(name) {
                        Some(value)
                    } else if unwrapped_object {
                        Some(first)
                    } else {
                        None
                    }
                }
                other => Some(other),
            }
        }
        _ => None,
    }
}

fn param_array<'a>(params: Option<&'a Value>, name: &str) -> Result<&'a [Value], RpcError> {
    match lookup(params, name, false) {
        Some(Value::Array(items)) => Ok(items),
        _ => Err(invalid_params()),
    }
}

fn param_string(params: Option<&Value>, name: &str) -> Result<String, RpcError> {
    match lookup(params, name, false) {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(invalid_params()),
    }
}

fn param_bool(params: Option<&Value>, name: &str, absent: Option<bool>) -> Result<bool, RpcError> {
    match lookup(params, name, false) {
        None => absent.ok_or_else(invalid_params),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid_params()),
    }
}

fn param_i64(params: Option<&Value>, name: &str) -> Result<i64, RpcError> {
    match lookup(params, name, false) {
        Some(Value::Number(number)) => number.as_i64().ok_or_else(invalid_params),
        _ => Err(invalid_params()),
    }
}

fn notification(params: Option<&Value>) -> Result<&Value, RpcError> {
    let (map, whole) = match params {
        Some(value @ Value::Object(map)) => (map, value),
        Some(Value::Array(items)) => match items.first() {
            Some(value @ Value::Object(map)) => (map, value),
            _ => return Err(invalid_params()),
        },
        _ => return Err(invalid_params()),
    };
    if map.contains_key("overlay") || map.contains_key("receivingPlayers") {
        return Ok(whole);
    }
    match map.get("message") {
        Some(message) if message.is_object() => Ok(message),
        _ => Err(invalid_params()),
    }
}

/// Management calls against the running server's lists, rules and settings.
pub struct LiveActions {
    ctx: Arc<crate::context::ServerContext>,
}

impl std::fmt::Debug for LiveActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveActions").finish_non_exhaustive()
    }
}

impl LiveActions {
    pub fn new(ctx: Arc<crate::context::ServerContext>) -> Self {
        Self { ctx }
    }

    fn allowlist(&self) -> Result<Value, RpcError> {
        let lists = self.ctx.lists.lock().expect("lists lock");
        Ok(list_json(
            &lists
                .whitelist
                .iter()
                .map(player_of_named)
                .collect::<Vec<_>>(),
            Player::to_json,
        ))
    }

    fn allowlist_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let players = parse_players(param_array(params, "players")?)?;
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        let before: Vec<Player> = lists.whitelist.iter().map(player_of_named).collect();
        lists.whitelist = players.iter().map(named_of).collect();
        lists.save_whitelist();
        drop(lists);
        publish_allowlist_diff(&self.ctx, &before, &players);
        Ok(list_json(&players, Player::to_json))
    }

    fn allowlist_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_players(param_array(params, "add")?)?;
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        let before: Vec<Player> = lists.whitelist.iter().map(player_of_named).collect();
        let mut players = before.clone();
        for player in incoming {
            if !players
                .iter()
                .any(|existing| same_player(existing, &player))
            {
                players.push(player);
            }
        }
        lists.whitelist = players.iter().map(named_of).collect();
        lists.save_whitelist();
        drop(lists);
        publish_allowlist_diff(&self.ctx, &before, &players);
        Ok(list_json(&players, Player::to_json))
    }

    fn allowlist_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let gone = parse_players(param_array(params, "remove")?)?;
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        let before: Vec<Player> = lists.whitelist.iter().map(player_of_named).collect();
        let mut players = before.clone();
        players.retain(|existing| !gone.iter().any(|player| same_player(existing, player)));
        lists.whitelist = players.iter().map(named_of).collect();
        lists.save_whitelist();
        drop(lists);
        publish_allowlist_diff(&self.ctx, &before, &players);
        Ok(list_json(&players, Player::to_json))
    }

    fn allowlist_clear(&self) -> Result<Value, RpcError> {
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        let before: Vec<Player> = lists.whitelist.iter().map(player_of_named).collect();
        lists.whitelist.clear();
        lists.save_whitelist();
        drop(lists);
        publish_allowlist_diff(&self.ctx, &before, &[]);
        Ok(Value::Array(Vec::new()))
    }

    fn bans(&self) -> Result<Value, RpcError> {
        let lists = self.ctx.lists.lock().expect("lists lock");
        Ok(list_json(
            &lists
                .banned_players
                .iter()
                .map(user_of_ban)
                .collect::<Vec<_>>(),
            UserBan::to_json,
        ))
    }

    fn bans_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let bans = parse_all(param_array(params, "bans")?, |value| {
            parse_user_ban(value, false)
        })?;
        let stored = self.store_user_bans(bans, true)?;
        Ok(list_json(&stored, UserBan::to_json))
    }

    fn bans_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_all(param_array(params, "add")?, |value| {
            parse_user_ban(value, true)
        })?;
        let mut current: Vec<UserBan> = self
            .ctx
            .lists
            .lock()
            .expect("lists lock")
            .banned_players
            .iter()
            .map(user_of_ban)
            .collect();
        for ban in &incoming {
            upsert_user(&mut current, ban.clone());
        }
        let stored = self.store_user_bans(current, false)?;
        let kick: Vec<Player> = incoming.into_iter().map(|ban| ban.player).collect();
        drop_then_kick(&self.ctx, &kick, "multiplayer.disconnect.banned");
        Ok(list_json(&stored, UserBan::to_json))
    }

    fn bans_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let gone = parse_players(param_array(params, "remove")?)?;
        let mut current: Vec<UserBan> = self
            .ctx
            .lists
            .lock()
            .expect("lists lock")
            .banned_players
            .iter()
            .map(user_of_ban)
            .collect();
        current.retain(|ban| !gone.iter().any(|player| same_player(&ban.player, player)));
        let stored = self.store_user_bans(current, false)?;
        Ok(list_json(&stored, UserBan::to_json))
    }

    fn bans_clear(&self) -> Result<Value, RpcError> {
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        lists.banned_players.clear();
        lists.save_bans();
        Ok(Value::Array(Vec::new()))
    }

    fn store_user_bans(&self, bans: Vec<UserBan>, fresh: bool) -> Result<Vec<UserBan>, RpcError> {
        let created = crate::util::now_text();
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        let previous = lists.banned_players.clone();
        lists.banned_players = bans
            .iter()
            .map(|ban| {
                let kept = previous
                    .iter()
                    .find(|old| same_player(&user_of_ban(old).player, &ban.player));
                ban_record(
                    &ban.player,
                    &ban.reason,
                    &ban.source,
                    &ban.expires,
                    if fresh {
                        created.clone()
                    } else {
                        kept.map(|old| old.created.clone())
                            .unwrap_or_else(|| created.clone())
                    },
                )
            })
            .collect();
        lists.save_bans();
        Ok(lists.banned_players.iter().map(user_of_ban).collect())
    }

    fn ip_bans(&self) -> Result<Value, RpcError> {
        let lists = self.ctx.lists.lock().expect("lists lock");
        Ok(list_json(
            &lists
                .banned_ips
                .iter()
                .filter_map(ip_of_ban)
                .collect::<Vec<_>>(),
            IpBan::to_json,
        ))
    }

    fn ip_bans_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let bans = parse_all(param_array(params, "banlist")?, |value| {
            parse_ip_ban(value, false)
        })?;
        Ok(list_json(&self.store_ip_bans(bans)?, IpBan::to_json))
    }

    fn ip_bans_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_all(param_array(params, "add")?, |value| {
            parse_ip_ban(value, true)
        })?;
        let mut current: Vec<IpBan> = self
            .ctx
            .lists
            .lock()
            .expect("lists lock")
            .banned_ips
            .iter()
            .filter_map(ip_of_ban)
            .collect();
        for ban in &incoming {
            upsert_ip(&mut current, ban.clone());
        }
        let stored = self.store_ip_bans(current)?;
        let ips: Vec<String> = incoming.iter().map(|ban| ban.ip.to_string()).collect();
        let kicked = online_with_ip(&self.ctx, &ips);
        drop_kick_uuids(&self.ctx, &kicked, "multiplayer.disconnect.banned");
        Ok(list_json(&stored, IpBan::to_json))
    }

    fn ip_bans_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let ips = parse_ip_list(param_array(params, "ip")?)?;
        let mut current: Vec<IpBan> = self
            .ctx
            .lists
            .lock()
            .expect("lists lock")
            .banned_ips
            .iter()
            .filter_map(ip_of_ban)
            .collect();
        current.retain(|ban| !ips.contains(&ban.ip));
        Ok(list_json(&self.store_ip_bans(current)?, IpBan::to_json))
    }

    fn ip_bans_clear(&self) -> Result<Value, RpcError> {
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        lists.banned_ips.clear();
        lists.save_bans();
        Ok(Value::Array(Vec::new()))
    }

    fn store_ip_bans(&self, bans: Vec<IpBan>) -> Result<Vec<IpBan>, RpcError> {
        let created = crate::util::now_text();
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        lists.banned_ips = bans
            .iter()
            .map(|ban| crate::lists::Ban {
                uuid: None,
                name: ban.ip.to_string(),
                created: created.clone(),
                source: ban.source.clone().unwrap_or_default(),
                expires: ban.expires.clone().unwrap_or_else(|| "forever".to_owned()),
                reason: ban
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Banned by an operator.".to_owned()),
            })
            .collect();
        lists.save_bans();
        Ok(lists.banned_ips.iter().filter_map(ip_of_ban).collect())
    }

    fn operators(&self) -> Result<Value, RpcError> {
        let lists = self.ctx.lists.lock().expect("lists lock");
        Ok(list_json(
            &lists.ops.iter().map(operator_of).collect::<Vec<_>>(),
            Operator::to_json,
        ))
    }

    fn operators_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let operators = parse_all(param_array(params, "operators")?, parse_operator)?;
        self.write_ops(&operators)
    }

    fn operators_add(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let incoming = parse_all(param_array(params, "add")?, parse_operator)?;
        let mut current: Vec<Operator> = self
            .ctx
            .lists
            .lock()
            .expect("lists lock")
            .ops
            .iter()
            .map(operator_of)
            .collect();
        for operator in incoming {
            upsert_operator(&mut current, operator);
        }
        self.write_ops(&current)
    }

    fn operators_remove(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let gone = parse_players(param_array(params, "remove")?)?;
        let mut current: Vec<Operator> = self
            .ctx
            .lists
            .lock()
            .expect("lists lock")
            .ops
            .iter()
            .map(operator_of)
            .collect();
        current.retain(|operator| {
            !gone
                .iter()
                .any(|player| same_player(&operator.player, player))
        });
        self.write_ops(&current)
    }

    fn operators_clear(&self) -> Result<Value, RpcError> {
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        lists.ops.clear();
        lists.save_ops();
        Ok(Value::Array(Vec::new()))
    }

    fn write_ops(&self, operators: &[Operator]) -> Result<Value, RpcError> {
        let mut stored = Vec::new();
        for operator in operators {
            let level = i32::try_from(operator.permission_level).map_err(|_| invalid_params())?;
            stored.push(crate::lists::Op {
                uuid: operator
                    .player
                    .id
                    .as_deref()
                    .and_then(crate::util::parse_uuid),
                name: operator.player.name.clone().unwrap_or_default(),
                level,
                bypasses_player_limit: operator.bypasses_player_limit,
            });
        }
        let mut lists = self.ctx.lists.lock().expect("lists lock");
        lists.ops = stored;
        lists.save_ops();
        Ok(list_json(
            &lists.ops.iter().map(operator_of).collect::<Vec<_>>(),
            Operator::to_json,
        ))
    }

    fn gamerules(&self) -> Result<Value, RpcError> {
        let rules = self.ctx.game_rules.lock().expect("game rules lock");
        Ok(Value::Array(
            rules
                .entries()
                .into_iter()
                .map(|(key, value)| rule_json(&key, value))
                .collect(),
        ))
    }

    fn gamerules_update(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let spec = lookup(params, "gamerule", true).ok_or_else(invalid_params)?;
        let obj = spec.as_object().ok_or_else(invalid_params)?;
        let raw = obj
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(invalid_params)?;
        let short = short_key(raw)?;
        let value = obj.get("value").ok_or_else(invalid_params)?;
        let mut rules = self.ctx.game_rules.lock().expect("game rules lock");
        let kind = match rules.get(&short) {
            Some(crate::rules::Value::Bool(_)) => RuleKind::Boolean,
            Some(crate::rules::Value::Int(_)) => RuleKind::Integer,
            None => infer_kind(value)?,
        };
        let rule = coerce(kind, value)?;
        let stored = match rule.kind {
            RuleKind::Boolean => crate::rules::Value::Bool(rule.boolean),
            RuleKind::Integer => {
                crate::rules::Value::Int(i32::try_from(rule.integer).map_err(|_| invalid_params())?)
            }
        };
        let json = rule.to_json(&short);
        rules.set(&short, stored);
        drop(rules);
        self.ctx.management_bus.publish(&match stored {
            crate::rules::Value::Bool(boolean) => crate::notify::Note::GameruleUpdated {
                key: short,
                boolean: Some(boolean),
                integer: None,
            },
            crate::rules::Value::Int(integer) => crate::notify::Note::GameruleUpdated {
                key: short,
                boolean: None,
                integer: Some(integer),
            },
        });
        Ok(json)
    }

    fn motd(&self) -> Result<Value, RpcError> {
        Ok(json!({ "message": self.ctx.shown_motd.lock().expect("motd lock").clone() }))
    }

    fn motd_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let message = param_string(params, "message")?;
        *self.ctx.shown_motd.lock().expect("motd lock") = message.clone();
        write_property(&self.ctx, "motd", &message);
        Ok(json!({ "message": message }))
    }

    fn use_allowlist(&self) -> Result<Value, RpcError> {
        Ok(json!({ "used": self.ctx.white_list() }))
    }

    fn use_allowlist_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let used = param_bool(params, "use", None)?;
        self.ctx
            .white_list
            .store(used, std::sync::atomic::Ordering::Relaxed);
        write_property(&self.ctx, "white-list", if used { "true" } else { "false" });
        Ok(json!({ "used": used }))
    }

    fn max_players(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "max": self.ctx.player_limit.load(std::sync::atomic::Ordering::Relaxed)
        }))
    }

    fn max_players_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let max = param_i64(params, "max")?;
        if max < 1 {
            return Err(invalid_params());
        }
        let max = usize::try_from(max).map_err(|_| invalid_params())?;
        self.ctx
            .player_limit
            .store(max, std::sync::atomic::Ordering::Relaxed);
        write_property(&self.ctx, "max-players", &max.to_string());
        Ok(json!({ "max": max }))
    }

    fn view_distance(&self) -> Result<Value, RpcError> {
        Ok(json!({
            "view_distance": self.ctx.view_distance.load(std::sync::atomic::Ordering::Relaxed)
        }))
    }

    fn view_distance_set(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let distance = param_i64(params, "view_distance")?;
        if !(2..=32).contains(&distance) {
            return Err(invalid_params());
        }
        let distance = u8::try_from(distance).map_err(|_| invalid_params())?;
        self.ctx
            .view_distance
            .store(distance, std::sync::atomic::Ordering::Relaxed);
        write_property(&self.ctx, "view-distance", &distance.to_string());
        Ok(json!({ "view_distance": distance }))
    }

    fn kick(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let kicks = parse_all(param_array(params, "kick")?, parse_kick)?;
        let result = list_json(&kicks, |kick| kick.player.to_json());
        for kick in &kicks {
            if let Some(uuid) = find_online(&self.ctx, &kick.player) {
                let reason = match &kick.message {
                    Some(text) => mc_nbt::Tag::String(text.clone()),
                    None => {
                        mc_protocol::text::translate("multiplayer.disconnect.kicked", Vec::new())
                    }
                };
                let _ = self
                    .ctx
                    .events
                    .send(crate::context::Event::Kick { uuid, reason });
            }
        }
        Ok(result)
    }

    fn save(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let _flush = param_bool(params, "flush", Some(false))?;
        crate::save(&self.ctx);
        Ok(Value::Bool(true))
    }

    fn stop(&self) -> Result<Value, RpcError> {
        self.ctx
            .management_bus
            .publish(&crate::notify::Note::ServerStopping);
        self.ctx.shutdown.notify_one();
        Ok(Value::Bool(true))
    }

    fn system_message(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let body = notification(params)?;
        let obj = body.as_object().ok_or_else(invalid_params)?;
        let (message, overlay, receivers) =
            if obj.contains_key("overlay") || obj.contains_key("receivingPlayers") {
                (
                    obj.get("message").ok_or_else(invalid_params)?,
                    match obj.get("overlay") {
                        Some(Value::Bool(value)) => *value,
                        _ => return Err(invalid_params()),
                    },
                    obj.get("receivingPlayers"),
                )
            } else {
                (body, false, None)
            };
        let line = text_of(message)?;
        for uuid in recipients(&self.ctx, receivers)? {
            let _ = self.ctx.events.send(crate::context::Event::Message {
                uuid,
                line: line.clone(),
                overlay,
            });
        }
        Ok(Value::Bool(true))
    }
}

impl ManagementActions for LiveActions {
    fn call(&self, method: &str, params: Option<&Value>) -> Option<Result<Value, (i64, String)>> {
        Some(match method {
            "minecraft:allowlist" => self.allowlist(),
            "minecraft:allowlist/set" => self.allowlist_set(params),
            "minecraft:allowlist/add" => self.allowlist_add(params),
            "minecraft:allowlist/remove" => self.allowlist_remove(params),
            "minecraft:allowlist/clear" => self.allowlist_clear(),
            "minecraft:bans" => self.bans(),
            "minecraft:bans/set" => self.bans_set(params),
            "minecraft:bans/add" => self.bans_add(params),
            "minecraft:bans/remove" => self.bans_remove(params),
            "minecraft:bans/clear" => self.bans_clear(),
            "minecraft:ip_bans" => self.ip_bans(),
            "minecraft:ip_bans/set" => self.ip_bans_set(params),
            "minecraft:ip_bans/add" => self.ip_bans_add(params),
            "minecraft:ip_bans/remove" => self.ip_bans_remove(params),
            "minecraft:ip_bans/clear" => self.ip_bans_clear(),
            "minecraft:operators" => self.operators(),
            "minecraft:operators/set" => self.operators_set(params),
            "minecraft:operators/add" => self.operators_add(params),
            "minecraft:operators/remove" => self.operators_remove(params),
            "minecraft:operators/clear" => self.operators_clear(),
            "minecraft:gamerules" => self.gamerules(),
            "minecraft:gamerules/update" => self.gamerules_update(params),
            "minecraft:serversettings/motd" => self.motd(),
            "minecraft:serversettings/motd/set" => self.motd_set(params),
            "minecraft:serversettings/use_allowlist" => self.use_allowlist(),
            "minecraft:serversettings/use_allowlist/set" => self.use_allowlist_set(params),
            "minecraft:serversettings/max_players" => self.max_players(),
            "minecraft:serversettings/max_players/set" => self.max_players_set(params),
            "minecraft:serversettings/view_distance" => self.view_distance(),
            "minecraft:serversettings/view_distance/set" => self.view_distance_set(params),
            "minecraft:players/kick" => self.kick(params),
            "minecraft:server/save" => self.save(params),
            "minecraft:server/stop" => self.stop(),
            "minecraft:server/system_message" => self.system_message(params),
            _ => return None,
        })
    }
}

fn publish_allowlist_diff(
    ctx: &crate::context::ServerContext,
    before: &[Player],
    after: &[Player],
) {
    for player in after {
        if !before.iter().any(|old| same_player(old, player)) {
            ctx.management_bus
                .publish(&crate::notify::Note::AllowlistAdded(note_player(player)));
        }
    }
    for player in before {
        if !after.iter().any(|new| same_player(player, new)) {
            ctx.management_bus
                .publish(&crate::notify::Note::AllowlistRemoved(note_player(player)));
        }
    }
}

fn note_player(player: &Player) -> crate::notify::Player {
    crate::notify::Player {
        id: player.id.clone(),
        name: player.name.clone(),
    }
}

fn named_of(player: &Player) -> crate::lists::Named {
    crate::lists::Named {
        uuid: player.id.as_deref().and_then(crate::util::parse_uuid),
        name: player.name.clone().unwrap_or_default(),
    }
}

fn player_of_named(named: &crate::lists::Named) -> Player {
    Player {
        id: named.uuid.map(crate::util::uuid_string),
        name: present(&named.name),
    }
}

fn present(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn user_of_ban(ban: &crate::lists::Ban) -> UserBan {
    UserBan {
        player: Player {
            id: ban.uuid.map(crate::util::uuid_string),
            name: present(&ban.name),
        },
        reason: present(&ban.reason),
        source: present(&ban.source),
        expires: if ban.expires.is_empty() || ban.expires == "forever" {
            None
        } else {
            Some(ban.expires.clone())
        },
    }
}

fn ban_record(
    player: &Player,
    reason: &Option<String>,
    source: &Option<String>,
    expires: &Option<String>,
    created: String,
) -> crate::lists::Ban {
    crate::lists::Ban {
        uuid: player.id.as_deref().and_then(crate::util::parse_uuid),
        name: player.name.clone().unwrap_or_default(),
        created,
        source: source.clone().unwrap_or_default(),
        expires: expires.clone().unwrap_or_else(|| "forever".to_owned()),
        reason: reason
            .clone()
            .unwrap_or_else(|| "Banned by an operator.".to_owned()),
    }
}

fn ip_of_ban(ban: &crate::lists::Ban) -> Option<IpBan> {
    Some(IpBan {
        ip: ban.name.parse().ok()?,
        reason: present(&ban.reason),
        source: present(&ban.source),
        expires: if ban.expires.is_empty() || ban.expires == "forever" {
            None
        } else {
            Some(ban.expires.clone())
        },
    })
}

fn operator_of(op: &crate::lists::Op) -> Operator {
    Operator {
        player: Player {
            id: op.uuid.map(crate::util::uuid_string),
            name: present(&op.name),
        },
        permission_level: i64::from(op.level),
        bypasses_player_limit: op.bypasses_player_limit,
    }
}

fn rule_json(key: &str, value: crate::rules::Value) -> Value {
    let short = key.strip_prefix("minecraft:").unwrap_or(key);
    match value {
        crate::rules::Value::Bool(boolean) => Rule {
            kind: RuleKind::Boolean,
            boolean,
            integer: 0,
        }
        .to_json(short),
        crate::rules::Value::Int(integer) => Rule {
            kind: RuleKind::Integer,
            boolean: false,
            integer: i64::from(integer),
        }
        .to_json(short),
    }
}

fn write_property(ctx: &crate::context::ServerContext, key: &str, value: &str) {
    if let Some(dir) = &ctx.config.server_dir {
        crate::properties::set(dir, key, value);
    }
}

fn find_online(ctx: &crate::context::ServerContext, player: &Player) -> Option<u128> {
    let players = ctx.players.lock().expect("players lock");
    players
        .values()
        .find(|online| match (&player.id, &player.name) {
            (Some(id), _) => crate::util::uuid_string(online.profile.uuid) == *id,
            (None, Some(name)) => online.profile.name.eq_ignore_ascii_case(name),
            (None, None) => false,
        })
        .map(|online| online.profile.uuid)
}

fn online_with_ip(ctx: &crate::context::ServerContext, ips: &[String]) -> Vec<u128> {
    ctx.players
        .lock()
        .expect("players lock")
        .values()
        .filter(|player| {
            player
                .address
                .as_deref()
                .is_some_and(|address| ips.iter().any(|ip| ip == address))
        })
        .map(|player| player.profile.uuid)
        .collect()
}

fn drop_then_kick(ctx: &crate::context::ServerContext, players: &[Player], key: &str) {
    let uuids: Vec<u128> = players
        .iter()
        .filter_map(|player| find_online(ctx, player))
        .collect();
    drop_kick_uuids(ctx, &uuids, key);
}

fn drop_kick_uuids(ctx: &crate::context::ServerContext, uuids: &[u128], key: &str) {
    let reason = mc_protocol::text::translate(key, Vec::new());
    for uuid in uuids {
        let _ = ctx.events.send(crate::context::Event::Kick {
            uuid: *uuid,
            reason: reason.clone(),
        });
    }
}

fn text_of(message: &Value) -> Result<mc_nbt::Tag, RpcError> {
    let obj = message.as_object().ok_or_else(invalid_params)?;
    if let Some(key) = obj.get("translatable").and_then(Value::as_str) {
        let args = obj
            .get("translatableParams")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item.as_str()
                            .map(|text| mc_nbt::Tag::String(text.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(mc_protocol::text::translate(key, args));
    }
    if let Some(literal) = obj.get("literal").and_then(Value::as_str) {
        return Ok(mc_nbt::Tag::String(literal.to_owned()));
    }
    Err(invalid_params())
}

fn recipients(
    ctx: &crate::context::ServerContext,
    receivers: Option<&Value>,
) -> Result<Vec<u128>, RpcError> {
    let Some(receivers) = receivers else {
        return Ok(ctx
            .players
            .lock()
            .expect("players lock")
            .keys()
            .copied()
            .collect());
    };
    let items = receivers.as_array().ok_or_else(invalid_params)?;
    let players = parse_players(items)?;
    Ok(players
        .iter()
        .filter_map(|player| find_online(ctx, player))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    use super::*;

    fn no_names() -> Vec<String> {
        Vec::new()
    }

    fn no_count() -> usize {
        0
    }

    fn idle() -> ManagementHooks {
        ManagementHooks::new(no_names, no_count)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn call(id: i64, method: &str, params: Option<&str>) -> String {
        match params {
            None => format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#),
            Some(params) => {
                format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#)
            }
        }
    }

    async fn exchange(stream: &mut TcpStream, line: &str) -> Value {
        stream.write_all(line.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = Vec::new();
        loop {
            let mut byte = [0; 1];
            let read = timeout(Duration::from_secs(2), stream.read(&mut byte))
                .await
                .expect("timed out waiting for a management response")
                .unwrap();
            assert_eq!(read, 1, "connection closed before a full line");
            if byte[0] == b'\n' {
                break;
            }
            buf.push(byte[0]);
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        serde_json::from_slice(&buf).unwrap()
    }

    #[test]
    fn disabled_empty_secret_and_public_tls_do_not_listen() {
        let rt = runtime();
        rt.block_on(async {
            let disabled = start(
                ManagementConfig {
                    enabled: false,
                    host: "127.0.0.1".into(),
                    port: 0,
                    secret: "correct-secret".into(),
                    tls: false,
                },
                idle(),
            )
            .await;
            assert!(disabled.is_none());

            let empty = start(
                ManagementConfig {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 0,
                    secret: String::new(),
                    tls: false,
                },
                idle(),
            )
            .await;
            assert!(empty.is_none());

            let public = start(
                ManagementConfig {
                    enabled: true,
                    host: "0.0.0.0".into(),
                    port: 0,
                    secret: "correct-secret".into(),
                    tls: true,
                },
                idle(),
            )
            .await;
            assert!(public.is_none());

            let unresolved = start(
                ManagementConfig {
                    enabled: true,
                    host: String::new(),
                    port: 1,
                    secret: "correct-secret".into(),
                    tls: false,
                },
                idle(),
            )
            .await;
            assert!(unresolved.is_none());
        });
    }

    #[test]
    fn a_wrong_secret_is_refused_then_status_reports_the_player_count() {
        fn player_names() -> Vec<String> {
            vec!["Alex".into(), "Steve".into(), "Notch".into()]
        }
        fn player_count() -> usize {
            2
        }

        let rt = runtime();
        rt.block_on(async {
            let bound = start(
                ManagementConfig {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 0,
                    secret: "correct-secret".into(),
                    tls: false,
                },
                ManagementHooks::new(player_names, player_count),
            )
            .await
            .expect("listener");

            let mut stream = TcpStream::connect(bound).await.unwrap();

            let wrong = exchange(
                &mut stream,
                &call(1, METHOD_AUTHENTICATE, Some(r#"{"secret":"wrong"}"#)),
            )
            .await;
            assert_eq!(wrong["jsonrpc"], "2.0");
            assert_eq!(wrong["id"], 1);
            assert!(wrong.get("result").is_none());
            assert_eq!(wrong["error"]["code"].as_i64(), Some(SERVER_ERROR));
            assert_eq!(wrong["error"]["message"], "Invalid secret");

            // The failed login did not close the connection, and it did not authenticate it.
            let early = exchange(&mut stream, &call(2, METHOD_SERVER_STATUS, None)).await;
            assert_eq!(early["id"], 2);
            assert!(early.get("result").is_none());
            assert_eq!(early["error"]["code"].as_i64(), Some(SERVER_ERROR));
            assert_eq!(early["error"]["message"], "Not authenticated");

            let ok = exchange(
                &mut stream,
                &call(
                    3,
                    METHOD_AUTHENTICATE,
                    Some(r#"{"secret":"correct-secret"}"#),
                ),
            )
            .await;
            assert_eq!(ok["id"], 3);
            assert_eq!(ok["result"], true);
            assert!(ok.get("error").is_none());

            let status = exchange(&mut stream, &call(4, METHOD_SERVER_STATUS, None)).await;
            assert_eq!(status["id"], 4);
            let body = &status["result"];
            assert_eq!(body["started"], true);
            assert_eq!(body["version"]["name"], SERVER_BRAND);
            assert_eq!(body["version"]["protocol"].as_i64(), Some(GAME_PROTOCOL));
            let players = body["players"].as_array().unwrap();
            assert_eq!(players.len(), player_count());
            assert_eq!(players[0]["name"], "Alex");
            assert_eq!(players[1]["name"], "Steve");

            let listed = exchange(&mut stream, &call(5, METHOD_PLAYERS, None)).await;
            assert_eq!(listed["id"], 5);
            let listed = listed["result"].as_array().unwrap();
            assert_eq!(listed.len(), player_names().len());
            assert_eq!(listed[0]["name"], "Alex");
            assert_eq!(listed[1]["name"], "Steve");
            assert_eq!(listed[2]["name"], "Notch");

            // A second connection starts unauthenticated again.
            let mut other = TcpStream::connect(bound).await.unwrap();
            let denied = exchange(&mut other, &call(1, METHOD_PLAYERS, None)).await;
            assert_eq!(denied["error"]["message"], "Not authenticated");
        });
    }

    #[test]
    fn tls_on_loopback_is_still_plaintext() {
        fn player_names() -> Vec<String> {
            vec!["Alex".into()]
        }
        fn player_count() -> usize {
            1
        }

        let rt = runtime();
        rt.block_on(async {
            let bound = start(
                ManagementConfig {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 0,
                    secret: "correct-secret".into(),
                    tls: true,
                },
                ManagementHooks::new(player_names, player_count),
            )
            .await
            .expect("loopback plaintext listener");

            let mut stream = TcpStream::connect(bound).await.unwrap();
            let ok = exchange(
                &mut stream,
                &call(
                    1,
                    METHOD_AUTHENTICATE,
                    Some(r#"{"secret":"correct-secret"}"#),
                ),
            )
            .await;
            assert_eq!(ok["result"], true);
            let status = exchange(&mut stream, &call(2, METHOD_SERVER_STATUS, None)).await;
            assert_eq!(status["result"]["version"]["name"], SERVER_BRAND);
            assert_eq!(
                status["result"]["players"].as_array().unwrap().len(),
                player_count()
            );
        });
    }

    fn local_config() -> ManagementConfig {
        ManagementConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 0,
            secret: "correct-secret".into(),
            tls: false,
        }
    }

    async fn authed(hooks: ManagementHooks) -> TcpStream {
        let bound = start(local_config(), hooks).await.expect("listener");
        let mut stream = TcpStream::connect(bound).await.unwrap();
        let ok = exchange(
            &mut stream,
            &call(
                1,
                METHOD_AUTHENTICATE,
                Some(r#"{"secret":"correct-secret"}"#),
            ),
        )
        .await;
        assert_eq!(ok["result"], true);
        stream
    }

    fn rpc_ok(actions: &MemoryActions, method: &str, params: Option<Value>) -> Value {
        actions
            .call(method, params.as_ref())
            .unwrap_or_else(|| panic!("{method} is not implemented"))
            .unwrap_or_else(|err| panic!("{method} failed: {err:?}"))
    }

    fn rpc_err(actions: &MemoryActions, method: &str, params: Option<Value>) -> (i64, String) {
        actions
            .call(method, params.as_ref())
            .unwrap_or_else(|| panic!("{method} is not implemented"))
            .expect_err(method)
    }

    #[test]
    fn allowlist_add_get_and_remove() {
        let rt = runtime();
        rt.block_on(async {
            let mut stream = authed(idle().with_actions(MemoryActions::new())).await;
            let added = exchange(
                &mut stream,
                &call(
                    2,
                    "minecraft:allowlist/add",
                    Some(r#"[{"add":[{"name":"Alex"}]}]"#),
                ),
            )
            .await;
            assert_eq!(added["id"], 2);
            assert!(added.get("error").is_none());
            let list = added["result"].as_array().unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0]["name"], "Alex");

            let got = exchange(&mut stream, &call(3, "minecraft:allowlist", None)).await;
            assert_eq!(got["id"], 3);
            assert_eq!(got["result"], added["result"]);

            let removed = exchange(
                &mut stream,
                &call(
                    4,
                    "minecraft:allowlist/remove",
                    Some(r#"[{"remove":[{"name":"Alex"}]}]"#),
                ),
            )
            .await;
            assert_eq!(removed["id"], 4);
            assert_eq!(removed["result"].as_array().unwrap().len(), 0);

            let again = exchange(&mut stream, &call(5, "minecraft:allowlist", None)).await;
            assert_eq!(again["result"].as_array().unwrap().len(), 0);
        });
    }

    #[test]
    fn gamerule_updates_boolean_integer_and_rejects_a_boolean_string() {
        let rt = runtime();
        rt.block_on(async {
            let mut stream = authed(idle().with_actions(MemoryActions::new())).await;
            let bar = exchange(
                &mut stream,
                &call(
                    2,
                    "minecraft:gamerules/update",
                    Some(r#"{"gamerule":{"key":"locator_bar","value":false}}"#),
                ),
            )
            .await;
            assert_eq!(bar["id"], 2);
            assert!(bar.get("error").is_none());
            assert_eq!(bar["result"]["key"], "minecraft:locator_bar");
            assert_eq!(bar["result"]["type"], "boolean");
            assert_eq!(bar["result"]["value"].as_bool(), Some(false));

            let tick = exchange(
                &mut stream,
                &call(
                    3,
                    "minecraft:gamerules/update",
                    Some(r#"{"gamerule":{"key":"random_tick_speed","value":"3"}}"#),
                ),
            )
            .await;
            assert_eq!(tick["result"]["key"], "minecraft:random_tick_speed");
            assert_eq!(tick["result"]["type"], "integer");
            assert_eq!(tick["result"]["value"].as_i64(), Some(3));

            let bad = exchange(
                &mut stream,
                &call(
                    4,
                    "minecraft:gamerules/update",
                    Some(r#"{"gamerule":{"key":"locator_bar","value":"false"}}"#),
                ),
            )
            .await;
            assert_eq!(bad["id"], 4);
            assert!(bad.get("result").is_none());
            assert_eq!(bad["error"]["code"].as_i64(), Some(INVALID_PARAMS));
            assert_eq!(bad["error"]["message"], "Invalid params");

            let listed = exchange(&mut stream, &call(5, "minecraft:gamerules", None)).await;
            let rules = listed["result"].as_array().unwrap();
            assert_eq!(rules.len(), 2);
            assert_eq!(rules[0]["key"], "minecraft:locator_bar");
            assert_eq!(rules[0]["value"].as_bool(), Some(false));
            assert_eq!(rules[1]["key"], "minecraft:random_tick_speed");
            assert_eq!(rules[1]["value"].as_i64(), Some(3));
        });
    }

    #[test]
    fn unknown_serversettings_are_method_not_found() {
        let rt = runtime();
        rt.block_on(async {
            let mut idle_stream = authed(idle()).await;
            let missing = exchange(
                &mut idle_stream,
                &call(2, "minecraft:serversettings/view_distance", None),
            )
            .await;
            assert_eq!(missing["jsonrpc"], "2.0");
            assert_eq!(missing["id"], 2);
            assert!(missing.get("result").is_none());
            assert_eq!(missing["error"]["code"].as_i64(), Some(METHOD_NOT_FOUND));
            assert_eq!(missing["error"]["message"], "Method not found");

            let mut memory = authed(idle().with_actions(MemoryActions::new())).await;
            let missing = exchange(
                &mut memory,
                &call(2, "minecraft:serversettings/not_a_setting", None),
            )
            .await;
            assert_eq!(missing["id"], 2);
            assert!(missing.get("result").is_none());
            assert_eq!(missing["error"]["code"].as_i64(), Some(METHOD_NOT_FOUND));
            assert_eq!(missing["error"]["message"], "Method not found");

            let status = exchange(&mut memory, &call(3, METHOD_SERVER_STATUS, None)).await;
            assert_eq!(status["result"]["started"], true);
            assert_eq!(status["result"]["version"]["name"], SERVER_BRAND);
            assert_eq!(
                status["result"]["version"]["protocol"].as_i64(),
                Some(GAME_PROTOCOL)
            );
            assert_eq!(status["result"]["players"].as_array().unwrap().len(), 0);

            let players = exchange(&mut memory, &call(4, METHOD_PLAYERS, None)).await;
            assert_eq!(players["result"].as_array().unwrap().len(), 0);
        });
    }

    #[test]
    fn new_hooks_have_no_actions() {
        assert!(idle().actions.is_none());
    }

    #[test]
    fn memory_actions_store_lists_settings_and_server_calls() {
        let actions = MemoryActions::new();
        let alex = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

        assert_eq!(
            rpc_ok(&actions, "minecraft:serversettings/view_distance", None)["view_distance"],
            10
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:serversettings/view_distance/set",
                Some(json!({ "view_distance": 12 }))
            )["view_distance"],
            12
        );
        assert_eq!(rpc_ok(&actions, "minecraft:allowlist", None), json!([]));
        assert_eq!(
            rpc_ok(&actions, "minecraft:serversettings/motd", None)["message"],
            "A Minecraft Server"
        );
        assert_eq!(
            rpc_ok(&actions, "minecraft:serversettings/use_allowlist", None)["used"],
            false
        );
        assert_eq!(
            rpc_ok(&actions, "minecraft:serversettings/max_players", None)["max"],
            20
        );

        let added = rpc_ok(
            &actions,
            "minecraft:allowlist/add",
            Some(json!([{"add":[{"id":"AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE","name":"Alex"}]}])),
        );
        assert_eq!(added[0]["id"], alex);
        assert_eq!(added[0]["name"], "Alex");
        // Same id is already present, so a different name does not append.
        let still = rpc_ok(
            &actions,
            "minecraft:allowlist/add",
            Some(json!({"add":[{"id":alex,"name":"Steve"}]})),
        );
        assert_eq!(still.as_array().unwrap().len(), 1);
        assert_eq!(still[0]["name"], "Alex");
        // A different id is a different player even when the name matches.
        let two = rpc_ok(
            &actions,
            "minecraft:allowlist/add",
            Some(json!([[{"id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb","name":"Alex"}]])),
        );
        assert_eq!(two.as_array().unwrap().len(), 2);
        let err = rpc_err(
            &actions,
            "minecraft:allowlist/add",
            Some(json!({"add":[{}]})),
        );
        assert_eq!(err.0, INVALID_PARAMS);
        assert_eq!(
            rpc_ok(&actions, "minecraft:allowlist", None)
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:allowlist/set",
                Some(json!({"players":[{"name":"Notch"}]}))
            ),
            json!([{"name":"Notch"}])
        );
        assert_eq!(
            rpc_ok(&actions, "minecraft:allowlist/clear", None),
            json!([])
        );

        let banned = rpc_ok(
            &actions,
            "minecraft:bans/add",
            Some(json!({"add":[{"player":{"name":"Alex"},"reason":"grief","expires":"forever"}]})),
        );
        assert_eq!(banned[0]["player"]["name"], "Alex");
        assert_eq!(banned[0]["reason"], "grief");
        assert_eq!(banned[0]["source"], "Management server");
        assert_eq!(banned[0]["expires"], "forever");
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:bans/remove",
                Some(json!({"remove":[{"name":"Alex"}]}))
            ),
            json!([])
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:bans/set",
                Some(json!({"bans":[{"player":{"name":"Steve"},"source":"Admin"}]}))
            )[0]["source"],
            "Admin"
        );
        assert_eq!(rpc_ok(&actions, "minecraft:bans/clear", None), json!([]));

        let ip = rpc_ok(
            &actions,
            "minecraft:ip_bans/add",
            Some(json!({"add":[{"player":{"name":"10.0.0.8"},"reason":"spam"}]})),
        );
        assert_eq!(ip[0]["ip"], "10.0.0.8");
        assert_eq!(ip[0]["source"], "Management server");
        assert!(ip[0].get("player").is_none());
        let err = rpc_err(
            &actions,
            "minecraft:ip_bans/add",
            Some(json!({"add":[{"player":{"name":"Alex"}}]})),
        );
        assert_eq!(err.0, INVALID_PARAMS);
        assert_eq!(err.1, "Invalid params");
        assert_eq!(
            rpc_ok(&actions, "minecraft:ip_bans", None)
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:ip_bans/add",
                Some(json!([{"add":[{"ip":"127.0.0.1"}]}]))
            )
            .as_array()
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:ip_bans/remove",
                Some(json!({"ip":["10.0.0.8","127.0.0.1"]}))
            ),
            json!([])
        );
        rpc_ok(
            &actions,
            "minecraft:ip_bans/set",
            Some(json!({"banlist":[{"ip":"::1"}]})),
        );
        assert_eq!(rpc_ok(&actions, "minecraft:ip_bans/clear", None), json!([]));

        let op = rpc_ok(
            &actions,
            "minecraft:operators/add",
            Some(json!({"add":[{"player":{"name":"Alex"}}]})),
        );
        assert_eq!(op[0]["permissionLevel"], 4);
        assert_eq!(op[0]["bypassesPlayerLimit"], false);
        let op = rpc_ok(
            &actions,
            "minecraft:operators/add",
            Some(
                json!({"add":[{"player":{"name":"Alex"},"permissionLevel":2,"bypassesPlayerLimit":true}]}),
            ),
        );
        assert_eq!(op.as_array().unwrap().len(), 1);
        assert_eq!(op[0]["permissionLevel"], 2);
        assert_eq!(op[0]["bypassesPlayerLimit"], true);
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:operators/remove",
                Some(json!({"remove":[{"name":"Alex"}]}))
            ),
            json!([])
        );
        rpc_ok(
            &actions,
            "minecraft:operators/set",
            Some(json!({"operators":[{"player":{"name":"Steve"}}]})),
        );
        assert_eq!(
            rpc_ok(&actions, "minecraft:operators/clear", None),
            json!([])
        );

        let flag = rpc_ok(
            &actions,
            "minecraft:gamerules/update",
            Some(json!({"gamerule":{"key":"custom_flag","value":true}})),
        );
        assert_eq!(flag["key"], "minecraft:custom_flag");
        assert_eq!(flag["type"], "boolean");
        let err = rpc_err(
            &actions,
            "minecraft:gamerules/update",
            Some(json!([{"key":"custom_flag","value":"false"}])),
        );
        assert_eq!(err.0, INVALID_PARAMS);
        let count = rpc_ok(
            &actions,
            "minecraft:gamerules/update",
            Some(json!({"gamerule":{"key":"minecraft:custom_count","value":"7"}})),
        );
        assert_eq!(count["type"], "integer");
        assert_eq!(count["value"].as_i64(), Some(7));
        let err = rpc_err(
            &actions,
            "minecraft:gamerules/update",
            Some(json!({"gamerule":{"key":"random_tick_speed","value":true}})),
        );
        assert_eq!(err.0, INVALID_PARAMS);

        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:serversettings/motd/set",
                Some(json!({"message":"hello"}))
            )["message"],
            "hello"
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:serversettings/use_allowlist/set",
                Some(json!({"use":true}))
            )["used"],
            true
        );
        let err = rpc_err(
            &actions,
            "minecraft:serversettings/max_players/set",
            Some(json!({"max":0})),
        );
        assert_eq!(err.0, INVALID_PARAMS);
        assert_eq!(
            rpc_ok(&actions, "minecraft:serversettings/max_players", None)["max"],
            20
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:serversettings/max_players/set",
                Some(json!([{"max":8}]))
            )["max"],
            8
        );

        let kicked = rpc_ok(
            &actions,
            "minecraft:players/kick",
            Some(json!({"kick":[{"player":{"name":"Alex"},"message":"bye"}]})),
        );
        assert_eq!(kicked, json!([{"name":"Alex"}]));
        assert_eq!(actions.kicks()[0]["message"], "bye");
        assert_eq!(rpc_ok(&actions, "minecraft:server/save", None), true);
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:server/save",
                Some(json!({"flush":true}))
            ),
            true
        );
        assert_eq!(actions.saves(), vec![false, true]);
        assert!(!actions.stop_requested());
        assert_eq!(rpc_ok(&actions, "minecraft:server/stop", None), true);
        assert!(actions.stop_requested());
        let err = rpc_err(
            &actions,
            "minecraft:server/system_message",
            Some(json!({"message":"hi"})),
        );
        assert_eq!(err.0, INVALID_PARAMS);
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:server/system_message",
                Some(json!({"message":{"message":{"literal":"hi"},"overlay":true}}))
            ),
            true
        );
        assert_eq!(
            rpc_ok(
                &actions,
                "minecraft:server/system_message",
                Some(
                    json!([{"message":{"translatable":"multiplayer.disconnect.kicked"},"overlay":false}])
                )
            ),
            true
        );
        assert_eq!(actions.system_messages().len(), 2);
        assert_eq!(actions.system_messages()[0]["overlay"], true);
    }

    struct StealStatus;

    impl ManagementActions for StealStatus {
        fn call(
            &self,
            method: &str,
            _params: Option<&Value>,
        ) -> Option<Result<Value, (i64, String)>> {
            if method == METHOD_SERVER_STATUS || method == METHOD_PLAYERS {
                Some(Ok(Value::String("stolen".to_owned())))
            } else {
                None
            }
        }
    }

    #[test]
    fn dispatch_keeps_status_and_players_ahead_of_actions() {
        let mut authed = false;
        let early = dispatch(
            r#"{"jsonrpc":"2.0","id":1,"method":"minecraft:allowlist"}"#,
            &mut authed,
            "correct-secret",
            &idle().with_actions(MemoryActions::new()),
        );
        let early: Value = serde_json::from_str(&early).unwrap();
        assert_eq!(early["error"]["message"], "Not authenticated");
        assert!(!authed);

        authed = true;
        let hooks = idle().with_actions(StealStatus);
        let status: Value = serde_json::from_str(&dispatch(
            r#"{"jsonrpc":"2.0","id":2,"method":"minecraft:server/status"}"#,
            &mut authed,
            "correct-secret",
            &hooks,
        ))
        .unwrap();
        assert_eq!(status["result"]["version"]["name"], SERVER_BRAND);
        assert_eq!(status["result"]["started"], true);
        let players: Value = serde_json::from_str(&dispatch(
            r#"{"jsonrpc":"2.0","id":3,"method":"minecraft:players"}"#,
            &mut authed,
            "correct-secret",
            &hooks,
        ))
        .unwrap();
        assert!(players["result"].is_array());
        let missing: Value = serde_json::from_str(&dispatch(
            r#"{"jsonrpc":"2.0","id":4,"method":"minecraft:serversettings/view_distance"}"#,
            &mut authed,
            "correct-secret",
            &idle(),
        ))
        .unwrap();
        assert_eq!(missing["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(missing["error"]["message"], "Method not found");
    }

    #[test]
    fn live_allowlist_and_motd_change_the_running_server() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let actions = LiveActions::new(ctx.clone());
        let added = actions
            .call(
                "minecraft:allowlist/add",
                Some(&json!({"add":[{"name":"Alex"}]})),
            )
            .expect("method")
            .expect("added");
        assert_eq!(added[0]["name"], "Alex");
        assert!(
            ctx.lists
                .lock()
                .expect("lists")
                .whitelist
                .iter()
                .any(|named| named.name == "Alex")
        );
        let motd = actions
            .call(
                "minecraft:serversettings/motd/set",
                Some(&json!({"message":"hello"})),
            )
            .expect("method")
            .expect("motd");
        assert_eq!(motd["message"], "hello");
        assert_eq!(ctx.shown_motd.lock().expect("motd").as_str(), "hello");
    }

    #[test]
    fn an_authenticated_client_receives_a_notification() {
        let rt = runtime();
        rt.block_on(async {
            let bus = crate::notify::Bus::new();
            let bound = start(
                ManagementConfig {
                    enabled: true,
                    host: "127.0.0.1".into(),
                    port: 0,
                    secret: "correct-secret".into(),
                    tls: false,
                },
                idle().with_notes(bus.clone()),
            )
            .await
            .expect("listener");
            let mut stream = TcpStream::connect(bound).await.unwrap();
            let ok = exchange(
                &mut stream,
                &call(
                    1,
                    METHOD_AUTHENTICATE,
                    Some(r#"{"secret":"correct-secret"}"#),
                ),
            )
            .await;
            assert_eq!(ok["result"], true);
            bus.publish(&crate::notify::Note::ServerStarted);
            let mut buf = Vec::new();
            loop {
                let mut byte = [0; 1];
                let read = timeout(Duration::from_secs(2), stream.read(&mut byte))
                    .await
                    .expect("timed out waiting for a notification")
                    .unwrap();
                assert_eq!(read, 1);
                if byte[0] == b'\n' {
                    break;
                }
                buf.push(byte[0]);
            }
            let note: Value = serde_json::from_slice(&buf).unwrap();
            assert!(note.get("id").is_none());
            assert_eq!(note["method"], "minecraft:notification/server/started");
            assert!(note.get("params").is_none());
        });
    }
}
