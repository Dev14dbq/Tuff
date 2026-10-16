//! `cargo xtask probe`: connects to a server as a client (status, login,
//! configuration, spawn) and reports what the server sent, checking that every
//! packet we know parses with exactly our layout (no bytes left over).
//!
//! Run it against a local vanilla server and against ours to compare them:
//! this observes behaviour on the wire, without reading Mojang's code.

use std::collections::BTreeMap;
use std::io::{Read, Write as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use mc_protocol::frame::{decode_frame_body, split_frame, write_frame};
use mc_protocol::{Direction, PacketIds, Reader, State, Write as _};

pub fn run(args: &[String]) -> Result<()> {
    let mut addr = "127.0.0.1:25565".to_owned();
    let mut name = "Probe".to_owned();
    let mut version = None;
    let mut dump: Option<std::path::PathBuf> = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--addr" => addr = value()?,
            "--name" => name = value()?,
            "--version" => version = Some(value()?),
            "--dump" => dump = Some(value()?.into()),
            other => bail!("unknown option `{other}` for probe"),
        }
    }

    let ids = load_packet_ids(version)?;

    println!("== status {addr}");
    let mut client = Client::connect(&addr, ids.clone())?;
    client.handshake(1)?;
    client.send("minecraft:status_request", Vec::new())?;
    let (_, body) = client.expect("minecraft:status_response")?;
    println!("{}", Reader::new(&body).string(32767)?);

    println!("\n== login as {name}");
    let mut client = Client::connect(&addr, ids)?;
    client.handshake(2)?;
    let mut hello = Vec::new();
    hello.put_string(&name);
    hello.put_uuid(0);
    client.send("minecraft:hello", hello)?;
    loop {
        let (packet, body) = client.recv()?;
        match packet.as_str() {
            "minecraft:login_compression" => {
                let threshold = Reader::new(&body).varint()?;
                println!("compression threshold {threshold}");
                client.compression = usize::try_from(threshold).ok();
            }
            "minecraft:login_finished" => {
                report_check(&packet, check_login_finished(&body));
                break;
            }
            "minecraft:login_disconnect" => bail!("refused: {}", Reader::new(&body).string(32767)?),
            other => println!("login: {other}"),
        }
    }
    client.send("minecraft:login_acknowledged", Vec::new())?;
    client.state = State::Configuration;

    let mut info = Vec::new();
    info.put_string("en_us");
    info.put_i8(3);
    info.put_varint(0);
    info.put_bool(true);
    info.put_u8(0x7f);
    info.put_varint(1);
    info.put_bool(false);
    info.put_bool(true);
    info.put_varint(0);
    client.send("minecraft:client_information", info)?;

    println!("\n== configuration");
    let mut sequence = Vec::new();
    let mut registries = BTreeMap::new();
    let mut tags = BTreeMap::new();
    loop {
        let (packet, body) = client.recv()?;
        sequence.push(packet.clone());
        let mut r = Reader::new(&body);
        match packet.as_str() {
            "minecraft:select_known_packs" => {
                let count = r.length_prefix()?;
                for _ in 0..count {
                    let (ns, id, v) = (r.string(32767)?, r.string(32767)?, r.string(32767)?);
                    println!("known pack {ns}:{id} {v}");
                }
                client.send("minecraft:select_known_packs", body.clone())?;
            }
            "minecraft:registry_data" => {
                let registry = r.identifier()?;
                let count = r.length_prefix()?;
                let mut with_data = 0;
                for _ in 0..count {
                    r.identifier()?;
                    if r.bool()? {
                        r.nbt()?;
                        with_data += 1;
                    }
                }
                ensure!(
                    r.remaining() == 0,
                    "registry_data {registry}: {} bytes left",
                    r.remaining()
                );
                registries.insert(registry, (count, with_data));
            }
            "minecraft:update_tags" => {
                let count = r.length_prefix()?;
                for _ in 0..count {
                    let registry = r.identifier()?;
                    let tag_count = r.length_prefix()?;
                    for _ in 0..tag_count {
                        r.identifier()?;
                        let members = r.length_prefix()?;
                        for _ in 0..members {
                            r.varint()?;
                        }
                    }
                    tags.insert(registry, tag_count);
                }
                ensure!(
                    r.remaining() == 0,
                    "update_tags: {} bytes left",
                    r.remaining()
                );
            }
            "minecraft:update_enabled_features" => {
                let count = r.length_prefix()?;
                let flags: Vec<String> = (0..count)
                    .map(|_| r.identifier())
                    .collect::<Result<_, _>>()?;
                println!("feature flags {flags:?}");
            }
            "minecraft:custom_payload" => println!("plugin channel {}", r.identifier()?),
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body.clone())?,
            "minecraft:finish_configuration" => break,
            _ => {}
        }
    }
    println!("packets: {}", compact(&sequence));
    println!("registries ({}):", registries.len());
    for (registry, (count, with_data)) in &registries {
        println!("  {registry}: {count} entries, {with_data} with NBT");
    }
    println!("tags ({} registries):", tags.len());
    for (registry, count) in &tags {
        println!("  {registry}: {count} tags");
    }
    client.send("minecraft:finish_configuration", Vec::new())?;
    client.state = State::Play;

    println!("\n== play (until the first chunk batch is finished)");
    let mut sequence = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        ensure!(Instant::now() < deadline, "no chunk batch within 20 s");
        let (packet, body) = client.recv()?;
        sequence.push(packet.clone());
        if let Some(dir) = &dump {
            std::fs::create_dir_all(dir)?;
            let file = format!(
                "{:03}-{}.bin",
                sequence.len(),
                packet.trim_start_matches("minecraft:")
            );
            std::fs::write(dir.join(file), &body)?;
        }
        if let Some(result) = check_play(&packet, &body) {
            report_check(&packet, result);
        }
        match packet.as_str() {
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:player_position" => {
                client.send(
                    "minecraft:accept_teleportation",
                    teleport_confirmation(&body)?,
                )?;
            }
            "minecraft:chunk_batch_finished" => break,
            _ => {}
        }
    }
    println!("packets: {}", compact(&sequence));
    Ok(())
}

