//! Schedule for the `/pregen` admin command.
//!
//! Vanilla has no `/pregen`. `Schedule` chooses which chunks to visit and in
//! what order. `step` generates a few of them, writes them, and drops them.
//!
//! `Job::center_x` and `Job::center_z` are block coordinates. The center chunk
//! is `center.div_euclid(16)`, and the block center used for a circle is that
//! chunk's center (`chunk * 16 + 8`).

use serde_json::{Value, json};

const MAX_RADIUS_BLOCKS: i32 = 50_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Every chunk inside the Chebyshev chunk radius.
    Square,
    /// Chunks whose block centers lie inside the block radius.
    Circle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: u64,
    pub dimension: String,
    /// Block x. The center chunk is `center_x.div_euclid(16)`.
    pub center_x: i32,
    /// Block z. The center chunk is `center_z.div_euclid(16)`.
    pub center_z: i32,
    /// Radius in blocks, from 0 through 50_000. The chunk radius is
    /// `radius_blocks.div_euclid(16)`, so 0 is the center chunk and 16 is a
    /// 3×3 square.
    pub radius_blocks: i32,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub paused: bool,
    pub cancelled: bool,
}

/// One pregeneration job, or none. Chunks are not stored: a square or circle
/// is the radii plus a cursor `(ring, index)` on the Chebyshev rings.
#[derive(Debug)]
pub struct Schedule {
    job: Option<Job>,
    done: u64,
    total: u64,
    paused: bool,
    cancelled: bool,
    /// Chebyshev ring of the next candidate. Ring 0 is the center chunk.
    ring: i32,
    /// Index of the next candidate along [`Self::ring`].
    index: i32,
    /// Who ran `/pregen start`. Empty after a restart; progress still goes to the log.
    starter: String,
}

impl Schedule {
    pub const fn new() -> Self {
        Self {
            job: None,
            done: 0,
            total: 0,
            paused: false,
            cancelled: false,
            ring: 0,
            index: 0,
            starter: String::new(),
        }
    }

    pub fn set_starter(&mut self, name: &str) {
        self.starter = name.to_owned();
    }

    pub fn starter(&self) -> String {
        self.starter.clone()
    }

    /// Starts `job`, replacing a finished or cancelled one.
    ///
    /// Returns an error when `radius_blocks` is outside `0..=50_000`, or when
    /// a job is still unfinished and has not been cancelled.
    pub fn start(&mut self, job: Job) -> Result<(), String> {
        if !(0..=MAX_RADIUS_BLOCKS).contains(&job.radius_blocks) {
            return Err("radius must be between 0 and 50000 blocks".to_owned());
        }
        if self.job.is_some() && !self.cancelled && self.done < self.total {
            return Err("a pregeneration job is already in progress".to_owned());
        }
        self.total = count_chunks(job.radius_blocks, job.shape);
        self.job = Some(job);
        self.done = 0;
        self.paused = false;
        self.cancelled = false;
        self.ring = 0;
        self.index = 0;
        debug_assert!(self.total >= 1);
        Ok(())
    }

    pub fn pause(&mut self) {
        if self.job.is_some() {
            self.paused = true;
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn cancel(&mut self) {
        if self.job.is_some() {
            self.cancelled = true;
        }
    }

    pub fn progress(&self) -> Option<Progress> {
        self.job.as_ref()?;
        Some(Progress {
            done: self.done,
            total: self.total,
            paused: self.paused,
            cancelled: self.cancelled,
        })
    }

    /// The job's dimension, when one is running.
    pub fn dimension(&self) -> Option<String> {
        self.job.as_ref().map(|job| job.dimension.clone())
    }

    /// Up to `n` chunks still waiting. Nothing while paused, cancelled, or finished.
    pub fn take(&mut self, n: usize) -> Vec<(i32, i32)> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let Some(chunk) = self.next_chunk() else {
                break;
            };
            out.push(chunk);
        }
        out
    }

    /// Next chunk to generate, nearest to the center first (Chebyshev for
    /// square, Euclidean for circle). `None` when paused, cancelled, finished,
    /// or idle.
    ///
    /// A circle uses the same Chebyshev rings as a square and skips chunks
    /// outside the block radius, so nearer rings still come first.
    pub fn next_chunk(&mut self) -> Option<(i32, i32)> {
        if self.paused || self.cancelled || self.done >= self.total {
            return None;
        }
        let (origin_x, origin_z, chunk_radius, radius_blocks, shape) = {
            let job = self.job.as_ref()?;
            (
                job.center_x.div_euclid(16),
                job.center_z.div_euclid(16),
                job.radius_blocks.div_euclid(16),
                job.radius_blocks,
                job.shape,
            )
        };

        loop {
            if self.ring > chunk_radius {
                return None;
            }
            let (dx, dz) = ring_offset(self.ring, self.index)?;
            advance_cursor(&mut self.ring, &mut self.index);
            let inside = match shape {
                Shape::Square => true,
                Shape::Circle => within_circle(dx, dz, radius_blocks),
            };
            if !inside {
                continue;
            }
            self.done += 1;
            return Some((origin_x + dx, origin_z + dz));
        }
    }

