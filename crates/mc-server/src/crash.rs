//! Crash reports (minecraft.wiki, "Crash"): when the server falls over it
//! writes what happened to `crash-reports/crash-<date>_<time>-server.txt`.
//! A panic does, and so does the watchdog: with `max-tick-time` set, a tick
//! running longer than that is taken for a hung server, reported and
//! stopped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::context::ServerContext;

/// Something to read first, as the reports open with.
const COMMENTS: [&str; 8] = [
    "Who set us up the TNT?",
    "Everything's going to plan. No, really, that was supposed to happen.",
    "Uh... Did I do that?",
    "Oops.",
    "Why did you do that?",
    "I feel sad now :(",
    "Don't be sad, have a hug! <3",
    "Shall we play a game?",
];

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// When the tick running now began; nothing between ticks.
#[derive(Debug, Default)]
pub struct Heartbeat(AtomicU64);

impl Heartbeat {
    pub fn begin(&self) {
        self.0.store(now_millis().max(1), Ordering::Relaxed);
    }

    pub fn end(&self) {
        self.0.store(0, Ordering::Relaxed);
    }

    /// How long the tick running now has run.
    pub fn running(&self) -> Option<Duration> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            began => Some(Duration::from_millis(now_millis().saturating_sub(began))),
        }
    }
}

/// The report's text: what happened (`description`), the detail of it,
/// and the system it happened on.
pub fn report(description: &str, detail: &str, version: &str) -> String {
    let comment = COMMENTS[(now_millis() % COMMENTS.len() as u64) as usize];
    let time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let threads = std::thread::available_parallelism().map_or(0, usize::from);
    format!(
        "---- Minecraft Crash Report ----\n\
         // {comment}\n\
         \n\
         Time: {time}\n\
         Description: {description}\n\
         \n\
         {detail}\n\
         \n\
         \n\
         A detailed walkthrough of the error, its code path and all known details is as follows:\n\
         ---------------------------------------------------------------------------------------\n\
         \n\
         -- System Details --\n\
         Details:\n\
         \tMinecraft Version: {version}\n\
         \tMinecraft Version ID: {version}\n\
         \tOperating System: {os} ({arch})\n\
         \tServer: minecraft-rewrite {ours}\n\
         \tCPUs: {threads}\n\
         \tIs Modded: Unknown (can't tell)\n\
         \tType: Dedicated Server\n",
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        ours = env!("CARGO_PKG_VERSION"),
    )
}

/// Writes a report to `<server_dir>/crash-reports`; where it went.
pub fn write(server_dir: &Path, text: &str) -> std::io::Result<PathBuf> {
    let dir = server_dir.join("crash-reports");
    std::fs::create_dir_all(&dir)?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H.%M.%S");
    let mut path = dir.join(format!("crash-{stamp}-server.txt"));
    // Two in one second keep both.
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("crash-{stamp}-server-{n}.txt"));
        n += 1;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

fn save(server_dir: &Path, text: &str) {
    match write(server_dir, text) {
        Ok(path) => tracing::error!("This crash report has been saved to: {}", path.display()),
        Err(e) => tracing::error!("We were unable to save this crash report to disk: {e}"),
    }
}

/// From now on a panic writes a crash report.
pub fn install(server_dir: PathBuf, version: String) {
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        before(info);
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "Box<dyn Any>".to_owned());
        let at = info
            .location()
            .map_or_else(String::new, |l| format!(" at {}:{}", l.file(), l.line()));
        let thread = std::thread::current();
        let detail = format!(
            "panicked on thread '{}'{at}: {message}\n{}",
            thread.name().unwrap_or("unnamed"),
            std::backtrace::Backtrace::force_capture()
        );
        let description = match thread.name() {
            Some("tick") => "Exception in server tick loop",
            _ => "Exception in server",
        };
        save(&server_dir, &report(description, &detail, &version));
    }));
}

/// Watches the ticks: one running past `max` stops the server.
pub fn watchdog(ctx: Arc<ServerContext>, max: Duration) {
    let spawned = std::thread::Builder::new()
        .name("Server Watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(max);
                let Some(ran) = ctx.heartbeat.running() else {
                    continue;
                };
                if ran <= max {
                    continue;
                }
                tracing::error!(
                    "A single server tick took {:.2} seconds (should be max 0.05)",
                    ran.as_secs_f64()
                );
                tracing::error!("Considering it to be crashed, server will forcibly shutdown.");
                let detail = format!(
                    "The server tick has run for {} ms, more than max-tick-time ({} ms).",
                    ran.as_millis(),
                    max.as_millis()
                );
                if let Some(dir) = &ctx.config.server_dir {
                    save(
                        dir,
                        &report("Watching Server", &detail, &ctx.data.version.name),
                    );
                }
                std::process::exit(1);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("no server watchdog: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_has_the_vanilla_sections() {
        let text = report("Watching Server", "a tick hung", "26.3");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "---- Minecraft Crash Report ----");
        assert!(lines[1].starts_with("// "));
        assert!(lines[3].starts_with("Time: "));
        assert_eq!(lines[4], "Description: Watching Server");
        assert_eq!(lines[6], "a tick hung");
        assert!(text.contains("-- System Details --\nDetails:\n\tMinecraft Version: 26.3\n"));
    }

    #[test]
    fn reports_are_named_by_when_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("mc-crash-{}", std::process::id()));
        let a = write(&dir, "one").unwrap();
        let b = write(&dir, "two").unwrap();
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        // crash-yyyy-MM-dd_HH.mm.ss-server.txt
        assert!(name.starts_with("crash-") && name.ends_with("-server.txt"));
        assert_eq!(name.len(), "crash-2026-10-26_12.00.00-server.txt".len());
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "two");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_heartbeat_tells_how_long_a_tick_runs() {
        let h = Heartbeat::default();
        assert!(h.running().is_none());
        h.begin();
        assert!(h.running().is_some_and(|d| d < Duration::from_secs(5)));
        h.end();
        assert!(h.running().is_none());
    }
}
