//! Structure blocks and jigsaw blocks (minecraft.wiki: "Structure Block",
//! "Jigsaw Block"): game master blocks, used by operators in creative mode.
//!
//! A structure block in *save* mode writes the box it frames (its relative
//! position and size) as a template file `generated/<namespace>/
//! structures/<name>.nbt` in the world; in *load* mode it places a template
//! (that file, or else the data pack's) at that spot, turned and mirrored
//! as set and thinned by its integrity; *corner* blocks with the same name
//! let a save block find its box (detect size); *data* blocks only carry
//! text. Redstone power starts a save or a load.
//!
//! A jigsaw block holds the name it is found by, the one it joins to, the
//! pool to grow, what it turns into, the joint type and priorities; its
//! window can grow a structure from it.

use mc_features::structure::template::{Mirror, Processor, Rotation, Settings, Template};
use mc_nbt::{Compound, Tag};
use mc_protocol::Position;
use mc_protocol::text::translate;

use crate::context::{Event, ServerContext};
use crate::level::WorldLevel;

pub const STRUCTURE: &str = "minecraft:structure_block";
pub const JIGSAW: &str = "minecraft:jigsaw";

/// Offsets are within this many blocks, sizes up to it.
const LIMIT: i32 = 48;
/// A corner block is looked for this far from the save block.
const DETECT: i32 = 80;

fn text(s: &str) -> Tag {
    Tag::String(s.to_owned())
}

