//! Primed TNT and explosions (minecraft.wiki, "Explosion" and "TNT").
//!
//! An explosion casts rays from the centre through a 16×16×16 grid's
//! surface; each starts with `power × (0.7 to 1.3)` and loses
//! `(blast resistance + 0.3) × 0.3` in every block it passes and 0.225 per
//! 0.3-block step. Every block a ray reaches with strength left is blown away
//! (air included: the packet counts it). Entities within twice the power
//! take `⌊(i² + i) / 2 × 7 × 2·power + 1⌋` damage and are pushed, where `i` is
//! `(1 − distance / (2·power)) × exposure`.
//!
//! Block shapes count as full cubes where they stop motion, for exposure.

use std::collections::HashMap;
use std::sync::OnceLock;

use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::{MetadataValue, SetEntityData};
use mc_tick::Level as _;
use mc_tick::rng::Rng;

use crate::context::{Event, ServerContext};
use crate::level::{WorldLevel, to_pos, to_position};
use crate::tick::lock;

/// Ticks a lit TNT burns.
pub const TNT_FUSE: i32 = 80;
const POWER: f32 = 4.0;
/// Metadata index of the fuse.
const FUSE_INDEX: u8 = 8;
const PLAYER_EYE: f64 = 1.62;

/// The fuse as clients are told: every tick it changes.
pub fn fuse_data(entity_id: i32, fuse: i32) -> SetEntityData {
    SetEntityData {
        entity_id,
        entries: vec![(FUSE_INDEX, MetadataValue::VarInt(fuse))],
    }
}

fn rng() -> Rng {
    Rng::new(crate::util::random_u64())
}

/// Primed TNT at the centre of the block at `p`, popping up a little in a
/// random direction.
pub fn primed(ctx: &ServerContext, p: Position, fuse: i32, owner: Option<u128>) -> Entity {
    let angle = rng().unit() * std::f64::consts::TAU;
    Entity {
        id: ctx.next_entity_id(),
        uuid: crate::util::random_uuid(),
        kind: Kind::Tnt { fuse, owner },
        pos: [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5],
        vel: [-angle.sin() * 0.02, 0.2, -angle.cos() * 0.02],
        on_ground: false,
        age: 0,
        removed: false,
        tags: Vec::new(),
    }
}

/// Fuse of TNT set off by another explosion: 10 to 29 ticks.
pub fn chained_fuse(rng: &mut Rng) -> i32 {
    (rng.below((TNT_FUSE / 4) as u32) as i32) + TNT_FUSE / 8
}

/// Lights the TNT block at `p`: it becomes primed TNT. Returns whether it
/// did (the game rule `tnt_explodes` can forbid it).
pub fn ignite(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    p: Position,
    owner: Option<u128>,
) -> bool {
    if !ctx.game_rule_bool("minecraft:tnt_explodes") {
        return false;
    }
    level.set_block(p, ctx.shapes.air());
    let entity = primed(ctx, p, TNT_FUSE, owner);
    let _ = ctx.events.send(Event::EntitySpawned(entity.clone()));
    lock(&ctx.entities).push(entity);
    true
}

/// Blast resistance of every block state (`-1`: nothing, as air has), from
/// the public block tables.
fn table(ctx: &ServerContext) -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..ctx.data.blocks.state_count())
            .map(|s| {
                let blocks = &ctx.data.blocks;
                let Some(name) = blocks.block_name(s) else {
                    return -1.0;
                };
                let name = name.trim_start_matches("minecraft:");
                if matches!(name, "air" | "cave_air" | "void_air") {
                    return -1.0;
                }
                let wet = blocks.property(s, "waterlogged") == Some("true");
                let r = blast_resistance(name);
                if wet { r.max(100.0) } else { r }
            })
            .collect()
    })
}

