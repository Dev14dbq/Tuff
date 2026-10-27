//! `cargo xtask tickets`: which chunks tick, seen from outside. A bot stands
//! at spawn (chunk 0, 0) of a superflat world; in the chunks around it, and
//! in a force-loaded chunk far away, things are placed that only run when
//! the chunk does:
//!
//! - an item high in the air (falls: the chunk's entities tick),
//! - a furnace with fuel (smelts: block entities tick),
//! - a water source (flows: scheduled ticks run),
//! - wheat on farmland at a high random tick speed (grows: random ticks).
//!
//! After 20 s each is looked at with `execute if`. The table says, per
//! chunk along the axis (d, 0) and the diagonal (d, d), which of the four
//! happened. Run it on vanilla and on ours (`--only`) and compare.
//!
//! `--view <n>` and `--sim <n>` set `view-distance` and
//! `simulation-distance` (default 10 and 4).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use mc_protocol::Write as _;

use crate::parity::Server;
use crate::probe::{Client, Pose, follow_teleport};

const PORT: u16 = 25606;
/// A force-loaded chunk far from the bot.
const FAR: (i32, i32) = (40, 0);
const SURFACE: i32 = -60;
const WAIT: Duration = Duration::from_secs(20);

pub fn run(args: &[String]) -> Result<()> {
    let mut view = 10u8;
    let mut sim = 4u8;
    let mut only = "vanilla,ours".to_owned();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--view" => view = value()?.parse()?,
            "--sim" => sim = value()?.parse()?,
            "--only" => only = value()?.clone(),
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    for side in only.split(',') {
        println!("== {side}, view distance {view}, simulation distance {sim}");
        let seen = experiment(&root, side, view, sim)?;
        print_table(&seen, view);
    }
    Ok(())
}

/// What happened in one chunk.
#[derive(Clone, Copy, Default)]
struct Seen {
    /// The chunk took a block (loaded).
    loaded: bool,
    item_fell: bool,
    furnace: bool,
    water: bool,
    wheat: bool,
}

struct Results {
    /// (dx, dz) → what happened.
    chunks: Vec<((i32, i32), Seen)>,
    far: Seen,
}

