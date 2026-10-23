//! Paintings (minecraft.wiki, "Painting"): the variants come from the
//! `painting_variant` registry, each so many blocks wide and high. A
//! painting is put on the block face clicked; of the variants in
//! `#minecraft:placeable` that fit there, one of the largest is picked at
//! random. It hangs as long as every block behind it is solid, its space is
//! free and no other painting or frame is in the way.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use mc_entity::extra::{Decor, Hung};
use mc_entity::{Entity, Kind};

use crate::blocks::FACES;
use crate::context::ServerContext;
use crate::hits;
use crate::tick::lock;

/// A painting variant: its name and size in blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub width: i32,
    pub height: i32,
}

/// Every variant, in network order (the registry sorted by id).
pub fn variants(ctx: &ServerContext) -> Arc<Vec<Variant>> {
    static CACHE: OnceLock<std::sync::Mutex<HashMap<std::path::PathBuf, Arc<Vec<Variant>>>>> =
        OnceLock::new();
    let pack = ctx.data.data_pack();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = lock(cache).get(&pack.root) {
        return v.clone();
    }
    let size = |name: &str, key: &str| {
        pack.read_entry("minecraft:painting_variant", name)
            .ok()
            .and_then(|v| v[key].as_i64())
            .map_or(1, |n| n.clamp(1, 16) as i32)
    };
    let list: Arc<Vec<Variant>> = Arc::new(
        pack.registry_entries("minecraft:painting_variant")
            .unwrap_or_default()
            .into_iter()
            .map(|name| Variant {
                width: size(&name, "width"),
                height: size(&name, "height"),
                name,
            })
            .collect(),
    );
    lock(cache).insert(pack.root.clone(), list.clone());
    list
}

/// A variant by name.
pub fn variant(ctx: &ServerContext, name: &str) -> Option<Variant> {
    let full = mc_command::args::full_id(name);
    variants(ctx).iter().find(|v| v.name == full).cloned()
}

/// The network id of a variant (its place in the registry).
pub fn network_id(ctx: &ServerContext, name: &str) -> Option<i32> {
    let full = mc_command::args::full_id(name);
    variants(ctx)
        .iter()
        .position(|v| v.name == full)
        .map(|i| i as i32)
}

/// Offset of a painting's centre from its block's: half a block for an even
/// size.
fn half(n: i32) -> f64 {
    if n % 2 == 0 { 0.5 } else { 0.0 }
}

/// The way along the wall a painting grows, to the left of `facing` as one
/// looks at it from the front (`x`, `z`).
fn along(facing: u8) -> (i32, i32) {
    let (dx, _, dz) = FACES[usize::from(facing)];
    (dz, -dx)
}

/// The centre of a painting of `size` whose own block is `cell`, on the wall
/// it faces away from.
pub fn centre(cell: [i32; 3], facing: u8, size: (i32, i32)) -> [f64; 3] {
    let (dx, _, dz) = FACES[usize::from(facing)];
    let (ax, az) = along(facing);
    let (w, h) = (half(size.0), half(size.1));
    [
        f64::from(cell[0]) + 0.5 - f64::from(dx) * 0.46875 + f64::from(ax) * w,
        f64::from(cell[1]) + 0.5 + h,
        f64::from(cell[2]) + 0.5 - f64::from(dz) * 0.46875 + f64::from(az) * w,
    ]
}

/// The painting's own block from its centre.
pub fn cell_of(pos: [f64; 3], facing: u8, size: (i32, i32)) -> [i32; 3] {
    let (dx, _, dz) = FACES[usize::from(facing)];
    let (ax, az) = along(facing);
    let (w, h) = (half(size.0), half(size.1));
    [
        (pos[0] + f64::from(dx) * 0.46875 - f64::from(ax) * w - 0.5).round() as i32,
        (pos[1] - h - 0.5).round() as i32,
        (pos[2] + f64::from(dz) * 0.46875 - f64::from(az) * w - 0.5).round() as i32,
    ]
}

/// The blocks a painting covers.
fn cells(cell: [i32; 3], facing: u8, size: (i32, i32)) -> Vec<[i32; 3]> {
    let (ax, az) = along(facing);
    let mut out = Vec::new();
    for u in -(size.0 - 1) / 2..=size.0 / 2 {
        for v in -(size.1 - 1) / 2..=size.1 / 2 {
            out.push([cell[0] + ax * u, cell[1] + v, cell[2] + az * u]);
        }
    }
    out
}

fn painting(cell: [i32; 3], facing: u8, v: &Variant) -> Hung {
    let (dx, dy, dz) = FACES[usize::from(facing)];
    Hung {
        decor: Decor::Painting {
            variant: v.name.clone(),
            size: (v.width, v.height),
        },
        facing,
        yaw: 0.0,
        anchor: [cell[0] - dx, cell[1] - dy, cell[2] - dz],
    }
}

