//! `cargo xtask observe attach|conductor`: learns facts about every block
//! state from a vanilla server that the data reports do not carry.
//!
//! `attach`: which faces fences attach to.
//! `support`: which faces of each state hold up blocks that need support:
//! 1 a torch on top (centre), 2 a rail on top (rigid), 4 a floor button on
//! top (full face), 8 a hanging lantern below (centre of the bottom), 16 a
//! ceiling button below (full bottom face).
//! `push`: what each state does in front of a sticky piston: 1 it is
//! pushed, 2 it breaks, 4 it stops the piston, 8 it comes back when the
//! piston pulls.
//! `instrument`: the note block instrument each block gives the note block
//! on top of it (default states only; the index in the `instrument` values).
//! `conductor`: whether the block conducts redstone power: a powered lever
//! on its west face, a redstone lamp on its east face; the lamp lights only
//! through a conductor. A copy without the lever two blocks south tells power
//! sources (they light the lamp themselves) from conductors. Results in
//! `observed/conductor.json` (1 conducts, 2 is a power source, 16 the state
//! did not stay as placed, -1 not observed).
//!
//! Every state is placed on a frozen superflat world with an oak fence on
//! each horizontal side; after the neighbour updates settle, each fence's
//! connection towards the block is read back through a bot. Results go to
//! `cache/mcdata/<version>/observed/fence_attach.json`: one number per state
//! id, bits 1 north, 2 east, 4 south, 8 west; 16 if the state did not stay
//! as placed (its own reaction to the fences, or support it lacked).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::parity::{Mirror, Server, VANILLA_PORT};
use crate::probe::{load_data, packet_ids};

const PER_RUN: usize = 1500;
const ROW: usize = 39;
const Y: i32 = -59;

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("attach") => observe(&args[1..], Fact::Attach),
        Some("conductor") => observe(&args[1..], Fact::Conductor),
        Some("opacity") => observe(&args[1..], Fact::Opacity),
        Some("instrument") => observe(&args[1..], Fact::Instrument),
        Some("support") => observe(&args[1..], Fact::Support),
        Some("push") => observe(&args[1..], Fact::Push),
        _ => bail!(
            "usage: observe attach|conductor|opacity|instrument [--first <state>] [--count <n>]"
        ),
    }
}

fn cell(i: usize) -> (i32, i32) {
    (-76 + 4 * (i % ROW) as i32, -76 + 4 * (i / ROW) as i32)
}

#[derive(Clone, Copy, PartialEq)]
enum Fact {
    Attach,
    Conductor,
    Opacity,
    Instrument,
    Support,
    Push,
}

impl Fact {
    fn file(self) -> &'static str {
        match self {
            Fact::Attach => "fence_attach.json",
            Fact::Conductor => "conductor.json",
            Fact::Opacity => "opacity.json",
            Fact::Instrument => "instrument.json",
            Fact::Support => "support.json",
            Fact::Push => "push.json",
        }
    }

    fn what(self) -> &'static str {
        match self {
            Fact::Attach => {
                "faces of each block state an oak fence attaches to: 1 north, 2 east, 4 south, 8 west; 16 = state changed after placing; -1 = not observed"
            }
            Fact::Conductor => {
                "whether each block state conducts redstone power (lever west, lamp east): 1 yes, 0 no, 2 a power source; 16 = state changed after placing; -1 = not observed"
            }
            Fact::Opacity => {
                "how much sky light each block state takes away going down into a closed cell below it (0-15); +16 = state changed after placing; -1 = not observed"
            }
            Fact::Push => {
                "each state in front of a sticky piston: 1 pushed, 2 broken, 4 stops the piston, 8 pulled back; -1 = not observed"
            }
            Fact::Support => {
                "faces of each state that hold blocks up: 1 torch on top, 2 rail on top, 4 floor button on top, 8 hanging lantern below, 16 ceiling button below; -1 = not observed"
            }
            Fact::Instrument => {
                "instrument a note block placed on each block's default state takes: index in the note block's instrument values; -1 = not observed"
            }
        }
    }
}

