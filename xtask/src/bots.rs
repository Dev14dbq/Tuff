//! `cargo xtask bots`: load test. Starts N bots (offline-mode names), each logs
//! in, waits for its chunks, then walks in a circle sending movement like a
//! client, answering keep-alives. Reports joins, failures and traffic.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use mc_protocol::{PacketIds, Reader, State, Write as _};

use crate::probe::{Client, Pose, follow_teleport, is_timeout, load_packet_ids};

#[derive(Default)]
struct Stats {
    joined: AtomicUsize,
    failed: AtomicUsize,
    packets: AtomicU64,
    chunks: AtomicU64,
    moves: AtomicU64,
    teleports: AtomicU64,
    join_ms_total: AtomicU64,
    join_ms_max: AtomicU64,
}

pub fn run(args: &[String]) -> Result<()> {
    let mut addr = "127.0.0.1:25565".to_owned();
    let mut count = 100usize;
    let mut seconds = 30u64;
    let mut version = None;
    let mut view_distance = 2i8;
    let mut follow = false;
    let mut name = "Buddy".to_owned();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--addr" => addr = value()?,
            "--count" => count = value()?.parse().context("--count")?,
            "--seconds" => seconds = value()?.parse().context("--seconds")?,
            "--version" => version = Some(value()?),
            "--view-distance" => view_distance = value()?.parse().context("--view-distance")?,
            "--follow" => follow = true,
            "--name" => name = value()?,
            other => bail!("unknown option `{other}` for bots"),
        }
    }
    let ids = load_packet_ids(version)?;
    if follow {
        println!("{name} follows a player on {addr} for {seconds} s");
        return follow_bot(&addr, &name, ids, seconds, view_distance);
    }
    let stats = Arc::new(Stats::default());
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    println!("{count} bots → {addr} for {seconds} s");

    let handles: Vec<_> = (0..count)
        .map(|i| {
            let (addr, ids, stats) = (addr.clone(), ids.clone(), stats.clone());
            // Stagger connections a little, like players arriving.
            thread::sleep(Duration::from_millis(10));
            thread::spawn(move || {
                let name = format!("Bot{i:03}");
                if let Err(err) = bot(&addr, &name, ids, &stats, deadline, i, view_distance) {
                    stats.failed.fetch_add(1, Ordering::Relaxed);
                    eprintln!("Bot{i:03}: {err:#}");
                }
            })
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }

    let joined = stats.joined.load(Ordering::Relaxed);
    let failed = stats.failed.load(Ordering::Relaxed);
    let avg = stats.join_ms_total.load(Ordering::Relaxed) / joined.max(1) as u64;
    println!(
        "joined {joined}/{count}, failed {failed}; join time avg {avg} ms, max {} ms",
        stats.join_ms_max.load(Ordering::Relaxed)
    );
    println!(
        "received {} packets, {} chunks, {} teleports; sent {} moves in {:.1} s",
        stats.packets.load(Ordering::Relaxed),
        stats.chunks.load(Ordering::Relaxed),
        stats.teleports.load(Ordering::Relaxed),
        stats.moves.load(Ordering::Relaxed),
        started.elapsed().as_secs_f64()
    );
    ensure!(failed == 0, "{failed} bots failed");
    Ok(())
}

