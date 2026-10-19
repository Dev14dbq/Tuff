//! `cargo xtask parity <scenario>`: runs the same commands on a vanilla
//! server and on ours, both on a fresh superflat world with the game frozen,
//! and compares a region block by block after every `step`.
//!
//! Each server gets a bot that mirrors the blocks it is sent (chunks, block
//! updates, section updates), so what is compared is exactly what a client
//! would see. Scenario lines (`#` starts a comment):
//!
//! ```text
//! region <x1> <y1> <z1> <x2> <y2> <z2>   the blocks to compare
//! run <command>                          sent to both consoles
//! step [<ticks>]                         /tick step, wait, compare
//! compare                                compare without stepping
//! light                                  let light settle, compare it
//! ```
//!
//! Vanilla works out light only while the game runs, so `light` unfreezes
//! both servers for a moment, sends the bot away and back so it is sent the
//! chunks again, freezes them and compares sky and block light as a client
//! reads it.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use mc_data::McData;
use mc_protocol::{Reader, Write as _};

use crate::probe::{
    Client, is_timeout, join_player_with, load_data, packet_ids, teleport_confirmation,
};

pub(crate) const VANILLA_PORT: u16 = 25571;
const OURS_PORT: u16 = 25572;
const BOT: &str = "Parity";
/// Where the bot waits: away from the test area, inside view distance.
const BOT_SPOT: &str = "0 -40 -24";

pub fn run(args: &[String]) -> Result<()> {
    let mut scenario: Option<PathBuf> = None;
    let mut verbose = false;
    for arg in args {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            other if scenario.is_none() => scenario = Some(other.into()),
            other => bail!("unexpected argument `{other}` for parity"),
        }
    }
    let scenario = scenario.context("usage: parity <scenario file> [--verbose]")?;
    let script = std::fs::read_to_string(&scenario)
        .with_context(|| format!("reading {}", scenario.display()))?;
    let data = load_data(None)?;
    let root = crate::workspace_root();

    let mut vanilla = Server::vanilla(&root)?;
    let mut ours = Server::ours(&root)?;
    let result = (|| -> Result<usize> {
        let ids = packet_ids(&data);
        let mut mirrors = [
            Mirror::join(&format!("127.0.0.1:{VANILLA_PORT}"), ids.clone())?,
            Mirror::join(&format!("127.0.0.1:{OURS_PORT}"), ids)?,
        ];
        let mut servers = [&mut vanilla, &mut ours];
        for setup in [
            "gamerule random_tick_speed 0",
            "gamerule spawn_mobs false",
            "gamerule advance_time false",
            "gamerule advance_weather false",
            "kill @e[type=!player]",
            &format!("tp {BOT} {BOT_SPOT}"),
            "tick freeze",
        ] {
            for server in servers.iter_mut() {
                server.command(setup, false)?;
            }
        }
        settle(&mut mirrors, Duration::from_millis(1500))?;

        let mut region: Option<[i32; 6]> = None;
        let mut failures = 0;
        for (number, line) in script.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            match word {
                "region" => {
                    let v: Vec<i32> = rest
                        .split_whitespace()
                        .map(str::parse)
                        .collect::<Result<_, _>>()
                        .with_context(|| format!("line {}: region needs 6 numbers", number + 1))?;
                    let [x1, y1, z1, x2, y2, z2] = v[..] else {
                        bail!("line {}: region needs 6 numbers", number + 1);
                    };
                    region = Some([
                        x1.min(x2),
                        y1.min(y2),
                        z1.min(z2),
                        x1.max(x2),
                        y1.max(y2),
                        z1.max(z2),
                    ]);
                }
                "run" => {
                    for server in servers.iter_mut() {
                        server.send(rest)?;
                    }
                }
                "step" | "compare" => {
                    let ticks: u32 = if word == "step" && !rest.is_empty() {
                        rest.parse().context("step <ticks>")?
                    } else {
                        u32::from(word == "step")
                    };
                    for server in servers.iter_mut() {
                        if ticks > 0 {
                            server.send(&format!("tick step {ticks}"))?;
                        }
                        server.command("", verbose)?;
                    }
                    let wait = Duration::from_millis(400 + 60 * u64::from(ticks));
                    settle(&mut mirrors, wait)?;
                    let region = region.context("`region` must come before the first step")?;
                    let diffs = compare(&mirrors, region, &data);
                    let label = format!("line {:>3}: {line}", number + 1);
                    if diffs.is_empty() {
                        println!("  same  {label}");
                    } else {
                        failures += 1;
                        println!("  DIFF  {label}: {} blocks", diffs.len());
                        for diff in diffs.iter().take(24) {
                            println!("        {diff}");
                        }
                    }
                }
                "light" => {
                    for server in servers.iter_mut() {
                        server.command("tick unfreeze", verbose)?;
                    }
                    settle(&mut mirrors, Duration::from_millis(1500))?;
                    for spot in ["2000 -40 2000", BOT_SPOT] {
                        for server in servers.iter_mut() {
                            server.command(&format!("tp {BOT} {spot}"), verbose)?;
                        }
                        settle(&mut mirrors, Duration::from_millis(2500))?;
                    }
                    for server in servers.iter_mut() {
                        server.command("tick freeze", verbose)?;
                    }
                    let region = region.context("`region` must come before `light`")?;
                    let diffs = compare_light(&mirrors, region);
                    let label = format!("line {:>3}: {line}", number + 1);
                    if diffs.is_empty() {
                        println!("  same  {label}");
                    } else {
                        failures += 1;
                        println!("  DIFF  {label}: {} light values", diffs.len());
                        for diff in diffs.iter().take(12) {
                            println!("        {diff}");
                        }
                    }
                }
                other => bail!("line {}: unknown scenario word `{other}`", number + 1),
            }
        }
        Ok(failures)
    })();
    vanilla.stop();
    ours.stop();
    let failures = result?;
    ensure!(failures == 0, "{failures} comparisons differ");
    println!("all comparisons match");
    Ok(())
}

