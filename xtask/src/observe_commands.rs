//! `cargo xtask observe-commands [--lines <file>]`: what a local vanilla
//! server tells an operator about commands, for our server to match.
//!
//! - the Commands packet (the tree the client completes against, with the
//!   suggestion types the data report leaves out), decoded and raw;
//! - for each line of `--lines` (one command per line, `#` comments; by
//!   default `xtask/commands.txt`): `> text` asks for suggestions, any other
//!   line runs the command; the replies are recorded as text components;
//! - with `--packets a,b`, the bodies of those packets too (hex; a boss
//!   bar's random UUID left out).
//!
//! Output: `cache/mcdata/26.3/observed/commands.json` (+ `commands.bin`).

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use mc_nbt::Tag;
use mc_protocol::{Reader, Write as _};
use serde_json::{Value, json};

use crate::parity::Server;
use crate::probe::{Client, is_timeout, join_player, load_packet_ids};

const PORT: u16 = 25576;
const NAME: &str = "Probe";

pub fn run(args: &[String]) -> Result<()> {
    let root = crate::workspace_root();
    let mut lines_file = root.join("xtask/commands.txt");
    let mut ours = false;
    // `--area name`: its own server folders, port and output files, so
    // several areas can be observed side by side.
    let mut area: Option<String> = None;
    let mut port = PORT;
    let mut packets: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--lines" => lines_file = it.next().context("--lines needs a file")?.into(),
            "--ours" => ours = true,
            "--area" => area = Some(it.next().context("--area needs a name")?.clone()),
            "--port" => port = it.next().context("--port needs a number")?.parse()?,
            "--packets" => {
                packets = it
                    .next()
                    .context("--packets needs names")?
                    .split(',')
                    .map(|n| {
                        if n.contains(':') {
                            n.to_owned()
                        } else {
                            format!("minecraft:{n}")
                        }
                    })
                    .collect();
            }
            other => bail!("unknown option `{other}`"),
        }
    }
    let lines: Vec<String> = std::fs::read_to_string(&lines_file)
        .with_context(|| format!("reading {}", lines_file.display()))?
        .lines()
        // Trailing spaces matter in suggestion requests.
        .map(str::trim_start)
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let suffix = area.as_ref().map_or(String::new(), |a| format!("-{a}"));
    let dir = root.join(format!("cache/vanilla-server/26.3-commands{suffix}"));
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={port}\nlevel-name=world\n\
         level-type=minecraft\\:flat\ngamemode=creative\nspawn-protection=0\n\
         view-distance=2\nsimulation-distance=2\ngenerate-structures=false\n\
         pause-when-empty-seconds=0\nenable-status=true\nsync-chunk-writes=false\n\
         level-seed=1\nwhite-list=false\nenforce-whitelist=false\n"
    );
    if ours {
        let dir = root.join(format!("cache/ours-commands{suffix}"));
        let mut server = Server::ours_in(&root, &dir, &properties)?;
        let result = observe(
            &mut server,
            port,
            &lines,
            &dir.join("world/data/minecraft/game_rules.dat"),
            &packets,
        );
        server.stop();
        let (tree, _, replies, _) = result?;
        let out = root.join(format!(
            "cache/mcdata/26.3/observed/commands{suffix}-ours.json"
        ));
        std::fs::write(
            &out,
            serde_json::to_string_pretty(&json!({ "tree": tree, "lines": replies }))?,
        )?;
        println!("{} lines → {}", replies.len(), out.display());
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(dir.join("world"));
    // Made operator after joining, so the tree is sent again for it.
    let _ = std::fs::remove_file(dir.join("ops.json"));
    Server::prepare(&root, &dir, &properties)?;
    let mut server = Server::start_java(&root, &dir)?;
    let result = observe(
        &mut server,
        port,
        &lines,
        &dir.join("world/data/minecraft/game_rules.dat"),
        &packets,
    );
    server.stop();
    let (tree, raw, replies, rules) = result?;
    let out = root.join("cache/mcdata/26.3/observed");
    std::fs::create_dir_all(&out)?;
    if area.is_some() {
        let file = out.join(format!("commands{suffix}.json"));
        std::fs::write(
            &file,
            serde_json::to_string_pretty(&json!({ "lines": replies }))?,
        )?;
        println!("{} lines → {}", replies.len(), file.display());
        return Ok(());
    }
    std::fs::write(out.join("commands.bin"), &raw)?;
    std::fs::write(
        out.join("commands.json"),
        serde_json::to_string_pretty(
            &json!({ "tree": tree, "lines": replies, "game_rules": rules }),
        )?,
    )?;
    println!(
        "{} nodes, {} lines → {}",
        tree["nodes"].as_array().map_or(0, Vec::len),
        replies.len(),
        out.join("commands.json").display()
    );
    Ok(())
}

