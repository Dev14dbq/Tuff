//! `cargo xtask blocks`: two players on an offline-mode creative server with a
//! superflat world. The builder places and breaks blocks, and we print what
//! each player receives after every action. Run it against vanilla and against
//! ours to compare block interaction on the wire.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mc_data::McData;
use mc_protocol::{Position, Reader, Write as _};

use crate::probe::{Client, is_timeout, join_player, load_data, packet_ids, teleport_confirmation};

/// Packets that say nothing about blocks.
const QUIET: &[&str] = &[
    "minecraft:keep_alive",
    "minecraft:move_entity_pos",
    "minecraft:move_entity_pos_rot",
    "minecraft:move_entity_rot",
    "minecraft:entity_position_sync",
    "minecraft:rotate_head",
    "minecraft:set_time",
    "minecraft:bundle_delimiter",
    "minecraft:set_entity_motion",
    "minecraft:remove_entities",
    "minecraft:add_entity",
    "minecraft:set_entity_data",
    "minecraft:update_attributes",
];

pub fn run(args: &[String]) -> Result<()> {
    let mut addr = "127.0.0.1:25565".to_owned();
    let mut version = None;
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
            other => bail!("unknown option `{other}` for blocks"),
        }
    }
    let data = load_data(version)?;
    let ids = packet_ids(&data);

    let (mut watcher, _) = join_player(&addr, ids.clone(), "Watcher")?;
    settle(&mut watcher)?;
    let (mut builder, (bx, by, bz)) = join_player(&addr, ids, "Builder")?;
    settle(&mut builder)?;
    builder.send("minecraft:player_loaded", Vec::new())?;
    watcher.send("minecraft:player_loaded", Vec::new())?;
    collect(
        &mut builder,
        &mut watcher,
        &data,
        Duration::from_millis(800),
        true,
    )?;

    let (x, ground, z) = (bx.floor() as i32, by.floor() as i32 - 1, bz.floor() as i32);
    println!("builder at {bx:.2} {by:.2} {bz:.2}, ground block y {ground}");
    let mut seq = 0;
    let mut next_seq = || {
        seq += 1;
        seq
    };
    let step = |title: &str,
                builder: &mut Client,
                watcher: &mut Client,
                act: &mut dyn FnMut(&mut Client) -> Result<()>|
     -> Result<()> {
        println!("\n== {title}");
        act(builder)?;
        collect(builder, watcher, &data, Duration::from_millis(700), false)
    };
    let item = |name: &str| -> Result<i32> {
        data.builtin
            .id("minecraft:item", name)
            .with_context(|| format!("no item {name}"))
    };

    let stone = item("minecraft:stone")?;
    step("hold stone", &mut builder, &mut watcher, &mut |c| {
        hold(c, stone)
    })?;
    let s = next_seq();
    step(
        "place stone on the grass east of us (top face)",
        &mut builder,
        &mut watcher,
        &mut |c| use_item_on(c, (x + 2, ground, z), 1, [0.5, 1.0, 0.5], s),
    )?;
    step("punch", &mut builder, &mut watcher, &mut |c| {
        c.send("minecraft:punch", Vec::new())
    })?;
    let s = next_seq();
    step(
        "break it (creative: start digging)",
        &mut builder,
        &mut watcher,
        &mut |c| player_action(c, 0, (x + 2, ground + 1, z), 1, s),
    )?;
    let s = next_seq();
    step("place stone again", &mut builder, &mut watcher, &mut |c| {
        use_item_on(c, (x + 2, ground, z), 1, [0.5, 1.0, 0.5], s)
    })?;
    let log = item("minecraft:oak_log")?;
    let s = next_seq();
    step(
        "oak log on the stone's east face",
        &mut builder,
        &mut watcher,
        &mut |c| {
            hold(c, log)?;
            use_item_on(c, (x + 2, ground + 1, z), 5, [1.0, 0.5, 0.5], s)
        },
    )?;
    let stairs = item("minecraft:oak_stairs")?;
    let s = next_seq();
    step(
        "look west (yaw 90), oak stairs on top of the stone",
        &mut builder,
        &mut watcher,
        &mut |c| {
            let mut rot = Vec::new();
            rot.put_f32(90.0);
            rot.put_f32(30.0);
            rot.put_u8(0x01);
            c.send("minecraft:move_player_rot", rot)?;
            hold(c, stairs)?;
            use_item_on(c, (x + 2, ground + 1, z), 1, [0.5, 1.0, 0.5], s)
        },
    )?;
    let slab = item("minecraft:stone_slab")?;
    let s = next_seq();
    step(
        "stone slab on the upper half of the log's east face",
        &mut builder,
        &mut watcher,
        &mut |c| {
            hold(c, slab)?;
            use_item_on(c, (x + 3, ground + 1, z), 5, [1.0, 0.8, 0.5], s)
        },
    )?;
    let s = next_seq();
    step(
        "stone where we stand (should be refused)",
        &mut builder,
        &mut watcher,
        &mut |c| {
            hold(c, stone)?;
            use_item_on(c, (x, ground, z), 1, [0.5, 1.0, 0.5], s)
        },
    )?;
    let s = next_seq();
    step(
        "stone 30 blocks away (should be refused)",
        &mut builder,
        &mut watcher,
        &mut |c| use_item_on(c, (x + 30, ground, z), 1, [0.5, 1.0, 0.5], s),
    )?;
    let s = next_seq();
    step(
        "break grass 30 blocks away (should be refused)",
        &mut builder,
        &mut watcher,
        &mut |c| player_action(c, 0, (x + 30, ground, z), 1, s),
    )?;
    let s = next_seq();
    step(
        "use item in the air",
        &mut builder,
        &mut watcher,
        &mut |c| {
            let mut body = Vec::new();
            body.put_varint(0);
            body.put_varint(s);
            body.put_f32(90.0);
            body.put_f32(30.0);
            c.send("minecraft:use_item", body)
        },
    )?;
    Ok(())
}