pub(crate) struct Server {
    name: &'static str,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Server {
    pub(crate) fn vanilla(root: &Path) -> Result<Self> {
        let properties = format!(
            "online-mode=false\nserver-ip=127.0.0.1\nserver-port={VANILLA_PORT}\n\
             level-name=parity\nlevel-type=minecraft\\:flat\ngamemode=creative\n\
             spawn-protection=0\nview-distance=6\nsimulation-distance=6\nwhite-list=false\n\
             generate-structures=false\npause-when-empty-seconds=0\nmotd=parity\n\
             enable-status=true\nsync-chunk-writes=false\n"
        );
        let dir = root.join("cache/vanilla-server/26.3-parity");
        Self::prepare(root, &dir, &properties)?;
        let _ = std::fs::remove_dir_all(dir.join("parity"));
        Self::start_java(root, &dir)
    }

    /// Copies the accepted EULA of the local test server into `dir` and
    /// writes its `server.properties`.
    pub(crate) fn prepare(root: &Path, dir: &Path, properties: &str) -> Result<()> {
        let source = root.join("cache/vanilla-server/26.3");
        std::fs::create_dir_all(dir)?;
        let eula = source.join("eula.txt");
        ensure!(
            eula.is_file(),
            "no {} (accept the EULA for the local test server first)",
            eula.display()
        );
        std::fs::copy(&eula, dir.join("eula.txt"))?;
        std::fs::write(dir.join("server.properties"), properties)?;
        Ok(())
    }

    /// Starts the vanilla server jar in `dir`.
    pub(crate) fn start_java(root: &Path, dir: &Path) -> Result<Self> {
        let jar = root.join("reference/26.3/server.jar");
        let child = Command::new("java")
            .arg("-Xmx2G")
            .arg("-jar")
            .arg(&jar)
            .arg("nogui")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting java")?;
        Self::started("vanilla", child, "Done (", Duration::from_secs(120))
    }

