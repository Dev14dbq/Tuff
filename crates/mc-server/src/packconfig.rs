//! Which data packs a world runs with, and what `level.dat` keeps of it
//! (`DataPacks` and `enabled_features`): the choice the game makes when a
//! world is opened, and the starting choice for a new one
//! (`initial-enabled-packs`, `initial-disabled-packs`).

use mc_nbt::{Compound, Tag};
use tracing::{info, warn};

/// The feature flags the game knows. A new world may use all of them.
pub const ALL_FLAGS: [&str; 4] = [
    "minecraft:vanilla",
    "minecraft:trade_rebalance",
    "minecraft:redstone_experiments",
    "minecraft:minecart_improvements",
];

pub const VANILLA_FLAG: &str = "minecraft:vanilla";

/// `initial-enabled-packs` and `initial-disabled-packs`: the packs a new
/// world starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialPacks {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
}

impl Default for InitialPacks {
    fn default() -> Self {
        Self {
            enabled: vec!["vanilla".to_owned()],
            disabled: Vec::new(),
        }
    }
}

impl InitialPacks {
    /// From the two comma-separated properties: each piece trimmed, empty
    /// pieces kept (the game splits them the same way, so an empty value is
    /// one pack with no name).
    pub fn from_properties(enabled: &str, disabled: &str) -> Self {
        let split = |text: &str| text.split(',').map(|s| s.trim().to_owned()).collect();
        Self {
            enabled: split(enabled),
            disabled: split(disabled),
        }
    }
}

/// A pack the world could run.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: String,
    /// The feature flags it asks for.
    pub flags: Vec<String>,
    /// A pack found in the world folder is added by itself when new; the
    /// built-in feature packs are not.
    pub auto: bool,
}

/// The outcome: packs in the order they run, the ones left out, and the
/// feature flags that follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    pub features: Vec<String>,
}

/// The game's pick of packs (`configurePackRepository`). `candidates` is
/// sorted by id. `enabled` and `disabled` are the world's stored choice, or
/// the initial one for a new world. `allowed` limits the flags packs may
/// ask for: `None` for a new world, which can use every flag; for an old one
/// the flags it was saved with, which `forced` repeats.
pub fn configure(
    candidates: &[Candidate],
    enabled: &[String],
    disabled: &[String],
    allowed: Option<&[String]>,
    forced: &[String],
) -> Selection {
    let find = |id: &str| candidates.iter().find(|c| c.id == id);
    let permitted = |flags: &[String]| -> Vec<String> {
        flags
            .iter()
            .filter(|f| match allowed {
                Some(allowed) => !allowed.contains(f),
                None => !ALL_FLAGS.contains(&f.as_str()),
            })
            .cloned()
            .collect()
    };
    let mut selected: Vec<String> = Vec::new();
    for id in enabled {
        if find(id).is_some() {
            if !selected.contains(id) {
                selected.push(id.clone());
            }
        } else {
            warn!("Missing data pack {id}");
        }
    }
    for pack in candidates {
        if disabled.contains(&pack.id) {
            continue;
        }
        let missing = permitted(&pack.flags);
        let is_selected = selected.contains(&pack.id);
        if !is_selected && pack.auto {
            if missing.is_empty() {
                info!("Found new data pack {}, loading it automatically", pack.id);
                selected.push(pack.id.clone());
            } else {
                info!(
                    "Found new data pack {}, but can't load it due to missing features {}",
                    pack.id,
                    missing.join(", ")
                );
            }
        }
        if is_selected && !missing.is_empty() {
            warn!(
                "Pack {} requires features {} that are not enabled for this world, disabling pack.",
                pack.id,
                missing.join(", ")
            );
            selected.retain(|id| id != &pack.id);
        }
    }
    if selected.is_empty() {
        info!("No datapacks selected, forcing vanilla");
        selected.push("vanilla".to_owned());
    }
    let mut features: Vec<String> = forced.to_vec();
    for id in &selected {
        for flag in find(id).map_or(&[][..], |c| &c.flags[..]) {
            if !features.contains(flag) {
                features.push(flag.clone());
            }
        }
    }
    features.sort();
    let disabled = candidates
        .iter()
        .filter(|c| !selected.contains(&c.id))
        .map(|c| c.id.clone())
        .collect();
    Selection {
        enabled: selected,
        disabled,
        features,
    }
}

/// What `level.dat` says about packs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LevelPacks {
    pub enabled: Vec<String>,
    pub disabled: Vec<String>,
    pub features: Vec<String>,
}