/// Confirm Teleportation for a Synchronize Player Position body: the teleport
/// id plus the position and rotation we end up with (absolute teleport assumed).
pub(crate) fn teleport_confirmation(position: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader::new(position);
    let id = r.varint()?;
    let (x, y, z) = (r.f64()?, r.f64()?, r.f64()?);
    for _ in 0..3 {
        r.f64()?; // velocity
    }
    let (yaw, pitch) = (r.f32()?, r.f32()?);
    let mut confirm = Vec::new();
    confirm.put_varint(id);
    for v in [x, y, z] {
        confirm.put_f64(v);
    }
    confirm.put_f32(yaw);
    confirm.put_f32(pitch);
    Ok(confirm)
}

/// Packet ids from extracted data (`cache/mcdata/<version>`, newest by default).
pub(crate) fn load_packet_ids(version: Option<String>) -> Result<PacketIds> {
    let cache = crate::workspace_root().join("cache").join("mcdata");
    let version = match version {
        Some(version) => version,
        None => newest_cache_version(&cache)?,
    };
    let data = mc_data::McData::load(&cache.join(&version))?;
    let mut ids = PacketIds::new();
    for p in &data.packets {
        if let (Some(state), Some(direction)) = (
            State::from_report_name(&p.phase),
            Direction::from_report_name(&p.direction),
        ) {
            ids.insert(state, direction, &p.name, p.id);
        }
    }
    Ok(ids)
}