    /// JSON for this job, including the cursor and pause/cancel flags.
    pub fn save(&self) -> String {
        serde_json::to_string(&self.to_json()).unwrap_or_else(|_| "{}".into())
    }

    /// Restores a schedule. Malformed JSON, an invalid radius, or a cursor
    /// that could not have been produced here becomes an empty idle schedule.
    pub fn load(text: &str) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Self::new();
        };
        let Some(root) = value.as_object() else {
            return Self::new();
        };
        let Some(job_value) = root.get("job") else {
            return Self::new();
        };
        if job_value.is_null() {
            return Self::new();
        }
        let Some(job) = parse_job(job_value) else {
            return Self::new();
        };
        let Some(done) = root.get("done").and_then(Value::as_u64) else {
            return Self::new();
        };
        let Some(paused) = root.get("paused").and_then(Value::as_bool) else {
            return Self::new();
        };
        let Some(cancelled) = root.get("cancelled").and_then(Value::as_bool) else {
            return Self::new();
        };
        let Some(ring) = root.get("ring").and_then(json_i32) else {
            return Self::new();
        };
        let Some(index) = root.get("index").and_then(json_i32) else {
            return Self::new();
        };
        if !cursor_ok(ring, index, job.radius_blocks) {
            return Self::new();
        }
        Self {
            total: count_chunks(job.radius_blocks, job.shape),
            job: Some(job),
            done,
            paused,
            cancelled,
            ring,
            index,
            starter: String::new(),
        }
    }

    fn to_json(&self) -> Value {
        let Some(job) = &self.job else {
            return json!({ "job": null });
        };
        json!({
            "job": {
                "id": job.id,
                "dimension": job.dimension,
                "center_x": job.center_x,
                "center_z": job.center_z,
                "radius_blocks": job.radius_blocks,
                "shape": shape_name(job.shape),
            },
            "done": self.done,
            "total": self.total,
            "paused": self.paused,
            "cancelled": self.cancelled,
            "ring": self.ring,
            "index": self.index,
        })
    }
}

pub fn apply(schedule: &mut Schedule, line: &str, here: &str) -> Result<String, String> {
    let mut tokens = line.split_whitespace();
    let Some(first) = tokens.next() else {
        return Err(usage());
    };
    let rest: Vec<&str> =
        if first.eq_ignore_ascii_case("pregen") || first.eq_ignore_ascii_case("minecraft:pregen") {
            tokens.collect()
        } else {
            std::iter::once(first).chain(tokens).collect()
        };
    match rest.as_slice() {
        ["start", x, z, radius, tail @ ..] => start(schedule, x, z, radius, tail, here),
        ["pause"] => {
            schedule.pause();
            Ok(status_text(schedule))
        }
        ["continue"] => {
            schedule.resume();
            Ok(status_text(schedule))
        }
        ["cancel"] => {
            schedule.cancel();
            Ok("Pregeneration cancelled.".to_owned())
        }
        ["status"] | [] => Ok(status_text(schedule)),
        _ => Err(usage()),
    }
}

fn usage() -> String {
    "Usage: /pregen start <x> <z> <radius> [square|circle] [dimension], or pause, continue, cancel, status.".to_owned()
}

fn start(
    schedule: &mut Schedule,
    x: &str,
    z: &str,
    radius: &str,
    tail: &[&str],
    here: &str,
) -> Result<String, String> {
    let center_x = parse_i32(x, "x")?;
    let center_z = parse_i32(z, "z")?;
    let radius_blocks = parse_i32(radius, "radius")?;
    let (shape, dimension) = match tail {
        [] => (Shape::Square, here.to_owned()),
        [shape] if shape_of(shape).is_some() => (shape_of(shape).unwrap(), here.to_owned()),
        [dimension] => (Shape::Square, normalize_dimension(dimension)),
        [shape, dimension] => (
            shape_of(shape).ok_or_else(|| "shape is square or circle".to_owned())?,
            normalize_dimension(dimension),
        ),
        _ => return Err(usage()),
    };
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_millis() as u64);
    schedule.start(Job {
        id,
        dimension,
        center_x,
        center_z,
        radius_blocks,
        shape,
    })?;
    Ok(status_text(schedule))
}

