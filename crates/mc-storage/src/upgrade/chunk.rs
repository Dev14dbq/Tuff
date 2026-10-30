//! Region chunk, block state and point-of-interest fixes.

use mc_nbt::{Compound, Tag};

use super::Context;
use super::nbt::{
    boolean, compound_mut, each_compound, list, list_mut, ns, rename, rename_value, string,
};

/// Structure ids before 22w11a (2970), with the configured structure they
/// became: a fallback, and biomes that pick another.
#[allow(clippy::type_complexity)]
const STRUCTURES: [(&str, &str, &[(&[&str], &str)]); 18] = [
    (
        "mineshaft",
        "minecraft:mineshaft",
        &[(
            &[
                "minecraft:badlands",
                "minecraft:eroded_badlands",
                "minecraft:wooded_badlands",
            ],
            "minecraft:mineshaft_mesa",
        )],
    ),
    (
        "shipwreck",
        "minecraft:shipwreck",
        &[(
            &["minecraft:beach", "minecraft:snowy_beach"],
            "minecraft:shipwreck_beached",
        )],
    ),
    (
        "ocean_ruin",
        "minecraft:ocean_ruin_cold",
        &[(
            &[
                "minecraft:warm_ocean",
                "minecraft:lukewarm_ocean",
                "minecraft:deep_lukewarm_ocean",
            ],
            "minecraft:ocean_ruin_warm",
        )],
    ),
    (
        "village",
        "minecraft:village_plains",
        &[
            (&["minecraft:desert"], "minecraft:village_desert"),
            (&["minecraft:savanna"], "minecraft:village_savanna"),
            (&["minecraft:snowy_plains"], "minecraft:village_snowy"),
            (&["minecraft:taiga"], "minecraft:village_taiga"),
        ],
    ),
    (
        "ruined_portal",
        "minecraft:ruined_portal",
        &[
            (&["minecraft:desert"], "minecraft:ruined_portal_desert"),
            (
                &[
                    "minecraft:badlands",
                    "minecraft:eroded_badlands",
                    "minecraft:wooded_badlands",
                    "minecraft:windswept_hills",
                    "minecraft:windswept_forest",
                    "minecraft:windswept_gravelly_hills",
                    "minecraft:savanna_plateau",
                    "minecraft:windswept_savanna",
                    "minecraft:stony_shore",
                    "minecraft:meadow",
                    "minecraft:frozen_peaks",
                    "minecraft:jagged_peaks",
                    "minecraft:stony_peaks",
                    "minecraft:snowy_slopes",
                ],
                "minecraft:ruined_portal_mountain",
            ),
            (
                &[
                    "minecraft:bamboo_jungle",
                    "minecraft:jungle",
                    "minecraft:sparse_jungle",
                ],
                "minecraft:ruined_portal_jungle",
            ),
            (
                &[
                    "minecraft:deep_frozen_ocean",
                    "minecraft:deep_cold_ocean",
                    "minecraft:deep_ocean",
                    "minecraft:deep_lukewarm_ocean",
                    "minecraft:frozen_ocean",
                    "minecraft:ocean",
                    "minecraft:cold_ocean",
                    "minecraft:lukewarm_ocean",
                    "minecraft:warm_ocean",
                ],
                "minecraft:ruined_portal_ocean",
            ),
        ],
    ),
    ("pillager_outpost", "minecraft:pillager_outpost", &[]),
    ("mansion", "minecraft:mansion", &[]),
    ("jungle_pyramid", "minecraft:jungle_pyramid", &[]),
    ("desert_pyramid", "minecraft:desert_pyramid", &[]),
    ("igloo", "minecraft:igloo", &[]),
    ("swamp_hut", "minecraft:swamp_hut", &[]),
    ("stronghold", "minecraft:stronghold", &[]),
    ("monument", "minecraft:monument", &[]),
    ("fortress", "minecraft:fortress", &[]),
    ("endcity", "minecraft:end_city", &[]),
    ("buried_treasure", "minecraft:buried_treasure", &[]),
    ("nether_fossil", "minecraft:nether_fossil", &[]),
    ("bastion_remnant", "minecraft:bastion_remnant", &[]),
];