fn bot(
    addr: &str,
    name: &str,
    ids: PacketIds,
    stats: &Stats,
    deadline: Instant,
    index: usize,
    view_distance: i8,
) -> Result<()> {
    let begin = Instant::now();
    let mut client = Client::connect(addr, ids)?;
    client.handshake(2)?;
    let mut hello = Vec::new();
    hello.put_string(name);
    hello.put_uuid(0);
    client.send("minecraft:hello", hello)?;
    loop {
        let (packet, body) = client.recv()?;
        match packet.as_str() {
            "minecraft:login_compression" => {
                client.compression = usize::try_from(Reader::new(&body).varint()?).ok();
            }
            "minecraft:login_finished" => break,
            "minecraft:hello" => bail!("server is in online mode; bots need --offline"),
            "minecraft:login_disconnect" => bail!("refused: {}", Reader::new(&body).string(32767)?),
            _ => {}
        }
    }
    client.send("minecraft:login_acknowledged", Vec::new())?;
    client.state = State::Configuration;
    let mut info = Vec::new();
    info.put_string("en_us");
    info.put_i8(view_distance);
    info.put_varint(0); // chat enabled
    info.put_bool(true);
    info.put_u8(0x7f); // all skin parts
    info.put_varint(1); // right hand
    info.put_bool(false);
    info.put_bool(true);
    info.put_varint(0);
    client.send("minecraft:client_information", info)?;
    loop {
        let (packet, body) = client.recv()?;
        match packet.as_str() {
            "minecraft:select_known_packs" => client.send("minecraft:select_known_packs", body)?,
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:finish_configuration" => break,
            "minecraft:disconnect" => bail!("disconnected during configuration"),
            _ => {}
        }
    }
    client.send("minecraft:finish_configuration", Vec::new())?;
    client.state = State::Play;
    // Short reads so the bot keeps sending movement while the server is quiet.
    client.set_read_timeout(Duration::from_millis(50))?;

    let mut position: Option<(f64, f64, f64)> = None;
    let mut pose = Pose::default();
    let mut spawned = false;
    let mut next_tick = Instant::now();
    // The newest teleport, confirmed on the bot's next tick as a client
    // does: confirming each at once can ping-pong with a lagging vanilla
    // server, which then teleports again at network speed.
    let mut confirm: Option<Vec<u8>> = None;
    let mut step = 0u32;
    // Each bot flies its own circle, so chunk loading is spread out, 40
    // blocks above where it spawned: over the terrain vanilla takes every
    // step ("moved wrongly" corrections would hold a walking bot in place
    // while a server without collision checks lets it roam). It climbs a
    // block a tick to get there.
    let mut angle0 = index as f64 * 0.7;
    let mut cruise: Option<f64> = None;
    while Instant::now() < deadline {
        if Instant::now() >= next_tick {
            if let Some(confirm) = confirm.take() {
                client.send("minecraft:accept_teleportation", confirm)?;
            }
            if spawned {
                let (x0, y, z0) = position.expect("spawned implies a position");
                let top = *cruise.get_or_insert(y + 40.0);
                let y = (y + 1.0).min(top);
                position = Some((x0, y, z0));
                step += 1;
                let angle = angle0 + f64::from(step) * 0.05;
                let radius = 40.0;
                // The circle starts where the bot stands: no jump on the
                // first step.
                let mut moved = Vec::new();
                moved.put_f64(x0 + radius * (angle.cos() - angle0.cos()));
                moved.put_f64(y);
                moved.put_f64(z0 + radius * (angle.sin() - angle0.sin()));
                moved.put_u8(0x01);
                client.send("minecraft:move_player_pos", moved)?;
                stats.moves.fetch_add(1, Ordering::Relaxed);
                client.send("minecraft:client_tick_end", Vec::new())?;
            }
            next_tick = Instant::now() + Duration::from_millis(50);
        }
        let (packet, body) = match client.recv() {
            Ok(p) => p,
            // The read timeout fires when the server is quiet; keep walking.
            Err(err) if is_timeout(&err) => continue,
            Err(err) => return Err(err),
        };
        stats.packets.fetch_add(1, Ordering::Relaxed);
        match packet.as_str() {
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:player_position" => {
                stats.teleports.fetch_add(1, Ordering::Relaxed);
                confirm = Some(follow_teleport(&body, &mut pose)?);
                position = Some((pose.x, pose.y, pose.z));
                // The circle goes on from where the server put the bot; from
                // the old start the next step would land far away.
                angle0 += f64::from(step) * 0.05;
                step = 0;
            }
            "minecraft:level_chunk_with_light" => {
                stats.chunks.fetch_add(1, Ordering::Relaxed);
            }
            "minecraft:chunk_batch_finished" => {
                // As a client acknowledges each batch; vanilla sends the
                // next one only then.
                let mut received = Vec::new();
                received.put_f32(1000.0); // chunks per tick we could take
                client.send("minecraft:chunk_batch_received", received)?;
                if spawned {
                    continue;
                }
                spawned = true;
                // As a client does once its world is shown; vanilla rejects
                // movement before it.
                client.send("minecraft:player_loaded", Vec::new())?;
                stats.joined.fetch_add(1, Ordering::Relaxed);
                let ms = begin.elapsed().as_millis() as u64;
                stats.join_ms_total.fetch_add(ms, Ordering::Relaxed);
                stats.join_ms_max.fetch_max(ms, Ordering::Relaxed);
            }
            "minecraft:disconnect" => {
                let text: String = String::from_utf8_lossy(&body)
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect();
                bail!("kicked: {text}")
            }
            _ => {}
        }
    }
    ensure!(spawned, "never received its chunks");
    Ok(())
}

