//! level.dat (`Data`) and saved data file fixes.

use mc_nbt::{Compound, Tag};

use super::nbt::{
    block_pos, boolean, child, compound_mut, each_compound, flag, inline_pos, int, list_mut, long,
    num, rename, rename_with, string,
};

fn remove_features(data: &mut Compound, flags: &[&str]) {
    let Some(enabled) = list_mut(data, "enabled_features") else {
        return;
    };
    let mut removed = Vec::new();
    enabled.retain(|f| match f {
        Tag::String(s) if flags.contains(&s.as_str()) => {
            removed.push(Tag::String(s.clone()));
            false
        }
        _ => true,
    });
    if removed.is_empty() {
        return;
    }
    match data.get_mut("removed_features") {
        Some(Tag::List(list)) => list.extend(removed),
        _ => {
            data.insert("removed_features", Tag::List(removed));
        }
    }
}

/// 3440: the 1.20 experiment is part of the game.
pub(super) fn feature_1_20(data: &mut Compound) {
    remove_features(data, &["minecraft:update_1_20"]);
    if let Some(settings) = compound_mut(data, "WorldGenSettings")
        && let Some(Tag::Compound(dims)) = settings.get_mut("dimensions")
    {
        for (_, dim) in dims.iter_mut() {
            if let Tag::Compound(dim) = dim
                && let Some(generator) = compound_mut(dim, "generator")
                && let Some(source) = compound_mut(generator, "biome_source")
                && string(source, "preset") == Some("minecraft:overworld_update_1_20")
            {
                source.insert("preset", Tag::String("minecraft:overworld".into()));
            }
        }
    }
}

/// 3939: the 1.21 experiment is part of the game.
pub(super) fn feature_1_21(data: &mut Compound) {
    remove_features(data, &["minecraft:update_1_21"]);
}

/// 4067: bundles are part of the game.
pub(super) fn feature_bundle(data: &mut Compound) {
    remove_features(data, &["minecraft:bundle"]);
}

/// 4180: the Winter Drop is part of the game.
pub(super) fn feature_winter_drop(data: &mut Compound) {
    remove_features(data, &["minecraft:winter_drop"]);
}

/// 4424: the locator bar is part of the game.
pub(super) fn feature_locator_bar(data: &mut Compound) {
    remove_features(data, &["minecraft:locator_bar"]);
}

/// 3459: the dragon fight moved out of `DimensionData`.
pub(super) fn dragon_fight(data: &mut Compound) {
    if data.get("DragonFight").is_some() {
        return;
    }
    let mut fight = super::nbt::compound(data, "DimensionData")
        .and_then(|d| super::nbt::compound(d, "1"))
        .and_then(|d| super::nbt::compound(d, "DragonFight"))
        .cloned()
        .unwrap_or_default();
    if let Some(pos) = fight.remove("ExitPortalLocation") {
        fight.insert("ExitPortalLocation", block_pos(pos));
    }
    data.insert("DragonFight", Tag::Compound(fight));
}

/// 4544: the border is one `world_border` compound.
pub(super) fn world_border(data: &mut Compound) {
    let mut b = Compound::new();
    let d = |data: &Compound, k: &str, default: f64| Tag::Double(num(data, k).unwrap_or(default));
    b.insert("center_x", d(data, "BorderCenterX", 0.0));
    b.insert("center_z", d(data, "BorderCenterZ", 0.0));
    b.insert("size", d(data, "BorderSize", f64::from(5.999_997e7_f32)));
    b.insert(
        "lerp_time",
        Tag::Long(long(data, "BorderSizeLerpTime").unwrap_or(0)),
    );
    b.insert("lerp_target", d(data, "BorderSizeLerpTarget", 0.0));
    b.insert("safe_zone", d(data, "BorderSafeZone", 5.0));
    b.insert("damage_per_block", d(data, "BorderDamagePerBlock", 0.2));
    b.insert(
        "warning_blocks",
        Tag::Int(int(data, "BorderWarningBlocks").unwrap_or(5)),
    );
    b.insert(
        "warning_time",
        Tag::Int(int(data, "BorderWarningTime").unwrap_or(15)),
    );
    for k in [
        "BorderCenterX",
        "BorderCenterZ",
        "BorderSize",
        "BorderSizeLerpTime",
        "BorderSizeLerpTarget",
        "BorderSafeZone",
        "BorderDamagePerBlock",
        "BorderWarningBlocks",
        "BorderWarningTime",
    ] {
        data.remove(k);
    }
    data.insert("world_border", Tag::Compound(b));
}

