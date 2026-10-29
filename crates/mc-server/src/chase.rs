//! The `/chase` debug command.
//!
//! `/chase lead` opens one TCP listener and, once that player's position or
//! look has changed, writes one UTF-8 line to every connected client.
//! `/chase follow` connects out and teleports the player who followed.
//! Nothing listens until a player leads. The tick samples players only while
//! a lead or a follow is active.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::context::{DIMENSIONS, Event, Pose, ServerContext, dimension_index};

const DEFAULT_HOST: &str = "localhost";
const DEFAULT_PORT: u16 = 10000;
/// Vanilla's lead port is at least 1024. Follow may use any port from 1.
const LEAD_MIN_PORT: u16 = 1024;
const FOLLOW_MIN_PORT: u16 = 1;
const MAX_LINE: usize = 1024;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
    Mutex::new(State {
        lead: None,
        follows: HashMap::new(),
    })
});

struct State {
    lead: Option<Leader>,
    follows: HashMap<u128, Follower>,
}

/// Where a player is, in the form the chase line carries.
#[derive(Clone, Debug, PartialEq)]
struct Body {
    dimension: String,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
}

enum Sight {
    Offline,
    /// Online, but not in a dimension we can name yet.
    Waiting,
    At(Body),
}

/// Runs one chase line for `player` (`None` when the source is not a player).
/// `Ok(None)` is `/chase stop` with nothing to close: no message.
pub(crate) fn command(
    player: Option<u128>,
    level: u8,
    line: &str,
) -> Result<Option<String>, String> {
    if level < 2 {
        return Err("You do not have permission to use this command.".to_owned());
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let sub = words.get(1).copied().unwrap_or("");
    match sub.to_ascii_lowercase().as_str() {
        "lead" => lead_command(player, &words[2..]),
        "follow" => follow_command(player, &words[2..]),
        "stop" => {
            if words.len() != 2 {
                return Err(usage());
            }
            Ok(player.and_then(stop))
        }
        _ => Err(usage()),
    }
}

/// Notices movement of whoever is leading, and applies lines a follower received.
/// Cheap when nobody is leading or following.
pub(crate) fn tick(ctx: &ServerContext) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let mut teleports = Vec::new();
    {
        let mut state = STATE.lock().expect("chase");
        let lead_offline = match state.lead.as_mut() {
            Some(lead) => match sight(ctx, lead.player) {
                Sight::Offline => true,
                Sight::Waiting => false,
                Sight::At(body) => {
                    lead.observe(body);
                    false
                }
            },
            None => false,
        };
        if lead_offline {
            state.lead = None;
        }
        let mut offline = Vec::new();
        for (&uuid, follow) in &state.follows {
            match sight(ctx, uuid) {
                Sight::Offline => offline.push(uuid),
                Sight::Waiting | Sight::At(_) => {
                    let mut inbox = follow.inbox.lock().expect("chase inbox");
                    if !inbox.is_empty() {
                        teleports.push((uuid, std::mem::take(&mut *inbox)));
                    }
                }
            }
        }
        for uuid in offline {
            state.follows.remove(&uuid);
        }
        refresh_active(&state);
    }
    for (uuid, bodies) in teleports {
        for body in bodies {
            apply_teleport(ctx, uuid, &body);
        }
    }
}

fn lead_command(player: Option<u128>, args: &[&str]) -> Result<Option<String>, String> {
    let player = player.ok_or_else(|| "Only a player can use this command.".to_owned())?;
    let (host, port) = match args {
        [] => (DEFAULT_HOST, DEFAULT_PORT),
        [host] => (*host, DEFAULT_PORT),
        [host, port] => (*host, parse_port(port, LEAD_MIN_PORT)?),
        _ => return Err(usage()),
    };
    start_lead(player, host, port).map(Some)
}

fn follow_command(player: Option<u128>, args: &[&str]) -> Result<Option<String>, String> {
    let player = player.ok_or_else(|| "Only a player can use this command.".to_owned())?;
    let (host, port) = match args {
        [] => (DEFAULT_HOST, DEFAULT_PORT),
        [host] => (*host, DEFAULT_PORT),
        [host, port] => (*host, parse_port(port, FOLLOW_MIN_PORT)?),
        _ => return Err(usage()),
    };
    start_follow(player, host, port).map(Some)
}

fn parse_port(text: &str, min: u16) -> Result<u16, String> {
    match text.parse::<i32>() {
        Ok(value) if (i32::from(min)..=65535).contains(&value) => Ok(value as u16),
        _ => Err(format!("Port must be between {min} and 65535")),
    }
}

