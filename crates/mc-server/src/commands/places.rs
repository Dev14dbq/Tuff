//! Commands of the `places` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

#![allow(dead_code)]

use mc_command::args::{Arg, Coord, Env as _};
use mc_nbt::{Compound, Tag};
use mc_protocol::text::translate;

use super::run::err;
use super::{Result, Run, Store};
use crate::context::ServerContext;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["locate", "structure", "structure"] => locate_structure(r),
        ["locate", "biome", "biome"] => locate_biome(r),
        ["locate", "poi", "poi"] => locate_poi(r),
        ["setworldspawn", ..] => set_world_spawn(r),
        ["spawnpoint", ..] => spawn_point(r),
        _ => return None,
    })
}

/// Puts a stored result into a boss bar.
pub fn store(_ctx: &ServerContext, _store: &Store, _value: i32) {}

/// A rotation argument (yaw wrapped, pitch clamped), relative to the
/// source; none is 0, 0.
fn rotation(r: &Run, name: &str) -> (f32, f32) {
    let Some(Arg::Pos2([yaw, pitch])) = r.arg(name) else {
        return (0.0, 0.0);
    };
    let f = |c: &Coord, base: f32| {
        if c.relative {
            base + c.value as f32
        } else {
            c.value as f32
        }
    };
    let yaw = f(yaw, r.src.rot[0]);
    let pitch = f(pitch, r.src.rot[1]);
    // Wrapped as vanilla's Mth.wrapDegrees does.
    let mut yaw = yaw % 360.0;
    if yaw >= 180.0 {
        yaw -= 360.0;
    }
    if yaw < -180.0 {
        yaw += 360.0;
    }
    (yaw, pitch.clamp(-90.0, 90.0))
}

/// A block position argument, or the source's block.
fn pos_or_here(r: &Run, name: &str) -> (i32, i32, i32) {
    match r.block_pos(name) {
        Some(p) => (p.x, p.y, p.z),
        None => source_block(r),
    }
}

fn set_world_spawn(r: &mut Run) -> Result<i32> {
    let pos = pos_or_here(r, "pos");
    let (yaw, pitch) = rotation(r, "rotation");
    let spawn = crate::players::Respawn {
        dimension: "minecraft:overworld".into(),
        pos,
        yaw,
        pitch,
        forced: false,
    };
    *r.ctx.spawn.lock().expect("spawn lock") = spawn.clone();
    if let Err(e) = r.ctx.world.update_level(|data| {
        data.insert("spawn", Tag::Compound(spawn.to_nbt(false)));
    }) {
        tracing::warn!("saving the world spawn: {e:#}");
    }
    let packet = crate::connection::spawn_packet(r.ctx);
    let everyone: Vec<u128> = crate::tick::lock(&r.ctx.players).keys().copied().collect();
    for uuid in everyone {
        super::run::direct(
            r,
            uuid,
            crate::context::Direct::DefaultSpawn(packet.clone()),
        );
    }
    r.success(
        translate(
            "commands.setworldspawn.success",
            vec![
                Tag::Int(pos.0),
                Tag::Int(pos.1),
                Tag::Int(pos.2),
                Tag::Float(yaw),
                Tag::Float(pitch),
                Tag::String(spawn.dimension),
            ],
        ),
        true,
    );
    Ok(1)
}

fn spawn_point(r: &mut Run) -> Result<i32> {
    let targets = r.targets_or_self("targets", true)?;
    let pos = pos_or_here(r, "pos");
    let (yaw, pitch) = rotation(r, "rotation");
    let respawn = crate::players::Respawn {
        dimension: "minecraft:overworld".into(),
        pos,
        yaw,
        pitch,
        forced: true,
    };
    {
        let mut players = crate::tick::lock(&r.ctx.players);
        for t in &targets {
            if let Some(p) = players.get_mut(&t.uuid()) {
                p.extra.respawn = Some(respawn.clone());
            }
        }
    }
    let mut args = vec![
        Tag::Int(pos.0),
        Tag::Int(pos.1),
        Tag::Int(pos.2),
        Tag::Float(yaw),
        Tag::Float(pitch),
        Tag::String(respawn.dimension),
    ];
    let key = if targets.len() == 1 {
        args.push(targets[0].display());
        "commands.spawnpoint.success.single"
    } else {
        args.push(Tag::Int(targets.len() as i32));
        "commands.spawnpoint.success.multiple"
    };
    r.success(translate(key, args), true);
    Ok(targets.len() as i32)
}