/// Reads until the first chunk batch is done and no chunk came for a second.
fn settle(client: &mut Client) -> Result<()> {
    client.set_read_timeout(Duration::from_millis(100))?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut batches = 0;
    let mut last_chunk = Instant::now();
    while Instant::now() < deadline {
        if batches > 0 && last_chunk.elapsed() > Duration::from_secs(1) {
            return Ok(());
        }
        match client.recv() {
            Ok((packet, body)) => match packet.as_str() {
                "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
                "minecraft:player_position" => client.send(
                    "minecraft:accept_teleportation",
                    teleport_confirmation(&body)?,
                )?,
                "minecraft:level_chunk_with_light" => last_chunk = Instant::now(),
                "minecraft:chunk_batch_finished" => {
                    batches += 1;
                    last_chunk = Instant::now();
                    let mut received = Vec::new();
                    received.put_f32(64.0);
                    client.send("minecraft:chunk_batch_received", received)?;
                }
                _ => {}
            },
            Err(err) if is_timeout(&err) => {}
            Err(err) => return Err(err),
        }
    }
    bail!("chunks did not settle within 20 s")
}

/// Polls both players for `time`, printing what each one gets.
fn collect(
    builder: &mut Client,
    watcher: &mut Client,
    data: &McData,
    time: Duration,
    quiet: bool,
) -> Result<()> {
    builder.set_read_timeout(Duration::from_millis(10))?;
    watcher.set_read_timeout(Duration::from_millis(10))?;
    let until = Instant::now() + time;
    while Instant::now() < until {
        for (who, client) in [("builder", &mut *builder), ("watcher", &mut *watcher)] {
            loop {
                let (packet, body) = match client.recv() {
                    Ok(p) => p,
                    Err(err) if is_timeout(&err) => break,
                    Err(err) => return Err(err),
                };
                match packet.as_str() {
                    "minecraft:keep_alive" => client.send("minecraft:keep_alive", body.clone())?,
                    "minecraft:player_position" => client.send(
                        "minecraft:accept_teleportation",
                        teleport_confirmation(&body)?,
                    )?,
                    _ => {}
                }
                if quiet || QUIET.contains(&packet.as_str()) {
                    continue;
                }
                let summary = describe(&packet, &body, data)
                    .unwrap_or_else(|err| format!("(cannot parse: {err:#})"));
                println!(
                    "  {who}: {} {summary}",
                    packet.trim_start_matches("minecraft:")
                );
            }
        }
    }
    Ok(())
}

