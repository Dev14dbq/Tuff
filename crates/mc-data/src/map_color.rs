//! Base map colours (minecraft.wiki, "Map item format"): one id per material,
//! four shades stored later as `id * 4 + shade`. The blocks report has no
//! `map_color` in 26.3, so this table is the fallback. `0` is drawn as
//! unexplored (air, glass, and the other colours the wiki lists as NONE).

const NONE: u8 = 0;
const GRASS: u8 = 1;
const SAND: u8 = 2;
const WOOL: u8 = 3;
const FIRE: u8 = 4;
const ICE: u8 = 5;
const METAL: u8 = 6;
const PLANT: u8 = 7;
const SNOW: u8 = 8;
const CLAY: u8 = 9;
const DIRT: u8 = 10;
const STONE: u8 = 11;
const WATER: u8 = 12;
const WOOD: u8 = 13;
const QUARTZ: u8 = 14;
const ORANGE: u8 = 15;
const MAGENTA: u8 = 16;
const LIGHT_BLUE: u8 = 17;
const YELLOW: u8 = 18;
const LIME: u8 = 19;
const PINK: u8 = 20;
const GRAY: u8 = 21;
const LIGHT_GRAY: u8 = 22;
const CYAN: u8 = 23;
const PURPLE: u8 = 24;
const BROWN: u8 = 26;
const GREEN: u8 = 27;
const RED: u8 = 28;
const BLACK: u8 = 29;
const GOLD: u8 = 30;
const DIAMOND: u8 = 31;
const LAPIS: u8 = 32;
const EMERALD: u8 = 33;
const PODZOL: u8 = 34;
const NETHER: u8 = 35;
const CRIMSON_NYLIUM: u8 = 52;
const CRIMSON_STEM: u8 = 53;
const CRIMSON_HYPHAE: u8 = 54;
const WARPED_NYLIUM: u8 = 55;
const WARPED_STEM: u8 = 56;
const WARPED_HYPHAE: u8 = 57;
const WARPED_WART: u8 = 58;
const DEEPSLATE: u8 = 59;
const RAW_IRON: u8 = 60;
const GLOW_LICHEN: u8 = 61;

/// Longer dye names first so `light_gray` is not read as `gray`.
const DYES: &[(&str, usize)] = &[
    ("light_blue", 3),
    ("light_gray", 8),
    ("magenta", 2),
    ("orange", 1),
    ("yellow", 4),
    ("purple", 10),
    ("brown", 12),
    ("black", 15),
    ("white", 0),
    ("green", 13),
    ("lime", 5),
    ("pink", 6),
    ("gray", 7),
    ("cyan", 9),
    ("blue", 11),
    ("red", 14),
];

/// Base colour id of block `name` (`minecraft:` optional). `axis` is the log
/// axis (`x`/`y`/`z`); `part` is a bed's `head` or `foot`.
pub(super) fn of(name: &str, waterlogged: bool, axis: Option<&str>, part: Option<&str>) -> u8 {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let sideways = matches!(axis, Some("x" | "z"));
    if waterlogged && name.ends_with("_leaves") {
        return WATER;
    }
    if clear(name) {
        return NONE;
    }
    if let Some(color) = dyed(name, part == Some("head")) {
        return color;
    }
    if let Some(color) = wood(name, sideways) {
        return color;
    }
    if let Some(color) = material(name) {
        return color;
    }
    if let Some(color) = plant(name) {
        return color;
    }
    STONE
}

fn wool(dye: usize) -> u8 {
    if dye == 0 { SNOW } else { 14 + dye as u8 }
}

fn dyed(name: &str, bed_head: bool) -> Option<u8> {
    let (dye, rest) = DYES.iter().find_map(|(dye, index)| {
        name.strip_prefix(dye)
            .and_then(|rest| rest.strip_prefix('_'))
            .map(|rest| (*index, rest))
    })?;
    Some(match rest {
        "stained_glass_pane" => NONE,
        "banner" | "wall_banner" => WOOD,
        "terracotta" => 36 + dye as u8,
        "candle" => {
            if dye == 0 {
                WOOL
            } else {
                wool(dye)
            }
        }
        "bed" => {
            if bed_head {
                WOOL
            } else {
                wool(dye)
            }
        }
        "wool" | "carpet" | "concrete" | "concrete_powder" | "stained_glass"
        | "glazed_terracotta" | "shulker_box" => wool(dye),
        _ => return None,
    })
}