/// Blast resistance of a block (Java Edition values).
pub fn blast_resistance(name: &str) -> f32 {
    let n = name;
    let ends = |s: &str| n.ends_with(s);
    match n {
        "bedrock"
        | "barrier"
        | "command_block"
        | "chain_command_block"
        | "repeating_command_block"
        | "structure_block"
        | "jigsaw"
        | "test_block"
        | "test_instance_block"
        | "end_portal"
        | "end_portal_frame"
        | "end_gateway"
        | "nether_portal" => return 3_600_000.0,
        "light" => return 3_600_000.8,
        "water" | "lava" | "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass"
        | "bubble_column" => return 100.0,
        "reinforced_deepslate" => return 1200.0,
        "obsidian" | "crying_obsidian" | "respawn_anchor" | "enchanting_table"
        | "netherite_block" | "ancient_debris" | "anvil" | "chipped_anvil" | "damaged_anvil"
        | "heavy_core" => {
            return 1200.0;
        }
        "ender_chest" => return 600.0,
        "trial_spawner" | "vault" => return 50.0,
        "dragon_egg" => return 9.0,
        "end_stone" | "end_stone_bricks" => return 9.0,
        "blue_ice" => return 2.8,
        "cobweb" => return 4.0,
        "spawner" | "iron_door" | "iron_trapdoor" | "bell" | "chain" | "iron_chain" => return 5.0,
        "hopper" => return 4.8,
        "furnace" | "blast_furnace" | "smoker" | "dispenser" | "dropper" | "stonecutter"
        | "lantern" | "soul_lantern" => return 3.5,
        "conduit"
        | "beacon"
        | "sculk_shrieker"
        | "sculk_catalyst"
        | "observer"
        | "cocoa"
        | "lapis_block"
        | "lapis_ore"
        | "deepslate_lapis_ore" => return 3.0,
        "chest" | "trapped_chest" | "barrel" | "crafting_table" | "loom" | "cartography_table"
        | "fletching_table" | "smithing_table" | "lectern" => return 2.5,
        "cauldron"
        | "water_cauldron"
        | "lava_cauldron"
        | "powder_snow_cauldron"
        | "bone_block"
        | "bamboo_block" => return 2.0,
        "sculk_sensor"
        | "calibrated_sculk_sensor"
        | "bookshelf"
        | "chiseled_bookshelf"
        | "amethyst_block"
        | "budding_amethyst" => return 1.5,
        "terracotta" => return 4.2,
        "basalt" | "polished_basalt" | "smooth_basalt" => return 4.2,
        "netherrack" => return 0.4,
        "magma_block" | "dirt" | "coarse_dirt" | "rooted_dirt" | "podzol" | "soul_sand"
        | "soul_soil" | "sand" | "red_sand" | "ice" | "packed_ice" | "mud" | "hay_block"
        | "brewing_stand" | "piston" | "sticky_piston" | "piston_head" | "turtle_egg"
        | "sniffer_egg" | "cake" | "lever" | "moving_piston_x" => return 0.5,
        "grass_block" | "mycelium" | "farmland" | "gravel" | "clay" | "sponge" | "wet_sponge"
        | "composter" | "beehive" | "sculk" | "packed_mud" => return 0.6,
        "dirt_path" => return 0.65,
        "bee_nest" => return 0.3,
        "glass" | "tinted_glass" | "glass_pane" | "sea_lantern" | "glowstone" | "redstone_lamp" => {
            return 0.3;
        }
        "cactus" | "ladder" | "chorus_plant" | "chorus_flower" => return 0.4,
        "daylight_detector" | "snow_block" => return 0.2,
        "snow" => return 0.1,
        "powder_snow" => return 0.25,
        "moss_block" => return 0.1,
        "calcite" => return 0.75,
        "sandstone"
        | "red_sandstone"
        | "chiseled_sandstone"
        | "chiseled_red_sandstone"
        | "cut_sandstone"
        | "cut_red_sandstone"
        | "smooth_sandstone"
        | "smooth_red_sandstone"
        | "quartz_block"
        | "chiseled_quartz_block"
        | "quartz_pillar"
        | "smooth_quartz"
        | "quartz_bricks"
        | "note_block"
        | "jukebox_x" => return 0.8,
        "dripstone_block" | "pumpkin" | "carved_pumpkin" | "jack_o_lantern" | "melon"
        | "bamboo" | "bamboo_sapling" | "nether_wart_block" | "warped_wart_block"
        | "shroomlight" => return 1.0,
        "jukebox" | "grindstone" | "lightning_rod" | "iron_bars" | "iron_block" | "gold_block"
        | "diamond_block" | "emerald_block" | "redstone_block" | "coal_block"
        | "raw_iron_block" | "raw_gold_block" | "raw_copper_block" | "end_rod_x" => return 6.0,
        "sticky_x" => return 0.0,
        _ => {}
    }
    // Plants, torches, redstone parts and the like break at once.
    if ends("_sapling")
        || ends("_torch")
        || ends("_wall_torch")
        || ends("_tulip")
        || ends("_mushroom")
        || ends("_roots")
        || ends("_fungus")
        || ends("_vines")
        || ends("_vines_plant")
        || ends("_sprouts")
        || ends("_propagule")
        || n.starts_with("potted_")
        || matches!(
            n,
            "torch"
                | "redstone_wire"
                | "repeater"
                | "comparator"
                | "tripwire"
                | "tripwire_hook"
                | "fire"
                | "soul_fire"
                | "lily_pad"
                | "sugar_cane"
                | "nether_wart"
                | "short_grass"
                | "tall_grass"
                | "fern"
                | "large_fern"
                | "dead_bush"
                | "dandelion"
                | "poppy"
                | "blue_orchid"
                | "allium"
                | "azure_bluet"
                | "oxeye_daisy"
                | "cornflower"
                | "lily_of_the_valley"
                | "wither_rose"
                | "torchflower"
                | "sunflower"
                | "lilac"
                | "rose_bush"
                | "peony"
                | "wheat"
                | "carrots"
                | "potatoes"
                | "beetroots"
                | "melon_stem"
                | "pumpkin_stem"
                | "attached_melon_stem"
                | "attached_pumpkin_stem"
                | "torchflower_crop"
                | "pitcher_crop"
                | "pitcher_plant"
                | "sweet_berry_bush"
                | "cave_vines"
                | "cave_vines_plant"
                | "hanging_roots"
                | "spore_blossom"
                | "small_dripleaf"
                | "big_dripleaf"
                | "big_dripleaf_stem"
                | "glow_berries"
                | "slime_block"
                | "honey_block"
                | "tnt"
                | "structure_void"
                | "moving_piston"
                | "end_rod"
                | "scaffolding"
                | "frogspawn"
                | "firefly_bush"
                | "bush"
                | "short_dry_grass"
                | "tall_dry_grass"
                | "leaf_litter"
                | "wildflowers"
                | "pink_petals"
                | "golden_dandelion"
                | "closed_eyeblossom"
                | "open_eyeblossom"
        )
    {
        return 0.0;
    }
    if ends("_carpet") || n == "moss_carpet" {
        return 0.1;
    }
    if ends("_leaves") || n == "vine" || n == "glow_lichen" || n == "sculk_vein" {
        return 0.2;
    }
    if ends("glass") || ends("glass_pane") {
        return 0.3;
    }
    if ends("_button") || ends("_pressure_plate") {
        return 0.5;
    }
    if ends("rail") {
        return 0.7;
    }
    if ends("_wool") || ends("wool_slab") || ends("wool_stairs") {
        return 0.8;
    }
    if ends("_concrete_powder") {
        return 0.5;
    }
    if ends("_concrete") {
        return 1.8;
    }
    if ends("_glazed_terracotta") {
        return 1.4;
    }
    if ends("_terracotta") {
        return 4.2;
    }
    if ends("_bed") || ends("_banner") || ends("_candle") || n == "candle_cake" {
        return 0.2f32.max(if ends("_banner") { 1.0 } else { 0.2 });
    }
    if ends("_sign") || ends("_hanging_sign") {
        return 1.0;
    }
    if ends("_log") || ends("_wood") || ends("_stem") || ends("_hyphae") {
        return 2.0;
    }
    if ends("_shulker_box") || ends("shulker_box") {
        return 2.0;
    }
    if ends("_planks")
        || ends("_door")
        || ends("_trapdoor")
        || ends("_fence")
        || ends("_fence_gate")
        || ends("_shelf")
        || ends("oak_slab")
        || ends("oak_stairs")
        || ends("spruce_slab")
        || ends("spruce_stairs")
        || ends("birch_slab")
        || ends("birch_stairs")
        || ends("jungle_slab")
        || ends("jungle_stairs")
        || ends("acacia_slab")
        || ends("acacia_stairs")
        || ends("cherry_slab")
        || ends("cherry_stairs")
        || ends("mangrove_slab")
        || ends("mangrove_stairs")
        || ends("bamboo_slab")
        || ends("bamboo_stairs")
        || ends("bamboo_mosaic")
        || ends("crimson_slab")
        || ends("crimson_stairs")
        || ends("warped_slab")
        || ends("warped_stairs")
        || ends("poplar_slab")
        || ends("poplar_stairs")
    {
        return if n.starts_with("iron_") { 5.0 } else { 3.0 };
    }
    if ends("_ore") {
        return 3.0;
    }
    if ends("copper_chest") {
        return 6.0;
    }
    if ends("_froglight") {
        return 0.3;
    }
    if ends("_coral") || ends("_coral_fan") || ends("_coral_wall_fan") {
        return 0.0;
    }
    if ends("_coral_block") {
        return 1.5;
    }
    if ends("_skull") || ends("_head") {
        return 1.0;
    }
    if ends("_mushroom_block") {
        return 0.2;
    }
    if n == "mushroom_stem" {
        return 0.2;
    }
    if ends("_infested") || n.starts_with("infested_") {
        return 0.75;
    }
    if ends("_wall") || ends("_slab") || ends("_stairs") {
        // Stone-like slabs, stairs and walls share their block's value
        // (6 for the stones, bricks, deepslate and copper family).
        return if n.contains("sandstone") || n.contains("quartz") {
            0.8
        } else {
            6.0
        };
    }
    // Stone, bricks, deepslate, blackstone, tuff, prismarine, purpur,
    // copper and the rest of the hard blocks.
    6.0
}