impl LevelPacks {
    /// Sets `DataPacks` and `enabled_features` in level.dat's `Data`. Like
    /// the game, the flags are left out when they are only `minecraft:vanilla`.
    pub fn write(&self, data: &mut Compound) {
        let list = |ids: &[String]| Tag::List(ids.iter().cloned().map(Tag::String).collect());
        let mut packs = Compound::new();
        packs.insert("Enabled", list(&self.enabled));
        packs.insert("Disabled", list(&self.disabled));
        data.insert("DataPacks", Tag::Compound(packs));
        if self.features == [VANILLA_FLAG] {
            data.remove("enabled_features");
        } else {
            data.insert("enabled_features", list(&self.features));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str, flags: &[&str], auto: bool) -> Candidate {
        Candidate {
            id: id.to_owned(),
            flags: flags.iter().map(|f| (*f).to_owned()).collect(),
            auto,
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    /// What the game offers on a dedicated server, with one pack in the
    /// world folder.
    fn candidates() -> Vec<Candidate> {
        vec![
            pack("file/mine", &[], true),
            pack(
                "minecart_improvements",
                &["minecraft:minecart_improvements"],
                false,
            ),
            pack(
                "redstone_experiments",
                &["minecraft:redstone_experiments"],
                false,
            ),
            pack("trade_rebalance", &["minecraft:trade_rebalance"], false),
            pack("vanilla", &[VANILLA_FLAG], false),
        ]
    }

    #[test]
    fn a_new_world_takes_the_default_packs_and_the_ones_in_its_folder() {
        let init = InitialPacks::default();
        let got = configure(&candidates(), &init.enabled, &init.disabled, None, &[]);
        // What the game writes for a world with a pack folder: the pack it
        // found by itself comes after the listed ones, and the feature packs
        // stay in `Disabled`, in id order.
        assert_eq!(got.enabled, ids(&["vanilla", "file/mine"]));
        assert_eq!(
            got.disabled,
            ids(&[
                "minecart_improvements",
                "redstone_experiments",
                "trade_rebalance"
            ])
        );
        assert_eq!(got.features, ids(&[VANILLA_FLAG]));
    }

    #[test]
    fn initial_packs_can_turn_on_feature_packs_and_keep_others_out() {
        let init = InitialPacks::from_properties("vanilla, trade_rebalance", "file/mine");
        let got = configure(&candidates(), &init.enabled, &init.disabled, None, &[]);
        assert_eq!(got.enabled, ids(&["vanilla", "trade_rebalance"]));
        assert_eq!(
            got.disabled,
            ids(&["file/mine", "minecart_improvements", "redstone_experiments"])
        );
        assert_eq!(
            got.features,
            ids(&["minecraft:trade_rebalance", VANILLA_FLAG])
        );
    }

    #[test]
    fn names_that_do_not_exist_are_skipped_and_an_empty_choice_means_vanilla() {
        let init = InitialPacks::from_properties("nope", "file/mine");
        // `"".split(',')` is one empty name, as in the game.
        assert_eq!(InitialPacks::from_properties("", "").enabled, ids(&[""]));
        let got = configure(&candidates(), &init.enabled, &init.disabled, None, &[]);
        assert_eq!(got.enabled, ids(&["vanilla"]));
    }

    #[test]
    fn a_pack_needing_a_flag_the_old_world_lacks_stays_out() {
        let mut list = candidates();
        list.push(pack("file/trades", &["minecraft:trade_rebalance"], true));
        list.sort_by(|a, b| a.id.cmp(&b.id));
        let stored = ids(&[VANILLA_FLAG]);
        // Found by itself: not loaded. Listed as enabled: dropped.
        let got = configure(&list, &ids(&["vanilla"]), &[], Some(&stored), &stored);
        assert_eq!(got.enabled, ids(&["vanilla", "file/mine"]));
        let got = configure(
            &list,
            &ids(&["vanilla", "file/trades"]),
            &[],
            Some(&stored),
            &stored,
        );
        assert_eq!(got.enabled, ids(&["vanilla", "file/mine"]));
        assert!(got.disabled.contains(&"file/trades".to_owned()));
        // The flag the world was saved with is kept either way.
        assert_eq!(got.features, ids(&[VANILLA_FLAG]));
        // A world that has the flag runs it.
        let stored = ids(&[VANILLA_FLAG, "minecraft:trade_rebalance"]);
        let got = configure(&list, &ids(&["vanilla"]), &[], Some(&stored), &stored);
        assert!(got.enabled.contains(&"file/trades".to_owned()));
    }

    #[test]
    fn level_dat_leaves_out_the_default_flags() {
        let mut data = Compound::new();
        data.insert("enabled_features", Tag::List(vec![Tag::String("x".into())]));
        let mut packs = LevelPacks {
            enabled: ids(&["vanilla"]),
            disabled: ids(&["trade_rebalance"]),
            features: ids(&[VANILLA_FLAG]),
        };
        packs.write(&mut data);
        assert!(data.get("enabled_features").is_none());
        let Some(Tag::Compound(written)) = data.get("DataPacks") else {
            panic!("no DataPacks");
        };
        assert_eq!(
            written.get("Enabled"),
            Some(&Tag::List(vec![Tag::String("vanilla".into())]))
        );
        assert_eq!(
            written.get("Disabled"),
            Some(&Tag::List(vec![Tag::String("trade_rebalance".into())]))
        );
        packs.features = ids(&[VANILLA_FLAG, "minecraft:trade_rebalance"]);
        packs.write(&mut data);
        assert_eq!(
            data.get("enabled_features"),
            Some(&Tag::List(vec![
                Tag::String(VANILLA_FLAG.into()),
                Tag::String("minecraft:trade_rebalance".into())
            ]))
        );
    }
}