fn clear(name: &str) -> bool {
    matches!(
        name,
        "air"
            | "cave_air"
            | "void_air"
            | "barrier"
            | "structure_void"
            | "light"
            | "glass"
            | "glass_pane"
            | "iron_bars"
            | "ladder"
            | "lever"
            | "redstone_wire"
            | "repeater"
            | "comparator"
            | "tripwire"
            | "tripwire_hook"
            | "end_rod"
            | "nether_portal"
            | "cake"
            | "flower_pot"
            | "rail"
            | "torch"
            | "moving_piston"
    ) || name.ends_with("_glass_pane")
        || name.ends_with("_bars")
        || name.ends_with("_chain")
        || name.ends_with("_torch")
        || name.ends_with("_button")
        || name.ends_with("_rail")
        || name.starts_with("potted_")
        || name.ends_with("_candle_cake")
        || name == "candle_cake"
        || name.ends_with("_head")
        || name.ends_with("_skull")
}

fn is_wood_product(name: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "_log",
        "_wood",
        "_stem",
        "_hyphae",
        "_planks",
        "_stairs",
        "_slab",
        "_fence",
        "_fence_gate",
        "_door",
        "_trapdoor",
        "_sign",
        "_wall_sign",
        "_hanging_sign",
        "_wall_hanging_sign",
        "_pressure_plate",
        "_shelf",
        "_mosaic",
    ];
    SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        || name == "bamboo_block"
        || name == "stripped_bamboo_block"
}

fn wood(name: &str, sideways: bool) -> Option<u8> {
    if !is_wood_product(name) {
        return None;
    }
    if name.contains("crimson") {
        return Some(stem(name, CRIMSON_STEM, CRIMSON_HYPHAE));
    }
    if name.contains("warped") && !name.contains("wart") {
        return Some(stem(name, WARPED_STEM, WARPED_HYPHAE));
    }
    let (planks, side, stripped_side) = tree(name)?;
    if sideways && name.contains("stripped") && (name.ends_with("_log") || name.ends_with("_wood"))
    {
        return Some(stripped_side);
    }
    if sideways && !name.contains("stripped") && name.ends_with("_log") {
        return Some(side);
    }
    Some(planks)
}

fn stem(name: &str, stem_color: u8, hyphae: u8) -> u8 {
    if name.contains("hyphae") {
        hyphae
    } else {
        stem_color
    }
}

/// Plank colour, sideways-log colour, sideways stripped-log colour.
fn tree(name: &str) -> Option<(u8, u8, u8)> {
    const TREES: &[(&str, u8, u8, u8)] = &[
        ("pale_oak", QUARTZ, STONE, QUARTZ),
        ("dark_oak", BROWN, BROWN, BROWN),
        ("mangrove", RED, PODZOL, RED),
        ("cherry", 36, 43, 42),
        ("bamboo", YELLOW, YELLOW, YELLOW),
        ("acacia", ORANGE, STONE, ORANGE),
        ("jungle", DIRT, PODZOL, DIRT),
        ("spruce", PODZOL, BROWN, PODZOL),
        ("birch", SAND, QUARTZ, SAND),
        ("poplar", LIGHT_GRAY, PODZOL, LIGHT_GRAY),
        ("oak", WOOD, PODZOL, WOOD),
    ];
    TREES
        .iter()
        .find(|(kind, ..)| name.contains(kind))
        .map(|(_, planks, side, stripped)| (*planks, *side, *stripped))
}