/// How far `/locate structure` looks, in placement cells.
const STRUCTURE_RADIUS: i32 = 100;
/// How far `/locate biome` looks, in blocks, and its steps.
const BIOME_RADIUS: i32 = 6400;
const BIOME_STEP: i32 = 32;
const BIOME_STEP_Y: i32 = 64;

/// A resource-or-tag argument: the id, whether it is a tag, and how
/// messages print it (`minecraft:x` or `#minecraft:x`).
fn id_or_tag(r: &Run, name: &str) -> (String, bool, String) {
    match r.arg(name) {
        Some(Arg::IdOrTag { id, tag }) => {
            let shown = if *tag { format!("#{id}") } else { id.clone() };
            (id.clone(), *tag, shown)
        }
        _ => (String::new(), false, String::new()),
    }
}

/// The ids a resource-or-tag argument stands for in `registry`.
fn members(r: &Run, registry: &str, id: &str, tag: bool) -> Vec<String> {
    let env = &r.ctx.commands.env;
    if tag {
        env.tag(registry, id)
    } else if env.exists(registry, id) {
        vec![id.to_owned()]
    } else {
        Vec::new()
    }
}

/// The block the source is in.
fn source_block(r: &Run) -> (i32, i32, i32) {
    let p = r.src.pos;
    (
        p[0].floor() as i32,
        p[1].floor() as i32,
        p[2].floor() as i32,
    )
}

/// How a find is named: a tag with what matched it.
fn printed(shown: &str, tag: bool, matched: &str) -> String {
    if tag {
        format!("{shown} ({matched})")
    } else {
        shown.to_owned()
    }
}

/// The success message vanilla shows: the name (with what matched, for a
/// tag), green clickable coordinates and the distance, which is also the
/// result. `with_y` measures in 3D and shows y; structures show `~`.
fn found(
    r: &mut Run,
    key: &str,
    name: String,
    at: (i32, i32, i32),
    with_y: bool,
    started: std::time::Instant,
) -> i32 {
    let from = source_block(r);
    let (dx, dy, dz) = (at.0 - from.0, at.1 - from.1, at.2 - from.2);
    let squared = if with_y {
        dx * dx + dy * dy + dz * dz
    } else {
        dx * dx + dz * dz
    };
    let distance = (squared as f32).sqrt().floor() as i32;
    let y = if with_y {
        at.1.to_string()
    } else {
        "~".to_owned()
    };
    let mut click = Compound::new();
    click.insert("action", "suggest_command");
    click.insert(
        "command",
        Tag::String(format!("/tp @s {} {y} {}", at.0, at.2)),
    );
    let mut tooltip = Compound::new();
    tooltip.insert("translate", "chat.coordinates.tooltip");
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", Tag::Compound(tooltip));
    let coords = translate(
        "chat.coordinates",
        vec![Tag::Int(at.0), Tag::String(y), Tag::Int(at.2)],
    );
    let mut brackets = Compound::new();
    brackets.insert("translate", "chat.square_brackets");
    brackets.insert("with", Tag::List(vec![coords]));
    brackets.insert("color", "green");
    brackets.insert("click_event", Tag::Compound(click));
    brackets.insert("hover_event", Tag::Compound(hover));
    r.success(
        translate(
            key,
            vec![
                Tag::String(name.clone()),
                Tag::Compound(brackets),
                Tag::Int(distance),
            ],
        ),
        false,
    );
    tracing::info!(
        "Locating element {name} took {} ms",
        started.elapsed().as_millis()
    );
    distance
}

fn locate_structure(r: &mut Run) -> Result<i32> {
    let started = std::time::Instant::now();
    let (id, tag, shown) = id_or_tag(r, "structure");
    let wanted = members(r, "minecraft:worldgen/structure", &id, tag);
    if wanted.is_empty() {
        return Err(err(
            "commands.locate.structure.invalid",
            vec![Tag::String(shown)],
        ));
    }
    let from = source_block(r);
    let hit = r
        .ctx
        .world
        .pipeline()
        .and_then(|p| p.locate(&wanted, from, STRUCTURE_RADIUS));
    let Some((matched, at)) = hit else {
        return Err(err(
            "commands.locate.structure.not_found",
            vec![Tag::String(shown)],
        ));
    };
    Ok(found(
        r,
        "commands.locate.structure.success",
        printed(&shown, tag, &matched),
        at,
        false,
        started,
    ))
}

