//! `cargo xtask chunk-speed`: how fast a server delivers a joining player's
//! chunks. Logs in, acknowledges every chunk batch at once (as a fast client
//! would) and times the chunks until none arrive for a while. Run against
//! vanilla and ours on the same, fully generated world.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mc_protocol::Write as _;

use crate::probe::{Pose, follow_teleport, is_timeout, join_player_with, load_packet_ids};

pub fn run(args: &[String]) -> Result<()> {
    let mut addr = "127.0.0.1:25565".to_owned();
    let mut version = None;
    let mut view_distance = 10i8;
    let mut runs = 3usize;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--addr" => addr = value()?,
            "--version" => version = Some(value()?),
            "--view-distance" => view_distance = value()?.parse().context("--view-distance")?,
            "--runs" => runs = value()?.parse().context("--runs")?,
            other => bail!("unknown option `{other}` for chunk-speed"),
        }
    }
    let ids = load_packet_ids(version)?;
    println!("{addr}, view distance {view_distance}");
    for run in 1..=runs {
        println!(
            "run {run}: {}",
            measure(&addr, ids.clone(), &format!("Speed{run}"), view_distance)?
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// Joins as `name` and times the chunks until none arrive for 3 s.
pub(crate) fn measure(
    addr: &str,
    ids: mc_protocol::PacketIds,
    name: &str,
    view_distance: i8,
) -> Result<String> {
    let begin = Instant::now();
    let (mut client, _) = join_player_with(addr, ids, name, view_distance)?;
    let joined = begin.elapsed();
    client.set_read_timeout(Duration::from_millis(100))?;
    let mut arrivals: Vec<Duration> = Vec::new();
    let mut bytes = 0usize;
    let mut seen = std::collections::HashSet::new();
    let mut forgotten = 0usize;
    let mut pose = Pose::default();
    let mut last = Instant::now();
    // The first chunk may take long (a new world); then quiet for 3 s ends it.
    while last.elapsed()
        < if arrivals.is_empty() {
            Duration::from_secs(300)
        } else {
            Duration::from_secs(30)
        }
    {
        let (packet, body) = match client.recv() {
            Ok(p) => p,
            Err(err) if is_timeout(&err) => continue,
            Err(err) => return Err(err),
        };
        match packet.as_str() {
            "minecraft:level_chunk_with_light" => {
                arrivals.push(begin.elapsed());
                bytes += body.len();
                if body.len() >= 8 {
                    let at = |i: usize| {
                        i32::from_be_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]])
                    };
                    seen.insert((at(0), at(4)));
                }
                last = Instant::now();
            }
            "minecraft:chunk_batch_finished" => {
                let mut received = Vec::new();
                received.put_f32(1000.0); // chunks per tick we could take
                client.send("minecraft:chunk_batch_received", received)?;
            }
            "minecraft:forget_level_chunk" => forgotten += 1,
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:player_position" => client.send(
                "minecraft:accept_teleportation",
                follow_teleport(&body, &mut pose)?,
            )?,
            _ => {}
        }
    }
    let Some(&all) = arrivals.last() else {
        bail!("no chunks");
    };
    let at = |fraction: f64| {
        let i = ((arrivals.len() as f64 * fraction).ceil() as usize).clamp(1, arrivals.len());
        arrivals[i - 1].as_millis()
    };
    // A view is a square: anything short of that is a chunk that never came.
    let side = 2 * i64::from(view_distance) + 1;
    let gaps = if seen.len() as i64 == side * side && forgotten == 0 {
        String::new()
    } else {
        format!(
            " [{} distinct of {}, {} forgotten]",
            seen.len(),
            side * side,
            forgotten
        )
    };
    Ok(format!(
        "login {} ms, {} chunks{gaps} ({:.1} MB) — first {} ms, 50% {} ms, 90% {} ms, all {} ms ({:.0} chunks/s)",
        joined.as_millis(),
        arrivals.len(),
        bytes as f64 / 1e6,
        arrivals[0].as_millis(),
        at(0.5),
        at(0.9),
        all.as_millis(),
        arrivals.len() as f64 / all.as_secs_f64()
    ))
}
