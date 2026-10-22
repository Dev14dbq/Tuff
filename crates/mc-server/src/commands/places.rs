//! Commands of the `places` group (see `run::run` for the ones done
//! elsewhere). Each returns `None` for paths it does not handle.

#![allow(dead_code)]

use mc_command::args::{Arg, Coord, Env as _};
use mc_nbt::{Compound, Tag};
use mc_protocol::text::translate;

use super::run::err;
use super::{Result, Run};
use crate::context::ServerContext;

pub fn run(r: &mut Run, path: &[&str]) -> Option<Result<i32>> {
    Some(match path {
        ["locate", "structure", "structure"] => locate_structure(r),
        ["locate", "biome", "biome"] => locate_biome(r),
        ["locate", "poi", "poi"] => locate_poi(r),
        ["setworldspawn", ..] => set_world_spawn(r),
        ["spawnpoint", ..] => spawn_point(r),
        ["particle", "name", rest @ ..] => particle(r, rest),
        ["forceload", "add", ..] => forceload_change(r, true),
        ["forceload", "remove", "all"] => forceload_remove_all(r),
        ["forceload", "remove", ..] => forceload_change(r, false),
        ["forceload", "query"] => forceload_list(r),
        ["forceload", "query", "pos"] => forceload_query(r),
        ["fillbiome", "from", "to", "biome", ..] => fillbiome(r),
        _ => return None,
    })
}

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
        dimension: r.ctx.dimension_name().into(),
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

