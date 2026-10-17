//! `cargo xtask watch`: joins a server and prints chosen packets (hex) for a
//! while, answering keep-alives and chunk batches like a client. For seeing
//! what vanilla sends over time, e.g. while running console commands.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mc_protocol::Write as _;

use crate::probe::{is_timeout, join_player, load_packet_ids, teleport_confirmation};

pub fn run(args: &[String]) -> Result<()> {
    let mut addr = "127.0.0.1:25565".to_owned();
    let mut version = None;
    let mut seconds = 5u64;
    let mut wanted: HashSet<String> = HashSet::new();
    let mut name = "Watcher".to_owned();
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
            "--seconds" => seconds = value()?.parse().context("--seconds")?,
            "--name" => name = value()?,
            "--packets" => {
                wanted.extend(value()?.split(',').map(|p| {
                    if p.contains(':') {
                        p.to_owned()
                    } else {
                        format!("minecraft:{p}")
                    }
                }));
            }
            other => bail!("unknown option `{other}` for watch"),
        }
    }
    let ids = load_packet_ids(version)?;
    let begin = Instant::now();
    let (mut client, _) = join_player(&addr, ids, &name)?;
    client.set_read_timeout(Duration::from_millis(100))?;
    while begin.elapsed() < Duration::from_secs(seconds) {
        let (packet, body) = match client.recv() {
            Ok(p) => p,
            Err(err) if is_timeout(&err) => continue,
            Err(err) => return Err(err),
        };
        match packet.as_str() {
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body.clone())?,
            "minecraft:player_position" => client.send(
                "minecraft:accept_teleportation",
                teleport_confirmation(&body)?,
            )?,
            "minecraft:chunk_batch_finished" => {
                let mut received = Vec::new();
                received.put_f32(64.0);
                client.send("minecraft:chunk_batch_received", received)?;
            }
            _ => {}
        }
        if wanted.is_empty() || wanted.contains(&packet) {
            let hex: Vec<String> = body.iter().take(64).map(|b| format!("{b:02x}")).collect();
            println!(
                "{:>6} ms {} ({} bytes) {}",
                begin.elapsed().as_millis(),
                packet.trim_start_matches("minecraft:"),
                body.len(),
                hex.join(" ")
            );
        }
    }
    Ok(())
}
