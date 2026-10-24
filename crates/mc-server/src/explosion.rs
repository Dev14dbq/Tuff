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

use std::sync::OnceLock;

use mc_entity::{Entity, Kind};
use mc_protocol::Position;
use mc_protocol::packets::play::{MetadataValue, SetEntityData};
use mc_tick::Level as _;
use mc_tick::rng::Rng;

use crate::context::{Event, ServerContext};
use crate::level::WorldLevel;
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

/// The hiss of a lit fuse.
pub fn primed_sound(ctx: &ServerContext, tnt: &Entity) {
    let _ = ctx.events.send(Event::WorldSound {
        pos: tnt.pos,
        sound: "minecraft:entity.tnt.primed",
        category: 4,
        volume: 1.0,
        pitch: 1.0,
    });
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
    primed_sound(ctx, &entity);
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

/// [`table`] for a blue wither skull: what a wither may break resists no
/// more than 0.8.
fn weak_table(ctx: &ServerContext) -> &'static [f32] {
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let strong = table(ctx);
        (0..ctx.data.blocks.state_count())
            .map(|s| {
                let r = strong.get(s as usize).copied().unwrap_or(-1.0);
                let name = ctx.data.blocks.block_name(s).unwrap_or("");
                if r < 0.0 || crate::bosses::wither::immune_block(ctx, name) {
                    r
                } else {
                    r.min(0.8)
                }
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

/// What the explosion needs to know about its source.
#[derive(Clone, Copy, Debug)]
pub struct Blast {
    pub center: [f64; 3],
    pub power: f32,
    /// The primed TNT's entity id and the player who lit it.
    pub source: i32,
    pub owner: Option<u128>,
    /// A mob's (a creeper's): `mob_griefing` and
    /// `mob_explosion_drop_decay` rule it, not the TNT rules.
    pub mob: bool,
    /// A blue wither skull's: what it may break resists no more than 0.8.
    pub dangerous: bool,
    /// The kind of the mob behind it (`minecraft:wither`).
    pub by: Option<&'static str>,
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
            mob: false,
            dangerous: false,
            by: None,
        }
    }
}

/// Cells each side of the centre the block cache covers: rays reach about
/// 7 blocks, exposure checks the entities up to 9 away.
const CACHE_R: i32 = 11;
/// Marks a cached block that stops motion.
const SOLID: u32 = 1 << 31;
/// The rays reach 7 blocks; this much is read in one go.
const EAGER_R: i32 = 8;
const CACHE_SIDE: usize = (2 * CACHE_R + 1) as usize;

/// What one explosion at a time needs, kept between them: the blocks around
/// the centre read once, and room for the rays' results.
pub struct Scratch {
    stamp: u32,
    base: [i32; 3],
    /// (stamp, state) of each block of the cube around the centre.
    cells: Vec<(u32, u32)>,
    /// The blocks read in one go.
    cube: Vec<u32>,
    /// Running sums of the blocks that stop motion in that cube, so "is
    /// there any in this box" is eight lookups.
    solids: Vec<u32>,
    /// Resistance of each block of that cube (`-1`: none), and which of
    /// them the current explosion has taken.
    resist: Vec<f32>,
    eager_taken: Vec<u32>,
    eager_min: [i32; 3],
    positions: Vec<Position>,
    candidates: Vec<u32>,
}

impl Default for Scratch {
    fn default() -> Self {
        let n = CACHE_SIDE * CACHE_SIDE * CACHE_SIDE;
        Self {
            stamp: 0,
            base: [0; 3],
            cells: vec![(0, 0); n],
            cube: vec![0; (2 * EAGER_R as usize + 1).pow(3)],
            solids: vec![0; (2 * EAGER_R as usize + 2).pow(3)],
            resist: vec![-1.0; (2 * EAGER_R as usize + 1).pow(3)],
            eager_taken: vec![0; (2 * EAGER_R as usize + 1).pow(3)],
            eager_min: [0; 3],
            positions: Vec::new(),
            candidates: Vec::new(),
        }
    }
}

impl Scratch {
    /// Starts an explosion at `center`: nothing is cached yet.
    fn begin(&mut self, ctx: &ServerContext, center: [f64; 3], dangerous: bool) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.cells.fill((0, 0));
            self.eager_taken.fill(0);
            self.stamp = 1;
        }
        self.base = std::array::from_fn(|i| center[i].floor() as i32 - CACHE_R);
        // The part the rays can reach is read in one go.
        const EAGER: i32 = 2 * EAGER_R + 1;
        let min = self.base.map(|b| b + CACHE_R - EAGER_R);
        let mut cube = std::mem::take(&mut self.cube);
        ctx.world
            .read_cube(min, EAGER, ctx.rules.air, &ctx.data.blocks, &mut cube);
        for dx in 0..EAGER {
            for dy in 0..EAGER {
                let src = ((dx * EAGER + dy) * EAGER) as usize;
                let dst = (((dx + CACHE_R - EAGER_R) as usize * CACHE_SIDE
                    + (dy + CACHE_R - EAGER_R) as usize)
                    * CACHE_SIDE)
                    + (CACHE_R - EAGER_R) as usize;
                for dz in 0..EAGER as usize {
                    let state = cube[src + dz];
                    self.cells[dst + dz] = (
                        self.stamp,
                        state | (u32::from(ctx.rules.blocks_motion(state)) << 31),
                    );
                }
            }
        }
        let table = if dangerous {
            weak_table(ctx)
        } else {
            table(ctx)
        };
        for (r, &state) in self.resist.iter_mut().zip(&cube) {
            *r = table.get(state as usize).copied().unwrap_or(-1.0);
        }
        self.eager_min = min;
        self.cube = cube;
    }

    /// Sums up the solid blocks of the cube just read (after `begin`).
    fn index_solids(&mut self, ctx: &ServerContext) {
        const N: usize = 2 * EAGER_R as usize + 1;
        const M: usize = N + 1;
        // sums[(x * M + y) * M + z] = solids with all of x', y', z' below.
        for x in 0..N {
            for y in 0..N {
                for z in 0..N {
                    let solid = u32::from(ctx.rules.blocks_motion(self.cube[(x * N + y) * N + z]));
                    let at = |x: usize, y: usize, z: usize| self.solids[(x * M + y) * M + z];
                    let v = solid
                        + at(x, y + 1, z + 1)
                        + at(x + 1, y, z + 1)
                        + at(x + 1, y + 1, z)
                        + at(x, y, z)
                        - at(x, y, z + 1)
                        - at(x, y + 1, z)
                        - at(x + 1, y, z);
                    self.solids[((x + 1) * M + y + 1) * M + z + 1] = v;
                }
            }
        }
    }

    /// Whether no block of the cells `lo..=hi` stops motion; `false` when
    /// that is not known (the box leaves what was read in one go).
    fn open(&self, lo: [i32; 3], hi: [i32; 3]) -> bool {
        const N: i32 = 2 * EAGER_R + 1;
        const M: usize = N as usize + 1;
        let (a, b): ([i32; 3], [i32; 3]) = (
            std::array::from_fn(|i| lo[i] - self.eager_min[i]),
            std::array::from_fn(|i| hi[i] - self.eager_min[i] + 1),
        );
        if (0..3).any(|i| a[i] < 0 || b[i] > N) {
            return false;
        }
        let at =
            |x: i32, y: i32, z: i32| self.solids[(x as usize * M + y as usize) * M + z as usize];
        let sum = at(b[0], b[1], b[2])
            + at(a[0], a[1], b[2])
            + at(a[0], b[1], a[2])
            + at(b[0], a[1], a[2])
            - at(a[0], b[1], b[2])
            - at(b[0], a[1], b[2])
            - at(b[0], b[1], a[2])
            - at(a[0], a[1], a[2]);
        sum == 0
    }

    fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (dx, dy, dz) = (x - self.base[0], y - self.base[1], z - self.base[2]);
        let side = CACHE_SIDE as i32;
        ((0..side).contains(&dx) && (0..side).contains(&dy) && (0..side).contains(&dz))
            .then(|| (dx as usize * CACHE_SIDE + dy as usize) * CACHE_SIDE + dz as usize)
    }

    /// The block at `x, y, z` (its top bit: it stops motion), read from the
    /// world once per explosion.
    #[inline]
    fn block(&mut self, ctx: &ServerContext, level: &WorldLevel, x: i32, y: i32, z: i32) -> u32 {
        let Some(i) = self.index(x, y, z) else {
            let state = level.block(mc_tick::BlockPos::new(x, y, z));
            return state | (u32::from(ctx.rules.blocks_motion(state)) << 31);
        };
        let (stamp, value) = self.cells[i];
        if stamp == self.stamp {
            return value;
        }
        let state = level.block(mc_tick::BlockPos::new(x, y, z));
        let value = state | (u32::from(ctx.rules.blocks_motion(state)) << 31);
        self.cells[i] = (self.stamp, value);
        value
    }
}