/// One bot that sprints and hops after the other player on the server.
fn follow_bot(
    addr: &str,
    name: &str,
    ids: PacketIds,
    seconds: u64,
    view_distance: i8,
) -> Result<()> {
    let mut client = Client::connect(addr, ids)?;
    client.handshake(2)?;
    let mut hello = Vec::new();
    hello.put_string(name);
    hello.put_uuid(0);
    client.send("minecraft:hello", hello)?;
    loop {
        let (packet, body) = client.recv()?;
        match packet.as_str() {
            "minecraft:login_compression" => {
                client.compression = usize::try_from(Reader::new(&body).varint()?).ok();
            }
            "minecraft:login_finished" => break,
            "minecraft:hello" => bail!("server is in online mode; bots need --offline"),
            "minecraft:login_disconnect" => bail!("refused: {}", Reader::new(&body).string(32767)?),
            _ => {}
        }
    }
    client.send("minecraft:login_acknowledged", Vec::new())?;
    client.state = State::Configuration;
    let mut info = Vec::new();
    info.put_string("en_us");
    info.put_i8(view_distance);
    info.put_varint(0);
    info.put_bool(true);
    info.put_u8(0x7f);
    info.put_varint(1);
    info.put_bool(false);
    info.put_bool(true);
    info.put_varint(0);
    client.send("minecraft:client_information", info)?;
    loop {
        let (packet, body) = client.recv()?;
        match packet.as_str() {
            "minecraft:select_known_packs" => client.send("minecraft:select_known_packs", body)?,
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:finish_configuration" => break,
            "minecraft:disconnect" => bail!("disconnected during configuration"),
            _ => {}
        }
    }
    client.send("minecraft:finish_configuration", Vec::new())?;
    client.state = State::Play;
    client.set_read_timeout(Duration::from_millis(50))?;

    let mut names: HashMap<u128, String> = HashMap::new();
    let mut ents: HashMap<u128, Seen> = HashMap::new();
    let mut by_id: HashMap<i32, u128> = HashMap::new();
    let mut pose = Pose::default();
    let mut feet: Option<(f64, f64, f64)> = None;
    let mut spawned = false;
    let mut sprinting = false;
    let mut greeted = false;
    let mut tick = 0u32;
    let mut rising = 0u32;
    let mut sent_y = 0.0;
    let mut next_tick = Instant::now();
    let mut confirm: Option<Vec<u8>> = None;
    let mut report = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(seconds);

    while Instant::now() < deadline {
        if Instant::now() >= next_tick {
            if let Some(confirm) = confirm.take() {
                client.send("minecraft:accept_teleportation", confirm)?;
            }
            if spawned && let Some((x, y, z)) = feet {
                if !sprinting && client.entity_id != 0 {
                    let mut cmd = Vec::new();
                    cmd.put_varint(client.entity_id);
                    cmd.put_varint(1); // start sprinting
                    cmd.put_varint(0);
                    client.send("minecraft:player_command", cmd)?;
                    sprinting = true;
                }
                let target = follow_target(&names, &ents, name);
                if let Some((who, _)) = &target
                    && !greeted
                {
                    let mut chat = Vec::new();
                    chat.put_string("бегу за тобой");
                    client.send("minecraft:chat", chat)?;
                    println!("{name} is following {who}");
                    greeted = true;
                }
                let (nx, ny, nz, yaw, flags) = steer(
                    x,
                    y,
                    z,
                    &mut sent_y,
                    &mut tick,
                    &mut rising,
                    target.as_ref(),
                );
                feet = Some((nx, ny, nz));
                pose.yaw = yaw;
                let mut moved = Vec::new();
                moved.put_f64(nx);
                moved.put_f64(sent_y);
                moved.put_f64(nz);
                moved.put_f32(yaw);
                moved.put_f32(0.0);
                moved.put_u8(flags);
                client.send("minecraft:move_player_pos_rot", moved)?;
                client.send("minecraft:client_tick_end", Vec::new())?;
                if report.elapsed() >= Duration::from_secs(2) {
                    match &target {
                        Some((who, seen)) => {
                            let dx = seen.x - nx;
                            let dz = seen.z - nz;
                            println!(
                                "{name} at ({nx:.1}, {sent_y:.1}, {nz:.1}), {who} is {:.0} blocks away",
                                (dx * dx + dz * dz).sqrt()
                            );
                        }
                        None => println!("{name} is in, waiting for another player"),
                    }
                    report = Instant::now();
                }
            }
            next_tick = Instant::now() + Duration::from_millis(50);
        }
        let (packet, body) = match client.recv() {
            Ok(p) => p,
            Err(err) if is_timeout(&err) => continue,
            Err(err) => return Err(err),
        };
        match packet.as_str() {
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:login" => client.entity_id = Reader::new(&body).i32()?,
            "minecraft:player_position" => {
                confirm = Some(follow_teleport(&body, &mut pose)?);
                feet = Some((pose.x, pose.y, pose.z));
                sent_y = pose.y;
                rising = 0;
            }
            "minecraft:player_info_update" => {
                if let Err(err) = read_player_info(&body, &mut names) {
                    eprintln!("player list: {err:#}");
                }
            }
            "minecraft:add_entity" => {
                if let Ok((id, uuid, x, y, z)) = read_spawn(&body) {
                    by_id.insert(id, uuid);
                    ents.insert(uuid, Seen { x, y, z });
                }
            }
            "minecraft:entity_position_sync" => {
                if let Ok((id, x, y, z)) = read_sync(&body)
                    && let Some(uuid) = by_id.get(&id)
                    && let Some(seen) = ents.get_mut(uuid)
                {
                    (seen.x, seen.y, seen.z) = (x, y, z);
                }
            }
            "minecraft:remove_entities" => {
                if let Ok(ids) = read_remove(&body) {
                    for id in ids {
                        if let Some(uuid) = by_id.remove(&id) {
                            ents.remove(&uuid);
                        }
                    }
                }
            }
            "minecraft:chunk_batch_finished" => {
                let mut received = Vec::new();
                received.put_f32(1000.0);
                client.send("minecraft:chunk_batch_received", received)?;
                if spawned {
                    continue;
                }
                spawned = true;
                client.send("minecraft:player_loaded", Vec::new())?;
                println!("{name} spawned");
            }
            "minecraft:disconnect" => {
                let text: String = String::from_utf8_lossy(&body)
                    .chars()
                    .filter(|c| !c.is_control())
                    .collect();
                bail!("kicked: {text}")
            }
            _ => {}
        }
    }
    ensure!(spawned, "never received its chunks");
    Ok(())
}

