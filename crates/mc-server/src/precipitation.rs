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
    let (raining, thundering) = {
        let w = lock(&ctx.weather);
        (w.raining, w.thundering)
    };
    let mut rng = mc_tick::rng::Rng::new(crate::util::random_u64());
    let mut level = WorldLevel::new(ctx);
    let storm = raining && thundering && ctx.world.dimension == "minecraft:overworld";
    for (cx, cz) in chunks {
        // In a thunderstorm a chunk has 1 chance in 100000 a tick to be
        // struck.
        if storm && rng.below(100_000) == 0 {
            lightning(ctx, cx, cz, &mut rng);
        }
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

/// A strike in the chunk `cx, cz` (minecraft.wiki, "Lightning"): at a random
/// column's highest block, but a lightning rod at the top of its column
/// within 128 blocks takes it (the closest one), else a living thing under
/// the open sky within 3 blocks of the column; it must be raining there.
pub(crate) fn lightning(ctx: &ServerContext, cx: i32, cz: i32, rng: &mut mc_tick::rng::Rng) {
    let blocks = &ctx.data.blocks;
    let (x, z) = (
        cx * 16 + rng.below(16) as i32,
        cz * 16 + rng.below(16) as i32,
    );
    let Some(top) = ctx
        .world
        .top_where(x, z, |s| ctx.rules.blocks_motion(s) || blocks.is_fluid(s))
    else {
        return;
    };
    let mut at = [x, top, z];
    if let Some(rod) = nearest_rod(ctx, at) {
        at = [rod[0], rod[1] + 1, rod[2]];
    } else {
        let beings = beings_near(ctx, at);
        if !beings.is_empty() {
            at = beings[rng.below(beings.len() as u32) as usize];
        }
    }
    if !crate::worn::rain_at(ctx, [f64::from(at[0]), f64::from(at[1]), f64::from(at[2])]) {
        return;
    }
    let pos = [
        f64::from(at[0]) + 0.5,
        f64::from(at[1]),
        f64::from(at[2]) + 0.5,
    ];
    crate::projectiles::lightning::call_down(ctx, pos);
}

/// The lightning rod closest to `from` within 128 blocks that is the
/// highest block of its column.
fn nearest_rod(ctx: &ServerContext, from: [i32; 3]) -> Option<[i32; 3]> {
    let blocks = &ctx.data.blocks;
    let is_rod = |s: u32| {
        blocks
            .block_name(s)
            .is_some_and(|n| n.ends_with("lightning_rod"))
    };
    let (cx, cz) = (from[0] >> 4, from[2] >> 4);
    let mut best: Option<(i64, [i32; 3])> = None;
    for dx in -9..=9 {
        for dz in -9..=9 {
            if !ctx.world.chunk_loaded(cx + dx, cz + dz) {
                continue;
            }
            for p in ctx
                .world
                .find_in_existing(cx + dx, cz + dz, blocks, &is_rod)
            {
                let d = [p.x - from[0], p.y - from[1], p.z - from[2]].map(i64::from);
                let dist = d.iter().map(|v| v * v).sum::<i64>();
                if dist > 128 * 128 || best.is_some_and(|(b, _)| b <= dist) {
                    continue;
                }
                // Nothing above it.
                let highest = ctx.world.top_where(p.x, p.z, |s| !ctx.shapes.is_air(s));
                if highest == Some(p.y + 1) {
                    best = Some((dist, [p.x, p.y, p.z]));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The blocks where living things stand that see the sky, in the column of
/// `at` widened by 3 blocks, from 3 below it up.
fn beings_near(ctx: &ServerContext, at: [i32; 3]) -> Vec<[i32; 3]> {
    let mut found = Vec::new();
    let mut consider = |pos: [f64; 3]| {
        let cell = pos.map(|c| c.floor() as i32);
        if cell[0] >= at[0] - 3
            && cell[0] <= at[0] + 3
            && cell[2] >= at[2] - 3
            && cell[2] <= at[2] + 3
            && cell[1] >= at[1] - 3
            && crate::worn::sees_sky(ctx, cell)
        {
            found.push(cell);
        }
    };
    for e in lock(&ctx.entities).iter() {
        if let mc_entity::Kind::Mob(m) = &e.kind
            && !m.is_dead()
        {
            consider(e.pos);
        }
    }
    let alive = |uuid: u128| lock(&ctx.vitals).get(&uuid).is_some_and(|v| !v.dead);
    for p in lock(&ctx.players).values() {
        if let Some(pose) = p.pose
            && p.dimension == ctx.world.dimension
            && alive(p.profile.uuid)
        {
            consider([pose.x, pose.y, pose.z]);
        }
    }
    found
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
