//! `cargo xtask bots`: load test. Starts N bots (offline-mode names), each logs
//! in, waits for its chunks, then walks in a circle sending movement like a
//! client, answering keep-alives. Reports joins, failures and traffic.

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
            other => bail!("unknown option `{other}` for bots"),
        }
    }
    let ids = load_packet_ids(version)?;
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
