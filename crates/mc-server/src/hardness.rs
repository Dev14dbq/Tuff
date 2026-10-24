//! How hard blocks are to break (minecraft.wiki "Hardness", "Breaking"),
//! and which of them need the right tool to give anything.
//!
//! The table follows the wiki's "Blocks by hardness". Blocks it does not
//! cover give `None`: the server then trusts the client's dig time instead
//! of refusing a break it cannot judge.

const WOODS: [&str; 11] = [
    "oak",
    "spruce",
    "birch",
    "jungle",
    "acacia",
    "dark_oak",
    "mangrove",
    "cherry",
    "pale_oak",
    "crimson",
    "warped",
];

/// A block's hardness; negative for blocks that cannot be broken and 100
/// for fluids. `name` is the full id (`minecraft:stone`).
pub fn hardness(name: &str) -> Option<f32> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    exact(name)
        .or_else(|| shaped(name))
        .or_else(|| by_suffix(name))
}

fn exact(name: &str) -> Option<f32> {
    Some(match name {
        "bedrock" | "barrier" | "command_block" | "chain_command_block"
        | "repeating_command_block" | "end_portal" | "end_portal_frame" | "end_gateway"
        | "structure_block" | "jigsaw" | "light" | "moving_piston" | "nether_portal"
        | "test_block" | "test_instance_block" => -1.0,
        "water" | "lava" => 100.0,
        "reinforced_deepslate" => 55.0,
        "netherite_block" | "crying_obsidian" | "obsidian" | "respawn_anchor"
        | "trial_spawner" | "vault" => 50.0,
        "ancient_debris" => 30.0,
        "ender_chest" => 22.5,
        "creaking_heart" | "heavy_core" => 10.0,
        "anvil" | "chipped_anvil" | "damaged_anvil" | "bell" | "coal_block" | "diamond_block"
        | "emerald_block" | "iron_block" | "raw_copper_block" | "raw_iron_block"
        | "raw_gold_block" | "redstone_block" | "enchanting_table" | "iron_door"
        | "iron_trapdoor" | "spawner" | "iron_bars" | "chain" => 5.0,
        "cobweb" => 4.0,
        "furnace" | "blast_furnace" | "smoker" | "dispenser" | "dropper" | "lodestone"
        | "stonecutter" | "lantern" | "soul_lantern" | "copper_lantern" | "chiseled_deepslate"
        | "cracked_deepslate_bricks" | "cracked_deepslate_tiles" => 3.5,
        "copper_block" | "gold_block" | "lapis_block" | "chiseled_copper" | "coal_ore"
        | "conduit" | "copper_bulb" | "copper_chest" | "copper_door" | "copper_grate"
        | "copper_trapdoor" | "copper_ore" | "cut_copper" | "deepslate" | "diamond_ore"
        | "dragon_egg" | "emerald_ore" | "end_stone" | "end_stone_bricks" | "gold_ore"
        | "hopper" | "iron_ore" | "lapis_ore" | "lightning_rod" | "nether_gold_ore"
        | "nether_quartz_ore" | "observer" | "redstone_ore" | "sculk_catalyst"
        | "sculk_shrieker" | "beacon" | "oak_door" | "spruce_door" | "birch_door"
        | "jungle_door" | "acacia_door" | "dark_oak_door" | "mangrove_door" | "cherry_door"
        | "pale_oak_door" | "crimson_door" | "warped_door" | "bamboo_door" => 3.0,
        "blue_ice" => 2.8,
        "barrel" | "cartography_table" | "chest" | "crafting_table" | "fletching_table"
        | "lectern" | "loom" | "smithing_table" | "trapped_chest" => 2.5,
        "bamboo_mosaic" | "bone_block" | "bricks" | "campfire" | "soul_campfire" | "cauldron"
        | "chiseled_nether_bricks" | "cobblestone" | "grindstone" | "jukebox"
        | "mossy_cobblestone" | "nether_bricks" | "red_nether_bricks" | "polished_blackstone"
        | "smooth_quartz" | "smooth_red_sandstone" | "smooth_sandstone" | "smooth_stone"
        | "bamboo_block" | "stripped_bamboo_block" | "petrified_oak_slab" | "shulker_box"
        | "nether_brick_fence" => 2.0,
        "stone" | "granite" | "diorite" | "andesite" | "polished_granite"
        | "polished_diorite" | "polished_andesite" | "tuff" | "polished_tuff"
        | "chiseled_tuff" | "tuff_bricks" | "chiseled_tuff_bricks" | "stone_bricks"
        | "mossy_stone_bricks" | "cracked_stone_bricks" | "chiseled_stone_bricks"
        | "smooth_stone_bricks" | "blackstone" | "gilded_blackstone"
        | "chiseled_polished_blackstone" | "cracked_polished_blackstone_bricks"
        | "polished_blackstone_bricks" | "prismarine" | "prismarine_bricks"
        | "dark_prismarine" | "purpur_block" | "purpur_pillar" | "chiseled_purpur"
        | "bookshelf" | "chiseled_bookshelf" | "crafter" | "piston" | "sticky_piston"
        | "piston_head" | "mud_bricks" | "resin_bricks" | "chiseled_resin_bricks"
        | "dripstone_block" | "pointed_dripstone" | "amethyst_block" | "budding_amethyst"
        | "amethyst_cluster" | "large_amethyst_bud" | "medium_amethyst_bud"
        | "small_amethyst_bud" | "sculk_sensor" | "calibrated_sculk_sensor"
        | "infested_deepslate" => 1.5,
        "basalt" | "polished_basalt" | "smooth_basalt" | "terracotta" => 1.25,
        "pumpkin" | "carved_pumpkin" | "jack_o_lantern" | "melon" | "shroomlight"
        | "nether_wart_block" | "warped_wart_block" | "packed_mud" | "bamboo"
        | "infested_cobblestone" => 1.0,
        "quartz_block" | "chiseled_quartz_block" | "quartz_pillar" | "quartz_bricks"
        | "note_block" | "sandstone" | "chiseled_sandstone" | "cut_sandstone"
        | "red_sandstone" | "chiseled_red_sandstone" | "cut_red_sandstone" => 0.8,
        "calcite" | "infested_stone" | "infested_stone_bricks" => 0.75,
        "rail" | "powered_rail" | "detector_rail" | "activator_rail" | "mangrove_roots"
        | "muddy_mangrove_roots" => 0.7,
        "beehive" | "clay" | "composter" | "farmland" | "grass_block" | "gravel"
        | "honeycomb_block" | "mycelium" | "sponge" | "wet_sponge" => 0.6,
        "brewing_stand" | "cake" | "coarse_dirt" | "dirt" | "dried_kelp_block" | "hay_block"
        | "ice" | "packed_ice" | "frosted_ice" | "lever" | "magma_block" | "mud" | "target"
        | "turtle_egg" | "sniffer_egg" | "podzol" | "rooted_dirt" | "sand" | "red_sand"
        | "soul_sand" | "soul_soil" => 0.5,
        "cactus" | "ladder" | "chorus_flower" | "chorus_plant" | "netherrack" => 0.4,
        "bee_nest" | "glowstone" | "redstone_lamp" | "sea_lantern" | "glass" | "glass_pane"
        | "tinted_glass" => 0.3,
        "item_frame" | "glow_item_frame" => 0.25,
        "powder_snow" | "suspicious_gravel" | "suspicious_sand" => 0.25,
        "daylight_detector" | "glow_lichen" | "sculk" | "sculk_vein" | "snow_block"
        | "vine" | "cocoa" => 0.2,
        "moss_block" | "pale_moss_block" | "moss_carpet" | "pale_moss_carpet" | "snow"
        | "big_dripleaf" | "candle" => 0.1,
        _ => return None,
    })
}

