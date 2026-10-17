//! `cargo xtask registry-diff --a <addr> --b <addr>`: logs into two servers as a
//! client that does NOT know the vanilla data pack, so both send every
//! registry entry as NBT, and compares the entries. Meant for vanilla vs ours.
//!
//! Comparison is lenient where the client is: numbers compare by value whatever
//! their NBT type, and byte/int/long arrays equal lists of the same numbers.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use mc_nbt::Tag;
use mc_protocol::{PacketIds, Reader, State, Write as _};

use crate::probe::{Client, load_packet_ids};

type Registries = BTreeMap<String, Vec<(String, Option<Tag>)>>;

pub fn run(args: &[String]) -> Result<()> {
    let (mut a, mut b, mut version, mut details) = (None, None, None, 5usize);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--a" => a = Some(value()?),
            "--b" => b = Some(value()?),
            "--version" => version = Some(value()?),
            "--details" => details = value()?.parse()?,
            other => bail!("unknown option `{other}` for registry-diff"),
        }
    }
    let (a, b) = (a.context("--a is required")?, b.context("--b is required")?);
    let ids = load_packet_ids(version)?;
    let left = fetch(&a, ids.clone())?;
    let right = fetch(&b, ids)?;

    let mut total = (0, 0);
    for registry in left
        .keys()
        .chain(right.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let (Some(l), Some(r)) = (left.get(registry), right.get(registry)) else {
            println!(
                "{registry}: only on {}",
                if left.contains_key(registry) {
                    "a"
                } else {
                    "b"
                }
            );
            continue;
        };
        let l_map: BTreeMap<_, _> = l.iter().map(|(id, t)| (id.clone(), t.clone())).collect();
        let r_map: BTreeMap<_, _> = r.iter().map(|(id, t)| (id.clone(), t.clone())).collect();
        let order_same = l.iter().map(|(id, _)| id).eq(r.iter().map(|(id, _)| id));
        let mut differing = Vec::new();
        let mut same = 0;
        for (id, l_tag) in &l_map {
            match r_map.get(id) {
                None => differing.push(format!("{id}: only on a")),
                Some(r_tag) => {
                    let mut diffs = Vec::new();
                    compare(l_tag.as_ref(), r_tag.as_ref(), "", &mut diffs);
                    if diffs.is_empty() {
                        same += 1;
                    } else {
                        differing.push(format!("{id}: {}", diffs.join("; ")));
                    }
                }
            }
        }
        for id in r_map.keys().filter(|id| !l_map.contains_key(*id)) {
            differing.push(format!("{id}: only on b"));
        }
        total.0 += same;
        total.1 += differing.len();
        let order = if order_same {
            ""
        } else {
            " (entry order differs)"
        };
        println!("{registry}: {same} same, {} differ{order}", differing.len());
        for line in differing.iter().take(details) {
            println!("    {}", truncate(line, 400));
        }
    }
    println!("total: {} entries same, {} differ", total.0, total.1);
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Logs in (offline mode) and collects Registry Data, refusing known packs.
fn fetch(addr: &str, ids: PacketIds) -> Result<Registries> {
    let mut client = Client::connect(addr, ids)?;
    client.handshake(2)?;
    let mut hello = Vec::new();
    hello.put_string("RegistryDiff");
    hello.put_uuid(0);
    client.send("minecraft:hello", hello)?;
    loop {
        let (packet, body) = client.recv()?;
        match packet.as_str() {
            "minecraft:login_compression" => {
                client.compression = usize::try_from(Reader::new(&body).varint()?).ok();
            }
            "minecraft:login_finished" => break,
            "minecraft:hello" => bail!("{addr} is in online mode; run it with offline mode"),
            "minecraft:login_disconnect" => bail!("refused: {}", Reader::new(&body).string(32767)?),
            _ => {}
        }
    }
    client.send("minecraft:login_acknowledged", Vec::new())?;
    client.state = State::Configuration;

    let mut registries = Registries::new();
    loop {
        let (packet, body) = client.recv()?;
        let mut r = Reader::new(&body);
        match packet.as_str() {
            // We know no packs: the server must send everything as NBT.
            "minecraft:select_known_packs" => {
                let mut none = Vec::new();
                none.put_varint(0);
                client.send("minecraft:select_known_packs", none)?;
            }
            "minecraft:registry_data" => {
                let registry = r.identifier()?;
                let count = r.length_prefix()?;
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let id = r.identifier()?;
                    let data = if r.bool()? { Some(r.nbt()?) } else { None };
                    entries.push((id, data));
                }
                registries.insert(registry, entries);
            }
            "minecraft:keep_alive" => client.send("minecraft:keep_alive", body)?,
            "minecraft:finish_configuration" => return Ok(registries),
            "minecraft:disconnect" => bail!("{addr} disconnected us during configuration"),
            _ => {}
        }
    }
}

