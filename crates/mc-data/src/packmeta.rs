//! What a data pack's `pack.mcmeta` says that the server acts on: the
//! feature flags it needs (`features.enabled`) and the overlay folders that
//! apply to this game version (`overlays.entries`).

use serde_json::Value;

/// The data pack format of this game version, major and minor.
pub const DATA_PACK_FORMAT: (u32, u32) = (121, 0);

/// Every feature flag the game knows, in registry order.
pub const FEATURE_FLAGS: [&str; 4] = [
    "minecraft:vanilla",
    "minecraft:trade_rebalance",
    "minecraft:redstone_experiments",
    "minecraft:minecart_improvements",
];

/// What the server reads from a pack's metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackMeta {
    /// Flags the pack needs; it can only be enabled where all are on.
    pub features: Vec<String>,
    /// Overlay folders that apply to [`DATA_PACK_FORMAT`], lowest priority
    /// first (a later one wins over an earlier one and over the pack).
    pub overlays: Vec<String>,
}

fn flag_name(raw: &str) -> String {
    if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("minecraft:{raw}")
    }
}

/// Reads the metadata. `Err` is a pack vanilla refuses to read: no `pack`
/// section, or a feature flag that does not exist.
pub fn parse(meta: &Value) -> Result<PackMeta, String> {
    if !meta["pack"].is_object() {
        return Err("Missing metadata".to_owned());
    }
    let mut out = PackMeta::default();
    if let Some(enabled) = meta.get("features").and_then(|f| f.get("enabled")) {
        let list = enabled
            .as_array()
            .ok_or_else(|| "features.enabled is not a list".to_owned())?;
        for item in list {
            let name = flag_name(item.as_str().ok_or("feature flag is not a string")?);
            if !FEATURE_FLAGS.contains(&name.as_str()) {
                return Err(format!("Unknown feature flag: {name}"));
            }
            if !out.features.contains(&name) {
                out.features.push(name);
            }
        }
    }
    for entry in meta["overlays"]["entries"].as_array().into_iter().flatten() {
        let Some(directory) = entry["directory"].as_str() else {
            continue;
        };
        if directory.is_empty()
            || !directory
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            continue;
        }
        if overlay_applies(entry) {
            out.overlays.push(directory.to_owned());
        }
    }
    Ok(out)
}

/// A `min_format`/`max_format` value: a major, or `[major, minor]`.
fn format_pair(v: &Value, default_minor: u32) -> Option<(u32, u32)> {
    match v {
        Value::Number(n) => Some((n.as_u64()? as u32, default_minor)),
        Value::Array(a) if !a.is_empty() => Some((
            a[0].as_u64()? as u32,
            a.get(1)
                .and_then(Value::as_u64)
                .map_or(default_minor, |m| m as u32),
        )),
        _ => None,
    }
}

/// The legacy `formats` field: a number, `[min, max]` or
/// `{"min_inclusive": a, "max_inclusive": b}`, on majors only.
fn legacy_range(v: &Value) -> Option<(u32, u32)> {
    match v {
        Value::Number(n) => {
            let n = n.as_u64()? as u32;
            Some((n, n))
        }
        Value::Array(a) if a.len() == 2 => Some((a[0].as_u64()? as u32, a[1].as_u64()? as u32)),
        Value::Object(o) => Some((
            o.get("min_inclusive")?.as_u64()? as u32,
            o.get("max_inclusive")?.as_u64()? as u32,
        )),
        _ => None,
    }
}

fn overlay_applies(entry: &Value) -> bool {
    let min = entry.get("min_format").and_then(|v| format_pair(v, 0));
    let max = entry
        .get("max_format")
        .and_then(|v| format_pair(v, u32::MAX));
    if let (Some(min), Some(max)) = (min, max) {
        return min <= DATA_PACK_FORMAT && DATA_PACK_FORMAT <= max;
    }
    match entry.get("formats").and_then(legacy_range) {
        Some((lo, hi)) => lo <= DATA_PACK_FORMAT.0 && DATA_PACK_FORMAT.0 <= hi,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn features_and_overlays_are_read() {
        let meta = json!({
            "pack": {"description": "x", "min_format": 121, "max_format": 121},
            "features": {"enabled": ["minecraft:trade_rebalance", "redstone_experiments"]},
            "overlays": {"entries": [
                {"directory": "new", "min_format": 120, "max_format": [121, 3]},
                {"directory": "old", "formats": [48, 80]},
                {"directory": "exact", "formats": 121},
                {"directory": "future", "min_format": 122, "max_format": 130},
                {"directory": "bad dir", "formats": 121},
                {"directory": "nothing"}
            ]}
        });
        let parsed = parse(&meta).unwrap();
        assert_eq!(
            parsed.features,
            [
                "minecraft:trade_rebalance",
                "minecraft:redstone_experiments"
            ]
        );
        assert_eq!(parsed.overlays, ["new", "exact"]);
    }

    #[test]
    fn minor_versions_bound_the_range() {
        let entry = json!({"min_format": [121, 1], "max_format": 121});
        assert!(!overlay_applies(&entry), "121.0 is below 121.1");
        let entry = json!({"min_format": 121, "max_format": [121, 0]});
        assert!(overlay_applies(&entry));
        let entry = json!({"min_format": [100, 5], "max_format": [120, 3]});
        assert!(!overlay_applies(&entry));
    }

    #[test]
    fn unknown_flags_and_missing_pack_section_make_the_pack_unreadable() {
        assert!(parse(&json!({"features": {"enabled": []}})).is_err());
        let bad = json!({"pack": {}, "features": {"enabled": ["minecraft:nope"]}});
        assert!(parse(&bad).is_err());
        assert_eq!(parse(&json!({"pack": {}})).unwrap(), PackMeta::default());
    }
}