fn parse_i32(text: &str, name: &str) -> Result<i32, String> {
    text.parse()
        .map_err(|_| format!("{name} must be a whole number"))
}

fn shape_of(text: &str) -> Option<Shape> {
    if text.eq_ignore_ascii_case("square") {
        Some(Shape::Square)
    } else if text.eq_ignore_ascii_case("circle") {
        Some(Shape::Circle)
    } else {
        None
    }
}

fn normalize_dimension(name: &str) -> String {
    let short = name
        .strip_prefix("minecraft:")
        .unwrap_or(name)
        .to_ascii_lowercase();
    match short.as_str() {
        "overworld" | "world" => "minecraft:overworld".to_owned(),
        "nether" | "the_nether" => "minecraft:the_nether".to_owned(),
        "end" | "the_end" => "minecraft:the_end".to_owned(),
        _ => {
            if name.contains(':') {
                name.to_owned()
            } else {
                format!("minecraft:{name}")
            }
        }
    }
}

fn status_text(schedule: &Schedule) -> String {
    match schedule.progress() {
        None => "No pregeneration is running.".to_owned(),
        Some(progress) if progress.cancelled => {
            format!(
                "Pregeneration cancelled at {}/{}.",
                progress.done, progress.total
            )
        }
        Some(progress) if progress.done >= progress.total => {
            format!("Pregeneration finished, {} chunks.", progress.total)
        }
        Some(progress) => {
            let state = if progress.paused { "paused" } else { "running" };
            format!(
                "Pregeneration {state}: {}/{} chunks in {}.",
                progress.done,
                progress.total,
                schedule
                    .dimension()
                    .unwrap_or_else(|| "overworld".to_owned())
            )
        }
    }
}

/// Generates a few queued chunks, writes them, and drops them from memory.
pub fn step(ctx: &crate::context::ServerContext) {
    let dimension = {
        let schedule = ctx.pregen.lock().expect("pregen");
        let Some(progress) = schedule.progress() else {
            return;
        };
        if progress.paused || progress.cancelled || progress.done >= progress.total {
            return;
        }
        schedule.dimension()
    };
    let Some(dimension) = dimension else {
        return;
    };
    let Some(target) = ctx.in_dimension(&dimension) else {
        tracing::warn!("pregen: no dimension {dimension}");
        ctx.pregen.lock().expect("pregen").pause();
        persist(ctx);
        return;
    };
    let chunks = ctx.pregen.lock().expect("pregen").take(1);
    if chunks.is_empty() {
        return;
    }
    let finished = ctx
        .pregen
        .lock()
        .expect("pregen")
        .progress()
        .is_some_and(|progress| progress.done >= progress.total);
    let report = ctx.pregen.lock().expect("pregen").progress();
    persist(ctx);
    for (x, z) in chunks {
        let _ = target.world.block(
            mc_protocol::Position::new(x * 16 + 8, target.world.sea_level, z * 16 + 8),
            &target.data.blocks,
        );
    }
    target.world.save(&target.data.blocks);
    let dropped = target.world.unload_unused();
    crate::persist::unloaded(&target, &dropped);
    if let Some(progress) = report
        && (finished || progress.done.is_multiple_of(256))
    {
        let text = format!(
            "Pregen {}/{} chunks in {dimension}",
            progress.done, progress.total
        );
        tracing::info!("{text}");
        tell(ctx, &text);
    }
}

fn tell(ctx: &crate::context::ServerContext, text: &str) {
    let starter = ctx.pregen.lock().expect("pregen").starter();
    if starter.is_empty() || starter == "Server" || starter == "Rcon" {
        return;
    }
    let uuid = {
        let players = ctx.players.lock().expect("players");
        players
            .iter()
            .find(|(_, player)| player.profile.name == starter)
            .map(|(uuid, _)| *uuid)
    };
    let Some(uuid) = uuid else {
        return;
    };
    let _ = ctx.events.send(crate::context::Event::Message {
        uuid,
        line: mc_protocol::text::plain(text),
        overlay: false,
    });
}

pub fn persist(ctx: &crate::context::ServerContext) {
    let Some(dir) = &ctx.config.server_dir else {
        return;
    };
    let text = ctx.pregen.lock().expect("pregen").save();
    if let Err(err) = std::fs::write(dir.join("pregen.json"), text) {
        tracing::warn!("saving pregen.json: {err}");
    }
}

fn shape_name(shape: Shape) -> &'static str {
    match shape {
        Shape::Square => "square",
        Shape::Circle => "circle",
    }
}