/// Slabs, stairs and walls follow their material, mostly; the full block of
/// stone, sandstone and quartz differs from its slab.
fn shaped(name: &str) -> Option<f32> {
    let (base, kind) = [("_slab", 0), ("_stairs", 1), ("_wall", 1)]
        .iter()
        .find_map(|(suffix, kind)| name.strip_suffix(suffix).map(|b| (b, *kind)))?;
    if WOODS.contains(&base) || base == "bamboo" || base == "bamboo_mosaic" {
        return Some(2.0);
    }
    // Slabs of "rock" materials are 2; their stairs and walls follow the block.
    let slab_two = matches!(
        base,
        "stone"
            | "smooth_stone"
            | "stone_brick"
            | "cobblestone"
            | "mossy_cobblestone"
            | "brick"
            | "nether_brick"
            | "red_nether_brick"
            | "sandstone"
            | "cut_sandstone"
            | "smooth_sandstone"
            | "red_sandstone"
            | "cut_red_sandstone"
            | "smooth_red_sandstone"
            | "quartz"
            | "smooth_quartz"
            | "purpur"
            | "blackstone"
            | "polished_blackstone"
            | "polished_blackstone_brick"
            | "petrified_oak"
    );
    if kind == 0 && slab_two {
        return Some(2.0);
    }
    let block = match base {
        "stone_brick" => "stone_bricks",
        "brick" => "bricks",
        "nether_brick" => "nether_bricks",
        "red_nether_brick" => "red_nether_bricks",
        "mossy_stone_brick" => "mossy_stone_bricks",
        "end_stone_brick" => "end_stone_bricks",
        "mud_brick" => "mud_bricks",
        "resin_brick" => "resin_bricks",
        "polished_blackstone_brick" => "polished_blackstone_bricks",
        "prismarine_brick" => "prismarine_bricks",
        "tuff_brick" => "tuff_bricks",
        "quartz" => "quartz_block",
        "purpur" => "purpur_block",
        "smooth_stone" => "smooth_stone",
        "cobbled_deepslate" | "polished_deepslate" | "deepslate_brick" | "deepslate_tile" => {
            return Some(3.5);
        }
        other => other,
    };
    if block.contains("cinnabar") || block.contains("sulfur") {
        return Some(1.5);
    }
    exact(block).or_else(|| match block {
        "polished_blackstone" | "blackstone" => Some(1.5),
        _ => None,
    })
}

