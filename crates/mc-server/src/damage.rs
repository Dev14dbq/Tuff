//! Damage: how much of a hit gets through and what the death message says.
//! From the wiki ("Damage", "Armor", "Fall damage", "Drowning", "Death
//! messages"); per-type facts (`scaling`, `exhaustion`) come from the data
//! pack's `damage_type` entries.

use crate::context::ServerContext;
use crate::tick::lock;

/// Air a player holds, in ticks.
pub const MAX_AIR: i32 = 300;

/// Damage that scales with difficulty: peaceful none, easy half plus one
/// (never more than it was), normal as is, hard half again.
pub fn scale_for_difficulty(amount: f32, difficulty: i32) -> f32 {
    match difficulty {
        0 => 0.0,
        1 => (amount / 2.0 + 1.0).min(amount),
        2 => amount,
        _ => amount * 1.5,
    }
}

/// Damage types armor does not reduce (the `bypasses_armor` tag).
const BYPASSES_ARMOR: [&str; 19] = [
    "minecraft:bad_respawn_point",
    "minecraft:cramming",
    "minecraft:dragon_breath",
    "minecraft:drown",
    "minecraft:dry_out",
    "minecraft:ender_pearl",
    "minecraft:fall",
    "minecraft:fly_into_wall",
    "minecraft:freeze",
    "minecraft:generic_kill",
    "minecraft:in_wall",
    "minecraft:indirect_magic",
    "minecraft:magic",
    "minecraft:out_of_world",
    "minecraft:outside_border",
    "minecraft:sonic_boom",
    "minecraft:stalagmite",
    "minecraft:starve",
    "minecraft:wither",
];

/// Damage after armor points and toughness: each armor point takes 4%, at
/// most 80%; toughness makes big hits count for less.
pub fn armor_reduce(amount: f32, armor: f32, toughness: f32, damage_type: &str) -> f32 {
    if BYPASSES_ARMOR.contains(&damage_type) {
        return amount;
    }
    let f = 2.0 + toughness / 4.0;
    let points = (armor - amount / f).clamp(armor * 0.2, 20.0);
    amount * (1.0 - points / 25.0)
}

/// (armor points, toughness) of an armor item.
fn armor_of(name: &str) -> (f32, f32) {
    let Some((material, piece)) = name.strip_prefix("minecraft:").and_then(|n| {
        let (m, p) = n.rsplit_once('_')?;
        Some((m, p))
    }) else {
        return (0.0, 0.0);
    };
    let slot = match piece {
        "helmet" => 0,
        "chestplate" => 1,
        "leggings" => 2,
        "boots" => 3,
        _ => return (0.0, 0.0),
    };
    let points: [f32; 4] = match material {
        "leather" => [1.0, 3.0, 2.0, 1.0],
        "golden" => [2.0, 5.0, 3.0, 1.0],
        "chainmail" => [2.0, 5.0, 4.0, 1.0],
        "iron" => [2.0, 6.0, 5.0, 2.0],
        "diamond" => [3.0, 8.0, 6.0, 3.0],
        "netherite" => [3.0, 8.0, 6.0, 3.0],
        "turtle" => [2.0, 0.0, 0.0, 0.0],
        _ => return (0.0, 0.0),
    };
    let toughness = match material {
        "diamond" => 2.0,
        "netherite" => 3.0,
        _ => 0.0,
    };
    (points[slot], toughness)
}

/// A player's armor points and toughness from what they wear (slots 5–8).
pub fn worn_armor(ctx: &ServerContext, uuid: u128) -> (f32, f32) {
    let inventories = lock(&ctx.inventories);
    let Some(hands) = inventories.get(&uuid) else {
        return (0.0, 0.0);
    };
    hands.slots[5..9]
        .iter()
        .flatten()
        .filter_map(|s| ctx.items.name(s.item))
        .map(armor_of)
        .fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1))
}

/// A damage type's data-pack entry.
fn entry(ctx: &ServerContext, damage_type: &str) -> Option<serde_json::Value> {
    let path = damage_type
        .strip_prefix("minecraft:")
        .unwrap_or(damage_type);
    let file = ctx
        .data
        .data_pack()
        .root
        .join(format!("minecraft/damage_type/{path}.json"));
    serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
}

/// Whether a game rule spares players this damage: `fall_damage`,
/// `fire_damage`, `drowning_damage` and `freeze_damage` off turn off the
/// types of the matching damage-type tag.
pub fn ruled_out(ctx: &ServerContext, damage_type: &str) -> bool {
    let rules = crate::tick::lock(&ctx.game_rules);
    [
        ("is_fall", "fall_damage"),
        ("is_fire", "fire_damage"),
        ("is_drowning", "drowning_damage"),
        ("is_freezing", "freeze_damage"),
    ]
    .iter()
    .any(|(tag, rule)| {
        !rules.bool(rule)
            && ctx
                .commands
                .env
                .tag("minecraft:damage_type", tag)
                .iter()
                .any(|t| t == damage_type)
    })
}

/// Whether the type's damage always scales with difficulty.
pub fn scales_always(ctx: &ServerContext, damage_type: &str) -> bool {
    entry(ctx, damage_type).is_some_and(|e| e["scaling"] == "always")
}