fn experiment(root: &Path, side: &str, view: u8, sim: u8) -> Result<Results> {
    let properties = format!(
        "online-mode=false\nserver-ip=127.0.0.1\nserver-port={PORT}\nlevel-name=world\n\
         level-type=minecraft\\:flat\nview-distance={view}\nsimulation-distance={sim}\n\
         spawn-protection=0\npause-when-empty-seconds=0\nenable-status=true\nwhite-list=false\n"
    );
    let mut server = match side {
        "vanilla" => {
            let dir = root.join("cache/vanilla-server/26.3-tickets");
            let _ = std::fs::remove_dir_all(dir.join("world"));
            Server::prepare(root, &dir, &properties)?;
            Server::start_java(root, &dir)?
        }
        "ours" => Server::ours_in(root, &root.join("cache/ours-tickets"), &properties)?,
        other => bail!("--only vanilla,ours, not `{other}`"),
    };
    let done = (|| -> Result<Results> {
        let ids = crate::probe::load_packet_ids(None)?;
        let (client, stands) = crate::probe::join_player_with(
            &format!("127.0.0.1:{PORT}"),
            ids,
            "Standing",
            view as i8,
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let keeper = keep_alive(client, stop.clone())?;
        // Vanilla puts a new player anywhere within 10 blocks of the world
        // spawn, so the chunk they stand in varies: measure from there.
        let chunk = (
            (stands.0.floor() as i32) >> 4,
            (stands.2.floor() as i32) >> 4,
        );
        println!("  the bot stands in chunk {chunk:?}");
        let result = observe(&mut server, view, chunk);
        stop.store(true, Ordering::Relaxed);
        let _ = keeper.join();
        result
    })();
    server.stop();
    done
}

/// Answers the server's keep-alives and teleports, so the bot stays.
fn keep_alive(mut client: Client, stop: Arc<AtomicBool>) -> Result<std::thread::JoinHandle<()>> {
    client.set_read_timeout(Duration::from_millis(200))?;
    Ok(std::thread::spawn(move || {
        let mut pose = Pose::default();
        while !stop.load(Ordering::Relaxed) {
            let (packet, body) = match client.recv() {
                Ok(p) => p,
                // The read timeout fires when the server is quiet.
                Err(err) if crate::probe::is_timeout(&err) => continue,
                Err(_) => break,
            };
            let _ = match packet.as_str() {
                "minecraft:keep_alive" => client.send("minecraft:keep_alive", body),
                "minecraft:chunk_batch_finished" => {
                    let mut received = Vec::new();
                    received.put_f32(1000.0);
                    client.send("minecraft:chunk_batch_received", received)
                }
                "minecraft:player_position" => follow_teleport(&body, &mut pose)
                    .and_then(|c| client.send("minecraft:accept_teleportation", c)),
                _ => Ok(()),
            };
        }
    }))
}

fn observe(server: &mut Server, view: u8, stands: (i32, i32)) -> Result<Results> {
    for rule in [
        "gamerule spawn_mobs false",
        "gamerule random_tick_speed 3000",
        "gamerule advance_time false",
    ] {
        server.command(rule, false)?;
    }
    let view = i32::from(view);
    // Chunks to look at: along the axis and the diagonal, one past the view.
    let mut spots: Vec<(i32, i32)> = vec![(0, 0)];
    for d in 1..=view + 1 {
        spots.push((d, 0));
        spots.push((d, d));
    }
    let mut placed = Vec::new();
    for &(dx, dz) in &spots {
        let loaded = place(server, stands.0 + dx, stands.1 + dz)?;
        placed.push(((dx, dz), loaded));
    }
    // A force-loaded chunk with nobody near it.
    server.command(
        &format!("forceload add {} {}", FAR.0 * 16, FAR.1 * 16),
        false,
    )?;
    std::thread::sleep(Duration::from_secs(3));
    let far_loaded = place(server, FAR.0, FAR.1)?;
    std::thread::sleep(WAIT);
    let mut chunks = Vec::new();
    for ((dx, dz), loaded) in placed {
        let seen = look(server, stands.0 + dx, stands.1 + dz, loaded)?;
        chunks.push(((dx, dz), seen));
    }
    let far = look(server, FAR.0, FAR.1, far_loaded)?;
    Ok(Results { chunks, far })
}

/// Block coordinates of the indicators of chunk (`cx`, `cz`).
fn at(cx: i32, cz: i32, x: i32, z: i32) -> (i32, i32) {
    (cx * 16 + x, cz * 16 + z)
}

/// Places the four indicators; whether the chunk took them.
fn place(server: &mut Server, cx: i32, cz: i32) -> Result<bool> {
    let (fx, fz) = at(cx, cz, 4, 4);
    let reply = ask(
        server,
        &format!(
            "setblock {fx} {SURFACE} {fz} minecraft:furnace{{Items:[\
             {{Slot:0b,id:\"minecraft:raw_iron\",count:1}},\
             {{Slot:1b,id:\"minecraft:coal\",count:1}}]}}"
        ),
    )?;
    if !reply.contains("Changed the block") {
        return Ok(false);
    }
    let (wx, wz) = at(cx, cz, 12, 12);
    ask(
        server,
        &format!("setblock {wx} {SURFACE} {wz} minecraft:water"),
    )?;
    let (gx, gz) = at(cx, cz, 4, 12);
    ask(
        server,
        &format!(
            "setblock {gx} {} {gz} minecraft:farmland[moisture=7]",
            SURFACE - 1
        ),
    )?;
    ask(
        server,
        &format!("setblock {gx} {SURFACE} {gz} minecraft:wheat[age=0]"),
    )?;
    let (ix, iz) = at(cx, cz, 8, 8);
    ask(
        server,
        &format!(
            "summon minecraft:item {ix}.5 20 {iz}.5 \
             {{Item:{{id:\"minecraft:stone\",count:1}}}}"
        ),
    )?;
    Ok(true)
}

fn look(server: &mut Server, cx: i32, cz: i32, loaded: bool) -> Result<Seen> {
    if !loaded {
        return Ok(Seen::default());
    }
    let (fx, fz) = at(cx, cz, 4, 4);
    let (wx, wz) = at(cx, cz, 12, 12);
    let (gx, gz) = at(cx, cz, 4, 12);
    let (ix, iz) = at(cx, cz, 8, 8);
    let passed = |server: &mut Server, command: &str| -> Result<bool> {
        Ok(ask(server, command)?.contains("passed"))
    };
    Ok(Seen {
        loaded,
        furnace: passed(
            server,
            &format!("execute if data block {fx} {SURFACE} {fz} Items[{{Slot:2b}}]"),
        )?,
        water: passed(
            server,
            &format!("execute if block {} {SURFACE} {wz} minecraft:water", wx + 1),
        )?,
        wheat: passed(
            server,
            &format!("execute if block {gx} {SURFACE} {gz} minecraft:wheat[age=7]"),
        )?,
        item_fell: passed(
            server,
            &format!(
                "execute if entity @e[type=minecraft:item,x={ix}.5,y={SURFACE},z={iz}.5,distance=..4]"
            ),
        )?,
    })
}

/// Sends a command and returns the first line the server answers with.
fn ask(server: &mut Server, command: &str) -> Result<String> {
    server.drain()?;
    server.wait_for(command, "", Duration::from_secs(30))
}

fn print_table(seen: &Results, view: u8) {
    let mark = |s: &Seen, f: fn(&Seen) -> bool| {
        if !s.loaded {
            '-'
        } else if f(s) {
            'x'
        } else {
            '.'
        }
    };
    let cell = |s: &Seen| {
        format!(
            "{}{}{}{}",
            mark(s, |s| s.item_fell),
            mark(s, |s| s.furnace),
            mark(s, |s| s.water),
            mark(s, |s| s.wheat)
        )
    };
    println!("  item, furnace, water, wheat: x ran, . did not, - chunk not loaded");
    println!("  d | axis (d, 0) | diagonal (d, d)");
    let get = |p: (i32, i32)| {
        seen.chunks
            .iter()
            .find(|(k, _)| *k == p)
            .map(|(_, s)| *s)
            .unwrap_or_default()
    };
    println!("  0 | {} |", cell(&get((0, 0))));
    for d in 1..=i32::from(view) + 1 {
        println!(
            "  {d:<2}| {}        | {}",
            cell(&get((d, 0))),
            cell(&get((d, d)))
        );
    }
    println!(
        "  force-loaded chunk {:?}, nobody near: {}",
        FAR,
        cell(&seen.far)
    );
}
