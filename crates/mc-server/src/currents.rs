//! Flowing water and lava carry what is in them (minecraft.wiki: "Water" →
//! "Flow"): dropped items, mobs and the like drift the way the fluid runs.
//!
//! The way a fluid runs at a block is the sum, over the four sides, of how
//! much lower its surface is there than here (towards the lower side); a
//! falling fluid beside a wall runs straight down. Everything whose box
//! reaches into the fluid is pushed by the average of the flows of the
//! blocks it touches, at 0.014 a tick for water (a fiftieth of it less for
//! lava: 0.0023 in the Overworld, 0.007 in the Nether).

use crate::context::ServerContext;

/// The fluid at a block: lava or not, whether it falls, and how high its
/// own surface stands (ninths of a block).
fn fluid(ctx: &ServerContext, state: u32) -> Option<(bool, bool, f64)> {
    let blocks = &ctx.data.blocks;
    let lava = match blocks.block_name(state)? {
        "minecraft:water" => false,
        "minecraft:lava" => true,
        "minecraft:bubble_column"
        | "minecraft:kelp"
        | "minecraft:kelp_plant"
        | "minecraft:seagrass"
        | "minecraft:tall_seagrass" => return Some((false, false, 8.0 / 9.0)),
        _ if blocks.property(state, "waterlogged") == Some("true") => {
            return Some((false, false, 8.0 / 9.0));
        }
        _ => return None,
    };
    let level: i32 = blocks
        .property(state, "level")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let falling = level >= 8;
    let amount = if level == 0 || falling { 8 } else { 8 - level };
    Some((lava, falling, f64::from(amount) / 9.0))
}

fn state_at(ctx: &ServerContext, x: i32, y: i32, z: i32) -> Option<u32> {
    crate::hits::block_at(ctx, [x, y, z])
}

/// The way the fluid at `(x, y, z)` runs: a unit vector, or zero.
fn flow(ctx: &ServerContext, lava: bool, falling: bool, own: f64, at: [i32; 3]) -> [f64; 3] {
    let same_or_empty = |s: Option<u32>| match s.and_then(|s| fluid(ctx, s)) {
        None => true,
        Some((l, _, _)) => l == lava,
    };
    let height = |s: Option<u32>| {
        s.and_then(|s| fluid(ctx, s))
            .filter(|f| f.0 == lava)
            .map_or(0.0, |f| f.2)
    };
    let solid = |s: Option<u32>| s.is_some_and(|s| ctx.rules.blocks_motion(s));
    let mut v = [0.0_f64; 3];
    for (dx, dz) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
        let n = state_at(ctx, at[0] + dx, at[1], at[2] + dz);
        if !same_or_empty(n) {
            continue;
        }
        let mut f = height(n);
        let mut diff = 0.0;
        if f == 0.0 {
            if !solid(n) {
                let below = state_at(ctx, at[0] + dx, at[1] - 1, at[2] + dz);
                if same_or_empty(below) {
                    f = height(below);
                    if f > 0.0 {
                        diff = own - (f - 8.0 / 9.0);
                    }
                }
            }
        } else {
            diff = own - f;
        }
        if diff != 0.0 {
            v[0] += f64::from(dx) * diff;
            v[2] += f64::from(dz) * diff;
        }
    }
    let unit = |v: [f64; 3]| {
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if len < 1e-9 {
            [0.0; 3]
        } else {
            [v[0] / len, v[1] / len, v[2] / len]
        }
    };
    let mut out = unit(v);
    if falling {
        for (dx, dz) in [(0, -1), (0, 1), (-1, 0), (1, 0)] {
            let side = state_at(ctx, at[0] + dx, at[1], at[2] + dz);
            let above = state_at(ctx, at[0] + dx, at[1] + 1, at[2] + dz);
            if solid(side) || solid(above) {
                out = unit([out[0], out[1] - 6.0, out[2]]);
                break;
            }
        }
    }
    out
}