/// 4548: the world spawn is one `spawn` compound with a dimension.
pub(super) fn spawn(data: &mut Compound) {
    let mut s = Compound::new();
    s.insert("dimension", Tag::String("minecraft:overworld".into()));
    s.insert(
        "pos",
        super::nbt::int_array(
            int(data, "SpawnX").unwrap_or(0),
            int(data, "SpawnY").unwrap_or(0),
            int(data, "SpawnZ").unwrap_or(0),
        ),
    );
    s.insert(
        "yaw",
        Tag::Float(num(data, "SpawnAngle").unwrap_or(0.0) as f32),
    );
    s.insert("pitch", Tag::Float(0.0));
    for k in ["SpawnX", "SpawnY", "SpawnZ", "SpawnAngle"] {
        data.remove(k);
    }
    data.insert("spawn", Tag::Compound(s));
}

/// 4657: the border warning time is in ticks.
pub(super) fn border_warning_ticks(data: &mut Compound) {
    if let Some(b) = compound_mut(data, "world_border") {
        warning_ticks(b);
    }
}

fn warning_ticks(b: &mut Compound) {
    let seconds = int(b, "warning_time").unwrap_or(15);
    b.insert("warning_time", Tag::Int(seconds * 20));
}

/// Game rules renamed in 25w44a (4658), with how the old string value reads.
const RULES: [(&str, &str, Rule); 59] = [
    (
        "allowEnteringNetherUsingPortals",
        "allow_entering_nether_using_portals",
        Rule::Bool,
    ),
    (
        "announceAdvancements",
        "show_advancement_messages",
        Rule::Bool,
    ),
    (
        "blockExplosionDropDecay",
        "block_explosion_drop_decay",
        Rule::Bool,
    ),
    ("commandBlockOutput", "command_block_output", Rule::Bool),
    ("enableCommandBlocks", "command_blocks_work", Rule::Bool),
    ("commandBlocksEnabled", "command_blocks_work", Rule::Bool),
    (
        "commandModificationBlockLimit",
        "max_block_modifications",
        Rule::Int(1, i32::MAX),
    ),
    (
        "disableElytraMovementCheck",
        "elytra_movement_check",
        Rule::Not,
    ),
    (
        "disablePlayerMovementCheck",
        "player_movement_check",
        Rule::Not,
    ),
    ("disableRaids", "raids", Rule::Not),
    ("doDaylightCycle", "advance_time", Rule::Bool),
    ("doEntityDrops", "entity_drops", Rule::Bool),
    ("doImmediateRespawn", "immediate_respawn", Rule::Bool),
    ("doInsomnia", "spawn_phantoms", Rule::Bool),
    ("doLimitedCrafting", "limited_crafting", Rule::Bool),
    ("doMobLoot", "mob_drops", Rule::Bool),
    ("doMobSpawning", "spawn_mobs", Rule::Bool),
    ("doPatrolSpawning", "spawn_patrols", Rule::Bool),
    ("doTileDrops", "block_drops", Rule::Bool),
    ("doTraderSpawning", "spawn_wandering_traders", Rule::Bool),
    ("doVinesSpread", "spread_vines", Rule::Bool),
    ("doWardenSpawning", "spawn_wardens", Rule::Bool),
    ("doWeatherCycle", "advance_weather", Rule::Bool),
    ("drowningDamage", "drowning_damage", Rule::Bool),
    (
        "enderPearlsVanishOnDeath",
        "ender_pearls_vanish_on_death",
        Rule::Bool,
    ),
    ("fallDamage", "fall_damage", Rule::Bool),
    ("fireDamage", "fire_damage", Rule::Bool),
    ("forgiveDeadPlayers", "forgive_dead_players", Rule::Bool),
    ("freezeDamage", "freeze_damage", Rule::Bool),
    ("globalSoundEvents", "global_sound_events", Rule::Bool),
    ("keepInventory", "keep_inventory", Rule::Bool),
    ("lavaSourceConversion", "lava_source_conversion", Rule::Bool),
    ("locatorBar", "locator_bar", Rule::Bool),
    ("logAdminCommands", "log_admin_commands", Rule::Bool),
    (
        "maxCommandChainLength",
        "max_command_sequence_length",
        Rule::Int(0, i32::MAX),
    ),
    (
        "maxCommandForkCount",
        "max_command_forks",
        Rule::Int(0, i32::MAX),
    ),
    (
        "maxEntityCramming",
        "max_entity_cramming",
        Rule::Int(0, i32::MAX),
    ),
    (
        "minecartMaxSpeed",
        "max_minecart_speed",
        Rule::Int(i32::MIN, i32::MAX),
    ),
    (
        "mobExplosionDropDecay",
        "mob_explosion_drop_decay",
        Rule::Bool,
    ),
    ("mobGriefing", "mob_griefing", Rule::Bool),
    (
        "naturalRegeneration",
        "natural_health_regeneration",
        Rule::Bool,
    ),
    (
        "playersNetherPortalCreativeDelay",
        "players_nether_portal_creative_delay",
        Rule::Int(0, i32::MAX),
    ),
    (
        "playersNetherPortalDefaultDelay",
        "players_nether_portal_default_delay",
        Rule::Int(0, i32::MAX),
    ),
    (
        "playersSleepingPercentage",
        "players_sleeping_percentage",
        Rule::Int(0, i32::MAX),
    ),
    (
        "projectilesCanBreakBlocks",
        "projectiles_can_break_blocks",
        Rule::Bool,
    ),
    ("pvp", "pvp", Rule::Bool),
    (
        "randomTickSpeed",
        "random_tick_speed",
        Rule::Int(0, i32::MAX),
    ),
    ("reducedDebugInfo", "reduced_debug_info", Rule::Bool),
    ("sendCommandFeedback", "send_command_feedback", Rule::Bool),
    ("showDeathMessages", "show_death_messages", Rule::Bool),
    (
        "snowAccumulationHeight",
        "max_snow_accumulation_height",
        Rule::Int(0, 8),
    ),
    ("spawnMonsters", "spawn_monsters", Rule::Bool),
    (
        "spawnRadius",
        "respawn_radius",
        Rule::Int(i32::MIN, i32::MAX),
    ),
    ("spawnerBlocksEnabled", "spawner_blocks_work", Rule::Bool),
    (
        "spectatorsGenerateChunks",
        "spectators_generate_chunks",
        Rule::Bool,
    ),
    ("tntExplodes", "tnt_explodes", Rule::Bool),
    (
        "tntExplosionDropDecay",
        "tnt_explosion_drop_decay",
        Rule::Bool,
    ),
    ("universalAnger", "universal_anger", Rule::Bool),
    (
        "waterSourceConversion",
        "water_source_conversion",
        Rule::Bool,
    ),
];