fn string(d: &Compound, key: &str) -> String {
    match d.get(key) {
        Some(Tag::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn int(d: &Compound, key: &str) -> i32 {
    match d.get(key) {
        Some(Tag::Int(v)) => *v,
        Some(Tag::Short(v)) => i32::from(*v),
        Some(Tag::Byte(v)) => i32::from(*v),
        _ => 0,
    }
}

fn flag(d: &Compound, key: &str) -> bool {
    int(d, key) != 0
}

/// The block entity's data at `p` (the block must be there).
fn data(ctx: &ServerContext, p: Position, id: &str) -> Option<Compound> {
    let state = ctx.world.block(p, &ctx.data.blocks)?;
    if ctx.data.blocks.block_name(state)? != id {
        return None;
    }
    Some(
        ctx.world
            .block_entity(p, &ctx.data.blocks)
            .map_or_else(|| blank(id), |be| be.data),
    )
}

fn store(ctx: &ServerContext, p: Position, id: &str, data: Compound) {
    let blocks = &ctx.data.blocks;
    ctx.world.set_block_entity(
        mc_world::BlockEntity {
            x: p.x,
            y: p.y,
            z: p.z,
            id: id.to_owned(),
            data,
        },
        blocks,
    );
}

/// The data a new block of this kind starts with.
fn blank(id: &str) -> Compound {
    let mut d = Compound::new();
    if id == JIGSAW {
        d.insert("name", text("minecraft:empty"));
        d.insert("target", text("minecraft:empty"));
        d.insert("pool", text("minecraft:empty"));
        d.insert("final_state", text("minecraft:air"));
        d.insert("joint", text("rollable"));
        d.insert("selection_priority", Tag::Int(0));
        d.insert("placement_priority", Tag::Int(0));
    } else {
        d.insert("name", text(""));
        d.insert("author", text(""));
        d.insert("metadata", text(""));
        for k in ["posX", "posY", "posZ"] {
            d.insert(k, Tag::Int(0));
        }
        for k in ["sizeX", "sizeY", "sizeZ"] {
            d.insert(k, Tag::Int(0));
        }
        d.insert("rotation", text("NONE"));
        d.insert("mirror", text("NONE"));
        d.insert("mode", text("DATA"));
        d.insert("integrity", Tag::Float(1.0));
        d.insert("seed", Tag::Long(0));
        for k in ["ignoreEntities", "strict"] {
            d.insert(k, Tag::Byte(i8::from(k == "ignoreEntities")));
        }
        for k in ["powered", "showair", "showboundingbox"] {
            d.insert(k, Tag::Byte(i8::from(k == "showboundingbox")));
        }
    }
    d
}

/// A game master block was placed: it gets its data (a structure block
/// starts in the mode its state says).
pub fn placed(ctx: &ServerContext, p: Position, state: u32) {
    let blocks = &ctx.data.blocks;
    match blocks.block_name(state) {
        Some(JIGSAW) => store(ctx, p, JIGSAW, blank(JIGSAW)),
        Some(STRUCTURE) => {
            let mut d = blank(STRUCTURE);
            d.insert("mode", text(&mode_name(blocks.property(state, "mode"))));
            store(ctx, p, STRUCTURE, d);
        }
        _ => {}
    }
}

fn mode_name(prop: Option<&str>) -> String {
    prop.unwrap_or("data").to_uppercase()
}

/// A player clicked a structure or jigsaw block: an operator in creative
/// gets its data and their game opens the window.
pub fn open(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    id: &str,
) -> Option<mc_protocol::packets::play::BlockEntityData> {
    let d = data(ctx, p, id)?;
    if !crate::command_blocks::may_use(ctx, uuid, game_mode) {
        return None;
    }
    Some(mc_protocol::packets::play::BlockEntityData {
        position: p,
        kind: ctx.world.block_entity_kind(id)?,
        data: d,
    })
}

fn tell(ctx: &ServerContext, p: Position, id: &str) {
    if let (Some(kind), Some(d)) = (ctx.world.block_entity_kind(id), data(ctx, p, id))
        && let Some(data) = crate::world::client_data(id, &d)
    {
        let _ = ctx.events.send(Event::BlockEntity {
            position: p,
            kind,
            data,
        });
    }
}

// -- Jigsaw ---------------------------------------------------------------

/// The jigsaw window's settings.
#[allow(clippy::too_many_arguments)]
pub fn set_jigsaw(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    name: &str,
    target: &str,
    pool: &str,
    final_state: &str,
    joint: &str,
    selection_priority: i32,
    placement_priority: i32,
) {
    if !crate::command_blocks::may_use(ctx, uuid, game_mode) {
        return;
    }
    let Some(mut d) = data(ctx, p, JIGSAW) else {
        return;
    };
    d.insert("name", text(name));
    d.insert("target", text(target));
    d.insert("pool", text(pool));
    d.insert("final_state", text(final_state));
    d.insert(
        "joint",
        text(if joint == "aligned" { "aligned" } else { "rollable" }),
    );
    d.insert("selection_priority", Tag::Int(selection_priority));
    d.insert("placement_priority", Tag::Int(placement_priority));
    store(ctx, p, JIGSAW, d);
    tell(ctx, p, JIGSAW);
}

/// "Generate" in the jigsaw window: grows `levels` levels of the pool from
/// the block, joining the pieces named by its target. Returns what to
/// tell the player, if anything.
pub fn generate_jigsaw(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    levels: i32,
) -> Vec<Tag> {
    if !crate::command_blocks::may_use(ctx, uuid, game_mode) {
        return Vec::new();
    }
    let Some(d) = data(ctx, p, JIGSAW) else {
        return Vec::new();
    };
    let (pool, target) = (string(&d, "pool"), string(&d, "target"));
    // The piece joins on the side the block faces.
    let blocks = &ctx.data.blocks;
    let front = ctx
        .world
        .block(p, blocks)
        .and_then(|s| blocks.property(s, "orientation").map(str::to_owned))
        .and_then(|o| mc_tick::Dir::from_name(o.split('_').next().unwrap_or("north")))
        .unwrap_or(mc_tick::Dir::North);
    let (dx, dy, dz) = front.offset();
    let at = Position::new(p.x + dx, p.y + dy, p.z + dz);
    match crate::commands::place::place_jigsaw(ctx, &pool, &target, levels.clamp(1, 20), at) {
        Ok((changes, entities)) => {
            apply(ctx, changes, entities);
            Vec::new()
        }
        Err(true) => vec![translate(
            "commands.place.jigsaw.invalid",
            vec![Tag::String(pool)],
        )],
        Err(false) => vec![translate("commands.place.jigsaw.failed", Vec::new())],
    }
}

fn apply(ctx: &ServerContext, changes: Vec<(Position, u32)>, entities: Vec<(Position, Compound)>) {
    let mut out = crate::commands::Output::default();
    crate::commands::world::apply(ctx, changes, false, &mut out);
    for (p, nbt) in entities {
        if let Some(state) = ctx.world.block(p, &ctx.data.blocks) {
            crate::commands::world::put_block_data(ctx, p, state, nbt);
        }
    }
    for event in out.events {
        let _ = ctx.events.send(event);
    }
}

// -- Structure block -----------------------------------------------------

fn rotation(name: &str) -> Rotation {
    match name {
        "CLOCKWISE_90" => Rotation(1),
        "CLOCKWISE_180" => Rotation(2),
        "COUNTERCLOCKWISE_90" => Rotation(3),
        _ => Rotation(0),
    }
}

fn mirror(name: &str) -> Mirror {
    match name {
        "LEFT_RIGHT" => Mirror::LeftRight,
        "FRONT_BACK" => Mirror::FrontBack,
        _ => Mirror::None,
    }
}

/// `namespace:path` of a structure's name, if it is a valid one.
fn template_id(name: &str) -> Option<String> {
    let id = if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    };
    let (ns, path) = id.split_once(':')?;
    let ok = |s: &str, slash: bool| {
        !s.is_empty()
            && s.chars().all(|c| {
                c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, '_' | '-' | '.')
                    || (slash && c == '/')
            })
    };
    (ok(ns, false) && ok(path, true) && !path.contains("..")).then_some(id)
}

/// Where a saved structure's file lives.
fn file_of(ctx: &ServerContext, id: &str) -> Option<std::path::PathBuf> {
    let (ns, path) = id.split_once(':')?;
    Some(
        ctx.world
            .folder()?
            .join("generated")
            .join(ns)
            .join("structures")
            .join(format!("{path}.nbt")),
    )
}

/// The structure `id`: one a structure block saved, else the data pack's.
fn load(ctx: &ServerContext, id: &str) -> Option<std::sync::Arc<Template>> {
    let features = crate::commands::place::features(ctx)?;
    if let Some(path) = file_of(ctx, id)
        && let Some(t) = Template::load(&path, &features.facts)
    {
        return Some(std::sync::Arc::new(t));
    }
    features.structures.template(id)
}

/// A state as a palette entry.
fn palette_entry(ctx: &ServerContext, state: u32) -> Compound {
    let mut e = Compound::new();
    let described = ctx.data.blocks.describe(state).unwrap_or("minecraft:air[]");
    let (name, props) = described.split_once('[').unwrap_or((described, ""));
    e.insert("Name", text(name));
    let mut properties = Compound::new();
    for pair in props.trim_end_matches(']').split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            properties.insert(k, text(v));
        }
    }
    if !properties.is_empty() {
        e.insert("Properties", Tag::Compound(properties));
    }
    e
}

