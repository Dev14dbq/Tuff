//! Weather on the ground (minecraft.wiki, "Snow" → "Generation", "Ice",
//! "Cauldron"): each game tick every ticking chunk has a 1 in 16 chance to
//! look at one random column. Still water on top freezes where it is cold,
//! falling snow piles up a layer (up to the `max_snow_accumulation_height`
//! game rule), and the block under the top gets the rain or snow (a
//! cauldron fills).

use mc_protocol::Position;

use crate::context::{Event, ServerContext};
use crate::level::{WorldLevel, to_pos};
use crate::tick::lock;

/// One game tick of precipitation.
pub fn tick(ctx: &ServerContext) {
    let chunks = crate::random_ticks::ticking_chunks(ctx);
    if chunks.is_empty() {
        return;
    }
    let raining = lock(&ctx.weather).raining;
    let mut rng = mc_tick::rng::Rng::new(crate::util::random_u64());
    let mut level = WorldLevel::new(ctx);
    for (cx, cz) in chunks {
        if rng.below(16) != 0 {
            continue;
        }
        let (x, z) = (
            cx * 16 + rng.below(16) as i32,
            cz * 16 + rng.below(16) as i32,
        );
        column(ctx, &mut level, x, z, raining, &mut rng);
    }
    let changes = level.into_changes();
    if !changes.is_empty() {
        let _ = ctx.events.send(Event::BlocksChanged { by: 0, changes });
    }
}

fn column(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    x: i32,
    z: i32,
    raining: bool,
    rng: &mut mc_tick::rng::Rng,
) {
    use mc_tick::Level as _;
    let blocks = &ctx.data.blocks;
    let Some(top_y) = ctx
        .world
        .top_where(x, z, |s| ctx.rules.blocks_motion(s) || blocks.is_fluid(s))
    else {
        return;
    };
    let top = Position::new(x, top_y, z);
    let below = Position::new(x, top_y - 1, z);
    let features = ctx.world.pipeline().map(|p| &p.features);
    let biome = ctx.world.biome(top, blocks).unwrap_or(0);
    let temperature = |p: Position| {
        features.map_or(0.8, |f| {
            f.temperature(biome, mc_features::P::new(p.x, p.y, p.z))
        })
    };
    let has_precipitation = features.is_none_or(|f| f.has_precipitation(biome));
    let dark = |p: Position| ctx.world.block_light(p) < 10;
    let logic = level.logic();
    // Still water freezes unless water is all around it.
    let water = level.block(to_pos(below));
    let still_water = blocks.block_name(water) == Some("minecraft:water")
        && blocks.property(water, "level") == Some("0");
    if still_water && temperature(below) < 0.15 && dark(below) {
        let open = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dz)| {
            let s = level.block(to_pos(Position::new(x + dx, below.y, z + dz)));
            blocks.block_name(s) != Some("minecraft:water")
        });
        if open && let Some(ice) = blocks.default_state("minecraft:ice") {
            level.set_block(below, ice);
        }
    }
    if !raining {
        return;
    }
    let height = lock(&ctx.game_rules).int("minecraft:max_snow_accumulation_height");
    let cold_top = temperature(top) < 0.15;
    if height > 0 && has_precipitation && cold_top && dark(top) {
        let here = level.block(to_pos(top));
        let snow = blocks.default_state("minecraft:snow");
        let is_snow = blocks.block_name(here) == Some("minecraft:snow");
        if let Some(snow) = snow
            && (ctx.shapes.is_air(here) || is_snow)
            && mc_tick::support::can_survive(level, logic, to_pos(top), snow)
        {
            if is_snow {
                let layers: i32 = blocks
                    .property(here, "layers")
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(8);
                if layers < height.min(8) {
                    level.set_block(
                        top,
                        ctx.shapes.with(here, "layers", &(layers + 1).to_string()),
                    );
                }
            } else {
                level.set_block(top, snow);
            }
        }
    }
    if has_precipitation {
        let rain = temperature(below) >= 0.15;
        mc_tick::cauldron::precipitation(level, logic, to_pos(below), rain, rng);
    }
}