/// Strength a ray loses in the block `state`, `None` for nothing.
fn resistance(ctx: &ServerContext, state: u32) -> Option<f32> {
    let r = *table(ctx).get(state as usize)?;
    (r >= 0.0).then_some(r)
}

/// What the explosion needs to know about its source.
#[derive(Clone, Copy, Debug)]
pub struct Blast {
    pub center: [f64; 3],
    pub power: f32,
    /// The primed TNT's entity id and the player who lit it.
    pub source: i32,
    pub owner: Option<u128>,
}

impl Blast {
    pub fn tnt(entity: &Entity, owner: Option<u128>) -> Self {
        // Vanilla's height is a float.
        let y = entity.pos[1] + f64::from(0.98_f32 / 16.0);
        Self {
            center: [entity.pos[0], y, entity.pos[2]],
            power: POWER,
            source: entity.id,
            owner,
        }
    }
}

/// The blocks the rays reach (air among them), each once.
fn blown_positions(
    ctx: &ServerContext,
    level: &WorldLevel,
    blast: &Blast,
    rng: &mut Rng,
) -> Vec<Position> {
    let mut seen: HashMap<(i32, i32, i32), ()> = HashMap::new();
    let mut out = Vec::new();
    let step = f64::from(0.3f32);
    for x in 0..16 {
        for y in 0..16 {
            for z in 0..16 {
                if x != 0 && x != 15 && y != 0 && y != 15 && z != 0 && z != 15 {
                    continue;
                }
                let mut d = [
                    f64::from(x) / 15.0 * 2.0 - 1.0,
                    f64::from(y) / 15.0 * 2.0 - 1.0,
                    f64::from(z) / 15.0 * 2.0 - 1.0,
                ];
                let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                for v in &mut d {
                    *v /= len;
                }
                let mut strength = blast.power * (0.7 + rng.unit() as f32 * 0.6);
                let mut at = blast.center;
                while strength > 0.0 {
                    let p = to_pos(Position::new(
                        at[0].floor() as i32,
                        at[1].floor() as i32,
                        at[2].floor() as i32,
                    ));
                    if !level.in_world(p) {
                        break;
                    }
                    if let Some(r) = resistance(ctx, level.block(p)) {
                        strength -= (r + 0.3) * 0.3;
                    }
                    if strength > 0.0 && seen.insert((p.x, p.y, p.z), ()).is_none() {
                        out.push(to_position(p));
                    }
                    for i in 0..3 {
                        at[i] += d[i] * step;
                    }
                    strength -= 0.225_000_01;
                }
            }
        }
    }
    out
}