type Observed = (Value, Vec<u8>, Vec<Value>, Value);

fn observe(
    server: &mut Server,
    port: u16,
    lines: &[String],
    rules_file: &std::path::Path,
    packets: &[String],
) -> Result<Observed> {
    let ids = load_packet_ids(None)?;
    let (mut client, _) = join_player(&format!("127.0.0.1:{port}"), ids, NAME)?;
    client.set_read_timeout(Duration::from_millis(100))?;
    server.wait_for(&format!("op {NAME}"), "operator", Duration::from_secs(10))?;
    // The tree is sent again for the new permission level.
    let mut raw = None;
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if let Some((name, body)) = next(&mut client)?
            && name == "minecraft:commands"
        {
            raw = Some(body);
        }
    }
    let raw = raw.context("no commands packet after op")?;
    let tree = decode_tree(&raw)?;
    // The game rules as a new world has them, before any line changes one.
    server.wait_for("save-all flush", "Saved the game", Duration::from_secs(30))?;
    let rules = game_rules(rules_file).unwrap_or(Value::Null);
    // What the save said to operators is not a reply to the first line.
    let until = Instant::now() + Duration::from_millis(800);
    while Instant::now() < until {
        next(&mut client)?;
    }
    let mut replies = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let mut entry = json!({ "line": line });
        if let Some(text) = line.strip_prefix('>') {
            let mut body = Vec::new();
            body.put_varint(n as i32 + 1);
            body.put_string(text.trim_start());
            client.send("minecraft:command_suggestion", body)?;
        } else {
            let mut body = Vec::new();
            body.put_string(line.trim_start_matches('/'));
            client.send("minecraft:chat_command", body)?;
        }
        let mut got = Vec::new();
        let until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < until {
            let Some((name, body)) = next(&mut client)? else {
                continue;
            };
            let mut r = Reader::new(&body);
            match name.as_str() {
                "minecraft:system_chat" => {
                    let content = component(&r.nbt()?);
                    got.push(json!({ "chat": content, "overlay": r.bool()? }));
                }
                "minecraft:command_suggestions" => {
                    let _id = r.varint()?;
                    let (start, length) = (r.varint()?, r.varint()?);
                    let count = r.varint()?;
                    let mut matches = Vec::new();
                    for _ in 0..count {
                        let text = r.string(32767)?;
                        let tooltip = if r.bool()? {
                            component(&r.nbt()?)
                        } else {
                            Value::Null
                        };
                        matches.push(json!({ "text": text, "tooltip": tooltip }));
                    }
                    got.push(json!({ "start": start, "length": length, "matches": matches }));
                }
                "minecraft:disconnect" => bail!("disconnected after `{line}`"),
                n if packets.iter().any(|p| p == n) => {
                    // A boss bar's UUID is random.
                    let skip = if n == "minecraft:boss_event" { 16 } else { 0 };
                    let hex: String = body.iter().skip(skip).map(|b| format!("{b:02x}")).collect();
                    got.push(json!({ "packet": n, "body": hex }));
                }
                _ => {}
            }
        }
        entry["replies"] = Value::Array(got);
        replies.push(entry);
    }
    Ok((tree, raw, replies, rules))
}

/// `game_rules.dat` as `{name: value}`.
fn game_rules(path: &std::path::Path) -> Result<Value> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path)?).read_to_end(&mut bytes)?;
    let (_, tag, _) = mc_nbt::read_named(&bytes)?;
    let Tag::Compound(root) = tag else {
        bail!("game_rules.dat is not a compound");
    };
    Ok(match root.get("data") {
        Some(data) => component(data),
        None => Value::Null,
    })
}