/// The template of the box from `min` of `size`, as the file keeps it.
fn capture(ctx: &ServerContext, min: Position, size: [i32; 3]) -> Compound {
    let blocks = &ctx.data.blocks;
    let mut palette: Vec<Compound> = Vec::new();
    let mut index: std::collections::HashMap<u32, i32> = std::collections::HashMap::new();
    let mut list = Vec::new();
    let void = blocks.default_state("minecraft:structure_void");
    for y in 0..size[1] {
        for z in 0..size[2] {
            for x in 0..size[0] {
                let p = Position::new(min.x + x, min.y + y, min.z + z);
                let Some(state) = ctx.world.block(p, blocks) else {
                    continue;
                };
                if Some(state) == void {
                    continue;
                }
                let i = *index.entry(state).or_insert_with(|| {
                    palette.push(palette_entry(ctx, state));
                    palette.len() as i32 - 1
                });
                let mut b = Compound::new();
                b.insert("pos", Tag::List(vec![Tag::Int(x), Tag::Int(y), Tag::Int(z)]));
                b.insert("state", Tag::Int(i));
                if let Some(be) = ctx.world.block_entity(p, blocks) {
                    let mut nbt = be.data.clone();
                    nbt.insert("id", text(&be.id));
                    b.insert("nbt", Tag::Compound(nbt));
                }
                list.push(Tag::Compound(b));
            }
        }
    }
    let mut root = Compound::new();
    root.insert(
        "size",
        Tag::List(size.iter().map(|&v| Tag::Int(v)).collect()),
    );
    root.insert(
        "palette",
        Tag::List(palette.into_iter().map(Tag::Compound).collect()),
    );
    root.insert("blocks", Tag::List(list));
    root.insert("entities", Tag::List(Vec::new()));
    root.insert("DataVersion", Tag::Int(ctx.data.version.world_version));
    root
}

