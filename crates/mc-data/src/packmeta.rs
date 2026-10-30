//! What a data pack's `pack.mcmeta` says that the server acts on: the
//! feature flags it needs (`features.enabled`) and the overlay folders that
//! apply to this game version (`overlays.entries`).
//!
//! Like vanilla, the overlay list is validated as a whole: one bad entry
//! makes the pack unreadable, so it is not listed at all.

use serde_json::Value;

/// The data pack format of this game version, major and minor.
pub const DATA_PACK_FORMAT: (u32, u32) = (121, 0);

/// The last data pack format before minor versions existed.
const LAST_PRE_MINOR: u32 = 81;

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
/// section, a feature flag that does not exist, or a broken overlay list.
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
    if let Some(overlays) = meta.get("overlays") {
        out.overlays = overlays_for_version(overlays)?;
    }
    Ok(out)
}

/// A format as `min_format` / `max_format` hold it.
type Format = (u32, u32);

/// One overlay entry as written, before it is checked against the others.
struct Entry {
    directory: String,
    min: Option<Format>,
    max: Option<Format>,
    /// The legacy `formats` range, majors only.
    supported: Option<(u32, u32)>,
}

impl Entry {
    fn effective_min_major(&self) -> u32 {
        match (self.min, self.supported) {
            (Some(min), Some(sup)) => min.0.min(sup.0),
            (Some(min), None) => min.0,
            (None, Some(sup)) => sup.0,
            (None, None) => u32::MAX,
        }
    }
}

fn non_negative(v: &Value) -> Result<u32, String> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| "format numbers must be non-negative integers".to_owned())
}

/// `N` or `[major, minor]` (more elements are ignored, none is an error).
fn format_field(v: &Value, default_minor: u32) -> Result<Format, String> {
    match v {
        Value::Array(a) if (1..=256).contains(&a.len()) => Ok((
            non_negative(&a[0])?,
            a.get(1).map_or(Ok(default_minor), non_negative)?,
        )),
        Value::Array(_) => Err("format list must have 1 to 256 numbers".to_owned()),
        other => Ok((non_negative(other)?, default_minor)),
    }
}

/// The `formats` range: `N`, `[min, max]` or `{min_inclusive, max_inclusive}`.
fn range_field(v: &Value) -> Result<(u32, u32), String> {
    let (lo, hi) = match v {
        Value::Array(a) if a.len() == 2 => (non_negative(&a[0])?, non_negative(&a[1])?),
        Value::Object(o) => (
            non_negative(o.get("min_inclusive").ok_or("missing min_inclusive")?)?,
            non_negative(o.get("max_inclusive").ok_or("missing max_inclusive")?)?,
        ),
        Value::Array(_) => return Err("formats list must have two numbers".to_owned()),
        other => {
            let n = non_negative(other)?;
            (n, n)
        }
    };
    if lo > hi {
        return Err("min_inclusive must be less than or equal to max_inclusive".to_owned());
    }
    Ok((lo, hi))
}

fn read_entry(v: &Value) -> Result<Entry, String> {
    let directory = v["directory"]
        .as_str()
        .ok_or("overlay entry has no directory")?;
    if directory.is_empty()
        || !directory
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!("{directory} is not accepted directory name"));
    }
    Ok(Entry {
        directory: directory.to_owned(),
        min: v
            .get("min_format")
            .map(|f| format_field(f, 0))
            .transpose()?,
        max: v
            .get("max_format")
            .map(|f| format_field(f, u32::MAX))
            .transpose()?,
        supported: v.get("formats").map(range_field).transpose()?,
    })
}

/// The overlay folders of an `overlays` section that apply to this version.
fn overlays_for_version(section: &Value) -> Result<Vec<String>, String> {
    let list = section
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("overlays.entries is missing")?;
    let entries = list.iter().map(read_entry).collect::<Result<Vec<_>, _>>()?;
    let min_version = entries
        .iter()
        .map(Entry::effective_min_major)
        .min()
        .unwrap_or(u32::MAX);
    let require_old = min_version <= LAST_PRE_MINOR;
    let mut applicable = Vec::new();
    for entry in &entries {
        if entry.min.is_none() && entry.max.is_none() && entry.supported.is_none() {
            // Vanilla logs "Unknown or broken overlay entry" and goes on.
            continue;
        }
        let (low, high) = validate(entry, require_old)?;
        if low <= DATA_PACK_FORMAT && DATA_PACK_FORMAT <= high {
            applicable.push(entry.directory.clone());
        }
    }
    Ok(applicable)
}

