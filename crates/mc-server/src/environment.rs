//! Environment attributes (26.x, minecraft.wiki "Environment attribute"):
//! the gameplay rules a dimension type sets and a biome may set over it,
//! read from the data pack, so that a pack's own dimensions and biomes
//! change them as vanilla's do.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use mc_protocol::Position;
use serde_json::Value;

use crate::context::ServerContext;
use crate::tick::lock;

pub const WATER_EVAPORATES: &str = "minecraft:gameplay/water_evaporates";
pub const RESPAWN_ANCHOR_WORKS: &str = "minecraft:gameplay/respawn_anchor_works";
pub const PIGLINS_ZOMBIFY: &str = "minecraft:gameplay/piglins_zombify";
pub const CAN_START_RAID: &str = "minecraft:gameplay/can_start_raid";
pub const SNOW_GOLEM_MELTS: &str = "minecraft:gameplay/snow_golem_melts";
pub const NETHER_PORTAL_SPAWNS_PIGLIN: &str = "minecraft:gameplay/nether_portal_spawns_piglin";
pub const BED_RULE: &str = "minecraft:gameplay/bed_rule";
pub const STRAW_BED_RULE: &str = "minecraft:gameplay/straw_bed_rule";

/// The `attributes` of a data pack entry, read once.
fn attributes(ctx: &ServerContext, registry: &str, name: &str) -> Value {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), Value>>> = OnceLock::new();
    let mut cache = lock(CACHE.get_or_init(Default::default));
    let key = (registry.to_owned(), name.to_owned());
    cache
        .entry(key)
        .or_insert_with(|| {
            ctx.data
                .data_pack()
                .read_entry(registry, name)
                .map(|json| json["attributes"].clone())
                .unwrap_or(Value::Null)
        })
        .clone()
}

fn biome_at(ctx: &ServerContext, at: [i32; 3]) -> Option<String> {
    let id = ctx
        .world
        .biome(Position::new(at[0], at[1], at[2]), &ctx.data.blocks)?;
    let name = ctx.world.biome_name(id)?;
    Some(mc_command::args::full_id(name))
}

/// An attribute where `at` is (the biome's, else the dimension's), or the
/// dimension's when no place is given. A biome's modifier of the
/// dimension's value (an object with a `modifier`) is not applied yet.
pub fn get(ctx: &ServerContext, at: Option<[i32; 3]>, key: &str) -> Option<Value> {
    let plain = |v: &Value| !v.is_null() && v.get("modifier").is_none();
    if let Some(biome) = at.and_then(|p| biome_at(ctx, p)) {
        let v = &attributes(ctx, "minecraft:worldgen/biome", &biome)[key];
        if plain(v) {
            return Some(v.clone());
        }
    }
    let v = attributes(ctx, "minecraft:dimension_type", &ctx.world.dimension)[key].clone();
    plain(&v).then_some(v)
}

/// A yes-or-no attribute, `default` where nothing sets it.
pub fn flag(ctx: &ServerContext, at: Option<[i32; 3]>, key: &str, default: bool) -> bool {
    get(ctx, at, key)
        .and_then(|v| v.as_bool())
        .unwrap_or(default)
}

/// What a bed does where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BedRule {
    /// `always`, `when_dark` or `never`.
    pub can_sleep: String,
    pub can_set_spawn: String,
    /// It blows up when used.
    pub explodes: bool,
    /// It breaks when the sleeper gets up (a straw bed).
    pub destroy_on_leave: bool,
}

pub fn bed_rule(ctx: &ServerContext, at: [i32; 3], straw: bool) -> BedRule {
    let key = if straw { STRAW_BED_RULE } else { BED_RULE };
    let v = get(ctx, Some(at), key).unwrap_or(Value::Null);
    let word = |k: &str, d: &str| v[k].as_str().unwrap_or(d).to_owned();
    BedRule {
        can_sleep: word("can_sleep", "when_dark"),
        can_set_spawn: word("can_set_spawn", "always"),
        explodes: v["destroy_on_use"].as_bool().unwrap_or(false),
        destroy_on_leave: v["destroy_on_leave"].as_bool().unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dimension_sets_rules_and_a_biome_adds_its_own() {
        let Some(ctx) = crate::projectiles::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        // The test world is the overworld.
        assert!(!flag(&ctx, None, WATER_EVAPORATES, false));
        assert!(!flag(&ctx, None, RESPAWN_ANCHOR_WORKS, false));
        assert!(flag(&ctx, None, PIGLINS_ZOMBIFY, true));
        let bed = bed_rule(&ctx, [0, -60, 0], false);
        assert_eq!(
            (
                bed.can_sleep.as_str(),
                bed.can_set_spawn.as_str(),
                bed.explodes,
                bed.destroy_on_leave
            ),
            ("when_dark", "always", false, false)
        );
        let straw = bed_rule(&ctx, [0, -60, 0], true);
        assert_eq!(
            (
                straw.can_sleep.as_str(),
                straw.can_set_spawn.as_str(),
                straw.explodes,
                straw.destroy_on_leave
            ),
            ("when_dark", "never", false, true)
        );
        let nether = attributes(&ctx, "minecraft:dimension_type", "minecraft:the_nether");
        assert_eq!(nether[WATER_EVAPORATES], true);
        assert_eq!(nether[BED_RULE]["destroy_on_use"], true);
        let desert = attributes(&ctx, "minecraft:worldgen/biome", "minecraft:desert");
        assert_eq!(desert[SNOW_GOLEM_MELTS], true);
        let swamp = attributes(&ctx, "minecraft:worldgen/biome", "minecraft:swamp");
        assert_eq!(swamp["minecraft:gameplay/increased_fire_burnout"], true);
        let plains = attributes(&ctx, "minecraft:worldgen/biome", "minecraft:plains");
        assert!(plains["minecraft:gameplay/increased_fire_burnout"].is_null());
    }

    #[test]
    fn fire_on_bedrock_lasts_only_in_the_end() {
        let Some(ctx) = crate::projectiles::tests::flat() else {
            eprintln!("skipped: no game data");
            return;
        };
        let over = crate::level::WorldLevel::new(&ctx);
        assert!(mc_tick::Level::burns_forever(&over, "netherrack"));
        assert!(mc_tick::Level::burns_forever(&over, "magma_block"));
        assert!(!mc_tick::Level::burns_forever(&over, "bedrock"));
        let end = ctx.dimension(2).expect("the end");
        let end_level = crate::level::WorldLevel::new(&end);
        assert!(mc_tick::Level::burns_forever(&end_level, "bedrock"));
        let nether = ctx.dimension(1).expect("the nether");
        let nether_level = crate::level::WorldLevel::new(&nether);
        assert!(!mc_tick::Level::burns_forever(&nether_level, "bedrock"));
        assert!(mc_tick::Level::burns_forever(&nether_level, "netherrack"));
    }
}