#[derive(Clone, Copy)]
enum Rule {
    Bool,
    Not,
    Int(i32, i32),
}

fn rule_string(tag: &Tag) -> String {
    match tag {
        Tag::String(s) => s.clone(),
        Tag::Byte(b) => (*b != 0).to_string(),
        other => super::nbt::number(other).map_or_else(String::new, |n| (n as i64).to_string()),
    }
}

/// 4658: `GameRules` → `game_rules`, keyed by namespaced snake_case id with
/// typed values. Fire ticking became a spread radius around players.
pub(super) fn game_rules(data: &mut Compound) {
    let Some(Tag::Compound(mut rules)) = data.remove("GameRules") else {
        return;
    };
    let read_bool = |rules: &Compound, k: &str, default: bool| {
        rules.get(k).map_or(default, |t| rule_string(t) == "true")
    };
    let fire_tick = read_bool(&rules, "doFireTick", true);
    let away = read_bool(&rules, "allowFireTicksAwayFromPlayer", false);
    let radius = match (fire_tick, away) {
        (false, _) => 0,
        (true, false) => 128,
        (true, true) => -1,
    };
    for k in [
        "spawnChunkRadius",
        "entitiesWithPassengersCanUsePortals",
        "gameLoopFunction",
        "doFireTick",
        "allowFireTicksAwayFromPlayer",
    ] {
        rules.remove(k);
    }
    if radius != 128 {
        rules.insert(
            "minecraft:fire_spread_radius_around_player",
            Tag::Int(radius),
        );
    }
    for (old, new, kind) in RULES {
        let Some(value) = rules.remove(old) else {
            continue;
        };
        let text = rule_string(&value);
        let fixed = match kind {
            Rule::Bool => flag(text == "true"),
            Rule::Not => flag(text != "true"),
            Rule::Int(min, max) => match text.trim().parse::<i32>() {
                Ok(n) => Tag::Int(n.clamp(min, max)),
                Err(_) => value,
            },
        };
        rules.insert(format!("minecraft:{new}"), fixed);
    }
    data.insert("game_rules", Tag::Compound(rules));
}