fn usage() -> String {
    "Usage: /chase lead [bind_address] [port] | /chase follow [host] [port] | /chase stop"
        .to_owned()
}

fn start_lead(player: u128, host: &str, port: u16) -> Result<String, String> {
    let mut state = STATE.lock().expect("chase");
    // One listener for the whole server. A player who is already following
    // has to stop before they lead, and that path must not bind either.
    if state.lead.is_some() || state.follows.contains_key(&player) {
        return Err("Chase server is already running. Stop it using /chase stop".to_owned());
    }
    let mut leader = Leader::listen(host, port)
        .map_err(|err| format!("Could not start the chase server on {host}:{port}: {err}"))?;
    let actual = leader.net.local_addr().port();
    leader.player = player;
    state.lead = Some(leader);
    refresh_active(&state);
    Ok(format!(
        "Chase server is now running on port {actual}. Clients can follow you using /chase follow {host}"
    ))
}

fn start_follow(player: u128, host: &str, port: u16) -> Result<String, String> {
    let mut state = STATE.lock().expect("chase");
    if state.follows.contains_key(&player)
        || state
            .lead
            .as_ref()
            .is_some_and(|lead| lead.player == player)
    {
        return Err("You are already chasing someone. Stop it using /chase stop".to_owned());
    }
    let follow = Follower::start(host, port)
        .map_err(|err| format!("Could not follow {host}:{port}: {err}"))?;
    state.follows.insert(player, follow);
    refresh_active(&state);
    Ok(format!(
        "You are now chasing {host}:{port}. If that server does '/chase lead' then you will automatically go to the same position. Use '/chase stop' to stop chasing."
    ))
}

fn stop(player: u128) -> Option<String> {
    let mut state = STATE.lock().expect("chase");
    if state
        .lead
        .as_ref()
        .is_some_and(|lead| lead.player == player)
    {
        state.lead = None;
        refresh_active(&state);
        return Some("You are no longer being chased".to_owned());
    }
    if state.follows.remove(&player).is_some() {
        refresh_active(&state);
        return Some("You have now stopped chasing".to_owned());
    }
    None
}

fn refresh_active(state: &State) {
    ACTIVE.store(
        state.lead.is_some() || !state.follows.is_empty(),
        Ordering::Release,
    );
}

fn namespaced(name: &str) -> Option<&'static str> {
    dimension_index(name).map(|index| DIMENSIONS[index])
}

fn sight(ctx: &ServerContext, uuid: u128) -> Sight {
    let players = ctx.players.lock().expect("players");
    let Some(player) = players.get(&uuid) else {
        return Sight::Offline;
    };
    let Some(pose) = player.pose else {
        return Sight::Waiting;
    };
    let Some(dimension) = namespaced(&player.dimension) else {
        return Sight::Waiting;
    };
    Sight::At(Body {
        dimension: dimension.to_owned(),
        x: pose.x,
        y: pose.y,
        z: pose.z,
        yaw: pose.yaw,
        pitch: pose.pitch,
    })
}

fn apply_teleport(ctx: &ServerContext, uuid: u128, body: &Body) {
    let current = {
        let players = ctx.players.lock().expect("players");
        let Some(player) = players.get(&uuid) else {
            return;
        };
        player.dimension.clone()
    };
    let pose = Pose {
        x: body.x,
        y: body.y,
        z: body.z,
        yaw: body.yaw,
        pitch: body.pitch,
        on_ground: false,
    };
    let same = namespaced(&current).is_some_and(|name| name == body.dimension);
    let event = if same {
        Event::Teleport { uuid, pose }
    } else {
        Event::ChangeDimension {
            uuid,
            dimension: body.dimension.clone(),
            pose,
            after_death: false,
        }
    };
    let _ = ctx.of_player(uuid).events.send(event);
}

/// One UTF-8 line, ending in `\n`. Numbers use Rust's default Display.
fn format_line(body: &Body) -> String {
    format!(
        "execute in {} run tp @s {} {} {} {} {}\n",
        body.dimension, body.x, body.y, body.z, body.yaw, body.pitch
    )
}