/// A particle's type and options as the wire carries them; `None` for
/// options this server cannot send yet (geysers, trails, vibrations).
fn particle_bytes(ctx: &ServerContext, id: &str, o: &Compound) -> Option<Vec<u8>> {
    use mc_command::particle::{color, float};
    use mc_protocol::Write as _;
    let mut out = Vec::new();
    out.put_varint(ctx.data.builtin.id("minecraft:particle_type", id)?);
    let block = |o: &Compound| -> Option<i32> {
        let blocks = &ctx.data.blocks;
        let state = match o.get("block_state")? {
            Tag::String(name) => blocks.default_state(&mc_command::args::full_id(name))?,
            Tag::Compound(c) => {
                let Some(Tag::String(name)) = c.get("id") else {
                    return None;
                };
                let props: Vec<(String, String)> = match c.get("Properties") {
                    Some(Tag::Compound(p)) => p
                        .iter()
                        .filter_map(|(k, v)| match v {
                            Tag::String(v) => Some((k.to_owned(), v.clone())),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let props: Vec<(&str, &str)> = props
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                blocks.state_id(&mc_command::args::full_id(name), &props)?
            }
            _ => return None,
        };
        Some(state as i32)
    };
    match id.strip_prefix("minecraft:").unwrap_or(id) {
        "dust" => {
            out.put_i32(color(o.get("color")?, 3)?);
            out.put_f32(float(o, "scale", 1.0));
        }
        "dust_color_transition" => {
            out.put_i32(color(o.get("from_color")?, 3)?);
            out.put_i32(color(o.get("to_color")?, 3)?);
            out.put_f32(float(o, "scale", 1.0));
        }
        "entity_effect" | "flash" | "tinted_leaves" => out.put_i32(color(o.get("color")?, 4)?),
        "block" | "block_crumble" | "block_marker" | "dust_pillar" | "falling_dust" => {
            out.put_varint(block(o)?);
        }
        "item" => {
            let (name, count) = match o.get("item")? {
                Tag::String(n) => (n.clone(), 1),
                Tag::Compound(c) => match (c.get("id"), c.get("count")) {
                    (Some(Tag::String(n)), Some(Tag::Int(k))) => (n.clone(), *k),
                    (Some(Tag::String(n)), _) => (n.clone(), 1),
                    _ => return None,
                },
                _ => return None,
            };
            out.put_varint(ctx.items.id(&mc_command::args::full_id(&name))?);
            out.put_varint(count);
            out.put_varint(0);
            out.put_varint(0);
        }
        "sculk_charge" => out.put_f32(float(o, "roll", 0.0)),
        "shriek" => out.put_varint(float(o, "delay", 0.0) as i32),
        "dragon_breath" => out.put_f32(float(o, "power", 1.0)),
        "effect" | "instant_effect" => {
            out.put_i32(o.get("color").and_then(|c| color(c, 4)).unwrap_or(-1));
            out.put_f32(float(o, "power", 1.0));
        }
        "geyser" | "geyser_base" | "geyser_plume" | "geyser_poof" | "trail" | "vibration" => {
            return None;
        }
        _ => {}
    }
    Some(out)
}

fn particle(r: &mut Run, rest: &[&str]) -> Result<i32> {
    let Some(Arg::Particle { id, options }) = r.arg("name").cloned() else {
        return Err(err("command.failed", Vec::new()));
    };
    let pos = r.vec3("pos").unwrap_or(r.src.pos);
    let delta = r.vec3_raw("delta").unwrap_or([0.0; 3]);
    let speed = r.float("speed").unwrap_or(0.0);
    let count = r.int("count").unwrap_or(0);
    let force = rest.contains(&"force");
    let viewers = if rest.contains(&"viewers") {
        r.targets("viewers", true)?
    } else {
        super::target::everyone(r.ctx)
            .into_iter()
            .filter(|t| t.is_player())
            .collect()
    };
    let Some(bytes) = particle_bytes(r.ctx, &id, &options) else {
        return Err(err("commands.particle.failed", Vec::new()));
    };
    let packet = mc_protocol::packets::play::LevelParticles {
        particle: bytes,
        force,
        always_show: false,
        pos,
        delta: [delta[0] as f32, delta[1] as f32, delta[2] as f32],
        speed,
        count,
    };
    // Vanilla sends within 32 blocks, or 512 when forced.
    let range: f64 = if force { 512.0 } else { 32.0 };
    let mut sent = 0;
    for v in &viewers {
        let p = v.pos();
        let d = (p[0] - pos[0]).powi(2) + (p[1] - pos[1]).powi(2) + (p[2] - pos[2]).powi(2);
        if d < range * range {
            super::run::direct(
                r,
                v.uuid(),
                crate::context::Direct::Particles(packet.clone()),
            );
            sent += 1;
        }
    }
    if sent == 0 {
        return Err(err("commands.particle.failed", Vec::new()));
    }
    r.success(
        translate("commands.particle.success", vec![Tag::String(id)]),
        true,
    );
    Ok(sent)
}

const OVERWORLD: &str = "minecraft:overworld";

/// The chunk of a column position argument; positions past the world's
/// edge are refused.
fn chunk_arg(r: &Run, name: &str) -> Result<Option<(i32, i32)>> {
    let Some(Arg::Pos2([x, z])) = r.arg(name) else {
        return Ok(None);
    };
    let f = |c: &Coord, base: f64| {
        if c.relative {
            (base + c.value).floor() as i64
        } else {
            c.value.floor() as i64
        }
    };
    let (x, z) = (f(x, r.src.pos[0]), f(z, r.src.pos[2]));
    let edge = 30_000_000i64;
    if x < -edge || x >= edge || z < -edge || z >= edge {
        return Err(err("argument.pos.outofworld", Vec::new()));
    }
    Ok(Some(((x >> 4) as i32, (z >> 4) as i32)))
}

fn chunk_text(x: i32, z: i32) -> Tag {
    Tag::String(format!("[{x}, {z}]"))
}

/// How many chunks one `/forceload` may change.
const FORCELOAD_LIMIT: i64 = 256;

fn forceload_change(r: &mut Run, add: bool) -> Result<i32> {
    let Some(from) = chunk_arg(r, "from")? else {
        return Err(err("command.failed", Vec::new()));
    };
    let to = chunk_arg(r, "to")?.unwrap_or(from);
    let (min, max) = (
        (from.0.min(to.0), from.1.min(to.1)),
        (from.0.max(to.0), from.1.max(to.1)),
    );
    let count = i64::from(max.0 - min.0 + 1) * i64::from(max.1 - min.1 + 1);
    if count > FORCELOAD_LIMIT {
        return Err(err(
            "commands.forceload.toobig",
            vec![Tag::Int(FORCELOAD_LIMIT as i32), Tag::Long(count)],
        ));
    }
    let (changed, first) = {
        let mut forced = crate::tick::lock(&r.ctx.forced);
        let mut changed = 0;
        let mut first = None;
        for x in min.0..=max.0 {
            for z in min.1..=max.1 {
                if forced.set(x, z, add) {
                    changed += 1;
                    first.get_or_insert((x, z));
                }
            }
        }
        if changed > 0 {
            forced.save();
        }
        (changed, first)
    };
    let verb = if add { "added" } else { "removed" };
    let Some(first) = first else {
        return Err(err(
            &format!("commands.forceload.{verb}.failure"),
            Vec::new(),
        ));
    };
    let msg = if changed == 1 {
        translate(
            &format!("commands.forceload.{verb}.single"),
            vec![chunk_text(first.0, first.1), Tag::from(OVERWORLD)],
        )
    } else {
        translate(
            &format!("commands.forceload.{verb}.multiple"),
            vec![
                Tag::Int(changed),
                Tag::from(OVERWORLD),
                chunk_text(min.0, min.1),
                chunk_text(max.0, max.1),
            ],
        )
    };
    r.success(msg, true);
    Ok(changed)
}

fn forceload_remove_all(r: &mut Run) -> Result<i32> {
    {
        let mut forced = crate::tick::lock(&r.ctx.forced);
        forced.clear();
        forced.save();
    }
    r.success(
        translate("commands.forceload.removed.all", vec![Tag::from(OVERWORLD)]),
        true,
    );
    Ok(0)
}

fn forceload_list(r: &mut Run) -> Result<i32> {
    let list = crate::tick::lock(&r.ctx.forced).list();
    let shown = list
        .iter()
        .map(|(x, z)| format!("[{x}, {z}]"))
        .collect::<Vec<_>>()
        .join(", ");
    let msg = match list.len() {
        0 => {
            return Err(err(
                "commands.forceload.added.none",
                vec![Tag::from(OVERWORLD)],
            ));
        }
        1 => translate(
            "commands.forceload.list.single",
            vec![Tag::from(OVERWORLD), Tag::String(shown)],
        ),
        n => translate(
            "commands.forceload.list.multiple",
            vec![Tag::Int(n as i32), Tag::from(OVERWORLD), Tag::String(shown)],
        ),
    };
    r.success(msg, false);
    Ok(list.len() as i32)
}

fn forceload_query(r: &mut Run) -> Result<i32> {
    let Some((x, z)) = chunk_arg(r, "pos")? else {
        return Err(err("command.failed", Vec::new()));
    };
    let args = vec![chunk_text(x, z), Tag::from(OVERWORLD)];
    if !crate::tick::lock(&r.ctx.forced).contains(x, z) {
        return Err(err("commands.forceload.query.failure", args));
    }
    r.success(translate("commands.forceload.query.success", args), false);
    Ok(1)
}

/// `/fillbiome`: biomes are kept per 4×4×4 cell, so the box grows to
/// whole cells.
fn fillbiome(r: &mut Run) -> Result<i32> {
    let a = super::place::loaded_pos(r, "from")?;
    let b = super::place::loaded_pos(r, "to")?;
    let q = |v: i32| (v >> 2) << 2;
    let min = [q(a.x.min(b.x)), q(a.y.min(b.y)), q(a.z.min(b.z))];
    let max = [q(a.x.max(b.x)), q(a.y.max(b.y)), q(a.z.max(b.z))];
    let volume = (0..3)
        .map(|i| i64::from(max[i] - min[i] + 1))
        .product::<i64>();
    let limit = super::run::block_limit(r.ctx);
    if volume > limit {
        return Err(err(
            "commands.fillbiome.toobig",
            vec![Tag::Int(limit as i32), Tag::Long(volume)],
        ));
    }
    let name = match r.arg("biome") {
        Some(Arg::Id(s) | Arg::Str(s) | Arg::Raw(s)) => mc_command::args::full_id(s),
        _ => String::new(),
    };
    let world = &r.ctx.world;
    let Some(biome) = world.biome_id(&name) else {
        return Err(err(
            "argument.resource.not_found",
            vec![Tag::String(name), Tag::from("minecraft:worldgen/biome")],
        ));
    };
    let allowed: Option<Vec<u32>> = match r.arg("filter") {
        Some(Arg::IdOrTag { id, tag }) => Some(
            members(
                r,
                "minecraft:worldgen/biome",
                &mc_command::args::full_id(id),
                *tag,
            )
            .iter()
            .filter_map(|n| world.biome_id(n))
            .collect(),
        ),
        _ => None,
    };
    let replace = |old: u32| allowed.as_ref().is_none_or(|a| a.contains(&old));
    let cell_min = [min[0] >> 2, min[1] >> 2, min[2] >> 2];
    let cell_max = [max[0] >> 2, max[1] >> 2, max[2] >> 2];
    let mut count = 0;
    let mut chunks = Vec::new();
    for cz in (min[2] >> 4)..=(max[2] >> 4) {
        for cx in (min[0] >> 4)..=(max[0] >> 4) {
            let n = world.fill_biome(
                (cx, cz),
                cell_min,
                cell_max,
                biome,
                &replace,
                &r.ctx.data.blocks,
            );
            if n > 0 {
                chunks.push((cx, cz));
                count += n;
            }
        }
    }
    if count == 0 {
        return Err(err("commands.fillbiome.no_changes", Vec::new()));
    }
    r.out
        .events
        .push(crate::context::Event::BiomesChanged { chunks });
    let mut with = vec![Tag::Int(count as i32)];
    with.extend(min.iter().chain(max.iter()).map(|v| Tag::Int(*v)));
    r.success(translate("commands.fillbiome.success.count", with), true);
    Ok(count as i32)
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
