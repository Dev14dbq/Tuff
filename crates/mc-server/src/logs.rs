//! Console and `logs/latest.log` in vanilla's format
//! (`[02:00:58] [Server thread/INFO]: text`, local time); at start the last
//! run's `latest.log` is kept gzipped as `logs/<date>-<n>.log.gz`, as
//! vanilla does.

use std::fmt;
use std::io::Write as _;
use std::path::Path;
use std::sync::Mutex;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

struct Vanilla;

impl<S, N> FormatEvent<S, N> for Vanilla
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut w: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let level = match *event.metadata().level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };
        let thread = std::thread::current();
        let name = match thread.name() {
            Some("tick") | Some("main") | None => "Server thread",
            Some(n) if n.starts_with("tokio") => "Netty Server IO",
            Some(n) => n,
        };
        write!(
            w,
            "[{}] [{name}/{level}]: ",
            chrono::Local::now().format("%H:%M:%S")
        )?;
        ctx.field_format().format_fields(w.by_ref(), event)?;
        writeln!(w)
    }
}

/// Moves the last `latest.log` aside (gzipped, named by its day).
fn rotate(dir: &Path) {
    let latest = dir.join("latest.log");
    let Ok(meta) = std::fs::metadata(&latest) else {
        return;
    };
    let day: chrono::DateTime<chrono::Local> = meta
        .modified()
        .map_or_else(|_| chrono::Local::now(), Into::into);
    let day = day.format("%Y-%m-%d").to_string();
    let mut n = 1;
    let target = loop {
        let p = dir.join(format!("{day}-{n}.log.gz"));
        if !p.exists() {
            break p;
        }
        n += 1;
    };
    let Ok(bytes) = std::fs::read(&latest) else {
        return;
    };
    if let Ok(file) = std::fs::File::create(&target) {
        let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        if gz.write_all(&bytes).is_ok() && gz.finish().is_ok() {
            let _ = std::fs::remove_file(&latest);
        }
    }
}

/// Logs to the console, and to `<server dir>/logs/latest.log` when given.
pub fn init(server_dir: Option<&Path>) {
    let filter = || EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let console = tracing_subscriber::fmt::layer()
        .event_format(Vanilla)
        .with_filter(filter());
    let file = server_dir.and_then(|d| {
        let logs = d.join("logs");
        std::fs::create_dir_all(&logs).ok()?;
        rotate(&logs);
        let f = std::fs::File::create(logs.join("latest.log")).ok()?;
        Some(
            tracing_subscriber::fmt::layer()
                .event_format(Vanilla)
                .with_ansi(false)
                .with_writer(Mutex::new(f))
                .with_filter(filter()),
        )
    });
    let _ = tracing_subscriber::registry()
        .with(console)
        .with(file)
        .try_init();
}
