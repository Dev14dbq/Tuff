//! Startup glue for worlds saved by older versions or by Paper: the file
//! fix vanilla runs on every start, and `--forceUpgrade`. The work is in
//! [`mc_storage::upgrade`]; this logs what it did.

use std::path::Path;

use anyhow::Context as _;
use tracing::{info, warn};

use crate::notify::{Bus, Note};

pub use mc_storage::upgrade::world::Report;

/// Brings level.dat and the folder layout to `data_version` (26.3 is
/// `version_name`), moving Paper's split dimensions into the world. Fails
/// for worlds from before 1.18 or from a newer game, as vanilla refuses to
/// open them. A world that goes through the file fixers is announced on
/// `notes` as the game does: started, then finished or failed.
pub fn prepare_world(
    world: &Path,
    data_version: i32,
    version_name: &str,
    notes: &Bus,
) -> anyhow::Result<()> {
    let fixing = mc_storage::upgrade::files::level_version(world)
        .is_some_and(|v| v < data_version && mc_storage::upgrade::files::needs_file_fix(v));
    if fixing {
        info!("Starting upgrade for world \"{}\"", level_name(world));
        notes.publish(&Note::WorldUpgradeStarted);
    }
    let report = match mc_storage::upgrade::files::fix_world(world, data_version, version_name) {
        Ok(report) => report,
        Err(err) => {
            if fixing {
                notes.publish(&Note::WorldUpgradeFailed(
                    "The world upgrade failed during file fixing".to_owned(),
                ));
            }
            return Err(err).with_context(|| format!("upgrading {}", world.display()));
        }
    };
    if fixing {
        info!("Upgrade done for world \"{}\"", level_name(world));
        notes.publish(&Note::WorldUpgradeFinished);
    }
    for from in &report.paper {
        info!("Moved Paper dimension {} into the world", from.display());
    }
    if let Some(from) = report.from
        && from < data_version
    {
        info!("Upgraded level.dat from data version {from} to {data_version}");
        if !report.moved.is_empty() || !report.extracted.is_empty() {
            info!(
                "Applying file structure changes: {} moved, {} written from level.dat",
                report.moved.len(),
                report.extracted.len()
            );
        }
    }
    for kept in &report.kept {
        warn!("{} not moved: its new place is taken", kept.display());
    }
    Ok(())
}

/// `--forceUpgrade`: every stored chunk and player brought to
/// `data_version` and written back; `erase_cache` (`--eraseCache`) also
/// drops stored light and heightmaps.
pub fn upgrade_world(world: &Path, data_version: i32, erase_cache: bool) -> anyhow::Result<Report> {
    let mut left = 0_usize;
    let report =
        mc_storage::upgrade::world::upgrade_world(world, data_version, erase_cache, &mut |note| {
            left += 1;
            if left <= 20 {
                warn!("{note}");
            }
        })?;
    if left > 20 {
        warn!("... and {} more left as stored", left - 20);
    }
    Ok(report)
}

fn level_name(world: &Path) -> String {
    world
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_nbt::{Compound, Tag};
    use mc_storage::upgrade::files::write_gz;

    const TARGET: i32 = 5000;

    fn world(tag: &str, version: i32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tuff-upgrade-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut data = Compound::new();
        data.insert("DataVersion", Tag::Int(version));
        let mut root = Compound::new();
        root.insert("Data", Tag::Compound(data));
        write_gz(&dir.join("level.dat"), &root).unwrap();
        dir
    }

    fn methods(rx: &mut tokio::sync::broadcast::Receiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(line) = rx.try_recv() {
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            out.push(v["method"].as_str().unwrap().to_owned());
        }
        out
    }

    #[test]
    fn an_old_world_announces_its_upgrade() {
        let dir = world("old", 3465);
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        prepare_world(&dir, TARGET, "26.3", &bus).unwrap();
        assert_eq!(
            methods(&mut rx),
            [
                "minecraft:notification/world/upgrade_started",
                "minecraft:notification/world/upgrade_finished"
            ]
        );
        // Opened again it is current, and quiet.
        prepare_world(&dir, TARGET, "26.3", &bus).unwrap();
        assert!(methods(&mut rx).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_upgrade_says_so() {
        // A saved data file that does not load stops the fix.
        let dir = world("broken", 3465);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/scoreboard.dat"), b"garbage").unwrap();
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        assert!(prepare_world(&dir, TARGET, "26.3", &bus).is_err());
        assert_eq!(
            methods(&mut rx),
            [
                "minecraft:notification/world/upgrade_started",
                "minecraft:notification/world/upgrade_failed"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_world_the_game_refuses_announces_nothing() {
        // Before 1.18 the game turns the world away before any upgrade starts.
        let dir = world("ancient", 2730);
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        assert!(prepare_world(&dir, TARGET, "26.3", &bus).is_err());
        assert!(methods(&mut rx).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