fn observe(args: &[String], fact: Fact) -> Result<()> {
    let data = load_data(None)?;
    let total = data.blocks.state_count() as usize;
    let (mut first, mut count) = (0usize, total);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--first" => first = value()?.parse()?,
            "--count" => count = value()?.parse()?,
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let out_dir = root
        .join("cache/mcdata")
        .join(&data.version.name)
        .join("observed");
    std::fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(fact.file());
    // Keep earlier results, so a long run can be done in parts.
    let mut results: Vec<i32> = std::fs::read_to_string(&out_path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| serde_json::from_value(v["states"].clone()).ok())
        .filter(|v: &Vec<i32>| v.len() == total)
        .unwrap_or_else(|| vec![-1; total]);

    let end = (first + count).min(total);
    let mut start = first;
    while start < end {
        let is_default = |s: u32| {
            data.blocks
                .block_name(s)
                .and_then(|n| data.blocks.default_state(n))
                == Some(s)
        };
        let batch: Vec<u32> = (start..(start + PER_RUN).min(end))
            .map(|s| s as u32)
            .filter(|&s| data.blocks.block_name(s) != Some("minecraft:moving_piston"))
            .filter(|&s| fact != Fact::Instrument || is_default(s))
            .collect();
        let began = Instant::now();
        let observed = observe_batch(&root, &data, &batch, fact)?;
        for (state, bits) in observed {
            results[state as usize] = bits;
        }
        println!(
            "states {start}..{} done in {:.0} s",
            (start + PER_RUN).min(end),
            began.elapsed().as_secs_f64()
        );
        let json = serde_json::json!({
            "version": data.version.name,
            "what": fact.what(),
            "states": results,
        });
        std::fs::write(&out_path, serde_json::to_string(&json)?)?;
        start += PER_RUN;
    }
    println!("wrote {}", out_path.display());
    Ok(())
}