fn material(name: &str) -> Option<u8> {
    if let Some(color) = copper(name) {
        return Some(color);
    }
    if name.contains("deepslate") {
        return Some(DEEPSLATE);
    }
    if name.contains("diorite") {
        return Some(QUARTZ);
    }
    if name.contains("prismarine_brick") || name.starts_with("dark_prismarine") {
        return Some(DIAMOND);
    }
    if name.contains("amethyst") {
        return Some(PURPLE);
    }
    if name.starts_with("infested_") {
        return Some(CLAY);
    }
    Some(match name {
        "grass_block" | "slime_block" => GRASS,
        "sand" | "suspicious_sand" | "sandstone" | "smooth_sandstone" | "chiseled_sandstone"
        | "cut_sandstone" | "glowstone" | "end_stone" | "end_stone_bricks" | "bone_block"
        | "scaffolding" | "candle" | "ochre_froglight" | "frogspawn" => SAND,
        "cobweb" | "mushroom_stem" => WOOL,
        "soul_fire" => LIGHT_BLUE,
        "lava" | "tnt" | "fire" | "redstone_block" => FIRE,
        "ice" | "frosted_ice" | "packed_ice" | "blue_ice" => ICE,
        "iron_block"
        | "iron_door"
        | "iron_trapdoor"
        | "brewing_stand"
        | "anvil"
        | "chipped_anvil"
        | "damaged_anvil"
        | "grindstone"
        | "lodestone"
        | "heavy_core"
        | "lantern"
        | "soul_lantern"
        | "heavy_weighted_pressure_plate" => METAL,
        "snow" | "snow_block" | "powder_snow" => SNOW,
        "clay" => CLAY,
        "dirt"
        | "coarse_dirt"
        | "farmland"
        | "dirt_path"
        | "rooted_dirt"
        | "packed_mud"
        | "brown_mushroom_block" => DIRT,
        "water" | "bubble_column" | "kelp" | "kelp_plant" | "seagrass" | "tall_seagrass" => WATER,
        "gold_block"
        | "raw_gold_block"
        | "light_weighted_pressure_plate"
        | "bell"
        | "potent_sulfur" => GOLD,
        "diamond_block" | "beacon" | "conduit" => DIAMOND,
        "lapis_block" => LAPIS,
        "emerald_block" => EMERALD,
        "podzol" => PODZOL,
        "netherrack" | "nether_bricks" | "red_nether_bricks" | "magma_block"
        | "nether_gold_ore" | "nether_quartz_ore" => NETHER,
        "quartz_block"
        | "smooth_quartz"
        | "chiseled_quartz_block"
        | "quartz_pillar"
        | "quartz_bricks"
        | "sea_lantern"
        | "target" => QUARTZ,
        "terracotta" | "pumpkin" | "carved_pumpkin" | "jack_o_lantern" | "red_sand"
        | "red_sandstone" | "honey_block" | "honeycomb_block" | "lightning_rod"
        | "creaking_heart" | "open_eyeblossom" => ORANGE,
        "purpur_block" | "purpur_pillar" => MAGENTA,
        "hay_block" | "sponge" | "wet_sponge" | "sulfur" | "short_dry_grass" | "tall_dry_grass" => {
            YELLOW
        }
        "melon" => LIME,
        "mycelium" => PURPLE,
        "prismarine" => CYAN,
        "bricks" | "nether_wart_block" | "nether_wart" | "enchanting_table" | "shroomlight"
        | "red_mushroom_block" | "cinnabar" => RED,
        "obsidian" | "crying_obsidian" | "coal_block" | "end_portal" | "end_gateway"
        | "dragon_egg" | "basalt" | "polished_basalt" | "smooth_basalt" | "netherite_block"
        | "ancient_debris" | "respawn_anchor" | "blackstone" | "gilded_blackstone" | "sculk"
        | "sculk_vein" | "sculk_catalyst" | "sculk_shrieker" => BLACK,
        "sculk_sensor" | "calibrated_sculk_sensor" => CYAN,
        "calcite" => 36,
        "tuff" => 43,
        "dripstone_block" | "pointed_dripstone" => 48,
        "mud" => 45,
        "mud_bricks" => 44,
        "moss_block" | "moss_carpet" | "dried_kelp_block" => GREEN,
        "pale_moss_block" | "pale_moss_carpet" | "pale_hanging_moss" => LIGHT_GRAY,
        "crimson_nylium" => CRIMSON_NYLIUM,
        "warped_nylium" => WARPED_NYLIUM,
        "warped_wart_block" => WARPED_WART,
        "raw_iron_block" => RAW_IRON,
        "glow_lichen" | "verdant_froglight" => GLOW_LICHEN,
        "soul_sand" | "soul_soil" | "leaf_litter" => BROWN,
        "pale_oak_leaves" | "pale_oak_sapling" | "closed_eyeblossom" => METAL,
        "cherry_leaves" | "cherry_sapling" | "cactus_flower" | "pearlescent_froglight" => PINK,
        "orange_poplar_leaves" => ORANGE,
        "yellow_poplar_leaves" => YELLOW,
        "red_poplar_leaves" => RED,
        "resin_block" | "resin_bricks" | "resin_clump" => 37,
        "tinted_glass" | "dried_ghast" => GRAY,
        "andesite" | "polished_andesite" | "granite" | "polished_granite" | "stone"
        | "cobblestone" | "bedrock" | "gravel" | "suspicious_gravel" => STONE,
        _ => return None,
    })
}

fn copper(name: &str) -> Option<u8> {
    if !name.contains("copper") || name.ends_with("_ore") {
        return None;
    }
    if name.contains("oxidized") {
        return Some(WARPED_NYLIUM);
    }
    if name.contains("weathered") {
        return Some(WARPED_STEM);
    }
    if name.contains("exposed") {
        return Some(44);
    }
    Some(ORANGE)
}

