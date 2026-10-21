//! `cargo xtask hash-check --lines <file>`: whether our item component hashes
//! are the client's. For every line (`<item>[<components>]`) a local vanilla
//! server puts the stack in the hotbar; we read it back from Set Container
//! Slot, pick it up with a window click that describes it by our hashes, and
//! see whether vanilla corrects the cursor (it does when a hash differs).

use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use mc_protocol::components::{HashedStack, ItemStack};
use mc_protocol::{Reader, Write as _};

use crate::parity::Server;
use crate::probe::{Client, is_timeout, join_player, load_data, packet_ids};

const PORT: u16 = 25593;
const NAME: &str = "Probe";

pub fn run(args: &[String]) -> Result<()> {
    let root = crate::workspace_root();
    let mut it = args.iter();
    let mut lines_file = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--lines" => lines_file = Some(it.next().context("--lines needs a file")?.clone()),
            other => bail!("unknown option `{other}`"),
        }
    }
    let lines: Vec<String> = std::fs::read_to_string(lines_file.context("--lines <file>")?)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let data = load_data(None)?;
    let ids = data.network_ids()?;
    mc_protocol::components::install_registries(ids.iter().map(|(k, v)| (k.as_str(), v)));
    let dir = root.join("cache/vanilla-server/26.3-hashes");
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-type=minecraft\\:flat\ngamemode=creative\nspawn-protection=0\n\
         view-distance=2\nsimulation-distance=2\ngenerate-structures=false\n\
         pause-when-empty-seconds=0\nsync-chunk-writes=false\nwhite-list=false\nenforce-whitelist=false\n"
    );
    let _ = std::fs::remove_dir_all(dir.join("world"));
    let _ = std::fs::remove_file(dir.join("ops.json"));
    Server::prepare(&root, &dir, &properties)?;
    let mut server = Server::start_java(&root, &dir)?;
    let result = check(&mut server, &lines, packet_ids(&data));
    server.stop();
    let (good, bad) = result?;
    println!("{good} match, {bad} differ");
    Ok(())
}

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

fn hashed(out: &mut Vec<u8>, stack: Option<&HashedStack>) {
    out.put_bool(stack.is_some());
    if let Some(h) = stack {
        out.put_varint(h.item);
        out.put_varint(h.count);
        out.put_varint(h.added.len() as i32);
        for &(t, v) in &h.added {
            out.put_varint(t);
            out.put_i32(v);
        }
        out.put_varint(h.removed.len() as i32);
        for &t in &h.removed {
            out.put_varint(t);
        }
    }
}

fn click(
    client: &mut Client,
    state: i32,
    slot: i16,
    changed: &[(i16, Option<&HashedStack>)],
    carried: Option<&HashedStack>,
) -> Result<()> {
    let mut b = Vec::new();
    b.put_varint(0);
    b.put_varint(state);
    b.put_i16(slot);
    b.put_i8(0);
    b.put_varint(0);
    b.put_varint(changed.len() as i32);
    for (s, h) in changed {
        b.put_i16(*s);
        hashed(&mut b, *h);
    }
    hashed(&mut b, carried);
    client.send("minecraft:container_click", b)
}

fn check(server: &mut Server, lines: &[String], ids: mc_protocol::PacketIds) -> Result<(u32, u32)> {
    let (mut client, _) = join_player(&format!("127.0.0.1:{PORT}"), ids, NAME)?;
    client.set_read_timeout(Duration::from_millis(100))?;
    server.wait_for(&format!("op {NAME}"), "operator", Duration::from_secs(10))?;
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        next(&mut client)?;
    }
    let (mut good, mut bad) = (0, 0);
    for line in lines {
        // `!` first: a wrong hash on purpose (vanilla must correct it).
        let (line, spoil) = match line.strip_prefix('!') {
            Some(l) => (l, true),
            None => (line.as_str(), false),
        };
        let mut body = Vec::new();
        body.put_string(&format!("item replace entity {NAME} hotbar.0 with {line}"));
        client.send("minecraft:chat_command", body)?;
        let mut got = None;
        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            if let Some((name, body)) = next(&mut client)?
                && name == "minecraft:container_set_slot"
            {
                let mut r = Reader::new(&body);
                let (_window, state, slot) = (r.varint()?, r.varint()?, r.i16()?);
                if slot == 36 {
                    got = Some((state, ItemStack::decode(&mut r)?));
                }
            }
        }
        let Some((state, Some(stack))) = got else {
            println!("?    {line}: no slot");
            continue;
        };
        let mut h = HashedStack::of(&stack);
        if spoil && let Some(first) = h.added.first_mut() {
            first.1 ^= 1;
        }
        click(&mut client, state, 36, &[(36, None)], Some(&h))?;
        let mut corrected = false;
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            if let Some((name, _)) = next(&mut client)?
                && (name == "minecraft:set_cursor_item" || name == "minecraft:container_set_slot")
            {
                corrected = true;
            }
        }
        if corrected {
            bad += 1;
            println!("DIFF {line}\n     {:?}", stack.components);
        } else {
            good += 1;
            println!("ok   {line}");
        }
        // Drop it outside the window.
        click(&mut client, state, -999, &[], None)?;
        let until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < until {
            next(&mut client)?;
        }
    }
    Ok((good, bad))
}
