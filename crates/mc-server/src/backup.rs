//! A copy of the world folder, taken while the server keeps running.
//!
//! `backup-interval` in `server.properties` is the minutes between copies
//! (0 leaves it to `/backup`). `backup-keep` is how many copies stay in
//! `<server>/backups`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::ServerContext;

static BUSY: AtomicBool = AtomicBool::new(false);

/// Starts a copy on another thread. False when one is already running, or
/// there is no world folder to copy.
pub fn request(ctx: &ServerContext) -> bool {
    let Some(world) = ctx.config.world.clone() else {
        return false;
    };
    let Some(server) = ctx.config.server_dir.clone() else {
        return false;
    };
    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    let keep = ctx.config.backup_keep;
    std::thread::spawn(move || {
        let into = server.join("backups");
        match copy_world(&world, &into, keep) {
            Ok(path) => tracing::info!("Backup saved as {}", path.display()),
            Err(e) => tracing::warn!("Backup failed: {e}"),
        }
        BUSY.store(false, Ordering::Release);
    });
    true
}

/// Copies `world` into a new folder of `into` and drops the oldest copies
/// past `keep`.
pub fn copy_world(world: &Path, into: &Path, keep: usize) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(into)?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = into.join(format!("{secs}"));
    copy_dir(world, &dest)?;
    rotate(into, keep.max(1))?;
    Ok(dest)
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "session.lock" {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn rotate(into: &Path, keep: usize) -> std::io::Result<()> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(into)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let extra = dirs.len().saturating_sub(keep);
    for old in dirs.into_iter().take(extra) {
        std::fs::remove_dir_all(old)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_keeps_the_newest_and_drops_the_rest() {
        let root = std::env::temp_dir().join(format!("mc-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let world = root.join("world");
        std::fs::create_dir_all(&world).unwrap();
        std::fs::write(world.join("level.dat"), b"one").unwrap();
        let into = root.join("backups");
        let first = copy_world(&world, &into, 1).unwrap();
        assert_eq!(std::fs::read(first.join("level.dat")).unwrap(), b"one");
        std::thread::sleep(std::time::Duration::from_millis(2));
        std::fs::write(world.join("level.dat"), b"two").unwrap();
        let second = copy_world(&world, &into, 1).unwrap();
        assert_eq!(std::fs::read(second.join("level.dat")).unwrap(), b"two");
        let left: Vec<_> = std::fs::read_dir(&into)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(left, vec![second]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