/// Whether a ray from `from` to `to` passes through no block that stops
/// motion (voxel traversal).
fn clear(ctx: &ServerContext, level: &WorldLevel, from: [f64; 3], to: [f64; 3]) -> bool {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let mut cell = [
        from[0].floor() as i32,
        from[1].floor() as i32,
        from[2].floor() as i32,
    ];
    let end = [
        to[0].floor() as i32,
        to[1].floor() as i32,
        to[2].floor() as i32,
    ];
    let step: [i32; 3] = std::array::from_fn(|i| if d[i] > 0.0 { 1 } else { -1 });
    let mut t_max: [f64; 3] = std::array::from_fn(|i| {
        if d[i] == 0.0 {
            f64::INFINITY
        } else {
            let edge = if d[i] > 0.0 {
                f64::from(cell[i] + 1)
            } else {
                f64::from(cell[i])
            };
            (edge - from[i]) / d[i]
        }
    });
    let t_delta: [f64; 3] = std::array::from_fn(|i| {
        if d[i] == 0.0 {
            f64::INFINITY
        } else {
            (1.0 / d[i]).abs()
        }
    });
    loop {
        let state = level.block(mc_tick::BlockPos::new(cell[0], cell[1], cell[2]));
        if ctx.rules.blocks_motion(state) {
            return false;
        }
        if cell == end {
            return true;
        }
        let axis = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] {
            0
        } else if t_max[1] <= t_max[2] {
            1
        } else {
            2
        };
        if t_max[axis] > 1.0 {
            return true;
        }
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
    }
}