/// Pushes `vel` of something with this box (`min`, `max`) by the fluids it
/// touches. Returns whether it is in any.
pub fn push(ctx: &ServerContext, min: [f64; 3], max: [f64; 3], vel: &mut [f64; 3]) -> bool {
    let nether = ctx.world.dimension == "minecraft:the_nether";
    let mut inside = false;
    for lava in [false, true] {
        let mut sum = [0.0_f64; 3];
        let mut count = 0;
        let range = |i: usize| {
            (
                (min[i] + 0.001).floor() as i32,
                (max[i] - 0.001).ceil() as i32 - 1,
            )
        };
        let ((x0, x1), (y0, y1), (z0, z1)) = (range(0), range(1), range(2));
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    let Some((l, falling, own)) =
                        state_at(ctx, x, y, z).and_then(|s| fluid(ctx, s))
                    else {
                        continue;
                    };
                    if l != lava {
                        continue;
                    }
                    // The same fluid above fills the block to the top.
                    let full = state_at(ctx, x, y + 1, z)
                        .and_then(|s| fluid(ctx, s))
                        .is_some_and(|f| f.0 == lava);
                    let top = f64::from(y) + if full { 1.0 } else { own };
                    if top < min[1] + 0.001 {
                        continue;
                    }
                    inside = true;
                    let depth = top - (min[1] + 0.001);
                    let mut f = flow(ctx, lava, falling, own, [x, y, z]);
                    if depth < 0.4 {
                        for v in &mut f {
                            *v *= depth;
                        }
                    }
                    for (s, v) in sum.iter_mut().zip(f) {
                        *s += v;
                    }
                    count += 1;
                }
            }
        }
        let len = (sum[0] * sum[0] + sum[1] * sum[1] + sum[2] * sum[2]).sqrt();
        if count == 0 || len < 1e-9 {
            continue;
        }
        let scale = match (lava, nether) {
            (false, _) => 0.014,
            (true, true) => 0.007,
            (true, false) => 0.002_333_333_333_333_333_5,
        };
        // Averaged, then a unit push.
        let mut push = [
            sum[0] / len * scale,
            sum[1] / len * scale,
            sum[2] / len * scale,
        ];
        let size = (push[0] * push[0] + push[1] * push[1] + push[2] * push[2]).sqrt();
        if vel[0].abs() < 0.003 && vel[2].abs() < 0.003 && size < 0.0045 {
            for p in &mut push {
                *p = *p / size * 0.0045;
            }
        }
        for (v, p) in vel.iter_mut().zip(push) {
            *v += p;
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::level::WorldLevel;

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    #[test]
    fn flowing_water_pushes_downstream_and_still_water_does_not() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let water = |level: u32| {
            ctx.data
                .blocks
                .state_id("minecraft:water", &[("level", &level.to_string())])
                .unwrap()
        };
        // A stream running east, getting shallower: levels 1, 2, 3 at x = 0, 1, 2.
        let mut level = WorldLevel::new(&ctx);
        for (x, l) in [(0, 1), (1, 2), (2, 3)] {
            level.set_block(mc_protocol::Position::new(x, -60, 0), water(l));
        }
        drop(level);
        let (min, max) = ([1.3, -60.0, 0.3], [1.7, -59.6, 0.7]);
        let mut vel = [0.0; 3];
        assert!(push(&ctx, min, max, &mut vel));
        assert!(vel[0] > 0.0, "{vel:?}");
        assert!(vel[2].abs() < 1e-9);
        // A single source with nothing around runs nowhere.
        let mut level = WorldLevel::new(&ctx);
        level.set_block(mc_protocol::Position::new(20, -60, 0), water(0));
        drop(level);
        let mut still = [0.0; 3];
        assert!(push(
            &ctx,
            [20.3, -60.0, 0.3],
            [20.7, -59.6, 0.7],
            &mut still
        ));
        assert_eq!(still, [0.0; 3]);
        // Out of the water nothing happens.
        let mut dry = [0.0; 3];
        assert!(!push(
            &ctx,
            [30.3, -60.0, 0.3],
            [30.7, -59.6, 0.7],
            &mut dry
        ));
    }
}