/// The unit directions of the rays: the surface of a 16×16×16 grid.
fn directions() -> &'static [[f64; 3]] {
    static DIRS: OnceLock<Vec<[f64; 3]>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut out = Vec::new();
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
                    out.push(d);
                }
            }
        }
        out
    })
}

/// The blocks the rays reach (air among them), each once, into
/// `sc.positions`.
fn blown_positions(
    ctx: &ServerContext,
    level: &WorldLevel,
    blast: &Blast,
    rng: &mut Rng,
    sc: &mut Scratch,
) {
    let table = if blast.dangerous {
        weak_table(ctx)
    } else {
        table(ctx)
    };
    let step = f64::from(0.3f32);
    let (min_y, max_y) = (ctx.world.min_y, ctx.world.min_y + ctx.world.height as i32);
    sc.positions.clear();
    const N: i32 = 2 * EAGER_R + 1;
    let stamp = sc.stamp;
    let min = sc.eager_min;
    for d in directions() {
        let mut strength = blast.power * (0.7 + rng.unit() as f32 * 0.6);
        let mut at = blast.center;
        while strength > 0.0 {
            let (x, y, z) = (
                at[0].floor() as i32,
                at[1].floor() as i32,
                at[2].floor() as i32,
            );
            if y < min_y || y >= max_y {
                break;
            }
            let (rx, ry, rz) = (x - min[0], y - min[1], z - min[2]);
            let (resistance, fresh) =
                if (0..N).contains(&rx) && (0..N).contains(&ry) && (0..N).contains(&rz) {
                    let i = ((rx * N + ry) * N + rz) as usize;
                    let fresh = sc.eager_taken[i] != stamp;
                    (sc.resist[i], (i, fresh))
                } else {
                    // Beyond what was read (rays do not get this far).
                    let state = sc.block(ctx, level, x, y, z) & !SOLID;
                    (
                        table.get(state as usize).copied().unwrap_or(-1.0),
                        (usize::MAX, true),
                    )
                };
            if resistance >= 0.0 {
                strength -= (resistance + 0.3) * 0.3;
            }
            if strength > 0.0 {
                if fresh.0 != usize::MAX {
                    sc.eager_taken[fresh.0] = stamp;
                }
                if fresh.1 {
                    sc.positions.push(Position::new(x, y, z));
                }
            }
            at[0] += d[0] * step;
            at[1] += d[1] * step;
            at[2] += d[2] * step;
            strength -= 0.225_000_01;
        }
    }
}