/// The configured structure an old structure key became, guessed from the
/// biomes most common in the chunk's palettes where that matters.
fn configured(key: &str, chunk: &Compound) -> Option<String> {
    let key = key.to_ascii_lowercase();
    let (_, fallback, by_biome) = STRUCTURES.iter().find(|(k, _, _)| *k == key)?;
    if by_biome.is_empty() {
        return Some((*fallback).to_owned());
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for section in list(chunk, "sections") {
        let Tag::Compound(section) = section else {
            continue;
        };
        let Some(biomes) = super::nbt::compound(section, "biomes") else {
            continue;
        };
        for biome in list(biomes, "palette") {
            let Tag::String(biome) = biome else {
                continue;
            };
            let Some((_, to)) = by_biome
                .iter()
                .find(|(names, _)| names.contains(&biome.as_str()))
            else {
                continue;
            };
            match counts.iter_mut().find(|(k, _)| k == to) {
                Some(c) => c.1 += 1,
                None => counts.push((to, 1)),
            }
        }
    }
    let best = counts.iter().max_by_key(|(_, n)| *n).map(|(k, _)| *k);
    Some(best.unwrap_or(fallback).to_owned())
}

/// 2970: structure starts and references are keyed by configured structure.
pub(super) fn configured_structures(chunk: &mut Compound, _: &Context) {
    let Some(Tag::Compound(mut structures)) = chunk.remove("structures") else {
        return;
    };
    if let Some(Tag::Compound(starts)) = structures.remove("starts") {
        let mut out = Compound::new();
        for (key, start) in starts.iter() {
            let Tag::Compound(start) = start else {
                continue;
            };
            if string(start, "id").unwrap_or("INVALID") == "INVALID" {
                continue;
            }
            if let Some(new) = configured(key, chunk)
                && out.get(&new).is_none()
            {
                let mut start = start.clone();
                start.insert("id", Tag::String(new.clone()));
                out.insert(new, Tag::Compound(start));
            }
        }
        structures.insert("starts", Tag::Compound(out));
    }
    if let Some(Tag::Compound(refs)) = structures.remove("References") {
        let mut out = Compound::new();
        for (key, positions) in refs.iter() {
            let Tag::LongArray(positions) = positions else {
                continue;
            };
            if positions.is_empty() {
                continue;
            }
            let Some(new) = configured(key, chunk) else {
                continue;
            };
            match out.get_mut(&new) {
                Some(Tag::LongArray(existing)) => existing.extend_from_slice(positions),
                _ => {
                    out.insert(new, Tag::LongArray(positions.clone()));
                }
            }
        }
        structures.insert("References", Tag::Compound(out));
    }
    chunk.insert("structures", Tag::Compound(structures));
}

fn each_section(chunk: &mut Compound, f: impl FnMut(&mut Compound)) {
    each_compound(chunk, "sections", f);
}

fn drop_light(section: &mut Compound) {
    section.remove("BlockLight");
    section.remove("SkyLight");
}

/// 3077: light of a chunk that was never lit is not kept.
pub(super) fn unlit_light(chunk: &mut Compound, _: &Context) {
    if !boolean(chunk, "isLightOn").unwrap_or(false) {
        each_section(chunk, drop_light);
    }
}

/// 3451 / 4537: stored light is thrown away; the server lights the chunk
/// again when it loads.
pub(super) fn delete_light(chunk: &mut Compound, _: &Context) {
    chunk.remove("isLightOn");
    each_section(chunk, drop_light);
}

/// 3108: only the Overworld blends old terrain into new.
pub(super) fn blending_outside_overworld(chunk: &mut Compound, ctx: &Context) {
    if ctx.dimension != "minecraft:overworld" {
        chunk.remove("blending_data");
    }
}

fn map_status(chunk: &mut Compound, f: impl Fn(&str) -> Option<&'static str>) {
    if let Some(Tag::String(s)) = chunk.get_mut("Status")
        && let Some(new) = f(&ns(s))
    {
        *s = new.to_owned();
    }
    if let Some(retrogen) = compound_mut(chunk, "below_zero_retrogen")
        && let Some(Tag::String(s)) = retrogen.get_mut("target_status")
        && let Some(new) = f(&ns(s))
    {
        *s = new.to_owned();
    }
}

/// 3450: `liquid_carvers` and `heightmaps` statuses are gone.
pub(super) fn remap_statuses(chunk: &mut Compound, _: &Context) {
    map_status(chunk, |s| match s {
        "minecraft:liquid_carvers" => Some("minecraft:carvers"),
        "minecraft:heightmaps" => Some("minecraft:spawn"),
        _ => None,
    });
}

/// 4885: beds have no block entity.
pub(super) fn drop_bed_entities(chunk: &mut Compound, _: &Context) {
    if let Some(list) = list_mut(chunk, "block_entities") {
        list.retain(|be| match be {
            Tag::Compound(be) => string(be, "id").map(ns).as_deref() != Some("minecraft:bed"),
            _ => true,
        });
    }
}

const SKIP_BLENDING: [&str; 4] = [
    "minecraft:empty",
    "minecraft:structure_starts",
    "minecraft:structure_references",
    "minecraft:biomes",
];

fn blending(min_section: i32, max_section: i32) -> Tag {
    let mut c = Compound::new();
    c.insert("min_section", Tag::Int(min_section));
    c.insert("max_section", Tag::Int(max_section));
    Tag::Compound(c)
}

/// 4997: blending data is recomputed from the status: generated Overworld
/// chunks blend over the whole height, chunks still waiting on their
/// below-zero extension over the old 0..256.
pub(super) fn reset_blending(chunk: &mut Compound, ctx: &Context) {
    chunk.remove("blending_data");
    if ctx.dimension != "minecraft:overworld" {
        return;
    }
    let Some(status) = string(chunk, "Status").map(ns) else {
        return;
    };
    if !SKIP_BLENDING.contains(&status.as_str()) {
        chunk.insert("blending_data", blending(-4, 20));
        return;
    }
    let target = super::nbt::compound(chunk, "below_zero_retrogen")
        .and_then(|r| string(r, "target_status").map(ns));
    if target.is_some_and(|t| !SKIP_BLENDING.contains(&t.as_str())) {
        chunk.insert("blending_data", blending(0, 16));
    }
}

/// 5009: blending heights are floats; "no value" is the largest float.
pub(super) fn blending_heights(chunk: &mut Compound, _: &Context) {
    if let Some(data) = compound_mut(chunk, "blending_data")
        && let Some(heights) = list_mut(data, "heights")
    {
        for h in heights.iter_mut() {
            let v = super::nbt::number(h).unwrap_or(f64::MAX);
            *h = Tag::Float(if v == f64::MAX { f32::MAX } else { v as f32 });
        }
    }
}

/// 5013: `noise` and `surface` merged into `biomes`, `carvers` became
/// `terrain`. A chunk caught in a merged step loses its half-made blocks
/// (only those below zero when it was waiting on its below-zero extension).
pub(super) fn merge_terrain_statuses(chunk: &mut Compound, _: &Context) {
    let fix = |s: &str| -> String {
        match s {
            "minecraft:carvers" => "minecraft:terrain".into(),
            "minecraft:noise" | "minecraft:surface" => "minecraft:biomes".into(),
            other => other.into(),
        }
    };
    let intermediate = |s: &str| matches!(s, "minecraft:noise" | "minecraft:surface");
    let old = ns(string(chunk, "Status").unwrap_or("minecraft:empty"));
    chunk.insert("Status", Tag::String(fix(&old)));
    let mut old_target = None;
    if let Some(retrogen) = compound_mut(chunk, "below_zero_retrogen") {
        let t = ns(string(retrogen, "target_status").unwrap_or("minecraft:empty"));
        retrogen.insert("target_status", Tag::String(fix(&t)));
        old_target = Some(t);
    }
    let target_intermediate = old_target.as_deref().is_some_and(intermediate);
    if !intermediate(&old) && !target_intermediate {
        return;
    }
    let below_zero_only = old_target.is_some() && !target_intermediate;
    chunk.remove("Heightmaps");
    chunk.remove("blending_data");
    each_section(chunk, |section| {
        let y = super::nbt::int(section, "Y").unwrap_or(0);
        if !(below_zero_only && y >= 0) {
            section.remove("block_states");
        }
    });
}

// ---- block states ----

/// 3692 / 4541: block renames.
pub(super) fn rename_block(state: &mut Compound, table: &[(&str, &str)]) {
    rename_value(state, "Name", table);
}

/// 4294: the creaking heart's `active` is a three-way `creaking_heart_state`.
pub(super) fn creaking_heart(state: &mut Compound) {
    if string(state, "Name").map(ns).as_deref() != Some("minecraft:creaking_heart") {
        return;
    }
    if let Some(props) = compound_mut(state, "Properties")
        && let Some(Tag::String(active)) = props.remove("active")
    {
        let value = if active == "true" {
            "awake"
        } else {
            "uprooted"
        };
        props.insert("creaking_heart_state", Tag::String(value.into()));
    }
}

/// 4305: the test block's `test_block_mode` is `mode`.
pub(super) fn test_block_mode(state: &mut Compound) {
    if string(state, "Name").map(ns).as_deref() == Some("minecraft:test_block")
        && let Some(props) = compound_mut(state, "Properties")
    {
        rename(props, "test_block_mode", "mode");
    }
}

/// 5006: block states are `{id, properties}`.
pub(super) fn state_field_names(state: &mut Compound) {
    rename(state, "Name", "id");
    rename(state, "Properties", "properties");
}

// ---- points of interest ----

/// 3097: unemployed villagers and nitwits do not hold points of interest.
pub(super) fn drop_jobless_poi(root: &mut Compound) {
    let Some(Tag::Compound(sections)) = root.get_mut("Sections") else {
        return;
    };
    for (_, section) in sections.iter_mut() {
        let Tag::Compound(section) = section else {
            continue;
        };
        if let Some(records) = list_mut(section, "Records") {
            records.retain(|r| match r {
                Tag::Compound(r) => !matches!(
                    string(r, "type"),
                    Some("minecraft:unemployed" | "minecraft:nitwit") | None
                ),
                _ => false,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context<'static> {
        Context {
            dimension: "minecraft:overworld",
        }
    }

    #[test]
    fn old_villages_get_their_biome_variant() {
        let mut biomes = Compound::new();
        biomes.insert(
            "palette",
            Tag::List(vec![Tag::String("minecraft:desert".into())]),
        );
        let mut section = Compound::new();
        section.insert("biomes", Tag::Compound(biomes));
        let mut start = Compound::new();
        start.insert("id", Tag::String("village".into()));
        let mut starts = Compound::new();
        starts.insert("Village", Tag::Compound(start));
        let mut refs = Compound::new();
        refs.insert("Village", Tag::LongArray(vec![7]));
        refs.insert("Igloo", Tag::LongArray(vec![]));
        let mut structures = Compound::new();
        structures.insert("starts", Tag::Compound(starts));
        structures.insert("References", Tag::Compound(refs));
        let mut chunk = Compound::new();
        chunk.insert("sections", Tag::List(vec![Tag::Compound(section)]));
        chunk.insert("structures", Tag::Compound(structures));
        configured_structures(&mut chunk, &ctx());
        let structures = super::super::nbt::compound(&chunk, "structures").unwrap();
        let starts = super::super::nbt::compound(structures, "starts").unwrap();
        assert!(starts.get("minecraft:village_desert").is_some());
        let refs = super::super::nbt::compound(structures, "References").unwrap();
        assert_eq!(
            refs.get("minecraft:village_desert"),
            Some(&Tag::LongArray(vec![7]))
        );
        assert_eq!(refs.len(), 1);
    }

    #[test]
    fn statuses_merge_and_half_made_blocks_go() {
        let mut section = Compound::new();
        section.insert("Y", Tag::Byte(2));
        section.insert("block_states", Tag::Compound(Compound::new()));
        let mut chunk = Compound::new();
        chunk.insert("Status", Tag::String("noise".into()));
        chunk.insert("sections", Tag::List(vec![Tag::Compound(section)]));
        merge_terrain_statuses(&mut chunk, &ctx());
        assert_eq!(
            chunk.get("Status"),
            Some(&Tag::String("minecraft:biomes".into()))
        );
        let [Tag::Compound(section)] = list(&chunk, "sections") else {
            panic!("no section");
        };
        assert!(section.get("block_states").is_none());

        let mut chunk = Compound::new();
        chunk.insert("Status", Tag::String("minecraft:full".into()));
        reset_blending(&mut chunk, &ctx());
        let Some(Tag::Compound(b)) = chunk.get("blending_data") else {
            panic!("no blending_data");
        };
        assert_eq!(b.get("min_section"), Some(&Tag::Int(-4)));
    }
}