fn state_name(data: &McData, state: i32) -> String {
    u32::try_from(state)
        .ok()
        .and_then(|s| data.blocks.describe(s))
        .map_or_else(|| format!("state {state}"), str::to_owned)
}

fn describe(packet: &str, body: &[u8], data: &McData) -> Result<String> {
    let mut r = Reader::new(body);
    Ok(match packet {
        "minecraft:block_update" => {
            let pos = r.position()?;
            let state = r.varint()?;
            format!("{pos:?} {}", state_name(data, state))
        }
        "minecraft:block_changed_ack" => format!("sequence {}", r.varint()?),
        "minecraft:level_event" => {
            let (event, pos, value, global) = (r.i32()?, r.position()?, r.i32()?, r.bool()?);
            let value = if event == 2001 {
                state_name(data, value)
            } else {
                value.to_string()
            };
            format!("event {event} at {pos:?} data {value} global {global}")
        }
        "minecraft:swing_animation" => format!(
            "entity {} hand {} type {} duration {}",
            r.varint()?,
            r.varint()?,
            r.varint()?,
            r.varint()?
        ),
        "minecraft:animate" => format!("entity {} animation {}", r.varint()?, r.u8()?),
        "minecraft:block_destruction" => format!(
            "entity {} at {:?} stage {}",
            r.varint()?,
            r.position()?,
            r.u8()?
        ),
        "minecraft:sound" => {
            let id = r.varint()?;
            let sound = if id == 0 {
                format!("inline {}", r.identifier()?)
            } else {
                data.builtin
                    .get("minecraft:sound_event")
                    .and_then(|ids| ids.iter().find(|(_, v)| **v == id - 1))
                    .map_or_else(|| format!("sound {}", id - 1), |(k, _)| k.clone())
            };
            let category = r.varint()?;
            let (x, y, z) = (r.i32()?, r.i32()?, r.i32()?);
            let (volume, pitch, seed) = (r.f32()?, r.f32()?, r.i64()?);
            format!(
                "{sound} category {category} at {} {} {} volume {volume} pitch {pitch} seed {seed:#x} ({} left)",
                f64::from(x) / 8.0,
                f64::from(y) / 8.0,
                f64::from(z) / 8.0,
                r.remaining()
            )
        }
        "minecraft:set_equipment" => {
            let entity = r.varint()?;
            format!("entity {entity} {:02x?}", r.rest())
        }
        "minecraft:container_set_slot" | "minecraft:set_player_inventory" => {
            format!("{} bytes: {:02x?}", body.len(), &body[..body.len().min(24)])
        }
        _ => format!("{} bytes", body.len()),
    })
}

fn hold(client: &mut Client, item: i32) -> Result<()> {
    let mut slot = Vec::new();
    slot.put_i16(36); // first hotbar slot
    slot.put_varint(1); // count
    slot.put_varint(item);
    slot.put_varint(0); // components added
    slot.put_varint(0); // components removed
    client.send("minecraft:set_creative_mode_slot", slot)?;
    let mut carried = Vec::new();
    carried.put_i16(0);
    client.send("minecraft:set_carried_item", carried)
}

fn use_item_on(
    client: &mut Client,
    (x, y, z): (i32, i32, i32),
    face: i32,
    cursor: [f32; 3],
    sequence: i32,
) -> Result<()> {
    let mut body = Vec::new();
    body.put_varint(0); // main hand
    body.put_position(Position::new(x, y, z));
    body.put_varint(face);
    for c in cursor {
        body.put_f32(c);
    }
    body.put_bool(false); // head inside a block
    body.put_bool(false); // world border hit
    body.put_varint(sequence);
    client.send("minecraft:use_item_on", body)
}

fn player_action(
    client: &mut Client,
    status: i32,
    (x, y, z): (i32, i32, i32),
    face: i8,
    sequence: i32,
) -> Result<()> {
    let mut body = Vec::new();
    body.put_varint(status);
    body.put_position(Position::new(x, y, z));
    body.put_i8(face);
    body.put_varint(sequence);
    client.send("minecraft:player_action", body)
}