fn write_file(path: &std::path::Path, root: Compound) -> bool {
    use std::io::Write as _;
    let mut bytes = Vec::new();
    if mc_nbt::write_named("", &Tag::Compound(root), &mut bytes).is_err() {
        return false;
    }
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        return false;
    }
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&bytes).is_ok()
        && z.finish()
            .is_ok_and(|zipped| std::fs::write(path, zipped).is_ok())
}

/// The box a structure block frames: its corner and size.
fn frame(p: Position, d: &Compound) -> (Position, [i32; 3]) {
    (
        Position::new(
            p.x + int(d, "posX"),
            p.y + int(d, "posY"),
            p.z + int(d, "posZ"),
        ),
        [int(d, "sizeX"), int(d, "sizeY"), int(d, "sizeZ")],
    )
}

fn save(ctx: &ServerContext, p: Position, d: &Compound) -> Tag {
    let name = string(d, "name");
    let Some(id) = template_id(&name) else {
        return translate("structure_block.invalid_structure_name", vec![text(&name)]);
    };
    let (min, size) = frame(p, d);
    let ok = size.iter().all(|&s| s >= 1)
        && file_of(ctx, &id).is_some_and(|path| write_file(&path, capture(ctx, min, size)));
    translate(
        if ok {
            "structure_block.save_success"
        } else {
            "structure_block.save_failure"
        },
        vec![text(&name)],
    )
}

fn place(ctx: &ServerContext, p: Position, d: &mut Compound) -> Tag {
    let name = string(d, "name");
    let Some(id) = template_id(&name) else {
        return translate("structure_block.invalid_structure_name", vec![text(&name)]);
    };
    let Some(t) = load(ctx, &id) else {
        return translate("structure_block.load_not_found", vec![text(&name)]);
    };
    let (at, size) = frame(p, d);
    let want = [t.size.x, t.size.y, t.size.z];
    if !flag(d, "strict") && size != want {
        // The block takes the structure's size first.
        d.insert("sizeX", Tag::Int(want[0]));
        d.insert("sizeY", Tag::Int(want[1]));
        d.insert("sizeZ", Tag::Int(want[2]));
        return translate("structure_block.load_prepare", vec![text(&name)]);
    }
    let integrity = match d.get("integrity") {
        Some(Tag::Float(f)) => f.clamp(0.0, 1.0),
        _ => 1.0,
    };
    let seed = match d.get("seed") {
        Some(Tag::Long(s)) => *s,
        _ => 0,
    };
    let mut settings = Settings {
        rotation: rotation(&string(d, "rotation")),
        mirror: mirror(&string(d, "mirror")),
        ..Settings::default()
    };
    if integrity < 1.0 {
        settings.processors.push(std::sync::Arc::new(Processor::BlockRot {
            integrity,
            rottable: None,
        }));
        settings.seed = Some(seed);
    }
    match crate::commands::place::place_template(ctx, &t, at, &settings, seed) {
        Some((changes, entities)) => {
            apply(ctx, changes, entities);
            translate("structure_block.load_success", vec![text(&name)])
        }
        None => translate("structure_block.load_not_found", vec![text(&name)]),
    }
}

