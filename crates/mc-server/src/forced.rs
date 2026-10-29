//! Force-loaded chunks (`/forceload`), kept as vanilla keeps them: tickets
//! of type `minecraft:forced` in the dimension's
//! `data/minecraft/chunk_tickets.dat`. Other tickets in the file are kept.
//!
//! The chunks are remembered and saved. [`crate::tickets`] keeps them
//! loaded and ticking with nobody near, and writes portal tickets into
//! the same file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};

const FORCED: &str = "minecraft:forced";

#[derive(Debug)]
pub struct Forced {
    /// By (z, x): the order vanilla lists them in.
    chunks: BTreeSet<(i32, i32)>,
    /// Tickets of other types, as read.
    others: Vec<Tag>,
    file: Option<PathBuf>,
    data_version: i32,
}

impl Forced {
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        Self::load_in(world, "overworld", data_version)
    }

    /// The tickets of the dimension whose folder is `minecraft/<name>`.
    pub fn load_in(world: Option<&Path>, name: &str, data_version: i32) -> Self {
        let file = world.map(|w| {
            w.join(format!(
                "dimensions/minecraft/{name}/data/minecraft/chunk_tickets.dat"
            ))
        });
        let tickets = file
            .as_ref()
            .and_then(|f| crate::world::read_nbt_gz_compound(f).ok())
            .and_then(|root| match root.get("data") {
                Some(Tag::Compound(d)) => match d.get("tickets") {
                    Some(Tag::List(l)) => Some(l.clone()),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_default();
        let mut chunks = BTreeSet::new();
        let mut others = Vec::new();
        for t in tickets {
            let forced = match &t {
                Tag::Compound(c) => match (c.get("type"), c.get("chunk_pos")) {
                    (Some(Tag::String(ty)), Some(Tag::IntArray(p))) if ty == FORCED => {
                        p.get(..2).map(|p| (p[1], p[0]))
                    }
                    _ => None,
                },
                _ => None,
            };
            match forced {
                Some(zx) => {
                    chunks.insert(zx);
                }
                None => others.push(t),
            }
        }
        Forced {
            chunks,
            others,
            file,
            data_version,
        }
    }

    /// The forced chunks (x, z), in vanilla's order.
    pub fn list(&self) -> Vec<(i32, i32)> {
        self.chunks.iter().map(|&(z, x)| (x, z)).collect()
    }

    pub fn contains(&self, x: i32, z: i32) -> bool {
        self.chunks.contains(&(z, x))
    }

    /// Forces or frees a chunk; whether that changed anything.
    pub fn set(&mut self, x: i32, z: i32, forced: bool) -> bool {
        if forced {
            self.chunks.insert((z, x))
        } else {
            self.chunks.remove(&(z, x))
        }
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
    }

    /// Portal tickets the file was keeping, taken out so [`crate::tickets`]
    /// owns their remaining time. Other ticket types stay.
    pub fn take_portals(&mut self) -> Vec<Tag> {
        let mut out = Vec::new();
        self.others.retain(|t| {
            if is_portal(t) {
                out.push(t.clone());
                false
            } else {
                true
            }
        });
        out
    }

    pub fn save(&self) {
        let kept: Vec<Tag> = self
            .others
            .iter()
            .filter(|t| is_portal(t))
            .cloned()
            .collect();
        self.save_with(&kept);
    }

    /// Writes forceload tickets plus `portals` (the live portal tickets).
    /// Portal entries still in `others` are dropped, so a save after
    /// [`Self::take_portals`] does not write them twice.
    pub fn save_with(&self, portals: &[Tag]) {
        let Some(file) = &self.file else {
            return;
        };
        let mut tickets: Vec<Tag> = self
            .others
            .iter()
            .filter(|t| !is_portal(t))
            .cloned()
            .collect();
        tickets.extend(portals.iter().cloned());
        for &(z, x) in &self.chunks {
            let mut t = Compound::new();
            t.insert("chunk_pos", Tag::IntArray(vec![x, z]));
            t.insert("level", Tag::Int(31));
            t.insert("type", Tag::String(FORCED.into()));
            tickets.push(Tag::Compound(t));
        }
        let mut data = Compound::new();
        data.insert("tickets", Tag::List(tickets));
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving force-loaded chunks: {e:#}");
        }
    }
}

fn is_portal(tag: &Tag) -> bool {
    match tag {
        Tag::Compound(c) => {
            matches!(c.get("type"), Some(Tag::String(ty)) if ty == "minecraft:portal")
        }
        _ => false,
    }
}