/// Whether a painting (`me`, if it already hangs) of `size` can hang at
/// `cell` facing `facing`: solid blocks behind all of it, nothing solid in
/// its space and no frame or painting overlapping it.
pub fn survives(
    ctx: &ServerContext,
    entities: &[Entity],
    me: Option<i32>,
    cell: [i32; 3],
    facing: u8,
    size: (i32, i32),
) -> bool {
    let (dx, dy, dz) = FACES[usize::from(facing)];
    let ok = cells(cell, facing, size)
        .into_iter()
        .all(|c| hits::stops(ctx, [c[0] - dx, c[1] - dy, c[2] - dz]) && !hits::stops(ctx, c));
    if !ok {
        return false;
    }
    let mut probe = super::entity(
        ctx,
        centre(cell, facing, size),
        Hung {
            decor: Decor::Painting {
                variant: String::new(),
                size,
            },
            facing,
            yaw: 0.0,
            anchor: [0; 3],
        },
    );
    probe.id = me.unwrap_or(i32::MIN);
    // Touching edges do not count: the box is shrunk a little.
    let mine = probe.aabb();
    !entities.iter().any(|e| {
        let Kind::Hung(h) = &e.kind else { return false };
        if e.removed
            || Some(e.id) == me
            || !matches!(h.decor, Decor::Frame(_) | Decor::Painting { .. })
        {
            return false;
        }
        let b = e.aabb();
        (0..3).all(|i| b.min[i] < mine.max[i] - 1e-4 && b.max[i] > mine.min[i] + 1e-4)
    })
}

/// A painting put at `cell` (the space in front of the wall) facing
/// `facing`: `wanted` if the item names a variant (and it fits), else one of
/// the largest placeable ones that fit. `None` if none fits.
pub fn create(
    ctx: &ServerContext,
    cell: [i32; 3],
    facing: u8,
    wanted: Option<&str>,
) -> Option<Entity> {
    let entities = lock(&ctx.entities).clone();
    let fits = |v: &Variant| survives(ctx, &entities, None, cell, facing, (v.width, v.height));
    let chosen = match wanted {
        Some(name) => Some(variant(ctx, name)?).filter(|v| fits(v))?,
        None => {
            let placeable = ctx
                .commands
                .env
                .tag("minecraft:painting_variant", "minecraft:placeable");
            let fitting: Vec<Variant> = variants(ctx)
                .iter()
                .filter(|v| placeable.contains(&v.name) && fits(v))
                .cloned()
                .collect();
            let best = fitting.iter().map(|v| v.width * v.height).max()?;
            let largest: Vec<&Variant> = fitting
                .iter()
                .filter(|v| v.width * v.height == best)
                .collect();
            let pick = (crate::util::random_u64() % largest.len() as u64) as usize;
            largest[pick].clone()
        }
    };
    let size = (chosen.width, chosen.height);
    Some(super::entity(
        ctx,
        centre(cell, facing, size),
        painting(cell, facing, &chosen),
    ))
}

/// A painting summoned or loaded with this variant: its size and centre.
pub fn hang(ctx: &ServerContext, name: &str, cell: [i32; 3], facing: u8) -> (Hung, [f64; 3]) {
    let v = variant(ctx, name).unwrap_or(Variant {
        name: mc_command::args::full_id(name),
        width: 1,
        height: 1,
    });
    let size = (v.width, v.height);
    (painting(cell, facing, &v), centre(cell, facing, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projectiles::tests::flat;
    use mc_protocol::Position;

    #[test]
    fn variants_have_sizes() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let kebab = variant(&ctx, "kebab").unwrap();
        assert_eq!((kebab.width, kebab.height), (1, 1));
        let skeleton = variant(&ctx, "minecraft:skeleton").unwrap();
        assert_eq!((skeleton.width, skeleton.height), (4, 3));
        assert!(network_id(&ctx, "minecraft:alban") < network_id(&ctx, "minecraft:aztec"));
    }

    #[test]
    fn centres_shift_for_even_sizes() {
        // Facing south (+z): it grows east (+x) and up.
        let c = centre([0, 0, 0], 3, (2, 2));
        assert!((c[0] - 1.0).abs() < 1e-9 && (c[1] - 1.0).abs() < 1e-9);
        assert!((c[2] - 0.03125).abs() < 1e-9);
        for facing in 2..6 {
            for size in [(1, 1), (2, 1), (4, 3), (3, 4)] {
                assert_eq!(
                    cell_of(centre([5, 6, 7], facing, size), facing, size),
                    [5, 6, 7]
                );
            }
        }
    }

    #[test]
    fn the_largest_painting_that_fits_is_put_up() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let stone = ctx.data.blocks.default_state("minecraft:stone").unwrap();
        // A wall 4 wide and 2 high at z = 0, facing south.
        for x in 0..4 {
            for y in -60..-58 {
                ctx.world
                    .set_block(Position::new(x, y, 0), stone, &ctx.data.blocks, &ctx.rules);
            }
        }
        let e = create(&ctx, [1, -60, 1], 3, None).unwrap();
        let Kind::Hung(h) = &e.kind else { panic!() };
        let Decor::Painting { size, variant } = &h.decor else {
            panic!()
        };
        // 4×2 fits around the clicked block (one to the west, two to the
        // east): nothing bigger does.
        assert_eq!(*size, (4, 2), "{variant}");
        assert_eq!(cell_of(e.pos, 3, *size), [1, -60, 1]);
        lock(&ctx.entities).push(e);
        // Where it hangs nothing else goes.
        assert!(create(&ctx, [2, -60, 1], 3, None).is_none());
        // An item naming a variant puts that one up, if it fits.
        assert!(create(&ctx, [20, -60, 1], 3, Some("minecraft:kebab")).is_none());
        ctx.world.set_block(
            Position::new(20, -60, 0),
            stone,
            &ctx.data.blocks,
            &ctx.rules,
        );
        let e = create(&ctx, [20, -60, 1], 3, Some("minecraft:kebab")).unwrap();
        assert!(
            matches!(&e.kind, Kind::Hung(h) if matches!(&h.decor, Decor::Painting { variant, .. } if variant == "minecraft:kebab"))
        );
    }
}
