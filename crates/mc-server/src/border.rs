//! The world border (`/worldborder`), kept as vanilla keeps it in the
//! dimension's `data/minecraft/world_border.dat`.

use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::InitializeBorder;

/// The widest a border may be.
pub const MAX_SIZE: f64 = 59_999_968.0;

#[derive(Debug)]
pub struct Border {
    pub center_x: f64,
    pub center_z: f64,
    /// Size now, and where it is moving to over `lerp_time` ms.
    pub size: f64,
    pub lerp_target: f64,
    pub lerp_time: i64,
    pub damage_per_block: f64,
    pub safe_zone: f64,
    pub warning_blocks: i32,
    pub warning_time: i32,
    file: Option<PathBuf>,
    data_version: i32,
}

fn f(c: &Compound, k: &str, default: f64) -> f64 {
    match c.get(k) {
        Some(Tag::Double(v)) => *v,
        Some(Tag::Float(v)) => f64::from(*v),
        _ => default,
    }
}

fn i(c: &Compound, k: &str, default: i64) -> i64 {
    match c.get(k) {
        Some(Tag::Int(v)) => i64::from(*v),
        Some(Tag::Long(v)) => *v,
        _ => default,
    }
}

impl Border {
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        let file =
            world.map(|w| w.join("dimensions/minecraft/overworld/data/minecraft/world_border.dat"));
        let d = file
            .as_ref()
            .and_then(|f| crate::world::read_nbt_gz_compound(f).ok())
            .and_then(|root| match root.get("data") {
                Some(Tag::Compound(c)) => Some(c.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let size = f(&d, "size", MAX_SIZE);
        Border {
            center_x: f(&d, "center_x", 0.0),
            center_z: f(&d, "center_z", 0.0),
            size,
            lerp_target: f(&d, "lerp_target", size),
            lerp_time: i(&d, "lerp_time", 0),
            damage_per_block: f(&d, "damage_per_block", 0.2),
            safe_zone: f(&d, "safe_zone", 5.0),
            warning_blocks: i(&d, "warning_blocks", 5) as i32,
            warning_time: i(&d, "warning_time", 300) as i32,
            file,
            data_version,
        }
    }

    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut data = Compound::new();
        data.insert("center_x", Tag::Double(self.center_x));
        data.insert("center_z", Tag::Double(self.center_z));
        data.insert("size", Tag::Double(self.size));
        data.insert("lerp_target", Tag::Double(self.lerp_target));
        data.insert("lerp_time", Tag::Long(self.lerp_time));
        data.insert("damage_per_block", Tag::Double(self.damage_per_block));
        data.insert("safe_zone", Tag::Double(self.safe_zone));
        data.insert("warning_blocks", Tag::Int(self.warning_blocks));
        data.insert("warning_time", Tag::Int(self.warning_time));
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving the world border: {e:#}");
        }
    }

    /// One tick (50 ms) of a moving border.
    pub fn tick(&mut self) {
        if self.lerp_time <= 0 {
            return;
        }
        let step = (self.lerp_target - self.size) * 50.0 / self.lerp_time as f64;
        self.size += step;
        self.lerp_time = (self.lerp_time - 50).max(0);
        if self.lerp_time == 0 {
            self.size = self.lerp_target;
        }
    }

    pub fn packet(&self) -> InitializeBorder {
        InitializeBorder {
            x: self.center_x,
            z: self.center_z,
            old_size: self.size,
            new_size: self.lerp_target,
            speed: self.lerp_time,
            portal_boundary: 29_999_984,
            warning_blocks: self.warning_blocks,
            warning_time: self.warning_time,
        }
    }
}