#[derive(Debug)]
struct Seen {
    x: f64,
    y: f64,
    z: f64,
}

/// The other player we can see, by name.
fn follow_target<'a>(
    names: &'a HashMap<u128, String>,
    ents: &'a HashMap<u128, Seen>,
    me: &str,
) -> Option<(String, &'a Seen)> {
    let mut best: Option<(&str, u128)> = None;
    for (uuid, name) in names {
        if name == me {
            continue;
        }
        if best.as_ref().is_none_or(|(n, _)| name.as_str() < *n) {
            best = Some((name, *uuid));
        }
    }
    let (name, uuid) = best?;
    ents.get(&uuid).map(|seen| (name.to_owned(), seen))
}

/// Sprint toward `target`, hop once we're at their height, and drop for a
/// tick now and then while climbing so a long rise is not "flying".
fn steer(
    mut x: f64,
    mut y: f64,
    mut z: f64,
    sent_y: &mut f64,
    tick: &mut u32,
    rising: &mut u32,
    target: Option<&(String, &Seen)>,
) -> (f64, f64, f64, f32, u8) {
    *tick = tick.wrapping_add(1);
    let mut yaw = 0.0f32;
    let mut feet_gap = 0.0;
    if let Some((_, seen)) = target {
        let (dx, dz) = (seen.x - x, seen.z - z);
        let dist = (dx * dx + dz * dz).sqrt();
        feet_gap = (y - seen.y).abs();
        if dist > 3.0 {
            let speed = if dist > 20.0 { 0.72_f64 } else { 0.30 };
            let step = speed.min((dist - 2.4).max(0.0));
            if dist > 1e-4 {
                x += dx / dist * step;
                z += dz / dist * step;
                yaw = (-dx).atan2(dz).to_degrees() as f32;
            }
        } else {
            let ang = f64::from(*tick) * 0.11;
            let (ox, oz) = (seen.x + 2.5 * ang.cos(), seen.z + 2.5 * ang.sin());
            let (odx, odz) = (ox - x, oz - z);
            let od = (odx * odx + odz * odz).sqrt().max(1e-6);
            let step = 0.26_f64.min(od);
            x += odx / od * step;
            z += odz / od * step;
            yaw = (-odx).atan2(odz).to_degrees() as f32;
        }
        y += (seen.y - y).clamp(-0.5, 0.42);
    }
    let hop = if target.is_none() || feet_gap < 1.2 {
        hop_height(*tick)
    } else {
        0.0
    };
    let mut at = y + hop;
    if at - *sent_y >= -0.03125 {
        *rising += 1;
        if feet_gap > 1.5 && *rising >= 36 {
            at = *sent_y - 0.1;
            *rising = 0;
        }
    } else {
        *rising = 0;
    }
    let on_ground = hop == 0.0 && (at - y).abs() < 0.05 && feet_gap < 0.3;
    *sent_y = at;
    (x, y, z, yaw, u8::from(on_ground))
}