/// 4766: the day time is the Overworld clock.
pub(super) fn day_time_clock(data: &mut Compound) {
    let time = long(data, "Time").unwrap_or(0);
    let day = data.remove("DayTime").map_or(time, |t| match t {
        Tag::Long(v) => v,
        other => super::nbt::number(&other).map_or(time, |v| v as i64),
    });
    let mut clock = Compound::new();
    clock.insert("total_ticks", Tag::Long(day));
    clock.insert("paused", flag(false));
    let mut clocks = Compound::new();
    clocks.insert("minecraft:overworld", Tag::Compound(clock));
    data.insert("world_clocks", Tag::Compound(clocks));
}

/// 4771: difficulty, hardcore and the lock are one compound.
pub(super) fn difficulty(data: &mut Compound) {
    let name = match int(data, "Difficulty").unwrap_or(2) {
        0 => "peaceful",
        1 => "easy",
        3 => "hard",
        _ => "normal",
    };
    let mut d = Compound::new();
    d.insert("difficulty", Tag::String(name.into()));
    d.insert("hardcore", flag(boolean(data, "hardcore").unwrap_or(false)));
    d.insert(
        "locked",
        flag(boolean(data, "DifficultyLocked").unwrap_or(false)),
    );
    for k in ["Difficulty", "hardcore", "DifficultyLocked"] {
        data.remove(k);
    }
    data.insert("difficulty_settings", Tag::Compound(d));
}

/// 4771: what is about to leave level.dat gets the shape of its own file.
pub(super) fn split_preparation(data: &mut Compound) {
    rename_with(data, "DragonFight", "dragon_fight", |t| {
        let Tag::Compound(mut f) = t else {
            return t;
        };
        let respawning = boolean(&f, "IsRespawning").unwrap_or(false);
        rename(&mut f, "NeedsStateScanning", "needs_state_scanning");
        rename(&mut f, "DragonKilled", "dragon_killed");
        rename(&mut f, "PreviouslyKilled", "previously_killed");
        rename(&mut f, "Dragon", "dragon_uuid");
        rename(&mut f, "ExitPortalLocation", "exit_portal_location");
        rename(&mut f, "Gateways", "gateways");
        if respawning {
            f.insert("respawn_stage", Tag::String("start".into()));
            f.insert("respawn_time", Tag::Int(0));
        }
        f.remove("IsRespawning");
        Tag::Compound(f)
    });

    let mut trader = Compound::new();
    trader.insert(
        "spawn_delay",
        Tag::Int(int(data, "WanderingTraderSpawnDelay").unwrap_or(24000)),
    );
    trader.insert(
        "spawn_chance",
        Tag::Int(int(data, "WanderingTraderSpawnChance").unwrap_or(25)),
    );
    for k in [
        "WanderingTraderSpawnDelay",
        "WanderingTraderSpawnChance",
        "WanderingTraderId",
    ] {
        data.remove(k);
    }
    data.insert("wandering_trader_migration_data", Tag::Compound(trader));

    let mut weather = Compound::new();
    for (from, to) in [
        ("clearWeatherTime", "clear_weather_time"),
        ("rainTime", "rain_time"),
        ("thunderTime", "thunder_time"),
    ] {
        weather.insert(to, Tag::Int(int(data, from).unwrap_or(0)));
        data.remove(from);
    }
    for k in ["raining", "thundering"] {
        weather.insert(k, flag(boolean(data, k).unwrap_or(false)));
        data.remove(k);
    }
    data.insert("weather_data", Tag::Compound(weather));

    let mut events: Vec<Tag> = match data.remove("ScheduledEvents") {
        Some(Tag::List(list)) => list,
        _ => Vec::new(),
    };
    for e in &mut events {
        if let Tag::Compound(e) = e {
            rename(e, "Name", "id");
            rename(e, "TriggerTime", "trigger_time");
            rename_with(e, "Callback", "callback", |t| {
                let Tag::Compound(mut c) = t else {
                    return t;
                };
                rename(&mut c, "Type", "type");
                rename(&mut c, "Name", "id");
                Tag::Compound(c)
            });
        }
    }
    child(data, "scheduled_events").insert("events", Tag::List(events));

    rename_with(data, "WorldGenSettings", "world_gen_settings", |t| {
        let Tag::Compound(mut s) = t else {
            return t;
        };
        rename(&mut s, "generate_features", "generate_structures");
        Tag::Compound(s)
    });
}