/// The share of an entity's box points that see the centre.
fn exposure(
    ctx: &ServerContext,
    level: &WorldLevel,
    center: [f64; 3],
    min: [f64; 3],
    max: [f64; 3],
) -> f64 {
    let size: [f64; 3] = std::array::from_fn(|i| max[i] - min[i]);
    let delta: [f64; 3] = std::array::from_fn(|i| 1.0 / (size[i] * 2.0 + 1.0));
    let offset = |i: usize| (1.0 - (1.0 / delta[i]).floor() * delta[i]) / 2.0;
    let (mut seen, mut total) = (0u32, 0u32);
    let mut x = 0.0;
    while x <= 1.0 {
        let mut y = 0.0;
        while y <= 1.0 {
            let mut z = 0.0;
            while z <= 1.0 {
                let p = [
                    min[0] + size[0] * x + offset(0),
                    min[1] + size[1] * y,
                    min[2] + size[2] * z + offset(2),
                ];
                if clear(ctx, level, p, center) {
                    seen += 1;
                }
                total += 1;
                z += delta[2];
            }
            y += delta[1];
        }
        x += delta[0];
    }
    f64::from(seen) / f64::from(total.max(1))
}

/// The explosion of `blast`: blocks go (with their drops), entities in
/// `entities` and players are hurt and pushed. Returns the entities it
/// removed (id, uuid).
pub fn explode(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    blast: &Blast,
) -> Vec<(i32, u128)> {
    let mut rng = rng();
    let mut positions = blown_positions(ctx, level, blast, &mut rng);
    let count = positions.len() as i32;
    let blocks = &ctx.data.blocks;
    let decay = ctx.game_rule_bool("minecraft:tnt_explosion_drop_decay");
    // Shuffled, as vanilla does before dropping.
    for i in (1..positions.len()).rev() {
        positions.swap(i, rng.below(i as u32 + 1) as usize);
    }
    let mut drops: Vec<(Position, crate::loot::Drop)> = Vec::new();
    for p in positions {
        let Some(state) = ctx.world.block(p, blocks) else {
            continue;
        };
        if blocks.is_air(state) {
            continue;
        }
        let Some(name) = blocks.block_name(state) else {
            continue;
        };
        if name == "minecraft:tnt" {
            // Other TNT is set off with a short fuse.
            let mut tnt = primed(ctx, p, chained_fuse(&mut rng), None);
            if let Kind::Tnt { owner, .. } = &mut tnt.kind {
                *owner = blast.owner;
            }
            level.set_block(p, ctx.shapes.air());
            let _ = ctx.events.send(Event::EntitySpawned(tnt.clone()));
            entities.push(tnt);
            continue;
        }
        if crate::containers::kind_of(name).is_some() {
            let (items, _) = crate::containers::broken(ctx, p);
            for stack in items {
                drops.push((
                    p,
                    crate::loot::Drop {
                        item: stack.item,
                        count: stack.count,
                        components: crate::items::damage_components(stack.damage),
                    },
                ));
            }
            let _ = ctx.events.send(Event::ContainerGone { position: p });
        }
        let props = |k: &str| blocks.property(state, k).map(str::to_owned);
        drops.extend(
            ctx.loot
                .drops_exploded(name, &props, decay.then_some(blast.power))
                .into_iter()
                .map(|d| (p, d)),
        );
        level.set_block(p, ctx.shapes.air());
    }
    merge_and_drop(ctx, drops);

    // Entities within twice the power.
    let reach = f64::from(blast.power) * 2.0;
    let c = blast.center;
    let mut gone = Vec::new();
    let mut knockback: Vec<(u128, [f64; 3])> = Vec::new();
    let near = |min: [f64; 3], max: [f64; 3]| {
        (0..3).all(|i| min[i] <= c[i] + reach + 1.0 && max[i] >= c[i] - reach - 1.0)
    };
    for e in entities.iter_mut().filter(|e| !e.removed) {
        let b = e.aabb();
        if !near(b.min, b.max) {
            continue;
        }
        let dist =
            ((e.pos[0] - c[0]).powi(2) + (e.pos[1] - c[1]).powi(2) + (e.pos[2] - c[2]).powi(2))
                .sqrt()
                / reach;
        if dist > 1.0 {
            continue;
        }
        let eye = match e.kind {
            Kind::Tnt { .. } => e.pos[1],
            _ => e.pos[1] + e.size().1 * 0.85,
        };
        let mut dir = [e.pos[0] - c[0], eye - c[1], e.pos[2] - c[2]];
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        if len == 0.0 {
            continue;
        }
        for v in &mut dir {
            *v /= len;
        }
        let impact = (1.0 - dist) * exposure(ctx, level, c, b.min, b.max);
        if let Kind::Item { health, .. } = &mut e.kind {
            *health -= damage(impact, reach);
            if *health <= 0.0 {
                e.removed = true;
                gone.push((e.id, e.uuid));
                continue;
            }
        }
        for (v, d) in e.vel.iter_mut().zip(dir) {
            *v += d * impact;
        }
    }

    // Players.
    let players: Vec<(u128, i32, i32, crate::context::Pose, bool)> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            p.pose
                .map(|pose| (p.profile.uuid, p.entity_id, p.game_mode, pose, p.flying))
        })
        .collect();
    let attacker = blast
        .owner
        .and_then(|o| lock(&ctx.players).get(&o).map(|p| p.profile.name.clone()));
    for (uuid, _, mode, pose, flying) in players {
        // Spectators and flying creative players are out of it.
        if mode == 3 || (mode == 1 && flying) {
            continue;
        }
        let (min, max) = (
            [pose.x - 0.3, pose.y, pose.z - 0.3],
            [pose.x + 0.3, pose.y + 1.8, pose.z + 0.3],
        );
        let dist = ((pose.x - c[0]).powi(2) + (pose.y - c[1]).powi(2) + (pose.z - c[2]).powi(2))
            .sqrt()
            / reach;
        if dist > 1.0 {
            continue;
        }
        let mut dir = [pose.x - c[0], pose.y + PLAYER_EYE - c[1], pose.z - c[2]];
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        if len == 0.0 {
            continue;
        }
        for v in &mut dir {
            *v /= len;
        }
        let impact = (1.0 - dist) * exposure(ctx, level, c, min, max);
        let (kind, key) = if attacker.is_some() {
            (
                "minecraft:player_explosion",
                "death.attack.explosion.player",
            )
        } else {
            ("minecraft:explosion", "death.attack.explosion")
        };
        crate::vitals::try_hurt_by_entity(
            ctx,
            uuid,
            damage(impact, reach),
            kind,
            key,
            attacker.as_deref(),
            Some(blast.source),
        );
        knockback.push((uuid, [dir[0] * impact, dir[1] * impact, dir[2] * impact]));
    }
    let _ = ctx.events.send(Event::Explosion {
        center: c,
        radius: blast.power,
        block_count: count,
        particle: ctx.explosion_particle,
        sound: ctx.explosion_sound,
        knockback,
    });
    gone
}