/// One vanilla-ish jump, then a short pause on the ground.
fn hop_height(tick: u32) -> f64 {
    const ARC: [f64; 12] = [
        0.42, 0.75, 1.00, 1.17, 1.25, 1.25, 1.17, 1.02, 0.80, 0.50, 0.20, 0.0,
    ];
    let t = tick % 22;
    if t < 12 { ARC[t as usize] } else { 0.0 }
}

fn read_player_info(body: &[u8], names: &mut HashMap<u128, String>) -> Result<()> {
    let mut r = Reader::new(body);
    let actions = r.u8()?;
    let count = r.length_prefix()?;
    for _ in 0..count {
        let uuid = r.uuid()?;
        if actions & 0x01 != 0 {
            let name = r.string(16)?;
            let props = r.length_prefix()?;
            for _ in 0..props {
                r.string(32767)?;
                r.string(32767)?;
                if r.bool()? {
                    r.string(32767)?;
                }
            }
            names.insert(uuid, name);
        }
        if actions & 0x02 != 0 {
            let _ = r.bool()?;
        }
        if actions & 0x04 != 0 {
            r.varint()?;
        }
        if actions & 0x08 != 0 {
            r.bool()?;
        }
        if actions & 0x10 != 0 {
            r.varint()?;
        }
        if actions & 0x20 != 0 && r.bool()? {
            r.nbt()?;
        }
        if actions & 0x40 != 0 {
            r.varint()?;
        }
        if actions & 0x80 != 0 {
            r.bool()?;
        }
    }
    Ok(())
}

fn read_spawn(body: &[u8]) -> Result<(i32, u128, f64, f64, f64)> {
    let mut r = Reader::new(body);
    let id = r.varint()?;
    let uuid = r.uuid()?;
    r.varint()?;
    Ok((id, uuid, r.f64()?, r.f64()?, r.f64()?))
}

fn read_sync(body: &[u8]) -> Result<(i32, f64, f64, f64)> {
    let mut r = Reader::new(body);
    let id = r.varint()?;
    r.varint()?;
    Ok((id, r.f64()?, r.f64()?, r.f64()?))
}

fn read_remove(body: &[u8]) -> Result<Vec<i32>> {
    let mut r = Reader::new(body);
    let n = r.length_prefix()?;
    let mut ids = Vec::with_capacity(n.min(64));
    for _ in 0..n {
        ids.push(r.varint()?);
    }
    Ok(ids)
}