/// Finds the corner blocks named like this save block and frames the box
/// between them. `true` if it did.
fn detect(ctx: &ServerContext, p: Position, d: &mut Compound) -> bool {
    let name = string(d, "name");
    if name.is_empty() {
        return false;
    }
    let (mut lo, mut hi) = ([p.x, p.y, p.z], [p.x, p.y, p.z]);
    let mut found = false;
    for be in ctx.world.block_entities_of(STRUCTURE) {
        let near = (be.x - p.x).abs() <= DETECT
            && (be.y - p.y).abs() <= DETECT
            && (be.z - p.z).abs() <= DETECT;
        if !near || (be.x, be.y, be.z) == (p.x, p.y, p.z) {
            continue;
        }
        if string(&be.data, "mode") == "CORNER" && string(&be.data, "name") == name {
            found = true;
            for (i, v) in [be.x, be.y, be.z].into_iter().enumerate() {
                lo[i] = lo[i].min(v);
                hi[i] = hi[i].max(v);
            }
        }
    }
    let span = |i: usize| hi[i] - lo[i] + 1;
    if !found || (0..3).any(|i| span(i) <= 1) {
        return false;
    }
    let at = [p.x, p.y, p.z];
    for (i, (pos, size)) in [("posX", "sizeX"), ("posY", "sizeY"), ("posZ", "sizeZ")]
        .into_iter()
        .enumerate()
    {
        d.insert(pos, Tag::Int(lo[i] - at[i] + 1));
        d.insert(size, Tag::Int(span(i) - 2));
    }
    true
}

/// The structure block window's settings and the button pressed
/// (`action`: 0 update, 1 save, 2 load, 3 detect size). Returns what the
/// player is told.
#[allow(clippy::too_many_arguments)]
pub fn set_structure(
    ctx: &ServerContext,
    uuid: u128,
    game_mode: i32,
    p: Position,
    action: i32,
    mode: i32,
    name: &str,
    offset: [i8; 3],
    size: [u8; 3],
    mirror_n: i32,
    rotation_n: i32,
    metadata: &str,
    integrity: f32,
    seed: i64,
    flags: u8,
) -> Vec<Tag> {
    if !crate::command_blocks::may_use(ctx, uuid, game_mode) {
        return Vec::new();
    }
    update_structure(
        ctx, p, action, mode, name, offset, size, mirror_n, rotation_n, metadata, integrity, seed,
        flags,
    )
}

