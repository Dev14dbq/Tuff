//! Every fix from 1.18 (2860) to 26.3, in the order vanilla applies them.
//! A version with several fixes lists them in vanilla's order too.

use mc_nbt::Compound;

use super::Saved;
use super::block_entity as be;
use super::chunk;
use super::components as comp;
use super::entity;
use super::item;
use super::level;
use super::text;
use super::walk::Apply::{self, *};

const fn ent(f: fn(&str, &mut Compound)) -> Apply {
    Entity { players: false, f }
}

/// Fixes that apply to players as well as other entities.
const fn living(f: fn(&str, &mut Compound)) -> Apply {
    Entity { players: true, f }
}

fn pottery_shards(_: &str, i: &mut Compound) {
    item::rename_id(
        i,
        &[
            (
                "minecraft:pottery_shard_archer",
                "minecraft:archer_pottery_shard",
            ),
            (
                "minecraft:pottery_shard_prize",
                "minecraft:prize_pottery_shard",
            ),
            (
                "minecraft:pottery_shard_arms_up",
                "minecraft:arms_up_pottery_shard",
            ),
            (
                "minecraft:pottery_shard_skull",
                "minecraft:skull_pottery_shard",
            ),
        ],
    );
}

fn pottery_sherds(id: &str, i: &mut Compound) {
    if id.ends_with("_pottery_shard") {
        let new = id.replace("_pottery_shard", "_pottery_sherd");
        i.insert("id", mc_nbt::Tag::String(new));
    }
}

fn short_grass_item(_: &str, i: &mut Compound) {
    item::rename_id(i, &[("minecraft:grass", "minecraft:short_grass")]);
}

fn short_grass_block(s: &mut Compound) {
    chunk::rename_block(s, &[("minecraft:grass", "minecraft:short_grass")]);
}

fn turtle_scute(_: &str, i: &mut Compound) {
    item::rename_id(i, &[("minecraft:scute", "minecraft:turtle_scute")]);
}

fn iron_chain_item(_: &str, i: &mut Compound) {
    item::rename_id(i, &[("minecraft:chain", "minecraft:iron_chain")]);
}

fn iron_chain_block(s: &mut Compound) {
    chunk::rename_block(s, &[("minecraft:chain", "minecraft:iron_chain")]);
}

fn explorer_map_names(_: &str, i: &mut Compound) {
    item::rename_id(
        i,
        &[
            (
                "minecraft:ocean_explorer_map",
                "minecraft:ocean_monument_map",
            ),
            ("minecraft:swamp_explorer_map", "minecraft:swamp_hut_map"),
            (
                "minecraft:trial_explorer_map",
                "minecraft:buried_trial_chambers_map",
            ),
            (
                "minecraft:woodland_explorer_map",
                "minecraft:woodland_mansion_map",
            ),
            (
                "minecraft:jungle_explorer_map",
                "minecraft:jungle_pyramid_map",
            ),
        ],
    );
}

fn bed_components(id: &str, c: &mut Compound) {
    item::drop_bed_data(id, c);
}

