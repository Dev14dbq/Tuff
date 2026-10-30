//! File moves that hold up on Windows.

use std::io;
use std::path::Path;

/// `std::fs::rename`, retried for a moment on Windows when the file is held
/// open by someone else (an antivirus or indexer looking at a file we just
/// wrote), which there fails with "Access is denied" instead of waiting.
pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e)
                if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied && tries < 20 =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}
