//! What a tick spends its time on, while `/perf` is running.
//!
//! The tick thread adds to these only when recording is on. `/perf stop`
//! writes the medians and the shares to `debug/profiling`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use crate::context::ServerContext;

static ON: AtomicBool = AtomicBool::new(false);
static TICKS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static BLOCKS: AtomicU64 = AtomicU64::new(0);
static ENTITIES: AtomicU64 = AtomicU64::new(0);
static REDSTONE: AtomicU64 = AtomicU64::new(0);
static BLOCK_ENTITIES: AtomicU64 = AtomicU64::new(0);

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// `Some` while a profile is being taken, so the caller can time a phase.
pub fn mark() -> Option<Instant> {
    on().then(Instant::now)
}

pub fn add(slot: &AtomicU64, from: Option<Instant>) {
    if let Some(from) = from {
        slot.fetch_add(from.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

pub fn blocks() -> &'static AtomicU64 {
    &BLOCKS
}
pub fn entities() -> &'static AtomicU64 {
    &ENTITIES
}
pub fn redstone() -> &'static AtomicU64 {
    &REDSTONE
}
pub fn block_entities() -> &'static AtomicU64 {
    &BLOCK_ENTITIES
}

pub fn begin() {
    TICKS.lock().expect("profile").clear();
    for slot in [&BLOCKS, &ENTITIES, &REDSTONE, &BLOCK_ENTITIES] {
        slot.store(0, Ordering::Relaxed);
    }
    ON.store(true, Ordering::Release);
}

pub fn add_tick(nanos: u64) {
    if on() {
        TICKS.lock().expect("profile").push(nanos);
    }
}

pub struct Report {
    pub ticks: u64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub blocks_ms: f64,
    pub entities_ms: f64,
    pub redstone_ms: f64,
    pub block_entities_ms: f64,
    pub other_ms: f64,
}

fn percentile(ticks: &[u64], q: f64) -> f64 {
    if ticks.is_empty() {
        return 0.0;
    }
    let i = ((ticks.len() - 1) as f64 * q).round() as usize;
    ticks[i] as f64 / 1.0e6
}

pub fn finish() -> Report {
    ON.store(false, Ordering::Release);
    let mut ticks = TICKS.lock().expect("profile").clone();
    ticks.sort_unstable();
    let at = |q: f64| percentile(&ticks, q);
    let ms = |slot: &AtomicU64| slot.load(Ordering::Relaxed) as f64 / 1.0e6;
    let total = ticks.iter().sum::<u64>() as f64 / 1.0e6;
    let blocks_ms = ms(&BLOCKS);
    let entities_ms = ms(&ENTITIES);
    let redstone_ms = ms(&REDSTONE);
    let block_entities_ms = ms(&BLOCK_ENTITIES);
    let other_ms = (total - blocks_ms - entities_ms - redstone_ms - block_entities_ms).max(0.0);
    Report {
        ticks: ticks.len() as u64,
        p50_ms: at(0.50),
        p99_ms: at(0.99),
        blocks_ms,
        entities_ms,
        redstone_ms,
        block_entities_ms,
        other_ms,
    }
}

/// Writes the report and returns the file name the command shows.
pub fn save(ctx: &ServerContext, report: &Report) -> String {
    let name = format!(
        "{}-world.txt",
        chrono::Local::now().format("%Y-%m-%d_%H_%M_%S")
    );
    let Some(dir) = ctx.config.server_dir.clone() else {
        tracing::info!(
            "profile: {} ticks, mspt p50 {:.2} p99 {:.2}",
            report.ticks,
            report.p50_ms,
            report.p99_ms
        );
        return name;
    };
    let folder = dir.join("debug").join("profiling");
    if let Err(e) = std::fs::create_dir_all(&folder) {
        tracing::warn!("profile folder: {e}");
        return name;
    }
    let path = folder.join(&name);
    if let Err(e) = std::fs::write(&path, text(report)) {
        tracing::warn!("profile report: {e}");
    } else {
        tracing::info!("profile report saved as {}", path.display());
    }
    name
}

fn text(report: &Report) -> String {
    format!(
        "ticks: {}\nmspt p50: {:.3}\nmspt p99: {:.3}\nblocks: {:.3} ms\nentities: {:.3} ms\nredstone: {:.3} ms\nblock_entities: {:.3} ms\nother: {:.3} ms\n",
        report.ticks,
        report.p50_ms,
        report.p99_ms,
        report.blocks_ms,
        report.entities_ms,
        report.redstone_ms,
        report.block_entities_ms,
        report.other_ms
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_come_from_the_ticks_recorded() {
        let ticks: Vec<u64> = (1..=100).map(|n| n * 1_000_000).collect();
        assert_eq!(percentile(&ticks, 0.50), 51.0);
        assert_eq!(percentile(&ticks, 0.99), 99.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn the_report_file_names_the_phases() {
        let dir = std::env::temp_dir().join(format!("mc-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let data = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let Some(data) = mc_data::McData::load(&data).ok() else {
            return;
        };
        let mut config = crate::Config::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3"),
        );
        config.server_dir = Some(dir.clone());
        let Ok(ctx) = crate::context::ServerContext::new(config, data) else {
            return;
        };
        let report = Report {
            ticks: 3,
            p50_ms: 12.5,
            p99_ms: 40.0,
            blocks_ms: 4.0,
            entities_ms: 5.0,
            redstone_ms: 1.0,
            block_entities_ms: 2.0,
            other_ms: 0.5,
        };
        let name = save(&ctx, &report);
        let body = std::fs::read_to_string(dir.join("debug/profiling").join(&name)).unwrap();
        assert!(body.contains("mspt p50: 12.500"), "{body}");
        assert!(body.contains("redstone: 1.000 ms"), "{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
