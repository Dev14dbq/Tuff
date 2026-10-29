//! Startup glue for worlds saved by older versions or by Paper: the file
//! fix vanilla runs on every start, and `--forceUpgrade`. The work is in
//! [`mc_storage::upgrade`]; this logs what it did.

use std::path::Path;

use anyhow::Context as _;
use tracing::{info, warn};

pub use mc_storage::upgrade::world::Report;

/// Brings level.dat and the folder layout to `data_version` (26.3 is
/// `version_name`), moving Paper's split dimensions into the world. Fails
/// for worlds from before 1.18 or from a newer game, as vanilla refuses to
/// open them.
pub fn prepare_world(world: &Path, data_version: i32, version_name: &str) -> anyhow::Result<()> {
    let report = mc_storage::upgrade::files::fix_world(world, data_version, version_name)
        .with_context(|| format!("upgrading {}", world.display()))?;
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