fn parse_line(line: &str) -> Option<Body> {
    let rest = line.trim().strip_prefix("execute in ")?;
    let (dimension, rest) = rest.split_once(" run tp @s ")?;
    let dimension = namespaced(dimension)?.to_owned();
    let mut nums = rest.split_whitespace();
    let x = nums.next()?.parse().ok()?;
    let y = nums.next()?.parse().ok()?;
    let z = nums.next()?.parse().ok()?;
    let yaw = nums.next()?.parse().ok()?;
    let pitch = nums.next()?.parse().ok()?;
    if nums.next().is_some() {
        return None;
    }
    Some(Body {
        dimension,
        x,
        y,
        z,
        yaw,
        pitch,
    })
}

struct Leader {
    player: u128,
    /// Last pose we have seen. The first one is a baseline and is not sent.
    last: Option<Body>,
    net: Net,
}

impl Leader {
    fn listen(host: &str, port: u16) -> io::Result<Self> {
        Ok(Self {
            player: 0,
            last: None,
            net: Net::bind(host, port)?,
        })
    }

    fn observe(&mut self, body: Body) {
        let changed = self.last.as_ref().is_some_and(|prev| prev != &body);
        if self.last.is_some() && !changed {
            return;
        }
        if changed {
            self.net.send(&format_line(&body));
        }
        self.last = Some(body);
    }
}

/// The one chase listener. Dropping it closes the socket.
struct Net {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
}

impl Net {
    fn bind(host: &str, port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind((host, port))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let clients = Arc::new(Mutex::new(Vec::new()));
        let stop_thread = Arc::clone(&stop);
        let clients_thread = Arc::clone(&clients);
        let thread = thread::Builder::new()
            .name("chase-lead".into())
            .spawn(move || accept_loop(listener, clients_thread, stop_thread))?;
        Ok(Self {
            addr,
            stop,
            clients,
            thread: Some(thread),
        })
    }

    fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    fn send(&self, line: &str) {
        let bytes = line.as_bytes();
        let mut clients = self.clients.lock().expect("chase clients");
        clients.retain_mut(|stream| write_all_nonblocking(stream, bytes));
    }