/// [`set_structure`] for someone who may.
#[allow(clippy::too_many_arguments)]
fn update_structure(
    ctx: &ServerContext,
    p: Position,
    action: i32,
    mode: i32,
    name: &str,
    offset: [i8; 3],
    size: [u8; 3],
    mirror_n: i32,
    rotation_n: i32,
    metadata: &str,
    integrity: f32,
    seed: i64,
    flags: u8,
) -> Vec<Tag> {
    let Some(mut d) = data(ctx, p, STRUCTURE) else {
        return Vec::new();
    };
    let mode_s = ["SAVE", "LOAD", "CORNER", "DATA"][mode.clamp(0, 3) as usize];
    d.insert("name", text(name));
    d.insert("metadata", text(metadata));
    for (k, v) in ["posX", "posY", "posZ"].into_iter().zip(offset) {
        d.insert(k, Tag::Int(i32::from(v).clamp(-LIMIT, LIMIT)));
    }
    for (k, v) in ["sizeX", "sizeY", "sizeZ"].into_iter().zip(size) {
        d.insert(k, Tag::Int(i32::from(v).clamp(0, LIMIT)));
    }
    d.insert(
        "mirror",
        text(["NONE", "LEFT_RIGHT", "FRONT_BACK"][mirror_n.clamp(0, 2) as usize]),
    );
    d.insert(
        "rotation",
        text(["NONE", "CLOCKWISE_90", "CLOCKWISE_180", "COUNTERCLOCKWISE_90"][rotation_n.clamp(0, 3) as usize]),
    );
    d.insert("mode", text(mode_s));
    d.insert("integrity", Tag::Float(integrity.clamp(0.0, 1.0)));
    d.insert("seed", Tag::Long(seed));
    d.insert("ignoreEntities", Tag::Byte(i8::from(flags & 1 != 0)));
    d.insert("showair", Tag::Byte(i8::from(flags & 2 != 0)));
    d.insert("showboundingbox", Tag::Byte(i8::from(flags & 4 != 0)));
    d.insert("strict", Tag::Byte(i8::from(flags & 8 != 0)));
    // The block shows its mode.
    let blocks = &ctx.data.blocks;
    if let Some(state) = ctx.world.block(p, blocks)
        && let Some(new) = blocks.with_property(state, "mode", &mode_s.to_lowercase())
        && new != state
    {
        let mut level = WorldLevel::new(ctx);
        level.set_block(p, new);
        let changes = level.into_changes();
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
    let mut said = Vec::new();
    match action {
        1 if mode_s == "SAVE" => said.push(save(ctx, p, &d)),
        2 if mode_s == "LOAD" => said.push(place(ctx, p, &mut d)),
        3 if mode_s == "SAVE" => said.push(translate(
            if detect(ctx, p, &mut d) {
                "structure_block.size_success"
            } else {
                "structure_block.size_failure"
            },
            vec![text(&string(&d, "name"))],
        )),
        _ => {}
    }
    store(ctx, p, STRUCTURE, d);
    tell(ctx, p, STRUCTURE);
    said
}

/// A structure block's power changed: a save or load block starts when the
/// power comes.
pub fn power(ctx: &ServerContext, p: Position, powered: bool) {
    let Some(mut d) = data(ctx, p, STRUCTURE) else {
        return;
    };
    if flag(&d, "powered") == powered {
        return;
    }
    d.insert("powered", Tag::Byte(i8::from(powered)));
    if powered {
        match string(&d, "mode").as_str() {
            "SAVE" => {
                save(ctx, p, &d);
            }
            "LOAD" => {
                place(ctx, p, &mut d);
            }
            _ => {}
        }
    }
    store(ctx, p, STRUCTURE, d);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(world: &std::path::Path) -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        let mut config = crate::Config::new(dir);
        config.world = Some(world.to_owned());
        ServerContext::new(config, data).ok()
    }

    fn put(ctx: &ServerContext, p: Position, block: &str, props: &[(&str, &str)]) {
        let s = ctx.data.blocks.state_id(block, props).unwrap();
        WorldLevel::new(ctx).set_block(p, s);
    }

    fn name_at(ctx: &ServerContext, p: Position) -> String {
        let s = ctx.world.block(p, &ctx.data.blocks).unwrap();
        ctx.data.blocks.block_name(s).unwrap().to_owned()
    }

    #[test]
    fn saves_a_box_and_loads_it_elsewhere() {
        let dir = std::env::temp_dir().join(format!("mc-structure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(ctx) = context(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, -60, 0);
        put(&ctx, at, "minecraft:structure_block", &[("mode", "save")]);
        placed(&ctx, at, ctx.world.block(at, &ctx.data.blocks).unwrap());
        // Two blocks to keep, in a 2×1×1 box one block east of the structure block.
        put(&ctx, Position::new(1, -60, 0), "minecraft:gold_block", &[]);
        put(&ctx, Position::new(2, -60, 0), "minecraft:oak_stairs", &[("facing", "north")]);
        // Not an operator: nothing happens.
        assert!(
            set_structure(&ctx, 77, 1, at, 1, 0, "box", [1, 0, 0], [2, 1, 1], 0, 0, "", 1.0, 0, 1)
                .is_empty()
        );
        let said = update_structure(&ctx, at, 1, 0, "box", [1, 0, 0], [2, 1, 1], 0, 0, "", 1.0, 0, 1);
        assert_eq!(said.len(), 1);
        let file = dir.join("generated/minecraft/structures/box.nbt");
        assert!(file.exists(), "the structure file is written");
        // Load it two blocks higher up, mirrored left-right is not needed.
        let load = Position::new(10, -60, 0);
        put(&ctx, load, "minecraft:structure_block", &[("mode", "load")]);
        placed(&ctx, load, ctx.world.block(load, &ctx.data.blocks).unwrap());
        // The size differs at first: the block learns it.
        let first = update_structure(&ctx, load, 2, 1, "box", [0, 3, 0], [0, 0, 0], 0, 0, "", 1.0, 0, 1);
        assert_eq!(first.len(), 1);
        let second = update_structure(&ctx, load, 2, 1, "box", [0, 3, 0], [2, 1, 1], 0, 0, "", 1.0, 0, 1);
        assert_eq!(second.len(), 1);
        assert_eq!(name_at(&ctx, Position::new(10, -57, 0)), "minecraft:gold_block");
        assert_eq!(name_at(&ctx, Position::new(11, -57, 0)), "minecraft:oak_stairs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detects_the_box_between_corners() {
        let dir = std::env::temp_dir().join(format!("mc-structure-d-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(ctx) = context(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let save_at = Position::new(0, -60, 0);
        put(&ctx, save_at, "minecraft:structure_block", &[("mode", "save")]);
        placed(&ctx, save_at, ctx.world.block(save_at, &ctx.data.blocks).unwrap());
        // Corners at (0,-60,0) itself and (5,-57,4).
        let corner = Position::new(5, -57, 4);
        put(&ctx, corner, "minecraft:structure_block", &[("mode", "corner")]);
        placed(&ctx, corner, ctx.world.block(corner, &ctx.data.blocks).unwrap());
        update_structure(&ctx, corner, 0, 2, "frame", [0; 3], [0; 3], 0, 0, "", 1.0, 0, 0);
        let said = update_structure(&ctx, save_at, 3, 0, "frame", [0; 3], [0; 3], 0, 0, "", 1.0, 0, 0);
        assert_eq!(said.len(), 1);
        let d = data(&ctx, save_at, STRUCTURE).unwrap();
        // The box between the blocks, without them: 4×2×3, starting one in.
        assert_eq!(
            (int(&d, "posX"), int(&d, "posY"), int(&d, "posZ")),
            (1, 1, 1)
        );
        assert_eq!(
            (int(&d, "sizeX"), int(&d, "sizeY"), int(&d, "sizeZ")),
            (4, 2, 3)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_jigsaw_block_is_blank_and_grows_a_pool() {
        let dir = std::env::temp_dir().join(format!("mc-structure-j-{}", std::process::id()));
        let Some(ctx) = context(&dir) else {
            eprintln!("skipped: no game data");
            return;
        };
        let at = Position::new(0, -60, 0);
        put(&ctx, at, "minecraft:jigsaw", &[("orientation", "up_north")]);
        placed(&ctx, at, ctx.world.block(at, &ctx.data.blocks).unwrap());
        let d = data(&ctx, at, JIGSAW).unwrap();
        assert_eq!(string(&d, "pool"), "minecraft:empty");
        assert_eq!(string(&d, "joint"), "rollable");
        // Not an operator: no window and no change.
        assert!(open(&ctx, 5, 1, at, JIGSAW).is_none());
        set_jigsaw(&ctx, 5, 1, at, "a", "b", "c", "minecraft:air", "aligned", 1, 2);
        assert_eq!(string(&data(&ctx, at, JIGSAW).unwrap(), "name"), "minecraft:empty");
        // A pool that is not there fails.
        let bad = crate::commands::place::place_jigsaw(&ctx, "minecraft:nope", "minecraft:empty", 1, at);
        assert!(matches!(bad, Err(true)));
        // A real one places pieces.
        let pool = "minecraft:village/plains/town_centers";
        match crate::commands::place::place_jigsaw(&ctx, pool, "minecraft:empty", 1, Position::new(40, -60, 40)) {
            Ok((changes, _)) => assert!(!changes.is_empty()),
            Err(invalid) => eprintln!("pool unavailable here (invalid pool: {invalid})"),
        }
    }
}