/// Whether a ray from `from` to `to` passes through no block that stops
/// motion (voxel traversal).
fn clear(
    ctx: &ServerContext,
    level: &WorldLevel,
    sc: &mut Scratch,
    from: [f64; 3],
    to: [f64; 3],
) -> bool {
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
        if sc.block(ctx, level, cell[0], cell[1], cell[2]) & SOLID != 0 {
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
    sc: &mut Scratch,
    center: [f64; 3],
    min: [f64; 3],
    max: [f64; 3],
) -> f64 {
    // Nothing in the way anywhere between the box and the centre: every ray
    // is clear.
    let lo: [i32; 3] = std::array::from_fn(|i| min[i].min(center[i]).floor() as i32);
    let hi: [i32; 3] = std::array::from_fn(|i| max[i].max(center[i]).floor() as i32);
    if sc.open(lo, hi) {
        return 1.0;
    }
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
                if clear(ctx, level, sc, p, center) {
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

/// Entities by the cell of their position, so a blast looks at the few near
/// it. Entities only join during a batch (the vector is not reordered), and
/// their positions do not change meanwhile.
pub struct Grid {
    cells: mc_tick::fast::FastMap<(i32, i32, i32), Vec<u32>>,
}

const CELL: f64 = 8.0;

fn cell_of(pos: [f64; 3]) -> (i32, i32, i32) {
    (
        (pos[0] / CELL).floor() as i32,
        (pos[1] / CELL).floor() as i32,
        (pos[2] / CELL).floor() as i32,
    )
}

impl Grid {
    fn new(entities: &[Entity]) -> Self {
        let mut grid = Self {
            cells: Default::default(),
        };
        for (i, e) in entities.iter().enumerate() {
            if !e.removed {
                grid.add(i, e.pos);
            }
        }
        grid
    }

    fn add(&mut self, index: usize, pos: [f64; 3]) {
        self.cells
            .entry(cell_of(pos))
            .or_default()
            .push(index as u32);
    }

    /// Indices of the entities whose position is within `reach` of `c` (and
    /// then some: the cells touched).
    fn near(&self, c: [f64; 3], reach: f64, out: &mut Vec<u32>) {
        out.clear();
        let lo = cell_of([c[0] - reach, c[1] - reach, c[2] - reach]);
        let hi = cell_of([c[0] + reach, c[1] + reach, c[2] + reach]);
        for x in lo.0..=hi.0 {
            for y in lo.1..=hi.1 {
                for z in lo.2..=hi.2 {
                    if let Some(v) = self.cells.get(&(x, y, z)) {
                        out.extend_from_slice(v);
                    }
                }
            }
        }
    }
}

/// A player as an explosion sees them.
struct Victim {
    uuid: u128,
    mode: i32,
    pose: crate::context::Pose,
    flying: bool,
}

/// One explosion on its own.
#[cfg(test)]
fn explode(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    blast: &Blast,
) -> Vec<(i32, u128)> {
    explode_all(ctx, level, entities, std::slice::from_ref(blast))
}

/// The explosions of one tick, one after another: blocks go (with their
/// drops), entities in `entities` and players are hurt and pushed. Returns
/// the entities they removed (id, uuid).
pub fn explode_all(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    blasts: &[Blast],
) -> Vec<(i32, u128)> {
    if blasts.is_empty() {
        return Vec::new();
    }
    let mut sc = Scratch::default();
    let mut grid = Grid::new(entities);
    let mut rng = rng();
    let decay = ctx.game_rule_bool("minecraft:tnt_explosion_drop_decay");
    let victims: Vec<Victim> = lock(&ctx.players)
        .values()
        .filter(|p| p.dimension == ctx.world.dimension)
        .filter_map(|p| {
            p.pose.map(|pose| Victim {
                uuid: p.profile.uuid,
                mode: p.game_mode,
                pose,
                flying: p.flying,
            })
        })
        .collect();
    let mut gone = Vec::new();
    for blast in blasts {
        explode_one(
            ctx, level, entities, &mut grid, &mut sc, &mut rng, &victims, decay, blast, &mut gone,
        );
    }
    gone
}

#[allow(clippy::too_many_arguments)]
fn explode_one(
    ctx: &ServerContext,
    level: &mut WorldLevel,
    entities: &mut Vec<Entity>,
    grid: &mut Grid,
    sc: &mut Scratch,
    rng: &mut Rng,
    victims: &[Victim],
    decay: bool,
    blast: &Blast,
    gone: &mut Vec<(i32, u128)>,
) {
    sc.begin(ctx, blast.center, blast.dangerous);
    blown_positions(ctx, level, blast, rng, sc);
    let decay = if blast.mob {
        ctx.game_rule_bool("mob_explosion_drop_decay")
    } else {
        decay
    };
    if blast.mob && !ctx.game_rule_bool("mob_griefing") {
        sc.positions.clear();
    }
    let block_drops = ctx.game_rule_bool("block_drops");
    let count = sc.positions.len() as i32;
    let blocks = &ctx.data.blocks;
    // Shuffled, as vanilla does before dropping.
    for i in (1..sc.positions.len()).rev() {
        let j = rng.below(i as u32 + 1) as usize;
        sc.positions.swap(i, j);
    }
    let mut drops: Vec<(Position, crate::loot::Drop)> = Vec::new();
    for &p in &sc.positions {
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
            let mut tnt = primed(ctx, p, chained_fuse(rng), None);
            if let Kind::Tnt { owner, .. } = &mut tnt.kind {
                *owner = blast.owner;
            }
            level.set_block(p, ctx.shapes.air());
            primed_sound(ctx, &tnt);
            let _ = ctx.events.send(Event::EntitySpawned(tnt.clone()));
            grid.add(entities.len(), tnt.pos);
            entities.push(tnt);
            continue;
        }
        if crate::containers::kind_of(name).is_some() {
            let (items, _) = crate::containers::broken(ctx, p);
            for stack in items {
                drops.push((p, crate::loot::Drop::of(&stack)));
            }
            let _ = ctx.events.send(Event::ContainerGone { position: p });
        }
        let props = |k: &str| blocks.property(state, k).map(str::to_owned);
        if block_drops {
            drops.extend(
                ctx.loot
                    .drops_exploded(name, &props, decay.then_some(blast.power))
                    .into_iter()
                    .map(|d| (p, d)),
            );
        }
        level.set_block(p, ctx.shapes.air());
    }
    merge_and_drop(ctx, drops);
    // The blocks are gone: what entities see is read afresh.
    sc.begin(ctx, blast.center, false);
    sc.index_solids(ctx);

    // Entities within twice the power.
    let reach = f64::from(blast.power) * 2.0;
    let c = blast.center;
    let mut knockback: Vec<(u128, [f64; 3])> = Vec::new();
    let mut pushed = Vec::new();
    let mut candidates = std::mem::take(&mut sc.candidates);
    grid.near(c, reach + 2.0, &mut candidates);
    for &i in &candidates {
        let e = &mut entities[i as usize];
        if e.removed {
            continue;
        }
        let b = e.aabb();
        if !(0..3).all(|i| b.min[i] <= c[i] + reach + 1.0 && b.max[i] >= c[i] - reach - 1.0) {
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
        let impact = (1.0 - dist) * exposure(ctx, level, sc, c, b.min, b.max);
        if let Kind::Item { health, .. } = &mut e.kind {
            *health -= damage(impact, reach);
            if *health <= 0.0 {
                e.removed = true;
                gone.push((e.id, e.uuid));
                continue;
            }
        }
        if let Kind::Crystal(crystal) = &mut e.kind {
            crate::bosses::crystal::strike(crystal, blast.owner, "minecraft:explosion");
            continue;
        }
        if matches!(e.kind, Kind::Mob(_)) {
            let mut h = crate::mobs::hurt::Harm::new(
                damage(impact, reach),
                if blast.owner.is_some() {
                    "minecraft:player_explosion"
                } else {
                    "minecraft:explosion"
                },
            );
            h.player = blast.owner;
            h.by = blast
                .owner
                .and_then(|o| lock(&ctx.players).get(&o).map(|p| p.entity_id));
            h.direct = Some(blast.source);
            h.killer = blast.by.map(str::to_owned);
            crate::mobs::hurt::harm(ctx, e, &h);
        }
        for (v, d) in e.vel.iter_mut().zip(dir) {
            *v += d * impact;
        }
        // Clients move dropped items themselves and need the push.
        if matches!(e.kind, Kind::Item { .. }) {
            pushed.push(crate::dropped::row(e));
        }
    }
    sc.candidates = candidates;
    if !pushed.is_empty() {
        let _ = ctx.events.send(Event::EntitySync(pushed));
    }

    // Players.
    let attacker = blast
        .owner
        .and_then(|o| lock(&ctx.players).get(&o).map(|p| p.profile.name.clone()));
    for &Victim {
        uuid,
        mode,
        pose,
        flying,
    } in victims
    {
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
        let impact = (1.0 - dist) * exposure(ctx, level, sc, c, min, max);
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

    /// A cube of TNT set off in the middle, ticked until nothing is left.
    /// `cargo test -p mc-server --release mass_tnt_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn mass_tnt_cost() {
        let Some(ctx) = flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let tnt = block(&ctx, "minecraft:tnt");
        let (nx, ny, nz) = (24, 24, 24);
        let started = std::time::Instant::now();
        {
            let mut level = WorldLevel::new(&ctx);
            for x in 0..nx {
                for y in 0..ny {
                    for z in 0..nz {
                        level.set_block(Position::new(x, -60 + y, z), tnt);
                    }
                }
            }
            let _ = level.into_changes();
        }
        eprintln!("placed {} TNT in {:?}", nx * ny * nz, started.elapsed());
        let mut level = WorldLevel::new(&ctx);
        ignite(
            &ctx,
            &mut level,
            Position::new(nx / 2, -60 + ny / 2, nz / 2),
            None,
        );
        drop(level);
        let mut total = std::time::Duration::ZERO;
        let mut worst = std::time::Duration::ZERO;
        for tick in 1..=400 {
            let t = std::time::Instant::now();
            crate::tick::run_scheduled_ticks(&ctx);
            let took = t.elapsed();
            total += took;
            worst = worst.max(took);
            let n = lock(&ctx.entities).len();
            if tick % 10 == 0 || took.as_millis() > 200 {
                eprintln!("tick {tick}: {took:?}, {n} entities");
            }
            if n == 0 && tick > 5 {
                break;
            }
        }
        eprintln!("total {total:?}, worst tick {worst:?}");
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
            mob: false,
            dangerous: false,
            by: None,
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
            mob: false,
            dangerous: false,
            by: None,
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