/// Vanilla's checks on one overlay entry; the range it covers.
fn validate(entry: &Entry, require_old: bool) -> Result<(Format, Format), String> {
    let context = format!("Overlay \"{}\"", entry.directory);
    if entry.min.is_some() != entry.max.is_some() {
        return Err(format!(
            "{context} missing field, must declare both min_format and max_format"
        ));
    }
    if require_old && entry.supported.is_none() {
        return Err(format!(
            "{context} missing required field formats, must be present in all overlays for any overlays to work across game versions"
        ));
    }
    if let (Some(min), Some(max)) = (entry.min, entry.max) {
        if min > max {
            return Err(format!("{context} min_format is greater than max_format"));
        }
        if min.0 > LAST_PRE_MINOR && !require_old {
            if entry.supported.is_some() {
                return Err(format!(
                    "{context} key formats is deprecated starting from pack format {}. Remove formats from your pack.mcmeta.",
                    LAST_PRE_MINOR + 1
                ));
            }
        } else {
            let Some(sup) = entry.supported else {
                return Err(format!(
                    "{context} declares support for format {}, but game versions supporting formats 15 to {LAST_PRE_MINOR} require a formats field.",
                    min.0
                ));
            };
            if sup.0 != min.0 {
                return Err(format!(
                    "{context} version declaration mismatch between formats (from {}) and min_format",
                    sup.0
                ));
            }
            if sup.1 != max.0 && sup.1 != LAST_PRE_MINOR {
                return Err(format!(
                    "{context} version declaration mismatch between formats (up to {}) and max_format",
                    sup.1
                ));
            }
        }
        return Ok((min, max));
    }
    let (lo, hi) = entry.supported.expect("an entry with format information");
    if hi > LAST_PRE_MINOR {
        return Err(format!(
            "{context} declares support for version newer than {LAST_PRE_MINOR}, but is missing mandatory fields min_format and max_format"
        ));
    }
    Ok(((lo, 0), (hi, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with_overlays(entries: Value) -> Value {
        json!({"pack": {"description": "x"}, "overlays": {"entries": entries}})
    }

    #[test]
    fn features_and_overlays_are_read() {
        let meta = json!({
            "pack": {"description": "x"},
            "features": {"enabled": ["minecraft:trade_rebalance", "redstone_experiments"]},
            "overlays": {"entries": [
                {"directory": "new", "min_format": 120, "max_format": [121, 3]},
                {"directory": "future", "min_format": 122, "max_format": 130},
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
        assert_eq!(parsed.overlays, ["new"]);
    }

    #[test]
    fn legacy_overlays_work_next_to_new_ones() {
        // An overlay down to 81 makes `formats` mandatory on every entry.
        let ok = with_overlays(json!([
            {"directory": "old", "formats": [48, 81]},
            {"directory": "new", "min_format": 121, "max_format": 121, "formats": 121}
        ]));
        assert_eq!(parse(&ok).unwrap().overlays, ["new"]);
        let ok = with_overlays(json!([
            {"directory": "old", "formats": [48, 81]},
            {"directory": "new", "min_format": 82, "max_format": 130, "formats": [82, 81]}
        ]));
        assert!(parse(&ok).is_err(), "reversed legacy range");
        let ok = with_overlays(json!([
            {"directory": "old", "formats": [48, 81]},
            {"directory": "wide", "min_format": 82, "max_format": 130, "formats": [82, 130]}
        ]));
        assert_eq!(parse(&ok).unwrap().overlays, ["wide"]);
        let ok = with_overlays(json!([
            {"directory": "old", "formats": [48, 81]},
            {"directory": "odd", "min_format": 82, "max_format": 130, "formats": [82, 100]}
        ]));
        assert!(parse(&ok).is_err(), "formats end must be max_format or 81");
        let ok = with_overlays(json!([
            {"directory": "old", "formats": [48, 81]},
            {"directory": "wide", "min_format": 48, "max_format": 130, "formats": [48, 81]}
        ]));
        assert_eq!(parse(&ok).unwrap().overlays, ["wide"]);
    }

    #[test]
    fn a_bad_overlay_entry_makes_the_pack_unreadable() {
        for entries in [
            // Bad directory name.
            json!([{"directory": "bad dir", "min_format": 121, "max_format": 121}]),
            // Only one of min_format and max_format.
            json!([{"directory": "a", "min_format": 121}]),
            json!([{"directory": "a", "max_format": 121}]),
            // min above max.
            json!([{"directory": "a", "min_format": 122, "max_format": 121}]),
            json!([{"directory": "a", "min_format": [121, 2], "max_format": [121, 1]}]),
            // `formats` alone above 81.
            json!([{"directory": "a", "formats": 121}]),
            // formats left out while another overlay goes down to 81.
            json!([
                {"directory": "a", "formats": [60, 70]},
                {"directory": "b", "min_format": 121, "max_format": 121}
            ]),
            // formats next to new-style versions above 81.
            json!([{"directory": "a", "min_format": 121, "max_format": 121, "formats": 121}]),
            // No directory, reversed legacy range, wrong types.
            json!([{"min_format": 121, "max_format": 121}]),
            json!([{"directory": "a", "formats": [30, 20]}]),
            json!([{"directory": "a", "min_format": "x", "max_format": 121}]),
        ] {
            assert!(parse(&with_overlays(entries.clone())).is_err(), "{entries}");
        }
        assert!(parse(&json!({"pack": {}, "overlays": {}})).is_err());
    }

    #[test]
    fn an_entry_without_format_information_is_only_skipped() {
        let meta = with_overlays(json!([
            {"directory": "nothing"},
            {"directory": "a", "min_format": 121, "max_format": 121}
        ]));
        assert_eq!(parse(&meta).unwrap().overlays, ["a"]);
    }

    #[test]
    fn minor_versions_bound_the_range() {
        let applies =
            |entry: Value| parse(&with_overlays(json!([entry]))).map(|m| !m.overlays.is_empty());
        assert_eq!(
            applies(json!({"directory": "a", "min_format": [121, 1], "max_format": 121})),
            Ok(false),
            "121.0 is below 121.1"
        );
        assert_eq!(
            applies(json!({"directory": "a", "min_format": 121, "max_format": [121, 0]})),
            Ok(true)
        );
        assert_eq!(
            applies(json!({"directory": "a", "min_format": [100, 5], "max_format": [120, 3]})),
            Ok(false)
        );
        assert_eq!(
            applies(json!({"directory": "a", "min_format": 121, "max_format": 121})),
            Ok(true)
        );
    }

    #[test]
    fn unknown_flags_and_missing_pack_section_make_the_pack_unreadable() {
        assert!(parse(&json!({"features": {"enabled": []}})).is_err());
        let bad = json!({"pack": {}, "features": {"enabled": ["minecraft:nope"]}});
        assert!(parse(&bad).is_err());
        assert_eq!(parse(&json!({"pack": {}})).unwrap(), PackMeta::default());
    }
}