    fn ours(root: &Path) -> Result<Self> {
        let dir = root.join("cache/ours-parity");
        let properties = format!(
            "online-mode=false\nserver-ip=127.0.0.1\nserver-port={OURS_PORT}\n\
             level-type=minecraft\\:flat\nview-distance=6\nwhite-list=false\n\
             gamemode=creative\n"
        );
        Self::ours_in(root, &dir, &properties)
    }

    /// Builds and starts our server in a fresh `dir` with `properties`.
    pub(crate) fn ours_in(root: &Path, dir: &Path, properties: &str) -> Result<Self> {
        let status = Command::new("cargo")
            .args(["build", "--release", "-q", "-p", "mc-server"])
            .current_dir(root)
            .status()?;
        ensure!(status.success(), "building mc-server failed");
        // A server folder with a flat world, as the vanilla side has.
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("server.properties"), properties)?;
        let child = Command::new(root.join("target/release/mc-server"))
            .args(["--universe".as_ref(), dir.as_os_str()])
            .env("NO_COLOR", "1")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting mc-server")?;
        Self::started("ours", child, "Done (", Duration::from_secs(60))
    }

    fn started(
        name: &'static str,
        mut child: Child,
        ready: &str,
        within: Duration,
    ) -> Result<Self> {
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let (send, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match lines.recv_timeout(left) {
                Ok(line) if line.contains(ready) => break,
                Ok(_) => {}
                Err(_) => bail!("{name} server did not start"),
            }
        }
        Ok(Self {
            name,
            child,
            stdin,
            lines,
        })
    }

    /// Sends a console command without waiting for its feedback.
    pub(crate) fn send(&mut self, command: &str) -> Result<()> {
        writeln!(self.stdin, "{command}")?;
        Ok(())
    }

    /// Drops the server's console output so far.
    pub(crate) fn drain(&mut self) -> Result<()> {
        self.stdin.flush()?;
        while self.lines.try_recv().is_ok() {}
        Ok(())
    }

    /// Sends a console command and waits for a console line containing
    /// `needle` (without the log prefix).
    pub(crate) fn wait_for(
        &mut self,
        command: &str,
        needle: &str,
        timeout: Duration,
    ) -> Result<String> {
        if !command.is_empty() {
            writeln!(self.stdin, "{command}")?;
        }
        self.stdin.flush()?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(200)) {
                let text = strip_log_prefix(&line);
                if text.contains(needle) {
                    return Ok(text.to_owned());
                }
            }
        }
        bail!("no `{needle}` from the server after `{command}`")
    }

    /// Sends a console command and, with `echo`, prints what the server said
    /// about it.
    pub(crate) fn command(&mut self, command: &str, echo: bool) -> Result<()> {
        if !command.is_empty() {
            writeln!(self.stdin, "{command}")?;
        }
        self.stdin.flush()?;
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(line) = self.lines.try_recv() {
            if echo {
                println!("        [{}] {}", self.name, strip_log_prefix(&line));
            }
        }
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        let _ = writeln!(self.stdin, "stop");
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
    }
}

fn strip_log_prefix(line: &str) -> &str {
    // "[06:12:23] [Server thread/INFO]: text" or "...Z  INFO target: text".
    line.rsplit_once("]: ")
        .map(|(_, t)| t)
        .or_else(|| line.split_once(": ").map(|(_, t)| t))
        .unwrap_or(line)
}

/// Light arrays of a chunk by section bit.
type Arrays = BTreeMap<usize, Vec<u8>>;

/// The blocks one client has been sent.
pub(crate) struct Mirror {
    client: Client,
    chunks: HashMap<(i32, i32), Vec<Vec<u32>>>,
    /// Sky and block light arrays by section bit.
    light: HashMap<(i32, i32), [Arrays; 2]>,
}