fn observe_batch(
    root: &std::path::Path,
    data: &mc_data::McData,
    states: &[u32],
    fact: Fact,
) -> Result<Vec<(u32, i32)>> {
    if fact == Fact::Push {
        return observe_push(root, data, states);
    }
    let mut vanilla = Server::vanilla(root)?;
    let result = (|| -> Result<Vec<(u32, i32)>> {
        let mut mirror = Mirror::join(&format!("127.0.0.1:{VANILLA_PORT}"), packet_ids(data))?;
        for setup in [
            "gamerule random_tick_speed 0",
            "gamerule spawn_mobs false",
            "kill @e[type=!player]",
            "tp Parity 0 -40 0",
        ] {
            vanilla.send(setup)?;
        }
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(3))?;
        vanilla.send("fill -80 -60 -80 80 -60 80 stone")?;
        // Light only spreads while the game runs.
        if fact != Fact::Opacity {
            vanilla.send("tick freeze")?;
        }
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(3))?;
        if fact == Fact::Opacity {
            // A closed cell under each block, open only at the top.
            for i in 0..states.len() {
                let (x, z) = cell(i);
                for (dx, dy, dz) in [(-1, 8, 0), (1, 8, 0), (0, 8, -1), (0, 8, 1), (0, 7, 0)] {
                    vanilla.send(&format!("setblock {} {} {} stone", x + dx, Y + dy, z + dz))?;
                }
            }
        }
        for (i, &state) in states.iter().enumerate() {
            let (x, z) = cell(i);
            let text = data.blocks.describe(state).context("unknown state")?;
            let text = text.trim_end_matches("[]");
            if fact == Fact::Opacity {
                vanilla.send(&format!("setblock {x} {} {z} {text}", Y + 9))?;
                continue;
            }
            if fact == Fact::Support {
                // Copies of the block, each with one dependent block. The
                // dependent block goes first (a command leaves it even
                // unsupported); the block put next to it updates it, and it
                // then checks its support.
                for (dx, dz, on_top) in [
                    (0, 0, "torch"),
                    (2, 0, "rail"),
                    (0, 2, "stone_button[face=floor]"),
                ] {
                    let (x, z) = (x + dx, z + dz);
                    vanilla.send(&format!("setblock {x} {} {z} {on_top}", Y + 1))?;
                    vanilla.send(&format!("setblock {x} {Y} {z} {text}"))?;
                }
                for (dx, dz, hanging) in [
                    (0, 0, "lantern[hanging=true]"),
                    (2, 0, "stone_button[face=ceiling]"),
                ] {
                    let (x, z) = (x + dx, z + dz);
                    vanilla.send(&format!("setblock {x} {} {z} {hanging}", Y + 3))?;
                    vanilla.send(&format!("setblock {x} {} {z} {text}", Y + 4))?;
                }
                continue;
            }
            if fact == Fact::Instrument {
                vanilla.send(&format!("setblock {x} {Y} {z} {text}"))?;
                vanilla.send(&format!("setblock {x} {} {z} note_block", Y + 1))?;
                continue;
            }
            vanilla.send(&format!("setblock {x} {Y} {z} {text}"))?;
            if fact == Fact::Conductor {
                vanilla.send(&format!("setblock {x} {Y} {} {text}", z + 2))?;
            }
        }
        for i in 0..states.len() {
            let (x, z) = cell(i);
            match fact {
                Fact::Attach => {
                    for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                        vanilla.send(&format!("setblock {} {Y} {} oak_fence", x + dx, z + dz))?;
                    }
                }
                Fact::Conductor => {
                    vanilla.send(&format!("setblock {} {Y} {z} redstone_lamp", x + 1))?;
                    vanilla.send(&format!("setblock {} {Y} {} redstone_lamp", x + 1, z + 2))?;
                }
                Fact::Opacity | Fact::Instrument | Fact::Support | Fact::Push => {}
            }
        }
        if fact == Fact::Conductor {
            for i in 0..states.len() {
                let (x, z) = cell(i);
                vanilla.send(&format!(
                    "setblock {} {Y} {z} lever[face=wall,facing=west,powered=true]",
                    x - 1
                ))?;
            }
            // A lever set by a command does not update the neighbours of its
            // block; poke each lamp so it looks at its power.
            for i in 0..states.len() {
                let (x, z) = cell(i);
                for z in [z, z + 2] {
                    vanilla.send(&format!("setblock {} {Y} {z} stone", x + 2))?;
                    vanilla.send(&format!("setblock {} {Y} {z} air", x + 2))?;
                }
            }
        }
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(5))?;
        if fact == Fact::Opacity {
            // Light comes with chunk data: walk away and back so the client
            // is sent the chunks again, lit as they are now.
            vanilla.send("tp Parity 2000 -40 2000")?;
            vanilla.drain()?;
            wait_quiet(&mut mirror, Duration::from_secs(3))?;
            vanilla.send("tp Parity 0 -40 0")?;
            vanilla.drain()?;
            wait_quiet(&mut mirror, Duration::from_secs(4))?;
        }
        let connected = |state: Option<u32>, side: &str| {
            state.and_then(|s| data.blocks.property(s, side)) == Some("true")
        };
        Ok(states
            .iter()
            .enumerate()
            .map(|(i, &state)| {
                let (x, z) = cell(i);
                let mut bits = 0;
                if fact == Fact::Support {
                    let name = |x: i32, y: i32, z: i32| {
                        mirror
                            .block(x, y, z)
                            .and_then(|s| data.blocks.block_name(s))
                    };
                    let mut bits = 0;
                    for (bit, (dx, dy, dz), wanted) in [
                        (1, (0, 1, 0), "minecraft:torch"),
                        (2, (2, 1, 0), "minecraft:rail"),
                        (4, (0, 1, 2), "minecraft:stone_button"),
                        (8, (0, 3, 0), "minecraft:lantern"),
                        (16, (2, 3, 0), "minecraft:stone_button"),
                    ] {
                        if name(x + dx, Y + dy, z + dz) == Some(wanted) {
                            bits |= bit;
                        }
                    }
                    return (state, bits);
                }
                if fact == Fact::Instrument {
                    let values = [
                        "harp",
                        "basedrum",
                        "snare",
                        "hat",
                        "bass",
                        "flute",
                        "bell",
                        "guitar",
                        "chime",
                        "xylophone",
                        "iron_xylophone",
                        "cow_bell",
                        "didgeridoo",
                        "bit",
                        "banjo",
                        "pling",
                        "trumpet",
                        "trumpet_exposed",
                        "trumpet_oxidized",
                        "trumpet_weathered",
                        "zombie",
                        "skeleton",
                        "creeper",
                        "dragon",
                        "wither_skeleton",
                        "piglin",
                        "custom_head",
                    ];
                    let found = mirror
                        .block(x, Y + 1, z)
                        .and_then(|s| data.blocks.property(s, "instrument"))
                        .and_then(|v| values.iter().position(|n| *n == v));
                    return (state, found.map_or(-1, |i| i as i32));
                }
                if fact == Fact::Opacity {
                    let changed = mirror.block(x, Y + 9, z) != Some(state);
                    let bits = match mirror.light(0, x, Y + 8, z) {
                        Some(sky) => 15 - i32::from(sky),
                        None => -1,
                    };
                    return (state, bits + if changed && bits >= 0 { 16 } else { 0 });
                }
                if fact == Fact::Conductor {
                    let lit = |z: i32| {
                        mirror
                            .block(x + 1, Y, z)
                            .and_then(|s| data.blocks.property(s, "lit"))
                            == Some("true")
                    };
                    let (with_lever, alone) = (lit(z), lit(z + 2));
                    let changed = mirror.block(x, Y, z) != Some(state);
                    let bits = if alone { 2 } else { i32::from(with_lever) };
                    return (state, bits | if changed { 16 } else { 0 });
                }
                // The fence north of the block reaches south to it, and so on.
                if connected(mirror.block(x, Y, z - 1), "south") {
                    bits |= 1;
                }
                if connected(mirror.block(x + 1, Y, z), "west") {
                    bits |= 2;
                }
                if connected(mirror.block(x, Y, z + 1), "north") {
                    bits |= 4;
                }
                if connected(mirror.block(x - 1, Y, z), "east") {
                    bits |= 8;
                }
                if mirror.block(x, Y, z) != Some(state) {
                    bits |= 16;
                }
                (state, bits)
            })
            .collect())
    })();
    vanilla.stop();
    result
}