fn number(tag: &Tag) -> Option<f64> {
    Some(match *tag {
        Tag::Byte(v) => f64::from(v),
        Tag::Short(v) => f64::from(v),
        Tag::Int(v) => f64::from(v),
        Tag::Long(v) => v as f64,
        Tag::Float(v) => f64::from(v),
        Tag::Double(v) => v,
        _ => return None,
    })
}

/// Arrays and lists as one sequence of tags.
fn sequence(tag: &Tag) -> Option<Vec<Tag>> {
    Some(match tag {
        Tag::List(items) => items.clone(),
        Tag::ByteArray(v) => v.iter().map(|&x| Tag::Byte(x)).collect(),
        Tag::IntArray(v) => v.iter().map(|&x| Tag::Int(x)).collect(),
        Tag::LongArray(v) => v.iter().map(|&x| Tag::Long(x)).collect(),
        _ => return None,
    })
}

fn describe(tag: Option<&Tag>) -> String {
    match tag {
        None => "nothing".into(),
        Some(Tag::String(s)) => format!("{s:?}"),
        Some(Tag::Compound(c)) => format!("{{{} keys}}", c.len()),
        Some(other) => match number(other) {
            Some(n) => format!("{n}"),
            None => format!("{other:?}").chars().take(80).collect(),
        },
    }
}

fn compare(a: Option<&Tag>, b: Option<&Tag>, path: &str, diffs: &mut Vec<String>) {
    let here = if path.is_empty() { "<root>" } else { path };
    match (a, b) {
        (None, None) => {}
        (Some(a), Some(b)) => {
            if let (Some(x), Some(y)) = (number(a), number(b)) {
                let tolerance = 1e-6 * x.abs().max(y.abs()).max(1.0);
                if (x - y).abs() > tolerance {
                    diffs.push(format!("{here}: {x} vs {y}"));
                }
                return;
            }
            if let (Some(x), Some(y)) = (sequence(a), sequence(b)) {
                if x.len() != y.len() {
                    diffs.push(format!("{here}: {} vs {} elements", x.len(), y.len()));
                    return;
                }
                for (i, (x, y)) in x.iter().zip(&y).enumerate() {
                    compare(Some(x), Some(y), &format!("{path}[{i}]"), diffs);
                }
                return;
            }
            match (a, b) {
                (Tag::Compound(x), Tag::Compound(y)) => {
                    let mut keys: Vec<&str> = x.iter().map(|(k, _)| k).collect();
                    keys.extend(y.iter().map(|(k, _)| k).filter(|k| x.get(k).is_none()));
                    for key in keys {
                        let sub = if path.is_empty() {
                            key.to_owned()
                        } else {
                            format!("{path}.{key}")
                        };
                        compare(x.get(key), y.get(key), &sub, diffs);
                    }
                }
                (Tag::String(x), Tag::String(y)) if x == y => {}
                _ => diffs.push(format!(
                    "{here}: {} vs {}",
                    describe(Some(a)),
                    describe(Some(b))
                )),
            }
        }
        (a, b) => diffs.push(format!("{here}: {} vs {}", describe(a), describe(b))),
    }
}