impl Mirror {
    pub(crate) fn join(addr: &str, ids: mc_protocol::PacketIds) -> Result<Self> {
        let (client, _) = join_player_with(addr, ids, BOT, 6)?;
        client.set_read_timeout(Duration::from_millis(20))?;
        let mut mirror = Self {
            client,
            chunks: HashMap::new(),
            light: HashMap::new(),
        };
        mirror.pump()?;
        Ok(mirror)
    }

    /// Reads what has arrived; returns whether anything changed blocks.
    pub(crate) fn pump(&mut self) -> Result<bool> {
        let mut changed = false;
        loop {
            let (packet, body) = match self.client.recv() {
                Ok(p) => p,
                Err(err) if is_timeout(&err) => return Ok(changed),
                Err(err) => return Err(err),
            };
            let mut r = Reader::new(&body);
            match packet.as_str() {
                "minecraft:keep_alive" => self.client.send("minecraft:keep_alive", body.clone())?,
                "minecraft:player_position" => self.client.send(
                    "minecraft:accept_teleportation",
                    teleport_confirmation(&body)?,
                )?,
                "minecraft:chunk_batch_finished" => {
                    let mut received = Vec::new();
                    received.put_f32(64.0);
                    self.client
                        .send("minecraft:chunk_batch_received", received)?;
                }
                "minecraft:level_chunk_with_light" => {
                    let (key, content) = crate::chunk_diff::decode(&body)?;
                    self.chunks.insert(key, content.blocks);
                    self.light.insert(key, [content.sky, content.block_light]);
                    changed = true;
                }
                "minecraft:light_update" => {
                    let key = (r.varint()?, r.varint()?);
                    let update = crate::chunk_diff::decode_light(&mut r)?;
                    let light = self.light.entry(key).or_default();
                    for (i, (arrays, empty)) in [
                        (update.sky, update.empty_sky),
                        (update.block, update.empty_block),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        for section in empty {
                            light[i].insert(section, vec![0; 2048]);
                        }
                        light[i].extend(arrays);
                    }
                    changed = true;
                }
                "minecraft:forget_level_chunk" => {
                    let (z, x) = (r.i32()?, r.i32()?);
                    self.chunks.remove(&(x, z));
                }
                "minecraft:block_update" => {
                    let p = r.position()?;
                    let state = r.varint()? as u32;
                    self.set(p.x, p.y, p.z, state);
                    changed = true;
                }
                "minecraft:section_blocks_update" => {
                    let packed = r.i64()?;
                    let (sx, sy, sz) = (
                        (packed >> 42) as i32,
                        (packed << 44 >> 44) as i32,
                        (packed << 22 >> 42) as i32,
                    );
                    for _ in 0..r.length_prefix()? {
                        let entry = r.varlong()?;
                        let state = (entry >> 12) as u32;
                        let (lx, lz, ly) = ((entry >> 8) & 15, (entry >> 4) & 15, entry & 15);
                        self.set(
                            sx * 16 + lx as i32,
                            sy * 16 + ly as i32,
                            sz * 16 + lz as i32,
                            state,
                        );
                    }
                    changed = true;
                }
                _ => {}
            }
        }
    }

    fn index(x: i32, y: i32, z: i32) -> (usize, usize) {
        let local = (y + 64) as usize;
        (
            local / 16,
            ((local % 16) << 8) | (((z & 15) as usize) << 4) | (x & 15) as usize,
        )
    }

    fn set(&mut self, x: i32, y: i32, z: i32, state: u32) {
        if let Some(sections) = self.chunks.get_mut(&(x >> 4, z >> 4)) {
            let (section, i) = Self::index(x, y, z);
            if let Some(s) = sections.get_mut(section) {
                s[i] = state;
            }
        }
    }

