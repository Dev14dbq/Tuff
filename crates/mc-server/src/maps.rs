//! Map data as worlds keep it (`data/minecraft/maps/<id>.dat`, the last id
//! given out in `data/minecraft/maps/last_id.dat`; seen in a vanilla 26.3
//! world), for what the cartography table does to maps.

use std::path::PathBuf;

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::ItemStack;

use crate::context::ServerContext;

/// What the cartography table needs to know of a map.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MapState {
    pub scale: i32,
    pub locked: bool,
}

fn dir(ctx: &ServerContext) -> Option<PathBuf> {
    Some(ctx.world.folder()?.join("data/minecraft/maps"))
}

fn map_id(stack: &ItemStack) -> Option<i32> {
    match stack.components.get("map_id")? {
        Tag::Int(id) => Some(*id),
        _ => None,
    }
}

fn read(ctx: &ServerContext, id: i32) -> Option<Compound> {
    let path = dir(ctx)?.join(format!("{id}.dat"));
    crate::world::read_nbt_gz(&path).ok()
}

fn int(c: &Compound, key: &str) -> i32 {
    match c.get(key) {
        Some(Tag::Byte(v)) => i32::from(*v),
        Some(Tag::Int(v)) => *v,
        _ => 0,
    }
}

/// The scale and lock of the map a stack shows; `None` if it has no data.
pub fn state(ctx: &ServerContext, stack: &ItemStack) -> Option<MapState> {
    let root = read(ctx, map_id(stack)?)?;
    let Some(Tag::Compound(data)) = root.get("data") else {
        return None;
    };
    Some(MapState {
        scale: int(data, "scale"),
        locked: int(data, "locked") != 0,
    })
}

/// A fresh map id (the counter is kept on disk).
fn next_id(ctx: &ServerContext) -> Option<i32> {
    let path = dir(ctx)?.join("last_id.dat");
    let mut root = crate::world::read_nbt_gz(&path).unwrap_or_default();
    let last = match root.get("data") {
        Some(Tag::Compound(d)) => match d.get("map") {
            Some(Tag::Int(v)) => Some(*v),
            _ => None,
        },
        _ => None,
    };
    let id = last.map_or(0, |v| v + 1);
    let mut data = Compound::new();
    data.insert("map", Tag::Int(id));
    root.insert("data", Tag::Compound(data));
    root.insert("DataVersion", Tag::Int(ctx.data.version.world_version));
    std::fs::create_dir_all(dir(ctx)?).ok()?;
    crate::world::write_nbt_gz(&path, &root).ok()?;
    Some(id)
}

/// Applies a cartography table's `map_post_processing` to a stack: a
/// locked copy, or a map one scale out (centred on the new grid, blank),
/// each under a new id. Whether the stack changed.
pub fn post_process(ctx: &ServerContext, stack: &mut ItemStack) -> bool {
    let Some(Tag::String(kind)) = stack.components.reset("map_post_processing") else {
        return false;
    };
    let (Some(id), Some(mut root)) = (map_id(stack), map_id(stack).and_then(|i| read(ctx, i)))
    else {
        return true;
    };
    let Some(Tag::Compound(mut data)) = root.remove("data") else {
        return true;
    };
    match kind.as_str() {
        "lock" => {
            data.insert("locked", Tag::Byte(1));
        }
        "scale" => {
            let scale = (int(&data, "scale") + 1).min(4);
            let size = 128 * (1 << scale);
            let centre = |v: i32| (v + 64).div_euclid(size) * size + size / 2 - 64;
            let (x, z) = (int(&data, "xCenter"), int(&data, "zCenter"));
            data.insert("scale", Tag::Byte(scale as i8));
            data.insert("xCenter", Tag::Int(centre(x)));
            data.insert("zCenter", Tag::Int(centre(z)));
            data.insert("colors", Tag::ByteArray(vec![0; 128 * 128]));
            data.remove("banners");
            data.remove("frames");
        }
        _ => return true,
    }
    let _ = id;
    let Some(new) = next_id(ctx) else {
        return true;
    };
    root.insert("data", Tag::Compound(data));
    if let Some(dir) = dir(ctx) {
        let _ = crate::world::write_nbt_gz(&dir.join(format!("{new}.dat")), &root);
    }
    stack.components.set("map_id", Tag::Int(new));
    true
}