    #[cfg(test)]
    fn client_count(&self) -> usize {
        self.clients.lock().expect("chase clients").len()
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn accept_loop(listener: TcpListener, clients: Arc<Mutex<Vec<TcpStream>>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_nonblocking(true);
                clients.lock().expect("chase clients").push(stream);
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::park_timeout(Duration::from_millis(20));
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

fn write_all_nonblocking(stream: &mut TcpStream, bytes: &[u8]) -> bool {
    let mut sent = 0;
    while sent < bytes.len() {
        match stream.write(&bytes[sent..]) {
            Ok(0) => return false,
            Ok(n) => sent += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

struct Follower {
    inbox: Arc<Mutex<Vec<Body>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Follower {
    fn start(host: &str, port: u16) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let inbox = Arc::new(Mutex::new(Vec::new()));
        let stop_thread = Arc::clone(&stop);
        let inbox_thread = Arc::clone(&inbox);
        let host = host.to_owned();
        let thread = thread::Builder::new()
            .name("chase-follow".into())
            .spawn(move || follow_loop(host, port, stop_thread, inbox_thread))?;
        Ok(Self {
            inbox,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn follow_loop(host: String, port: u16, stop: Arc<AtomicBool>, inbox: Arc<Mutex<Vec<Body>>>) {
    while !stop.load(Ordering::Acquire) {
        match connect_one(&host, port) {
            Ok(stream) => read_stream(stream, &stop, &inbox),
            Err(_) => {
                if wait(&stop, Duration::from_millis(200)) {
                    break;
                }
            }
        }
    }
}

fn connect_one(host: &str, port: u16) -> io::Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "could not resolve host"))?;
    TcpStream::connect_timeout(&addr, Duration::from_millis(100))
}

/// True when `stop` was set while waiting.
fn wait(stop: &AtomicBool, total: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < total {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        thread::park_timeout(Duration::from_millis(20));
    }
    stop.load(Ordering::Acquire)
}

fn read_stream(mut stream: TcpStream, stop: &AtomicBool, inbox: &Mutex<Vec<Body>>) {
    let _ = stream.set_nonblocking(true);
    let _ = stream.set_nodelay(true);
    let mut buf = Vec::new();
    let mut tmp = [0u8; 512];
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut tmp) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                drain_lines(&mut buf, inbox);
                if buf.len() > MAX_LINE {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::park_timeout(Duration::from_millis(20));
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn drain_lines(buf: &mut Vec<u8>, inbox: &Mutex<Vec<Body>>) {
    while let Some(end) = buf.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buf.drain(..=end).collect();
        let text = String::from_utf8_lossy(&line);
        let Some(body) = parse_line(&text) else {
            continue;
        };
        let mut inbox = inbox.lock().expect("chase inbox");
        inbox.push(body);
        if inbox.len() > 32 {
            let last = inbox.pop().expect("just pushed");
            inbox.clear();
            inbox.push(last);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpStream;
    use std::time::Instant;

    fn body(dimension: &str, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> Body {
        Body {
            dimension: dimension.to_owned(),
            x,
            y,
            z,
            yaw,
            pitch,
        }
    }

    #[test]
    fn formats_the_chase_line() {
        let overworld = body("minecraft:overworld", 1.5, 64.0, -2.25, 90.0, -10.5);
        assert_eq!(
            format_line(&overworld),
            "execute in minecraft:overworld run tp @s 1.5 64 -2.25 90 -10.5\n"
        );
        assert_eq!(
            format_line(&overworld),
            format!(
                "execute in {} run tp @s {} {} {} {} {}\n",
                overworld.dimension,
                overworld.x,
                overworld.y,
                overworld.z,
                overworld.yaw,
                overworld.pitch
            )
        );
        assert_eq!(
            parse_line(&format_line(&overworld)).as_ref(),
            Some(&overworld)
        );

        let nether = body("minecraft:the_nether", 0.1, 70.0, 0.2, 180.0, 0.0);
        assert_eq!(
            format_line(&nether),
            "execute in minecraft:the_nether run tp @s 0.1 70 0.2 180 0\n"
        );
        let end = body("minecraft:the_end", 0.0, 80.0, 0.0, -45.5, 12.25);
        assert_eq!(
            format_line(&end),
            "execute in minecraft:the_end run tp @s 0 80 0 -45.5 12.25\n"
        );
        assert!(parse_line("execute in minecraft:overworld run tp @s 1 2 3\n").is_none());
        assert!(parse_line("execute in not_a_dimension run tp @s 1 2 3 4 5\n").is_none());
    }

    #[test]
    fn lead_writes_one_line_after_a_move_and_stop_closes_the_listener() {
        let mut lead = Leader::listen("127.0.0.1", 0).expect("bind");
        let addr = lead.net.local_addr();
        let mut client = TcpStream::connect(addr).expect("connect");
        client.set_nodelay(true).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let accepted = wait_until(|| lead.net.client_count() >= 1);
        assert!(accepted, "client was not accepted");

        let here = body("minecraft:overworld", 0.0, 64.0, 0.0, 0.0, 0.0);
        let moved = body("minecraft:overworld", 10.5, 64.0, -3.0, 90.0, -12.5);
        lead.observe(here);
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut probe = [0u8; 1];
        let early = client.read(&mut probe);
        assert!(
            matches!(
                early,
                Err(ref err)
                    if err.kind() == io::ErrorKind::TimedOut
                        || err.kind() == io::ErrorKind::WouldBlock
            ),
            "lead wrote before the player moved: {early:?}"
        );

        lead.observe(moved);
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = std::io::BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read line");
        assert_eq!(
            line,
            "execute in minecraft:overworld run tp @s 10.5 64 -3 90 -12.5\n"
        );

        drop(lead);
        let again = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        assert!(again.is_err(), "listener still accepted a connection");
    }

    #[test]
    fn a_second_lead_does_not_open_another_socket() {
        struct Stop(u128);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = stop(self.0);
            }
        }
        let _guard = Stop(7);
        let message = start_lead(7, "127.0.0.1", 0).expect("lead");
        let port: u16 = message
            .split_whitespace()
            .find_map(|word| word.trim_end_matches('.').parse().ok())
            .expect("port");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let _held = TcpStream::connect(addr).expect("first listener");
        let err = start_lead(8, "127.0.0.1", 0).unwrap_err();
        assert_eq!(
            err,
            "Chase server is already running. Stop it using /chase stop"
        );
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok(),
            "the first listener was replaced"
        );
        assert_eq!(stop(7), Some("You are no longer being chased".to_owned()));
        let closed = TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        assert!(closed.is_err(), "stop left the listener open");
    }

    #[test]
    fn rejects_the_console_and_a_low_permission() {
        let err = command(None, 4, "chase lead").unwrap_err();
        assert_eq!(err, "Only a player can use this command.");
        let err = command(Some(1), 1, "chase follow").unwrap_err();
        assert_eq!(err, "You do not have permission to use this command.");
        assert!(command(Some(1), 2, "chase stop").unwrap().is_none());
    }

    fn wait_until(mut ready: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(2) {
            if ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }
}