/// The heights `/locate biome` tries in each column: from the source's out,
/// alternately up and down, `step` apart, within `low..=high`.
fn out_from(origin: i32, low: i32, high: i32, step: i32) -> Vec<i32> {
    let goes_on = |i: i32| {
        let j = (origin - i).abs();
        origin - j >= low || origin + j <= high
    };
    let next = |i: i32| {
        let below = i <= origin;
        let j = (origin - i).abs();
        if !below || origin + j + step > high {
            let k = origin - j - if below { step } else { 0 };
            if k >= low {
                return k;
            }
        }
        origin + j + step
    };
    let mut out = Vec::new();
    let mut i = origin;
    while goes_on(i) {
        out.push(i);
        i = next(i);
    }
    out
}

/// The cells of a square spiral out from the middle (east first, then
/// south), `radius` rings of them, in vanilla's order.
fn spiral(radius: i32) -> impl Iterator<Item = (i32, i32)> {
    // East, south, west, north.
    const DIRS: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
    let legs = 4 * radius;
    let mut cursor = (0, 1);
    let mut leg = -1i32;
    let mut leg_size = 0;
    let mut leg_index = 0;
    std::iter::from_fn(move || {
        let d = DIRS[((leg + 4) % 4) as usize];
        cursor = (cursor.0 + d.0, cursor.1 + d.1);
        if leg_index >= leg_size {
            if leg >= legs {
                return None;
            }
            leg += 1;
            leg_index = 0;
            leg_size = leg / 2 + 1;
        }
        leg_index += 1;
        Some(cursor)
    })
}

fn locate_biome(r: &mut Run) -> Result<i32> {
    let started = std::time::Instant::now();
    let (id, tag, shown) = id_or_tag(r, "biome");
    let wanted = members(r, "minecraft:worldgen/biome", &id, tag);
    let from = source_block(r);
    let world = &r.ctx.world;
    let low = world.min_y + 1;
    let high = world.min_y + world.height as i32;
    let heights = out_from(from.1, low, high, BIOME_STEP_Y);
    let mut hit = None;
    'search: for (dx, dz) in spiral(BIOME_RADIUS / BIOME_STEP) {
        let x = from.0 + dx * BIOME_STEP;
        let z = from.2 + dz * BIOME_STEP;
        for &y in &heights {
            match world.noise_biome(x >> 2, y >> 2, z >> 2) {
                Some(b) if wanted.iter().any(|w| w == b) => {
                    hit = Some((b.to_owned(), (x, y, z)));
                    break 'search;
                }
                Some(_) => {}
                None => break 'search,
            }
        }
    }
    let Some((matched, at)) = hit else {
        return Err(err(
            "commands.locate.biome.not_found",
            vec![Tag::String(shown)],
        ));
    };
    Ok(found(
        r,
        "commands.locate.biome.success",
        printed(&shown, tag, &matched),
        at,
        true,
        started,
    ))
}

/// Points of interest (beds, job sites, bells...) are not tracked yet, so
/// none is ever near.
fn locate_poi(r: &mut Run) -> Result<i32> {
    let (_, _, shown) = id_or_tag(r, "poi");
    Err(err(
        "commands.locate.poi.not_found",
        vec![Tag::String(shown)],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_alternate_out_from_the_source() {
        assert_eq!(out_from(0, -63, 320, 64), vec![0, 64, 128, 192, 256, 320]);
        assert_eq!(
            out_from(100, -63, 320, 64),
            vec![100, 164, 36, 228, -28, 292]
        );
        assert_eq!(
            out_from(300, -63, 320, 64),
            vec![300, 236, 172, 108, 44, -20]
        );
    }

    #[test]
    fn spiral_goes_east_then_south() {
        let first: Vec<_> = spiral(2).take(10).collect();
        assert_eq!(
            first,
            vec![
                (0, 0),
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0),
                (-1, -1),
                (0, -1),
                (1, -1),
                (2, -1)
            ]
        );
        assert_eq!(spiral(2).count(), 25);
    }
}