/// Exhaustion a hit of this type costs (0.1 when unknown).
pub fn exhaustion(ctx: &ServerContext, damage_type: &str) -> f32 {
    entry(ctx, damage_type)
        .and_then(|e| e["exhaustion"].as_f64())
        .map_or(0.1, |e| e as f32)
}

/// The message of a fall that killed, by what the player was last on:
/// `death.fell.accident.*`, or `death.fell.assist` after a recent attack.
pub fn fall_key(block: Option<&str>, attacked_recently: bool) -> &'static str {
    if attacked_recently {
        return "death.fell.assist";
    }
    match block {
        Some("minecraft:ladder") => "death.fell.accident.ladder",
        Some("minecraft:vine") => "death.fell.accident.vines",
        Some("minecraft:weeping_vines" | "minecraft:weeping_vines_plant") => {
            "death.fell.accident.weeping_vines"
        }
        Some("minecraft:twisting_vines" | "minecraft:twisting_vines_plant") => {
            "death.fell.accident.twisting_vines"
        }
        Some("minecraft:scaffolding") => "death.fell.accident.scaffolding",
        Some(b) if b.ends_with("_water") || b == "minecraft:water" => "death.fell.accident.water",
        _ => "death.fell.accident.generic",
    }
}

/// The key of a death by a damage type that had an attacker: the `.player`
/// form of the message.
pub fn with_attacker(key: &str, attacker: bool) -> String {
    if attacker && !key.ends_with(".player") && !key.starts_with("death.fell") {
        format!("{key}.player")
    } else {
        key.to_owned()
    }
}

/// Air left after a tick, and whether the player drowns this tick.
/// Underwater air runs down 1 a tick; at -20 it is a hit of 2 and back to 0.
/// Out of water it comes back 4 a tick.
pub fn breathe(air: i32, underwater: bool) -> (i32, bool) {
    if !underwater {
        return ((air + 4).min(MAX_AIR), false);
    }
    let air = air - 1;
    if air <= -20 { (0, true) } else { (air, false) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn difficulty_scaling() {
        assert_eq!(scale_for_difficulty(6.0, 0), 0.0);
        assert_eq!(scale_for_difficulty(6.0, 1), 4.0);
        assert_eq!(scale_for_difficulty(1.0, 1), 1.0);
        assert_eq!(scale_for_difficulty(6.0, 2), 6.0);
        assert_eq!(scale_for_difficulty(6.0, 3), 9.0);
    }

    #[test]
    fn armor_takes_four_percent_a_point() {
        // 20 points (full diamond: toughness 8): a small hit loses 78%.
        let d = armor_reduce(2.0, 20.0, 8.0, "minecraft:mob_attack");
        assert!((d - 0.44).abs() < 1e-5);
        // Iron chestplate alone: 6 points, 6 damage.
        let d = armor_reduce(6.0, 6.0, 0.0, "minecraft:mob_attack");
        assert!((d - 6.0 * (1.0 - 3.0 / 25.0)).abs() < 1e-5);
        // Never below 20% of the armor.
        let d = armor_reduce(100.0, 10.0, 0.0, "minecraft:mob_attack");
        assert!((d - 100.0 * (1.0 - 2.0 / 25.0)).abs() < 1e-4);
    }

    #[test]
    fn armor_does_not_stop_the_unblockable() {
        for t in [
            "minecraft:fall",
            "minecraft:drown",
            "minecraft:starve",
            "minecraft:magic",
            "minecraft:wither",
            "minecraft:out_of_world",
        ] {
            assert_eq!(armor_reduce(5.0, 20.0, 8.0, t), 5.0, "{t}");
        }
        assert!(armor_reduce(5.0, 20.0, 8.0, "minecraft:lava") < 5.0);
    }

    #[test]
    fn armor_items() {
        assert_eq!(armor_of("minecraft:diamond_chestplate"), (8.0, 2.0));
        assert_eq!(armor_of("minecraft:netherite_boots"), (3.0, 3.0));
        assert_eq!(armor_of("minecraft:turtle_helmet"), (2.0, 0.0));
        assert_eq!(armor_of("minecraft:stone"), (0.0, 0.0));
        assert_eq!(armor_of("minecraft:diamond_horse_armor"), (0.0, 0.0));
    }

    #[test]
    fn fall_messages() {
        assert_eq!(fall_key(None, false), "death.fell.accident.generic");
        assert_eq!(
            fall_key(Some("minecraft:ladder"), false),
            "death.fell.accident.ladder"
        );
        assert_eq!(
            fall_key(Some("minecraft:vine"), false),
            "death.fell.accident.vines"
        );
        assert_eq!(
            fall_key(Some("minecraft:scaffolding"), false),
            "death.fell.accident.scaffolding"
        );
        assert_eq!(
            fall_key(Some("minecraft:ladder"), true),
            "death.fell.assist"
        );
    }

    #[test]
    fn attacker_forms() {
        assert_eq!(
            with_attacker("death.attack.magic", true),
            "death.attack.magic.player"
        );
        assert_eq!(
            with_attacker("death.attack.player", true),
            "death.attack.player"
        );
        assert_eq!(
            with_attacker("death.attack.magic", false),
            "death.attack.magic"
        );
    }

    #[test]
    fn drowning() {
        assert_eq!(breathe(300, true), (299, false));
        assert_eq!(breathe(-19, true), (0, true));
        assert_eq!(breathe(100, false), (104, false));
        assert_eq!(breathe(299, false), (300, false));
    }
}