// ---- saved data files (`data`) ----

/// 3565: random sequences sit under `sequences`.
pub(super) fn random_sequences(data: &mut Compound) {
    let inner = std::mem::take(data);
    data.insert("sequences", Tag::Compound(inner));
}

/// 3566: display slots are named.
pub(super) fn display_slots(data: &mut Compound) {
    const TEAMS: [&str; 16] = [
        "black",
        "dark_blue",
        "dark_green",
        "dark_aqua",
        "dark_red",
        "dark_purple",
        "gold",
        "gray",
        "dark_gray",
        "blue",
        "green",
        "aqua",
        "red",
        "light_purple",
        "yellow",
        "white",
    ];
    let Some(Tag::Compound(slots)) = data.remove("DisplaySlots") else {
        return;
    };
    let mut out = Compound::new();
    for (key, value) in slots.iter() {
        let index = key
            .strip_prefix("slot_")
            .and_then(|n| n.parse::<usize>().ok());
        let name = match index {
            Some(0) => Some("list".to_owned()),
            Some(1) => Some("sidebar".to_owned()),
            Some(2) => Some("below_name".to_owned()),
            Some(i) => TEAMS.get(i - 3).map(|t| format!("sidebar.team.{t}")),
            None => None,
        };
        out.insert(name.unwrap_or_else(|| key.to_owned()), value.clone());
    }
    data.insert("DisplaySlots", Tag::Compound(out));
}

/// 3807: map banner markers store `[I; x, y, z]`.
pub(super) fn map_banner_pos(data: &mut Compound) {
    each_compound(data, "banners", |b| rename_with(b, "Pos", "Pos", block_pos));
}

/// 3813: map frames and banners have snake_case fields.
pub(super) fn map_fields(data: &mut Compound) {
    each_compound(data, "frames", |f| {
        rename_with(f, "Pos", "pos", block_pos);
        rename(f, "Rotation", "rotation");
        rename(f, "EntityId", "entity_id");
    });
    each_compound(data, "banners", |b| {
        rename(b, "Pos", "pos");
        rename(b, "Color", "color");
        rename(b, "Name", "name");
    });
}

/// 4297: forced chunks are tickets.
pub(super) fn forced_tickets(data: &mut Compound) {
    let Some(Tag::LongArray(forced)) = data.remove("Forced") else {
        return;
    };
    let tickets = forced
        .into_iter()
        .map(|pos| {
            let mut t = Compound::new();
            t.insert("type", Tag::String("minecraft:forced".into()));
            t.insert("level", Tag::Int(31));
            t.insert("ticks_left", Tag::Long(0));
            t.insert("chunk_pos", Tag::Long(pos));
            Tag::Compound(t)
        })
        .collect();
    data.insert("tickets", Tag::List(tickets));
}

/// 4309: ticket chunk positions are `[I; x, z]`.
pub(super) fn ticket_positions(data: &mut Compound) {
    each_compound(data, "tickets", |t| {
        if let Some(key) = long(t, "chunk_pos")
            && matches!(t.get("chunk_pos"), Some(Tag::Long(_)))
        {
            let x = key as i32;
            let z = (key >> 32) as i32;
            t.insert("chunk_pos", Tag::IntArray(vec![x, z]));
        }
    });
}