fn plant(name: &str) -> Option<u8> {
    if name.ends_with("_leaves")
        || name.ends_with("_sapling")
        || name.ends_with("_propagule")
        || matches!(
            name,
            "short_grass"
                | "tall_grass"
                | "fern"
                | "large_fern"
                | "vine"
                | "cave_vines"
                | "cave_vines_plant"
                | "twisting_vines"
                | "twisting_vines_plant"
                | "weeping_vines"
                | "weeping_vines_plant"
                | "sugar_cane"
                | "cactus"
                | "dead_bush"
                | "bamboo"
                | "bamboo_sapling"
                | "lily_pad"
                | "spore_blossom"
                | "hanging_roots"
                | "small_dripleaf"
                | "big_dripleaf"
                | "big_dripleaf_stem"
                | "pink_petals"
                | "wildflowers"
                | "bush"
                | "firefly_bush"
                | "sweet_berry_bush"
                | "nether_sprouts"
                | "crimson_roots"
                | "warped_roots"
                | "crimson_fungus"
                | "warped_fungus"
                | "brown_mushroom"
                | "red_mushroom"
                | "azalea"
                | "flowering_azalea"
                | "moss_carpet"
        )
        || name.ends_with("_flower")
        || name.starts_with("potted_")
        || name.starts_with("wheat")
        || name.starts_with("carrots")
        || name.starts_with("potatoes")
        || name.starts_with("beetroots")
        || name.ends_with("_stem")
        || name.ends_with("_crop")
    {
        Some(PLANT)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_colours_for_common_blocks() {
        assert_eq!(of("minecraft:air", false, None, None), NONE);
        assert_eq!(of("minecraft:glass", false, None, None), NONE);
        assert_eq!(of("minecraft:glass_pane", false, None, None), NONE);
        assert_eq!(
            of("minecraft:white_stained_glass_pane", false, None, None),
            NONE
        );
        assert_eq!(of("minecraft:grass_block", false, None, None), GRASS);
        assert_eq!(of("minecraft:slime_block", false, None, None), GRASS);
        assert_eq!(of("minecraft:sand", false, None, None), SAND);
        assert_eq!(of("minecraft:water", false, None, None), WATER);
        assert_eq!(of("minecraft:oak_leaves", true, None, None), WATER);
        assert_eq!(of("minecraft:oak_leaves", false, None, None), PLANT);
        assert_eq!(of("minecraft:pale_oak_leaves", false, None, None), METAL);
        assert_eq!(of("minecraft:stone", false, None, None), STONE);
        assert_eq!(of("minecraft:deepslate", false, None, None), DEEPSLATE);
        assert_eq!(
            of("minecraft:deepslate_iron_ore", false, None, None),
            DEEPSLATE
        );
        assert_eq!(of("minecraft:netherrack", false, None, None), NETHER);
        assert_eq!(of("minecraft:white_wool", false, None, None), SNOW);
        assert_eq!(of("minecraft:orange_wool", false, None, None), ORANGE);
        assert_eq!(of("minecraft:black_wool", false, None, None), BLACK);
        assert_eq!(of("minecraft:white_terracotta", false, None, None), 36);
        assert_eq!(of("minecraft:black_terracotta", false, None, None), 51);
        assert_eq!(of("minecraft:terracotta", false, None, None), ORANGE);
        assert_eq!(of("minecraft:white_candle", false, None, None), WOOL);
        assert_eq!(of("minecraft:oak_log", false, Some("y"), None), WOOD);
        assert_eq!(of("minecraft:oak_log", false, Some("x"), None), PODZOL);
        assert_eq!(of("minecraft:oak_planks", false, None, None), WOOD);
        assert_eq!(of("minecraft:red_banner", false, None, None), WOOD);
        assert_eq!(of("minecraft:white_bed", false, None, Some("head")), WOOL);
        assert_eq!(of("minecraft:white_bed", false, None, Some("foot")), SNOW);
        assert_eq!(
            of("minecraft:oxidized_copper", false, None, None),
            WARPED_NYLIUM
        );
        assert_eq!(of("minecraft:raw_iron_block", false, None, None), RAW_IRON);
        assert_eq!(of("minecraft:glow_lichen", false, None, None), GLOW_LICHEN);
        assert_eq!(
            of("minecraft:crimson_hyphae", false, None, None),
            CRIMSON_HYPHAE
        );
        assert_eq!(of("minecraft:warped_stem", false, None, None), WARPED_STEM);
    }
}
