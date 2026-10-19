//! Weather: rain and thunder coming and going on their own (game rule
//! `advance_weather`) or set by `/weather`, kept as vanilla keeps it in
//! `data/minecraft/weather.dat`.
//!
//! The cycle follows the wiki ("Weather"): clear for 12000–180000 ticks,
//! rain for 12000–24000, thunder for 3600–15600 with 12000–180000 between;
//! the rain and thunder levels the client draws move by 0.01 a tick.

use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};

use crate::util::random_u64;

fn between(lo: i32, hi: i32) -> i32 {
    lo + (random_u64() % (hi - lo + 1) as u64) as i32
}

pub fn rain_delay() -> i32 {
    between(12000, 180000)
}

pub fn rain_duration() -> i32 {
    between(12000, 24000)
}

pub fn thunder_delay() -> i32 {
    between(12000, 180000)
}

pub fn thunder_duration() -> i32 {
    between(3600, 15600)
}

#[derive(Debug, Default)]
pub struct Weather {
    pub clear_time: i32,
    pub rain_time: i32,
    pub thunder_time: i32,
    pub raining: bool,
    pub thundering: bool,
    pub rain_level: f32,
    pub thunder_level: f32,
    file: Option<PathBuf>,
    data_version: i32,
}

/// What players must be told after a tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Rain started (`Some(true)`) or stopped.
    pub raining: Option<bool>,
    pub rain_level: Option<f32>,
    pub thunder_level: Option<f32>,
}

fn int(c: &Compound, k: &str) -> i32 {
    match c.get(k) {
        Some(Tag::Int(v)) => *v,
        Some(Tag::Byte(v)) => i32::from(*v),
        _ => 0,
    }
}

impl Weather {
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        let file = world.map(|w| w.join("data/minecraft/weather.dat"));
        let mut w = Weather {
            file: file.clone(),
            data_version,
            ..Weather::default()
        };
        if let Some(f) = &file
            && let Ok(root) = crate::world::read_nbt_gz_compound(f)
            && let Some(Tag::Compound(d)) = root.get("data")
        {
            w.clear_time = int(d, "clear_weather_time");
            w.rain_time = int(d, "rain_time");
            w.thunder_time = int(d, "thunder_time");
            w.raining = int(d, "raining") != 0;
            w.thundering = int(d, "thundering") != 0;
        }
        w.rain_level = if w.raining { 1.0 } else { 0.0 };
        w.thunder_level = if w.raining && w.thundering { 1.0 } else { 0.0 };
        w
    }

    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut data = Compound::new();
        data.insert("clear_weather_time", Tag::Int(self.clear_time));
        data.insert("rain_time", Tag::Int(self.rain_time));
        data.insert("thunder_time", Tag::Int(self.thunder_time));
        data.insert("raining", Tag::Byte(i8::from(self.raining)));
        data.insert("thundering", Tag::Byte(i8::from(self.thundering)));
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving the weather: {e:#}");
        }
    }

    /// `/weather clear|rain|thunder [duration]`.
    pub fn set(&mut self, clear: i32, rain: i32, raining: bool, thundering: bool) {
        self.clear_time = clear;
        self.rain_time = rain;
        self.thunder_time = rain;
        self.raining = raining;
        self.thundering = thundering;
    }

    /// One tick of the cycle (when `advance` is the game rule's value) and
    /// of the levels.
    pub fn tick(&mut self, advance: bool) -> Changes {
        let was_raining = self.rain_level > 0.2;
        if advance {
            if self.clear_time > 0 {
                self.clear_time -= 1;
                self.thunder_time = if self.thundering { 0 } else { 1 };
                self.rain_time = if self.raining { 0 } else { 1 };
                self.thundering = false;
                self.raining = false;
            } else {
                if self.thunder_time > 0 {
                    self.thunder_time -= 1;
                    if self.thunder_time == 0 {
                        self.thundering = !self.thundering;
                    }
                } else if self.thundering {
                    self.thunder_time = thunder_duration();
                } else {
                    self.thunder_time = thunder_delay();
                }
                if self.rain_time > 0 {
                    self.rain_time -= 1;
                    if self.rain_time == 0 {
                        self.raining = !self.raining;
                    }
                } else if self.raining {
                    self.rain_time = rain_duration();
                } else {
                    self.rain_time = rain_delay();
                }
            }
        }
        let mut changes = Changes::default();
        let (old_rain, old_thunder) = (self.rain_level, self.thunder_level);
        let step = |level: f32, up: bool| (level + if up { 0.01 } else { -0.01 }).clamp(0.0, 1.0);
        self.thunder_level = step(self.thunder_level, self.thundering);
        self.rain_level = step(self.rain_level, self.raining);
        if self.rain_level != old_rain {
            changes.rain_level = Some(self.rain_level);
        }
        if self.thunder_level != old_thunder {
            changes.thunder_level = Some(self.thunder_level);
        }
        let now_raining = self.rain_level > 0.2;
        if now_raining != was_raining {
            changes.raining = Some(now_raining);
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rain_fades_in() {
        let mut w = Weather::default();
        w.set(0, 1000, true, false);
        let mut started = false;
        for _ in 0..30 {
            started |= w.tick(true).raining == Some(true);
        }
        assert!(started);
        assert!((w.rain_level - 0.3).abs() < 1e-4);
    }
}