fn by_suffix(name: &str) -> Option<f32> {
    let ends = |s: &str| name.ends_with(s);
    let wood = WOODS.iter().any(|w| name.starts_with(&format!("{w}_")));
    Some(if wood
        && (ends("_planks")
            || ends("_log")
            || ends("_wood")
            || ends("_stem")
            || ends("_hyphae")
            || ends("_fence")
            || ends("_fence_gate")
            || ends("_shelf"))
    {
        2.0
    } else if wood && ends("_trapdoor") {
        3.0
    } else if ends("_ore") && name.starts_with("deepslate_") {
        4.5
    } else if ends("_ore") {
        3.0
    } else if ends("_glazed_terracotta") {
        1.4
    } else if ends("_concrete_powder") {
        0.5
    } else if ends("_concrete") {
        1.8
    } else if ends("_terracotta") {
        1.25
    } else if ends("_wool") {
        0.8
    } else if ends("_carpet") || ends("_candle") {
        0.1
    } else if ends("_stained_glass") || ends("_stained_glass_pane") {
        0.3
    } else if ends("_bed") {
        0.2
    } else if ends("_shulker_box") {
        2.0
    } else if ends("_banner") || ends("_sign") || ends("_head") || ends("_skull") {
        1.0
    } else if ends("_leaves") {
        0.2
    } else if ends("_mushroom_block") || ends("mushroom_stem") {
        0.2
    } else if ends("_button") || ends("_pressure_plate") {
        0.5
    } else if ends("_coral_block") {
        1.5
    } else if ends("_copper") || ends("_copper_block") || ends("_cut_copper") {
        3.0
    } else if ends("_copper_golem_statue") || ends("_copper_chest") {
        3.0
    } else if ends("_nylium") {
        0.4
    } else if name.starts_with("infested_") {
        0.75
    } else if ends("_bars") && name.contains("copper") || ends("_chain") {
        5.0
    } else {
        return None;
    })
}

/// Blocks that give nothing without a pickaxe (`requires correct tool for
/// drops`) beyond those the tool tiers name: stone, ores, metal blocks...
/// `mineable_with_pickaxe` is the block's membership of that tag. Where in
/// doubt this says no, so a doubtful block is never expected to take
/// longer than it does.
pub fn needs_tool(name: &str, needs_tier: bool, mineable_with_pickaxe: bool) -> bool {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    if needs_tier || name == "cobweb" {
        return true;
    }
    mineable_with_pickaxe
        && !matches!(
            name,
            "ice"
                | "packed_ice"
                | "blue_ice"
                | "frosted_ice"
                | "piston"
                | "sticky_piston"
                | "piston_head"
                | "conduit"
                | "amethyst_cluster"
                | "large_amethyst_bud"
                | "medium_amethyst_bud"
                | "small_amethyst_bud"
                | "pointed_dripstone"
                | "dripstone_block"
                | "honeycomb_block"
                | "end_rod"
                | "moving_piston"
        )
        && !name.ends_with("_button")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wiki_values() {
        assert_eq!(hardness("minecraft:stone"), Some(1.5));
        assert_eq!(hardness("minecraft:cobblestone"), Some(2.0));
        assert_eq!(hardness("minecraft:dirt"), Some(0.5));
        assert_eq!(hardness("minecraft:oak_log"), Some(2.0));
        assert_eq!(hardness("minecraft:iron_ore"), Some(3.0));
        assert_eq!(hardness("minecraft:deepslate_iron_ore"), Some(4.5));
        assert_eq!(hardness("minecraft:obsidian"), Some(50.0));
        assert_eq!(hardness("minecraft:bedrock"), Some(-1.0));
        assert_eq!(hardness("minecraft:stone_slab"), Some(2.0));
        assert_eq!(hardness("minecraft:stone_stairs"), Some(1.5));
        assert_eq!(hardness("minecraft:sandstone_slab"), Some(2.0));
        assert_eq!(hardness("minecraft:sandstone_stairs"), Some(0.8));
        assert_eq!(hardness("minecraft:oak_stairs"), Some(2.0));
        assert_eq!(hardness("minecraft:white_wool"), Some(0.8));
        assert_eq!(hardness("minecraft:red_terracotta"), Some(1.25));
        assert_eq!(hardness("minecraft:cobbled_deepslate_wall"), Some(3.5));
        assert_eq!(hardness("minecraft:something_new"), None);
    }

    #[test]
    fn stone_needs_a_pickaxe_and_dirt_does_not() {
        assert!(needs_tool("minecraft:stone", false, true));
        assert!(needs_tool("minecraft:iron_ore", true, true));
        assert!(!needs_tool("minecraft:dirt", false, false));
        assert!(!needs_tool("minecraft:ice", false, true));
        assert!(!needs_tool("minecraft:stone_button", false, true));
    }
}