pub(crate) fn newest_cache_version(cache: &std::path::Path) -> Result<String> {
    let mut versions: Vec<String> = std::fs::read_dir(cache)
        .with_context(|| format!("no {} (run `cargo xtask extract-data`)", cache.display()))?
        .flatten()
        .filter(|e| e.path().join("version.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    versions.sort();
    versions
        .pop()
        .context("no extracted versions in cache/mcdata")
}

/// `a, b ×3, c` for runs of the same packet.
fn compact(names: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < names.len() {
        let run = names[i..].iter().take_while(|n| **n == names[i]).count();
        let short = names[i].trim_start_matches("minecraft:");
        out.push(if run > 1 {
            format!("{short} ×{run}")
        } else {
            short.to_owned()
        });
        i += run;
    }
    out.join(", ")
}

fn report_check(packet: &str, result: Result<String>) {
    let short = packet.trim_start_matches("minecraft:");
    match result {
        Ok(summary) => println!("  ok   {short}: {summary}"),
        Err(err) => println!("  FAIL {short}: {err:#}"),
    }
}

fn finish(r: &Reader, summary: String) -> Result<String> {
    ensure!(
        r.remaining() == 0,
        "{} bytes left over: our layout is shorter than the server's",
        r.remaining()
    );
    Ok(summary)
}

fn check_login_finished(body: &[u8]) -> Result<String> {
    let mut r = Reader::new(body);
    let uuid = r.uuid()?;
    let name = r.string(16)?;
    let properties = r.length_prefix()?;
    for _ in 0..properties {
        r.string(64)?;
        r.string(32767)?;
        if r.bool()? {
            r.string(1024)?;
        }
    }
    let session = r.uuid()?;
    finish(
        &r,
        format!("{name} {uuid:032x}, {properties} properties, session {session:032x}"),
    )
}

fn check_play(packet: &str, body: &[u8]) -> Option<Result<String>> {
    let mut r = Reader::new(body);
    let r = &mut r;
    let result = (|| -> Result<String> {
        Ok(match packet {
            "minecraft:login" => {
                let entity = r.i32()?;
                let hardcore = r.bool()?;
                let dims: Vec<String> = (0..r.length_prefix()?)
                    .map(|_| r.identifier())
                    .collect::<Result<_, _>>()?;
                let (max, view, sim) = (r.varint()?, r.varint()?, r.varint()?);
                let (reduced, respawn, limited) = (r.bool()?, r.bool()?, r.bool()?);
                let dim_type = r.varint()?;
                let dim = r.identifier()?;
                let seed = r.i64()?;
                let (mode, prev) = (r.varint()?, r.varint()?);
                let (debug, flat) = (r.bool()?, r.bool()?);
                if r.bool()? {
                    r.identifier()?;
                    r.i64()?;
                }
                let (portal, sea) = (r.varint()?, r.varint()?);
                let (online, secure) = (r.bool()?, r.bool()?);
                let s = format!(
                    "entity {entity} hardcore {hardcore} dims {dims:?} max {max} view {view} sim {sim} \
                     reduced {reduced} respawn {respawn} limited {limited} type {dim_type} {dim} seed {seed:#x} \
                     mode {mode} prev {prev} debug {debug} flat {flat} portal {portal} sea {sea} \
                     online {online} secure {secure}"
                );
                finish(r, s)?
            }
            "minecraft:player_position" => {
                let id = r.varint()?;
                let v: Vec<f64> = (0..6).map(|_| r.f64()).collect::<Result<_, _>>()?;
                let (yaw, pitch, flags) = (r.f32()?, r.f32()?, r.i32()?);
                finish(
                    r,
                    format!(
                        "teleport {id} pos {:?} vel {:?} yaw {yaw} pitch {pitch} flags {flags:#x}",
                        &v[..3],
                        &v[3..]
                    ),
                )?
            }
            "minecraft:set_default_spawn_position" => {
                let dim = r.identifier()?;
                let pos = r.position()?;
                let (yaw, pitch) = (r.f32()?, r.f32()?);
                finish(r, format!("{dim} {pos:?} yaw {yaw} pitch {pitch}"))?
            }
            "minecraft:game_event" => {
                let (event, value) = (r.u8()?, r.f32()?);
                finish(r, format!("event {event} value {value}"))?
            }
            "minecraft:player_abilities" => {
                let (flags, speed, fov) = (r.u8()?, r.f32()?, r.f32()?);
                finish(r, format!("flags {flags:#x} speed {speed} fov {fov}"))?
            }
            "minecraft:set_chunk_cache_center" => {
                let (x, z) = (r.varint()?, r.varint()?);
                finish(r, format!("{x}, {z}"))?
            }
            "minecraft:chunk_batch_finished" => {
                let size = r.varint()?;
                finish(r, format!("batch of {size}"))?
            }
            "minecraft:system_chat" => {
                let tag = r.nbt()?;
                let overlay = r.bool()?;
                finish(r, format!("{tag:?} overlay {overlay}"))?
            }
            "minecraft:level_chunk_with_light" => check_chunk(r)?,
            _ => return Err(anyhow::anyhow!("unchecked")),
        })
    })();
    match result {
        Err(err) if err.to_string() == "unchecked" => None,
        other => Some(other),
    }
}

/// Chunk Data and Update Light, parsed with the documented layout. Section
/// containers: blocks use an indirect palette for 4..=8 bits, biomes for 1..=3.
fn check_chunk(r: &mut Reader) -> Result<String> {
    let (x, z) = (r.i32()?, r.i32()?);
    let mut heightmaps = Vec::new();
    for _ in 0..r.length_prefix()? {
        let kind = r.varint()?;
        let longs = r.length_prefix()?;
        r.bytes(longs * 8)?;
        heightmaps.push((kind, longs));
    }
    let data = r.byte_array(1 << 21)?.to_vec();
    let mut s = Reader::new(&data);
    let mut sections = 0;
    let mut non_air = Vec::new();
    let mut bits_seen = Vec::new();
    while s.remaining() > 0 {
        non_air.push(s.i16()?);
        s.i16()?; // fluids
        for (entries, indirect) in [(4096usize, 4u8..=8), (64, 1..=3)] {
            let bits = s.u8()?;
            bits_seen.push(bits);
            if bits == 0 {
                s.varint()?;
                continue;
            }
            if indirect.contains(&bits) {
                for _ in 0..s.length_prefix()? {
                    s.varint()?;
                }
            }
            s.bytes(entries.div_ceil(64 / bits as usize) * 8)?;
        }
        sections += 1;
    }
    let block_entities = r.length_prefix()?;
    for _ in 0..block_entities {
        r.u8()?;
        r.i16()?;
        r.varint()?;
        r.nbt()?;
    }
    // Light masks are byte bit sets in 26.3 (bit 0 = section below the world).
    let mut masks = Vec::new();
    for _ in 0..4 {
        let bytes = r.byte_array(1024)?;
        let bits: Vec<usize> = (0..bytes.len() * 8)
            .filter(|&i| bytes[i / 8] & (1 << (i % 8)) != 0)
            .collect();
        masks.push(bits);
    }
    let mut arrays = Vec::new();
    for _ in 0..2 {
        let count = r.length_prefix()?;
        for _ in 0..count {
            ensure!(r.length_prefix()? == 2048, "light array is not 2048 bytes");
            r.bytes(2048)?;
        }
        arrays.push(count);
    }
    bits_seen.sort_unstable();
    bits_seen.dedup();
    finish(
        r,
        format!(
            "({x}, {z}) heightmaps {heightmaps:?} sections {sections} non-air {:?} bits {bits_seen:?} \
             block entities {block_entities} light masks sky/block/empty-sky/empty-block {masks:?} arrays sky/block {arrays:?}",
            &non_air[..non_air.len().min(3)]
        ),
    )
}

pub(crate) struct Client {
    stream: TcpStream,
    buf: Vec<u8>,
    pub(crate) compression: Option<usize>,
    ids: PacketIds,
    pub(crate) state: State,
}

impl Client {
    pub(crate) fn connect(addr: &str, ids: PacketIds) -> Result<Self> {
        let stream =
            TcpStream::connect(addr).with_context(|| format!("cannot connect to {addr}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream,
            buf: Vec::new(),
            compression: None,
            ids,
            state: State::Handshake,
        })
    }

    pub(crate) fn set_read_timeout(&self, timeout: Duration) -> Result<()> {
        self.stream.set_read_timeout(Some(timeout))?;
        Ok(())
    }

    pub(crate) fn handshake(&mut self, intent: i32) -> Result<()> {
        let mut body = Vec::new();
        body.put_varint(mc_protocol::PROTOCOL_VERSION);
        body.put_string("localhost");
        body.put_u16(25565);
        body.put_varint(intent);
        self.send("minecraft:intention", body)?;
        self.state = if intent == 1 {
            State::Status
        } else {
            State::Login
        };
        Ok(())
    }

    pub(crate) fn send(&mut self, name: &str, body: Vec<u8>) -> Result<()> {
        let id = self
            .ids
            .id(self.state, Direction::Serverbound, name)
            .with_context(|| format!("no serverbound {name} in {:?}", self.state))?;
        let mut out = Vec::new();
        write_frame(&mut out, id, &body, self.compression);
        self.stream.write_all(&out)?;
        Ok(())
    }

    pub(crate) fn recv(&mut self) -> Result<(String, Vec<u8>)> {
        loop {
            if let Some((header, len)) = split_frame(&self.buf)? {
                let inner = decode_frame_body(&self.buf[header..header + len], self.compression)?;
                self.buf.drain(..header + len);
                let mut r = Reader::new(&inner);
                let id = r.varint()?;
                let name = self
                    .ids
                    .name(self.state, Direction::Clientbound, id)
                    .with_context(|| format!("unknown clientbound id {id} in {:?}", self.state))?
                    .to_owned();
                return Ok((name, r.rest().to_vec()));
            }
            let mut chunk = [0u8; 65536];
            let n = self
                .stream
                .read(&mut chunk)
                .context("reading from the server")?;
            ensure!(n > 0, "server closed the connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    pub(crate) fn expect(&mut self, wanted: &str) -> Result<(String, Vec<u8>)> {
        let (name, body) = self.recv()?;
        ensure!(name == wanted, "expected {wanted}, got {name}");
        Ok((name, body))
    }
}