fn parse_shape(value: &Value) -> Option<Shape> {
    match value.as_str()? {
        "square" | "Square" => Some(Shape::Square),
        "circle" | "Circle" => Some(Shape::Circle),
        _ => None,
    }
}

fn parse_job(value: &Value) -> Option<Job> {
    let obj = value.as_object()?;
    let radius_blocks = json_i32(obj.get("radius_blocks")?)?;
    if !(0..=MAX_RADIUS_BLOCKS).contains(&radius_blocks) {
        return None;
    }
    Some(Job {
        id: obj.get("id")?.as_u64()?,
        dimension: obj.get("dimension")?.as_str()?.to_owned(),
        center_x: json_i32(obj.get("center_x")?)?,
        center_z: json_i32(obj.get("center_z")?)?,
        radius_blocks,
        shape: parse_shape(obj.get("shape")?)?,
    })
}

fn json_i32(value: &Value) -> Option<i32> {
    i32::try_from(value.as_i64()?).ok()
}

/// `true` when `(ring, index)` is a cursor this schedule can save.
fn cursor_ok(ring: i32, index: i32, radius_blocks: i32) -> bool {
    if ring < 0 || index < 0 {
        return false;
    }
    let chunk_radius = radius_blocks.div_euclid(16);
    if ring == 0 {
        return index == 0;
    }
    // One step past the outer ring is the cursor after the last chunk of that
    // ring has been taken.
    if ring == chunk_radius.saturating_add(1) {
        return index == 0;
    }
    if ring > chunk_radius {
        return false;
    }
    match ring.checked_mul(8) {
        Some(len) => index < len,
        None => false,
    }
}

fn count_chunks(radius_blocks: i32, shape: Shape) -> u64 {
    match shape {
        Shape::Square => count_square(radius_blocks),
        Shape::Circle => count_circle(radius_blocks),
    }
}

fn count_square(radius_blocks: i32) -> u64 {
    let chunk_radius = radius_blocks.div_euclid(16).max(0) as u64;
    let side = chunk_radius * 2 + 1;
    side * side
}

/// Lattice points whose chunk centers fall inside the block radius.
///
/// Chunk `(dx, dz)` is inside when `(16 dx)² + (16 dz)² <= radius²`. For each
/// column the admissible `|dz|` is `floor(sqrt(radius² - dx_blocks²) / 16)`,
/// which matches that test without visiting every chunk.
fn count_circle(radius_blocks: i32) -> u64 {
    let radius = i64::from(radius_blocks);
    let radius_sq = radius * radius;
    let max_ring = radius_blocks.div_euclid(16);
    let mut total = 0u64;
    for dx in -max_ring..=max_ring {
        let dx_blocks = i64::from(dx) * 16;
        let dx_sq = dx_blocks * dx_blocks;
        if dx_sq > radius_sq {
            continue;
        }
        let max_dz = (radius_sq - dx_sq).isqrt().div_euclid(16);
        total += (max_dz * 2 + 1) as u64;
    }
    total
}

fn within_circle(dx: i32, dz: i32, radius_blocks: i32) -> bool {
    // Chunk centers are `chunk * 16 + 8`. Relative to the center chunk the
    // `+ 8` cancels, so the block delta is `dchunk * 16`.
    let dx = i64::from(dx) * 16;
    let dz = i64::from(dz) * 16;
    let radius = i64::from(radius_blocks);
    dx * dx + dz * dz <= radius * radius
}

/// Offset of candidate `index` on Chebyshev ring `ring`.
///
/// Ring 0 is only `(0, 0)`. Ring `r > 0` has `8r` chunks, clockwise from the
/// north-west corner: north edge west to east, then east, south, and west.
fn ring_offset(ring: i32, index: i32) -> Option<(i32, i32)> {
    if ring == 0 {
        return (index == 0).then_some((0, 0));
    }
    if ring < 0 || index < 0 {
        return None;
    }
    let side = ring.checked_mul(2)?;
    let len = side.checked_mul(4)?;
    if index >= len {
        return None;
    }
    let side_i = index / side;
    let off = index % side;
    let offset = match side_i {
        0 => (-ring + off, -ring),
        1 => (ring, -ring + off),
        2 => (ring - off, ring),
        3 => (-ring, ring - off),
        _ => return None,
    };
    Some(offset)
}