/// Each state east of a sticky piston facing east: power the piston from
/// behind, see where the block went; take the power away, see whether it
/// came back.
fn observe_push(
    root: &std::path::Path,
    data: &mc_data::McData,
    states: &[u32],
) -> Result<Vec<(u32, i32)>> {
    let mut vanilla = Server::vanilla(root)?;
    let result = (|| -> Result<Vec<(u32, i32)>> {
        let mut mirror = Mirror::join(&format!("127.0.0.1:{VANILLA_PORT}"), packet_ids(data))?;
        for setup in [
            "gamerule random_tick_speed 0",
            "gamerule spawn_mobs false",
            "kill @e[type=!player]",
            "tp Parity 0 -40 0",
            "tick freeze",
        ] {
            vanilla.send(setup)?;
        }
        vanilla.drain()?;
        // The floor only once the chunks are there (a fill over unloaded
        // chunks does nothing).
        wait_quiet(&mut mirror, Duration::from_secs(3))?;
        vanilla.send("fill -80 -60 -80 80 -60 80 stone")?;
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(2))?;
        for (i, &state) in states.iter().enumerate() {
            let (x, z) = cell(i);
            let text = data.blocks.describe(state).context("unknown state")?;
            let text = text.trim_end_matches("[]");
            vanilla.send(&format!("setblock {} {Y} {z} {text}", x + 1))?;
            vanilla.send(&format!("setblock {x} {Y} {z} sticky_piston[facing=east]"))?;
        }
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(3))?;
        let name = |mirror: &Mirror, x: i32, z: i32| {
            mirror
                .block(x, Y, z)
                .and_then(|s| data.blocks.block_name(s))
                .map(str::to_owned)
        };
        let names: Vec<Option<String>> = (0..states.len())
            .map(|i| {
                let (x, z) = cell(i);
                name(&mirror, x + 1, z)
            })
            .collect();
        for i in 0..states.len() {
            let (x, z) = cell(i);
            // From below: nothing the moved block touches is powered.
            vanilla.send(&format!("setblock {x} {} {z} redstone_block", Y - 1))?;
        }
        // A few ticks, frozen otherwise (as on the parity bench).
        vanilla.send("tick step 10")?;
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(4))?;
        let mut bits = vec![0i32; states.len()];
        for (i, own) in names.iter().enumerate() {
            let (x, z) = cell(i);
            let extended = mirror
                .block(x, Y, z)
                .and_then(|s| data.blocks.property(s, "extended"))
                == Some("true");
            let (at1, at2) = (name(&mirror, x + 1, z), name(&mirror, x + 2, z));
            bits[i] = if own.as_deref().is_none_or(|n| n == "minecraft:air") {
                // Gone before the piston moved (it could not stay there).
                -1
            } else if !extended {
                4
            } else if at2.is_some() && at2 == *own {
                1
            } else if at1.as_deref() == Some("minecraft:piston_head") {
                2
            } else {
                0
            };
        }
        for i in 0..states.len() {
            let (x, z) = cell(i);
            vanilla.send(&format!("setblock {x} {} {z} stone", Y - 1))?;
        }
        vanilla.send("tick step 10")?;
        vanilla.drain()?;
        wait_quiet(&mut mirror, Duration::from_secs(4))?;
        for (i, own) in names.iter().enumerate() {
            let (x, z) = cell(i);
            if bits[i] == 1 && name(&mirror, x + 1, z) == *own {
                bits[i] |= 8;
            }
        }
        Ok(states.iter().copied().zip(bits).collect())
    })();
    vanilla.stop();
    result
}

fn wait_quiet(mirror: &mut Mirror, min: Duration) -> Result<()> {
    let start = Instant::now();
    let mut quiet = Instant::now();
    while start.elapsed() < min || quiet.elapsed() < Duration::from_millis(1500) {
        if mirror.pump()? {
            quiet = Instant::now();
        }
        if start.elapsed() > Duration::from_secs(120) {
            bail!("vanilla kept changing blocks");
        }
    }
    Ok(())
}