/// The next packet, answering keep-alives; `None` when nothing came.
fn next(client: &mut Client) -> Result<Option<(String, Vec<u8>)>> {
    match client.recv() {
        Ok((name, body)) => {
            if name == "minecraft:keep_alive" {
                client.send("minecraft:keep_alive", body.clone())?;
            }
            Ok(Some((name, body)))
        }
        Err(e) if is_timeout(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A text component (NBT) as JSON.
pub(crate) fn component(tag: &Tag) -> Value {
    match tag {
        Tag::String(s) => Value::String(s.clone()),
        Tag::Byte(b) => json!(b),
        Tag::Short(v) => json!(v),
        Tag::Int(v) => json!(v),
        Tag::Long(v) => json!(v),
        Tag::Float(v) => json!(v),
        Tag::Double(v) => json!(v),
        Tag::List(l) => Value::Array(l.iter().map(component).collect()),
        Tag::Compound(c) => Value::Object(
            c.iter()
                .map(|(k, v)| (k.to_owned(), component(v)))
                .collect(),
        ),
        Tag::ByteArray(a) => json!(a),
        Tag::IntArray(a) => json!(a),
        Tag::LongArray(a) => json!(a),
    }
}

/// The Commands packet as JSON: nodes in packet order and the root index.
pub(crate) fn decode_tree(body: &[u8]) -> Result<Value> {
    let types = argument_types()?;
    let mut r = Reader::new(body);
    let count = r.varint()?;
    let mut nodes = Vec::new();
    for _ in 0..count {
        let flags = r.u8()?;
        let children: Vec<i32> = (0..r.varint()?)
            .map(|_| r.varint())
            .collect::<Result<_, _>>()?;
        let mut node = json!({ "flags": flags, "children": children });
        if flags & 0x08 != 0 {
            node["redirect"] = json!(r.varint()?);
        }
        let kind = flags & 0x03;
        if kind == 1 || kind == 2 {
            node["name"] = json!(r.string(32767)?);
        }
        if kind == 2 {
            let id = r.varint()?;
            let parser = types
                .get(id as usize)
                .cloned()
                .with_context(|| format!("argument type {id}"))?;
            node["properties"] = properties(&parser, &mut r)?;
            node["parser"] = json!(parser);
        }
        if flags & 0x10 != 0 {
            node["suggestions"] = json!(r.identifier()?);
        }
        nodes.push(node);
    }
    let root = r.varint()?;
    if r.remaining() != 0 {
        bail!("commands packet: {} bytes left", r.remaining());
    }
    Ok(json!({ "root": root, "nodes": nodes }))
}

/// Argument type names by network id.
fn argument_types() -> Result<Vec<String>> {
    let root = crate::workspace_root();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(
        root.join("cache/mcdata/26.3/reports/registries.json"),
    )?)?;
    let entries = v["minecraft:command_argument_type"]["entries"]
        .as_object()
        .context("no command_argument_type registry")?;
    let mut out = vec![String::new(); entries.len()];
    for (name, e) in entries {
        let id = e["protocol_id"].as_u64().unwrap_or(0) as usize;
        if id < out.len() {
            out[id] = name.clone();
        }
    }
    Ok(out)
}

/// An argument type's properties on the wire.
fn properties(parser: &str, r: &mut Reader) -> Result<Value> {
    Ok(match parser {
        "brigadier:float" | "brigadier:double" | "brigadier:integer" | "brigadier:long" => {
            let flags = r.u8()?;
            let read = |r: &mut Reader| -> Result<Value> {
                Ok(match parser {
                    "brigadier:float" => json!(r.f32()?),
                    "brigadier:double" => json!(r.f64()?),
                    "brigadier:integer" => json!(r.i32()?),
                    _ => json!(r.i64()?),
                })
            };
            let min = if flags & 1 != 0 {
                read(r)?
            } else {
                Value::Null
            };
            let max = if flags & 2 != 0 {
                read(r)?
            } else {
                Value::Null
            };
            json!({ "min": min, "max": max })
        }
        "brigadier:string" => json!({ "type": r.varint()? }),
        "minecraft:entity" | "minecraft:score_holder" => json!({ "flags": r.u8()? }),
        "minecraft:time" => json!({ "min": r.i32()? }),
        "minecraft:resource_or_tag"
        | "minecraft:resource_or_tag_key"
        | "minecraft:resource"
        | "minecraft:resource_key"
        | "minecraft:resource_selector" => json!({ "registry": r.identifier()? }),
        _ => Value::Null,
    })
}