/// `(version, fix)`, ascending.
pub(super) const FIXES: &[(i32, Apply)] = &[
    (2970, Chunk(chunk::configured_structures)),
    (3077, Chunk(chunk::unlit_light)),
    (3086, ent(entity::cat_and_frog_variants)),
    (3090, ent(entity::painting_fields)),
    (3093, ent(entity::goat_horns)),
    (3094, Item(item::goat_horn)),
    (3097, Item(item::filtered_books)),
    (3097, BlockEntity(be::filtered_signs)),
    (3097, ent(entity::british_shorthair)),
    (3097, Poi(chunk::drop_jobless_poi)),
    (3108, Chunk(chunk::blending_outside_overworld)),
    (3322, living(entity::effect_durations)),
    (3322, Item(item::effect_durations)),
    (3438, BlockEntity(be::brushable)),
    (3438, Item(pottery_shards)),
    (3439, BlockEntity(be::double_sided)),
    (3440, Level(level::feature_1_20)),
    (3447, Item(pottery_sherds)),
    (3450, Chunk(chunk::remap_statuses)),
    (3451, Chunk(chunk::delete_light)),
    (3459, Level(level::dragon_fight)),
    (3564, BlockEntity(be::sign_filtered_lines)),
    (3565, Saved(Saved::RandomSequences, level::random_sequences)),
    (3566, Saved(Saved::Scoreboard, level::display_slots)),
    (3568, BlockEntity(be::beacon_effects)),
    (3568, living(entity::effect_ids)),
    (3568, Item(item::effect_ids)),
    (3683, ent(entity::primed_tnt)),
    (3685, ent(item::projectile_item)),
    (3692, BlockState(short_grass_block)),
    (3692, Item(short_grass_item)),
    (3800, Item(turtle_scute)),
    (3803, Item(item::sweeping_edge)),
    (3807, Saved(Saved::Map, level::map_banner_pos)),
    (3808, ent(entity::body_armor)),
    (3809, ent(entity::chested_slots)),
    (3812, ent(entity::wolf_health)),
    (3813, ent(entity::block_positions)),
    (3813, BlockEntity(be::block_positions)),
    (3813, Saved(Saved::Map, level::map_fields)),
    (3813, Item(item::compass_pos)),
    (3814, Item(item::jump_strength)),
    (3814, living(entity::jump_strength)),
    (3818, BlockEntity(be::beehive)),
    (3818, BlockEntity(be::banner_patterns)),
    (3818, ent(item::arrow_potion_into_item)),
    (3818, Item(item::componentize)),
    (3818, ent(entity::cloud_potion)),
    (3820, BlockEntity(be::skull_profile)),
    (3820, Components(comp::lodestone_target)),
    (3825, Components(comp::name_to_item_name)),
    (3825, BlockEntity(be::ominous_banner_name)),
    (3825, BlockEntity(be::trial_spawner_config)),
    (3833, BlockEntity(be::brushable_empty_item)),
    (3939, Level(level::feature_1_21)),
    (3945, Components(comp::attribute_modifier_ids)),
    (3945, living(entity::attributes)),
    (3945, BlockEntity(be::jukebox)),
    (4054, Components(comp::ominous_rarity)),
    (4055, Components(comp::attribute_prefixes)),
    (4055, living(entity::attribute_prefixes)),
    (4059, Components(comp::food_to_consumable)),
    (4064, Components(comp::fire_resistant)),
    (4067, ent(entity::boats)),
    (4067, Level(level::feature_bundle)),
    (4068, Components(comp::lock)),
    (4068, BlockEntity(be::lock)),
    (4081, ent(entity::salmon_size)),
    (4173, ent(entity::tnt_minecart)),
    (4175, Components(comp::equippable_and_model_data)),
    (4176, BlockEntity(be::broken_lock)),
    (4176, Components(comp::broken_lock)),
    (4180, Level(level::feature_winter_drop)),
    (4181, BlockEntity(be::furnace)),
    (4187, ent(entity::follow_range)),
    (4290, Text(text::unflatten)),
    (4291, Text(text::flags_and_legacy_hover)),
    (4292, Text(text::events)),
    (4293, ent(entity::drop_chances)),
    (4294, BlockState(chunk::creaking_heart)),
    (4296, ent(entity::cloud_duration_scale)),
    (4297, Saved(Saved::Tickets, level::forced_tickets)),
    (4299, Components(comp::spawner_variants)),
    (4300, ent(entity::saddles)),
    (4301, ent(entity::equipment)),
    (4303, living(entity::fall_distance)),
    (4305, BlockState(chunk::test_block_mode)),
    (4306, ent(entity::thrown_potions)),
    (4307, Components(comp::tooltip_display)),
    (4309, Saved(Saved::Raids, level::raids)),
    (4309, Saved(Saved::Tickets, level::ticket_positions)),
    (4312, Player(entity::player_equipment)),
    (4314, Player(entity::player_positions)),
    (4314, ent(entity::inline_positions)),
    (4424, Level(level::feature_locator_bar)),
    (4535, ent(entity::copper_golem)),
    (4537, Chunk(chunk::delete_light)),
    (4541, BlockState(iron_chain_block)),
    (4541, Item(iron_chain_item)),
    (4544, Level(level::world_border)),
    (4548, Level(level::spawn)),
    (4548, Player(entity::player_respawn)),
    (4649, Components(comp::trident_animation)),
    (4657, Level(level::border_warning_ticks)),
    (4657, Saved(Saved::WorldBorder, level::border_file_warning)),
    (4658, Level(level::game_rules)),
    (4763, ent(entity::villager_data_finalized)),
    (4766, Level(level::day_time_clock)),
    (4769, Player(entity::recipe_book)),
    (4771, Level(level::difficulty)),
    (4771, Level(level::split_preparation)),
    (4885, Components(bed_components)),
    (4885, ent(entity::falling_bed)),
    (4885, Chunk(chunk::drop_bed_entities)),
    (4888, Components(comp::name_tag_attributes)),
    (4888, living(entity::name_tag_attribute)),
    (4996, Components(comp::pot_decorations)),
    (4996, BlockEntity(be::pot_sherds)),
    (4997, Chunk(chunk::reset_blending)),
    (5002, BlockEntity(be::sign_op_flag)),
    (5006, BlockState(chunk::state_field_names)),
    (5007, Components(comp::swing_animation)),
    (5008, Item(comp::explorer_map)),
    (5008, Components(comp::map_color)),
    (5009, Chunk(chunk::blending_heights)),
    (5012, Item(explorer_map_names)),
    (5013, Chunk(chunk::merge_terrain_statuses)),
    (5014, Components(comp::block_transformer)),
    (5016, Item(comp::buried_treasure_name)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes_are_in_version_order() {
        assert!(FIXES.windows(2).all(|w| w[0].0 <= w[1].0));
    }
}