    /// Sky (`kind` 0) or block (1) light at a position, if the client was
    /// sent it.
    pub(crate) fn light(&self, kind: usize, x: i32, y: i32, z: i32) -> Option<u8> {
        let arrays = &self.light.get(&(x >> 4, z >> 4))?[kind];
        let section = ((y + 64) >> 4) as usize + 1;
        let array = arrays.get(&section)?;
        let i = (((y & 15) as usize) << 8) | (((z & 15) as usize) << 4) | (x & 15) as usize;
        let byte = array[i / 2];
        Some(if i.is_multiple_of(2) {
            byte & 15
        } else {
            byte >> 4
        })
    }

    /// Light as a client reads it: no block light array is dark, no sky
    /// array takes the bottom of the nearest array above (open sky if none).
    fn light_seen(&self, kind: usize, x: i32, y: i32, z: i32) -> Option<u8> {
        let arrays = &self.light.get(&(x >> 4, z >> 4))?[kind];
        let section = ((y + 64) >> 4) as usize + 1;
        let i = (((y & 15) as usize) << 8) | (((z & 15) as usize) << 4) | (x & 15) as usize;
        let value = |a: &Vec<u8>, i: usize| {
            let byte = a[i / 2];
            if i.is_multiple_of(2) {
                byte & 15
            } else {
                byte >> 4
            }
        };
        if let Some(array) = arrays.get(&section) {
            return Some(value(array, i));
        }
        if kind == 1 {
            return Some(0);
        }
        Some(
            arrays
                .range(section + 1..)
                .next()
                .map_or(15, |(_, a)| value(a, i & 0xFF)),
        )
    }

    pub(crate) fn block(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        let sections = self.chunks.get(&(x >> 4, z >> 4))?;
        let (section, i) = Self::index(x, y, z);
        sections.get(section).map(|s| s[i])
    }
}

/// Reads from both mirrors until neither has changed for a while (at least `min`).
fn settle(mirrors: &mut [Mirror; 2], min: Duration) -> Result<()> {
    let start = Instant::now();
    let mut quiet_since = Instant::now();
    loop {
        let mut changed = false;
        for mirror in mirrors.iter_mut() {
            changed |= mirror.pump()?;
        }
        if changed {
            quiet_since = Instant::now();
        }
        if start.elapsed() >= min && quiet_since.elapsed() >= Duration::from_millis(250) {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(30) {
            bail!("servers kept changing blocks for 30 s");
        }
    }
}

fn compare_light(mirrors: &[Mirror; 2], [x1, y1, z1, x2, y2, z2]: [i32; 6]) -> Vec<String> {
    let mut diffs = Vec::new();
    for (kind, what) in [(0, "sky"), (1, "block")] {
        for y in y1..=y2 {
            for z in z1..=z2 {
                for x in x1..=x2 {
                    let (a, b) = (
                        mirrors[0].light_seen(kind, x, y, z),
                        mirrors[1].light_seen(kind, x, y, z),
                    );
                    if a != b {
                        diffs.push(format!("{x} {y} {z} {what}: vanilla {a:?} | ours {b:?}"));
                    }
                }
            }
        }
    }
    diffs
}

fn compare(
    mirrors: &[Mirror; 2],
    [x1, y1, z1, x2, y2, z2]: [i32; 6],
    data: &McData,
) -> Vec<String> {
    let name = |s: Option<u32>| match s {
        Some(s) => data.blocks.describe(s).map_or_else(
            || format!("state {s}"),
            |d| d.trim_start_matches("minecraft:").replace("[]", ""),
        ),
        None => "not loaded".into(),
    };
    let mut diffs = Vec::new();
    for y in y1..=y2 {
        for z in z1..=z2 {
            for x in x1..=x2 {
                let (a, b) = (mirrors[0].block(x, y, z), mirrors[1].block(x, y, z));
                if a != b {
                    diffs.push(format!(
                        "{x} {y} {z}: vanilla {} | ours {}",
                        name(a),
                        name(b)
                    ));
                }
            }
        }
    }
    diffs
}