fn advance_cursor(ring: &mut i32, index: &mut i32) {
    if *ring <= 0 {
        *ring = 1;
        *index = 0;
        return;
    }
    let Some(len) = ring.checked_mul(8) else {
        return;
    };
    let next = index.saturating_add(1);
    if next >= len {
        *ring = ring.saturating_add(1);
        *index = 0;
    } else {
        *index = next;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use super::*;

    fn job(id: u64, x: i32, z: i32, radius: i32, shape: Shape) -> Job {
        Job {
            id,
            dimension: "minecraft:overworld".to_owned(),
            center_x: x,
            center_z: z,
            radius_blocks: radius,
            shape,
        }
    }

    fn drain(schedule: &mut Schedule) -> Vec<(i32, i32)> {
        let mut chunks = Vec::new();
        while let Some(chunk) = schedule.next_chunk() {
            chunks.push(chunk);
        }
        chunks
    }

    /// Independent of `count_circle`: walk the square and apply the block test.
    fn brute_count(radius: i32, shape: Shape) -> u64 {
        let chunk_radius = radius.div_euclid(16);
        let mut total = 0u64;
        for dx in -chunk_radius..=chunk_radius {
            for dz in -chunk_radius..=chunk_radius {
                if shape == Shape::Square || block_inside(dx, dz, radius) {
                    total += 1;
                }
            }
        }
        total
    }

    fn block_inside(dx: i32, dz: i32, radius: i32) -> bool {
        let dx = i64::from(dx) * 16;
        let dz = i64::from(dz) * 16;
        let radius = i64::from(radius);
        dx * dx + dz * dz <= radius * radius
    }

    fn block_center(chunk: i32) -> i64 {
        i64::from(chunk) * 16 + 8
    }

    #[test]
    fn square_radius_0_yields_only_the_center() {
        let mut schedule = Schedule::new();
        schedule.start(job(1, 0, 0, 0, Shape::Square)).unwrap();
        assert_eq!(schedule.next_chunk(), Some((0, 0)));
        assert_eq!(schedule.next_chunk(), None);
        let progress = schedule.progress().unwrap();
        assert_eq!(progress.done, 1);
        assert_eq!(progress.total, 1);
        assert!(!progress.paused);
        assert!(!progress.cancelled);

        let mut loaded = Schedule::load(&schedule.save());
        assert_eq!(loaded.progress(), schedule.progress());
        assert_eq!(loaded.next_chunk(), None);
    }

    #[test]
    fn square_radius_16_is_three_by_three() {
        let mut schedule = Schedule::new();
        schedule.start(job(1, 0, 0, 16, Shape::Square)).unwrap();
        let chunks = drain(&mut schedule);
        assert_eq!(chunks[0], (0, 0));
        assert_eq!(chunks.len(), 9);
        let unique: BTreeSet<_> = chunks.iter().copied().collect();
        assert_eq!(unique.len(), chunks.len());
        let mut previous = 0;
        for (x, z) in &chunks {
            let distance = x.abs().max(z.abs());
            assert!(distance >= previous);
            assert!(distance <= 1);
            previous = distance;
        }
        let progress = schedule.progress().unwrap();
        assert_eq!(progress.done, progress.total);
        assert_eq!(progress.total, 9);
        assert_eq!(schedule.next_chunk(), None);
    }

    #[test]
    fn circle_radius_16_stays_inside_block_radius() {
        let mut schedule = Schedule::new();
        schedule.start(job(1, 0, 0, 16, Shape::Circle)).unwrap();
        let chunks = drain(&mut schedule);
        assert_eq!(chunks[0], (0, 0));
        let origin_x = block_center(0);
        let origin_z = block_center(0);
        for &(x, z) in &chunks {
            let dx = block_center(x) - origin_x;
            let dz = block_center(z) - origin_z;
            assert!(
                dx * dx + dz * dz <= 16 * 16,
                "chunk ({x}, {z}) center is outside the block radius"
            );
        }
        let got: BTreeSet<_> = chunks.iter().copied().collect();
        assert_eq!(got.len(), chunks.len());
        assert!(!got.contains(&(1, 1)));
        assert!(!got.contains(&(2, 0)));
        assert_eq!(
            got,
            BTreeSet::from([(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)])
        );
        assert_eq!(schedule.progress().unwrap().done, got.len() as u64);
        assert_eq!(schedule.progress().unwrap().total, got.len() as u64);
    }

    #[test]
    fn pause_and_resume_keeps_the_cursor() {
        let mut schedule = Schedule::new();
        schedule.start(job(1, 0, 0, 16, Shape::Square)).unwrap();
        schedule.pause();
        assert_eq!(schedule.next_chunk(), None);
        assert!(schedule.progress().unwrap().paused);
        schedule.resume();
        assert_eq!(schedule.next_chunk(), Some((0, 0)));

        let mut schedule = Schedule::new();
        let mut reference = Schedule::new();
        schedule.start(job(1, 0, 0, 16, Shape::Square)).unwrap();
        reference.start(job(1, 0, 0, 16, Shape::Square)).unwrap();
        assert_eq!(schedule.next_chunk(), reference.next_chunk());
        assert_eq!(schedule.next_chunk(), reference.next_chunk());
        schedule.pause();
        assert_eq!(schedule.next_chunk(), None);
        assert_eq!(schedule.next_chunk(), None);
        assert_eq!(schedule.progress().unwrap().done, 2);
        assert!(schedule.progress().unwrap().paused);
        assert!(schedule.start(job(2, 0, 0, 16, Shape::Square)).is_err());
        schedule.resume();
        assert!(!schedule.progress().unwrap().paused);
        assert_eq!(schedule.next_chunk(), reference.next_chunk());
        while let Some(chunk) = reference.next_chunk() {
            assert_eq!(schedule.next_chunk(), Some(chunk));
        }
        assert_eq!(schedule.next_chunk(), None);
        assert_eq!(schedule.progress().unwrap().done, 9);
    }

    #[test]
    fn save_load_round_trips_the_cursor() {
        let mut original = Schedule::new();
        let mut reference = Schedule::new();
        let mut started = job(4, 1, 2, 48, Shape::Circle);
        started.dimension = "minecraft:the_nether".to_owned();
        original.start(started.clone()).unwrap();
        reference.start(started).unwrap();
        for _ in 0..7 {
            assert_eq!(original.next_chunk(), reference.next_chunk());
        }
        let loaded = Schedule::load(&Schedule::load(&original.save()).save());
        assert_eq!(loaded.progress(), original.progress());
        let saved: Value = serde_json::from_str(&loaded.save()).unwrap();
        assert_eq!(saved["job"]["dimension"], "minecraft:the_nether");
        assert_eq!(saved["job"]["id"], 4);
        assert_eq!(saved["ring"].as_i64(), Some(i64::from(original.ring)));
        assert_eq!(saved["index"].as_i64(), Some(i64::from(original.index)));

        let mut loaded = loaded;
        assert_eq!(loaded.next_chunk(), reference.next_chunk());
        while let Some(chunk) = reference.next_chunk() {
            assert_eq!(loaded.next_chunk(), Some(chunk));
        }
        assert_eq!(loaded.next_chunk(), None);

        let mut paused = Schedule::new();
        paused.start(job(1, 0, 0, 32, Shape::Circle)).unwrap();
        assert!(paused.next_chunk().is_some());
        paused.pause();
        assert_eq!(paused.next_chunk(), None);
        let mut loaded = Schedule::load(&paused.save());
        assert_eq!(loaded.progress(), paused.progress());
        assert!(loaded.progress().unwrap().paused);
        assert_eq!(loaded.next_chunk(), None);
        loaded.resume();
        let mut fresh = Schedule::new();
        fresh.start(job(1, 0, 0, 32, Shape::Circle)).unwrap();
        fresh.next_chunk();
        assert_eq!(loaded.next_chunk(), fresh.next_chunk());

        let mut cancelled = Schedule::new();
        cancelled.start(job(1, 0, 0, 32, Shape::Circle)).unwrap();
        cancelled.next_chunk();
        cancelled.cancel();
        let mut loaded = Schedule::load(&cancelled.save());
        assert_eq!(loaded.progress(), cancelled.progress());
        assert!(loaded.progress().unwrap().cancelled);
        assert_eq!(loaded.progress().unwrap().done, 1);
        assert_eq!(loaded.next_chunk(), None);
    }

    #[test]
    fn second_start_while_unfinished_is_rejected() {
        let mut schedule = Schedule::new();
        schedule.start(job(1, 0, 0, 16, Shape::Square)).unwrap();
        assert_eq!(schedule.next_chunk(), Some((0, 0)));
        assert!(schedule.start(job(2, 100, 100, 16, Shape::Square)).is_err());
        let second = schedule.next_chunk().unwrap();
        assert_ne!(second, (0, 0));
        assert!(second.0.abs().max(second.1.abs()) <= 1);

        schedule.cancel();
        assert!(schedule.progress().unwrap().cancelled);
        assert_eq!(schedule.next_chunk(), None);
        schedule.resume();
        assert!(schedule.progress().unwrap().cancelled);
        assert_eq!(schedule.next_chunk(), None);

        schedule.start(job(9, 0, 0, 0, Shape::Circle)).unwrap();
        let progress = schedule.progress().unwrap();
        assert_eq!(progress.done, 0);
        assert!(!progress.cancelled);
        assert!(!progress.paused);
        assert_eq!(schedule.next_chunk(), Some((0, 0)));
        let saved: Value = serde_json::from_str(&schedule.save()).unwrap();
        assert_eq!(saved["job"]["id"], 9);

        let mut finished = Schedule::new();
        finished.start(job(1, 0, 0, 0, Shape::Square)).unwrap();
        assert_eq!(finished.next_chunk(), Some((0, 0)));
        assert_eq!(finished.next_chunk(), None);
        finished.start(job(2, 16, 0, 0, Shape::Square)).unwrap();
        assert_eq!(finished.next_chunk(), Some((1, 0)));
    }

    #[test]
    fn radius_5000_counts_without_storing_chunks() {
        for shape in [Shape::Square, Shape::Circle] {
            for radius in [0, 16, 48, 100] {
                let mut schedule = Schedule::new();
                schedule.start(job(1, 0, 0, radius, shape)).unwrap();
                assert_eq!(
                    schedule.progress().unwrap().total,
                    brute_count(radius, shape),
                    "radius {radius} {shape:?}"
                );
            }
        }

        for shape in [Shape::Square, Shape::Circle] {
            let mut schedule = Schedule::new();
            let started = Instant::now();
            schedule.start(job(1, 0, 0, 5_000, shape)).unwrap();
            let progress = schedule.progress().unwrap();
            let elapsed = started.elapsed();
            assert!(
                elapsed < Duration::from_secs(1),
                "radius 5000 {shape:?} took {elapsed:?}"
            );
            assert!(
                progress.total > 100_000,
                "{shape:?} total {}",
                progress.total
            );
            assert_eq!(progress.done, 0);
            assert!(std::mem::size_of_val(&schedule) < 4096);
            assert!(std::mem::size_of::<Schedule>() < 4096);
            let saved = schedule.save();
            assert!(saved.len() < 4096, "save is {} bytes", saved.len());

            let mut tiny = Schedule::new();
            tiny.start(job(1, 0, 0, 0, shape)).unwrap();
            assert!(
                saved.len() < tiny.save().len() + 64,
                "radius 5000 save stored chunk data ({} vs {} bytes)",
                saved.len(),
                tiny.save().len()
            );
        }

        let mut square = Schedule::new();
        square.start(job(1, 0, 0, 5_000, Shape::Square)).unwrap();
        let mut circle = Schedule::new();
        circle.start(job(1, 0, 0, 5_000, Shape::Circle)).unwrap();
        let square_total = square.progress().unwrap().total;
        let circle_total = circle.progress().unwrap().total;
        let chunk_radius = i64::from(5_000_i32.div_euclid(16));
        let side = chunk_radius * 2 + 1;
        assert_eq!(square_total, u64::try_from(side * side).unwrap());
        assert!(circle_total > 100_000);
        assert!(circle_total < square_total);
    }

    #[test]
    fn walk_matches_an_independent_count() {
        let centers = [(0, 0), (40, -25), (-17, 16), (8, 8)];
        let drained = [0, 15, 16, 17, 20, 24, 31, 32, 48];
        let counted = [64, 100, 255, 1_000];
        for &(x, z) in &centers {
            for &radius in drained.iter().chain(counted.iter()) {
                for shape in [Shape::Square, Shape::Circle] {
                    assert_region(x, z, radius, shape, radius <= 48);
                }
            }
        }
    }

    #[track_caller]
    fn assert_region(x: i32, z: i32, radius: i32, shape: Shape, compare_chunks: bool) {
        let mut schedule = Schedule::new();
        schedule.start(job(1, x, z, radius, shape)).unwrap();
        let total = schedule.progress().unwrap().total;
        assert_eq!(
            total,
            brute_count(radius, shape),
            "center ({x}, {z}) radius {radius} {shape:?}"
        );
        if !compare_chunks {
            return;
        }
        let chunks = drain(&mut schedule);
        assert_eq!(chunks.len() as u64, total);
        assert_eq!(schedule.progress().unwrap().done, total);
        assert_eq!(schedule.next_chunk(), None);
        let center_x = x.div_euclid(16);
        let center_z = z.div_euclid(16);
        assert_eq!(chunks[0], (center_x, center_z));
        let got: BTreeSet<_> = chunks.iter().copied().collect();
        assert_eq!(got.len(), chunks.len(), "duplicate chunk");
        let mut expected = BTreeSet::new();
        let chunk_radius = radius.div_euclid(16);
        for dx in -chunk_radius..=chunk_radius {
            for dz in -chunk_radius..=chunk_radius {
                if shape == Shape::Circle && !block_inside(dx, dz, radius) {
                    continue;
                }
                expected.insert((center_x + dx, center_z + dz));
            }
        }
        assert_eq!(got, expected);
        let mut previous = 0;
        for &(cx, cz) in &chunks {
            let distance = (cx - center_x).abs().max((cz - center_z).abs());
            assert!(
                distance >= previous,
                "radius {radius} {shape:?} went from {previous} to {distance}"
            );
            previous = distance;
        }
    }

    #[test]
    fn center_is_a_block_position() {
        let mut schedule = Schedule::new();
        schedule.start(job(1, -17, 16, 0, Shape::Square)).unwrap();
        assert_eq!(schedule.next_chunk(), Some((-2, 1)));

        let mut schedule = Schedule::new();
        schedule
            .start(job(1, i32::MIN, i32::MAX, 0, Shape::Circle))
            .unwrap();
        assert_eq!(
            schedule.next_chunk(),
            Some((i32::MIN.div_euclid(16), i32::MAX.div_euclid(16)))
        );

        let mut schedule = Schedule::new();
        schedule.start(job(1, -1, 32, 16, Shape::Square)).unwrap();
        let chunks = drain(&mut schedule);
        assert_eq!(chunks[0], (-1, 2));
        assert_eq!(chunks.len(), 9);
        for (x, z) in &chunks {
            assert!((x + 1).abs().max((z - 2).abs()) <= 1);
        }
    }

    #[test]
    fn bad_json_is_an_idle_schedule() {
        for text in ["", "nope", "{", "[]", "null", "42", "{}", "{\"job\":null}"] {
            let mut schedule = Schedule::load(text);
            assert!(schedule.progress().is_none(), "{text:?}");
            assert_eq!(schedule.next_chunk(), None);
        }
        let idle = Schedule::load(&Schedule::new().save());
        assert!(idle.progress().is_none());
    }

    #[test]
    fn radius_outside_0_to_50000_is_rejected() {
        let mut schedule = Schedule::new();
        assert!(schedule.start(job(1, 0, 0, -1, Shape::Square)).is_err());
        assert!(schedule.progress().is_none());
        assert!(schedule.start(job(1, 0, 0, 50_001, Shape::Circle)).is_err());
        assert!(schedule.progress().is_none());

        schedule.start(job(1, 0, 0, 0, Shape::Square)).unwrap();
        assert!(schedule.start(job(2, 0, 0, -5, Shape::Square)).is_err());
        assert_eq!(schedule.next_chunk(), Some((0, 0)));

        for shape in [Shape::Square, Shape::Circle] {
            let mut wide = Schedule::new();
            let started = Instant::now();
            wide.start(job(1, 3, -4, 50_000, shape)).unwrap();
            let elapsed = started.elapsed();
            assert!(
                elapsed < Duration::from_secs(1),
                "radius 50000 {shape:?} took {elapsed:?}"
            );
            let total = wide.progress().unwrap().total;
            assert!(total > 100_000);
            assert!(std::mem::size_of_val(&wide) < 4096);
            assert!(wide.save().len() < 4096);
        }
    }

    #[test]
    fn command_starts_pauses_and_cancels() {
        let mut schedule = Schedule::new();
        let text = apply(
            &mut schedule,
            "pregen start 8 -8 0 square the_nether",
            "minecraft:overworld",
        )
        .unwrap();
        assert!(text.contains("0/1"), "{text}");
        assert_eq!(
            schedule.dimension().as_deref(),
            Some("minecraft:the_nether")
        );
        apply(&mut schedule, "pregen pause", "minecraft:overworld").unwrap();
        assert!(schedule.progress().unwrap().paused);
        apply(&mut schedule, "continue", "minecraft:overworld").unwrap();
        assert!(!schedule.progress().unwrap().paused);
        let cancelled = apply(&mut schedule, "pregen cancel", "minecraft:overworld").unwrap();
        assert!(cancelled.contains("cancelled"));
        assert!(apply(&mut schedule, "pregen start nope", "minecraft:overworld").is_err());
    }

    #[test]
    fn step_finishes_one_flat_chunk_without_keeping_it() {
        let Some(ctx) = crate::testkit::flat() else {
            return;
        };
        let before = ctx.world.loaded_chunks();
        apply(
            &mut ctx.pregen.lock().expect("pregen"),
            "start 0 0 0",
            "minecraft:overworld",
        )
        .unwrap();
        step(&ctx);
        let progress = ctx.pregen.lock().expect("pregen").progress().unwrap();
        assert_eq!((progress.done, progress.total), (1, 1));
        assert!(ctx.world.loaded_chunks() <= before);
    }
}