/// `⌊(i² + i) / 2 × 7 × reach + 1⌋`.
fn damage(impact: f64, reach: f64) -> f32 {
    ((impact * impact + impact) / 2.0 * 7.0 * reach + 1.0).floor() as f32
}

/// Items dropped by the blocks: equal stacks merge into the first one of
/// their kind (up to 16 items, wherever it came from) before they are
/// thrown out at that block.
fn merge_and_drop(ctx: &ServerContext, drops: Vec<(Position, crate::loot::Drop)>) {
    const MERGE: i32 = 16;
    let mut merged: Vec<(Position, crate::loot::Drop)> = Vec::new();
    for (p, d) in drops {
        let max = ctx.items.max_stack(d.item).min(MERGE);
        let mut left = d.count;
        for (_, m) in merged.iter_mut() {
            if left == 0 {
                break;
            }
            if m.item == d.item && m.components == d.components && m.count < max {
                let n = (max - m.count).min(left);
                m.count += n;
                left -= n;
            }
        }
        while left > 0 {
            let n = left.min(max);
            merged.push((
                p,
                crate::loot::Drop {
                    count: n,
                    ..d.clone()
                },
            ));
            left -= n;
        }
    }
    for (p, d) in merged {
        crate::tick::drop_item(ctx, p, d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A superflat server context, `None` without the game data.
    fn flat() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    fn block(ctx: &ServerContext, name: &str) -> u32 {
        ctx.data.blocks.default_state(name).expect(name)
    }

    fn item(ctx: &ServerContext, pos: [f64; 3]) -> Entity {
        Entity {
            id: ctx.next_entity_id(),
            uuid: crate::util::random_uuid(),
            kind: Kind::item(1, 1, 0, 0),
            pos,
            vel: [0.0; 3],
            on_ground: false,
            age: 0,
            removed: false,
            tags: Vec::new(),
        }
    }

    #[test]
    fn explosion_digs_a_crater_and_spares_bedrock() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let (grass, bedrock) = (
            block(&ctx, "minecraft:grass_block"),
            block(&ctx, "minecraft:bedrock"),
        );
        let at = |x, y, z| ctx.world.block(Position::new(x, y, z), &ctx.data.blocks);
        assert_eq!(at(0, -61, 0), Some(grass));
        let mut level = WorldLevel::new(&ctx);
        let mut entities = vec![item(&ctx, [0.5, -59.0, 0.5]), item(&ctx, [8.2, -59.0, 0.5])];
        let blast = Blast {
            center: [0.5, -59.9, 0.5],
            power: 4.0,
            source: 0,
            owner: None,
        };
        let gone = explode(&ctx, &mut level, &mut entities, &blast);
        let changes = level.into_changes();
        assert!(changes.len() > 20, "{} blocks", changes.len());
        assert!(ctx.data.blocks.is_air(at(0, -61, 0).unwrap()));
        assert!(ctx.data.blocks.is_air(at(0, -62, 0).unwrap()));
        assert_eq!(at(0, -64, 0), Some(bedrock));
        // The item at the centre is destroyed, the one at the edge of the range is
        // pushed away instead.
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].0, entities[0].id);
        assert!(entities[1].vel[0] > 0.0 && !entities[1].removed);
    }

    #[test]
    fn explosions_set_off_tnt_nearby() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let tnt = block(&ctx, "minecraft:tnt");
        let mut level = WorldLevel::new(&ctx);
        level.set_block(Position::new(2, -60, 0), tnt);
        let mut entities = Vec::new();
        let blast = Blast {
            center: [0.5, -59.9, 0.5],
            power: 4.0,
            source: 0,
            owner: None,
        };
        explode(&ctx, &mut level, &mut entities, &blast);
        drop(level);
        assert!(
            ctx.data.blocks.is_air(
                ctx.world
                    .block(Position::new(2, -60, 0), &ctx.data.blocks)
                    .unwrap()
            )
        );
        let [
            Entity {
                kind: Kind::Tnt { fuse, .. },
                ..
            },
        ] = entities[..]
        else {
            panic!("{entities:?}");
        };
        assert!((10..30).contains(&fuse));
    }

    #[test]
    fn drops_follow_the_decay_rule() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let stone = ctx
            .data
            .blocks
            .block_name(block(&ctx, "minecraft:stone"))
            .unwrap()
            .to_owned();
        let none = |_: &str| None;
        // Without decay everything drops; with it, about 1 in 4 at power 4.
        let all = (0..400)
            .filter(|_| !ctx.loot.drops_exploded(&stone, &none, None).is_empty())
            .count();
        let some = (0..400)
            .filter(|_| !ctx.loot.drops_exploded(&stone, &none, Some(4.0)).is_empty())
            .count();
        assert_eq!(all, 400);
        assert!((60..140).contains(&some), "{some}");
    }

    #[test]
    fn resistances_follow_the_tables() {
        assert_eq!(blast_resistance("stone"), 6.0);
        assert_eq!(blast_resistance("dirt"), 0.5);
        assert_eq!(blast_resistance("oak_planks"), 3.0);
        assert_eq!(blast_resistance("oak_log"), 2.0);
        assert_eq!(blast_resistance("obsidian"), 1200.0);
        assert_eq!(blast_resistance("water"), 100.0);
        assert_eq!(blast_resistance("short_grass"), 0.0);
        assert_eq!(blast_resistance("white_wool"), 0.8);
        assert_eq!(blast_resistance("bedrock"), 3_600_000.0);
        assert_eq!(blast_resistance("oak_slab"), 3.0);
        assert_eq!(blast_resistance("stone_slab"), 6.0);
    }

    #[test]
    fn damage_follows_the_formula() {
        // Right at the centre in the open: (1 + 1) / 2 × 7 × 8 + 1.
        assert_eq!(damage(1.0, 8.0), 57.0);
        assert_eq!(damage(0.0, 8.0), 1.0);
    }

    #[test]
    fn chained_fuses_are_short() {
        let mut rng = Rng::new(7);
        for _ in 0..100 {
            assert!((10..30).contains(&chained_fuse(&mut rng)));
        }
    }
}