/// 4309: raid fields are snake_case.
pub(super) fn raids(data: &mut Compound) {
    if let Some(Tag::List(mut list)) = data.remove("Raids") {
        for r in &mut list {
            if let Tag::Compound(r) = r {
                inline_pos(r, "CX", "CY", "CZ", "center");
                for (from, to) in [
                    ("Id", "id"),
                    ("Started", "started"),
                    ("Active", "active"),
                    ("TicksActive", "ticks_active"),
                    ("BadOmenLevel", "raid_omen_level"),
                    ("GroupsSpawned", "groups_spawned"),
                    ("PreRaidTicks", "cooldown_ticks"),
                    ("PostRaidTicks", "post_raid_ticks"),
                    ("TotalHealth", "total_health"),
                    ("NumGroups", "group_count"),
                    ("Status", "status"),
                    ("HeroesOfTheVillage", "heroes_of_the_village"),
                ] {
                    rename(r, from, to);
                }
            }
        }
        data.insert("raids", Tag::List(list));
    }
    rename(data, "Tick", "tick");
    rename(data, "NextAvailableID", "next_id");
}

/// 4657 for a border already in its own file.
pub(super) fn border_file_warning(data: &mut Compound) {
    warning_ticks(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_rules_are_typed_and_renamed() {
        let mut rules = Compound::new();
        rules.insert("keepInventory", Tag::String("true".into()));
        rules.insert("doFireTick", Tag::String("false".into()));
        rules.insert("disableRaids", Tag::String("false".into()));
        rules.insert("randomTickSpeed", Tag::String("3".into()));
        rules.insert("myPluginRule", Tag::String("x".into()));
        let mut data = Compound::new();
        data.insert("GameRules", Tag::Compound(rules));
        game_rules(&mut data);
        let Some(Tag::Compound(r)) = data.get("game_rules") else {
            panic!("no game_rules");
        };
        assert_eq!(r.get("minecraft:keep_inventory"), Some(&Tag::Byte(1)));
        assert_eq!(r.get("minecraft:raids"), Some(&Tag::Byte(1)));
        assert_eq!(r.get("minecraft:random_tick_speed"), Some(&Tag::Int(3)));
        assert_eq!(
            r.get("minecraft:fire_spread_radius_around_player"),
            Some(&Tag::Int(0))
        );
        assert_eq!(r.get("myPluginRule"), Some(&Tag::String("x".into())));
    }

    #[test]
    fn spawn_border_and_difficulty() {
        let mut data = Compound::new();
        data.insert("SpawnX", Tag::Int(10));
        data.insert("SpawnY", Tag::Int(70));
        data.insert("SpawnZ", Tag::Int(-5));
        data.insert("BorderSize", Tag::Double(1000.0));
        data.insert("Difficulty", Tag::Byte(3));
        world_border(&mut data);
        spawn(&mut data);
        border_warning_ticks(&mut data);
        difficulty(&mut data);
        let Some(Tag::Compound(s)) = data.get("spawn") else {
            panic!("no spawn");
        };
        assert_eq!(s.get("pos"), Some(&Tag::IntArray(vec![10, 70, -5])));
        let Some(Tag::Compound(b)) = data.get("world_border") else {
            panic!("no world_border");
        };
        assert_eq!(b.get("size"), Some(&Tag::Double(1000.0)));
        assert_eq!(b.get("warning_time"), Some(&Tag::Int(300)));
        let Some(Tag::Compound(d)) = data.get("difficulty_settings") else {
            panic!("no difficulty_settings");
        };
        assert_eq!(d.get("difficulty"), Some(&Tag::String("hard".into())));
    }

    #[test]
    fn tickets_and_slots() {
        let mut data = Compound::new();
        data.insert("Forced", Tag::LongArray(vec![(5_i64 << 32) | 3]));
        forced_tickets(&mut data);
        ticket_positions(&mut data);
        let [Tag::Compound(t)] = super::super::nbt::list(&data, "tickets") else {
            panic!("no tickets");
        };
        assert_eq!(t.get("chunk_pos"), Some(&Tag::IntArray(vec![3, 5])));

        let mut slots = Compound::new();
        slots.insert("slot_1", Tag::String("kills".into()));
        slots.insert("slot_14", Tag::String("t".into()));
        let mut data = Compound::new();
        data.insert("DisplaySlots", Tag::Compound(slots));
        display_slots(&mut data);
        let Some(Tag::Compound(s)) = data.get("DisplaySlots") else {
            panic!("no slots");
        };
        assert!(s.get("sidebar").is_some() && s.get("sidebar.team.aqua").is_some());
    }
}
