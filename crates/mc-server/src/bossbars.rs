//! Boss bars made with `/bossbar`, kept as vanilla keeps them in the
//! world's `data/minecraft/custom_boss_events.dat`.

use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::{BossAction, BossEvent};

/// Colours in protocol order, with the chat colour of the bar's name.
pub const COLORS: [(&str, &str); 7] = [
    ("pink", "red"),
    ("blue", "blue"),
    ("red", "dark_red"),
    ("green", "green"),
    ("yellow", "yellow"),
    ("purple", "dark_blue"),
    ("white", "white"),
];

/// Styles in protocol order.
pub const STYLES: [&str; 5] = [
    "progress",
    "notched_6",
    "notched_10",
    "notched_12",
    "notched_20",
];

#[derive(Clone, Debug)]
pub struct Bar {
    pub id: String,
    pub name: Tag,
    pub value: i32,
    pub max: i32,
    pub visible: bool,
    pub color: usize,
    pub style: usize,
    flags: u8,
    /// Who sees it (online or not), in the order they were added.
    pub players: Vec<u128>,
    /// The bar's id on the client.
    pub uuid: u128,
}

impl Bar {
    pub fn progress(&self) -> f32 {
        (self.value as f32 / self.max as f32).clamp(0.0, 1.0)
    }

    /// The name as commands show it: in brackets, coloured as the bar,
    /// with the id on hover.
    pub fn display(&self) -> Tag {
        let mut hover = Compound::new();
        hover.insert("action", "show_text");
        hover.insert("value", self.id.as_str());
        let mut c = Compound::new();
        c.insert("translate", "chat.square_brackets");
        c.insert("with", Tag::List(vec![self.name.clone()]));
        c.insert("color", COLORS[self.color].1);
        c.insert("hover_event", Tag::Compound(hover));
        c.insert("insertion", self.id.as_str());
        Tag::Compound(c)
    }

    pub fn add_packet(&self) -> BossEvent {
        self.packet(BossAction::Add {
            name: self.name.clone(),
            progress: self.progress(),
            color: self.color as i32,
            overlay: self.style as i32,
            flags: self.flags,
        })
    }

    pub fn packet(&self, action: BossAction) -> BossEvent {
        BossEvent {
            uuid: self.uuid,
            action,
        }
    }
}

#[derive(Debug)]
pub struct BossBars {
    /// In the order they were added.
    bars: Vec<Bar>,
    /// The capacity vanilla's hash map has grown to, which sets the order
    /// it lists the bars in.
    capacity: usize,
    file: Option<PathBuf>,
    data_version: i32,
}

/// Java's `String.hashCode`.
fn java_hash(s: &str) -> i32 {
    s.encode_utf16()
        .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)))
}

/// The hash map bucket vanilla puts an id in.
fn bucket(id: &str, capacity: usize) -> usize {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let h = java_hash(ns).wrapping_mul(31).wrapping_add(java_hash(path)) as u32;
    ((h ^ (h >> 16)) as usize) & (capacity - 1)
}

/// A random version 4 UUID.
fn random_uuid(seed: &str) -> u128 {
    use std::hash::BuildHasher;
    let r = std::collections::hash_map::RandomState::new();
    let u = u128::from(r.hash_one((seed, 0))) << 64 | u128::from(r.hash_one((seed, 1)));
    u & !(0xf000 << 64) | (0x4000 << 64)
}

impl BossBars {
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        let file = world.map(|w| w.join("data/minecraft/custom_boss_events.dat"));
        let data = file
            .as_ref()
            .and_then(|f| crate::world::read_nbt_gz_compound(f).ok())
            .and_then(|root| match root.get("data") {
                Some(Tag::Compound(d)) => Some(d.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let mut bars = BossBars {
            bars: Vec::new(),
            capacity: 16,
            file,
            data_version,
        };
        for (id, v) in data.iter() {
            let Tag::Compound(c) = v else { continue };
            let int = |k: &str, d: i32| match c.get(k) {
                Some(Tag::Int(v)) => *v,
                _ => d,
            };
            let flag = |k: &str| matches!(c.get(k), Some(Tag::Byte(1)));
            let named = |k: &str, list: &[&str]| match c.get(k) {
                Some(Tag::String(s)) => list.iter().position(|n| n == s).unwrap_or(0),
                _ => 0,
            };
            let colors: Vec<&str> = COLORS.iter().map(|c| c.0).collect();
            let mut bar = bars.create(id, c.get("Name").cloned().unwrap_or_else(|| "".into()));
            bar.value = int("Value", 0);
            bar.max = int("Max", 100);
            bar.visible = !matches!(c.get("Visible"), Some(Tag::Byte(0)));
            bar.color = colors
                .iter()
                .position(|n| matches!(c.get("Color"), Some(Tag::String(s)) if s == n))
                .unwrap_or(6);
            bar.style = named("Overlay", &STYLES);
            bar.flags = u8::from(flag("DarkenScreen"))
                | u8::from(flag("PlayBossMusic")) << 1
                | u8::from(flag("CreateWorldFog")) << 2;
            if let Some(Tag::List(l)) = c.get("Players") {
                bar.players = l
                    .iter()
                    .filter_map(|p| match p {
                        Tag::IntArray(a) if a.len() == 4 => {
                            Some(a.iter().fold(0u128, |u, &w| u << 32 | u128::from(w as u32)))
                        }
                        _ => None,
                    })
                    .collect();
            }
            bars.insert(bar);
        }
        bars
    }

    /// A new bar (not yet added).
    pub fn create(&self, id: &str, name: Tag) -> Bar {
        Bar {
            id: id.to_owned(),
            name,
            value: 0,
            max: 100,
            visible: true,
            color: 6,
            style: 0,
            flags: 0,
            players: Vec::new(),
            uuid: random_uuid(id),
        }
    }

    pub fn insert(&mut self, bar: Bar) {
        self.bars.push(bar);
        while self.bars.len() * 4 > self.capacity * 3 {
            self.capacity *= 2;
        }
    }

    pub fn remove(&mut self, id: &str) -> Option<Bar> {
        let i = self.bars.iter().position(|b| b.id == id)?;
        Some(self.bars.remove(i))
    }

    pub fn get(&self, id: &str) -> Option<&Bar> {
        self.bars.iter().find(|b| b.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Bar> {
        self.bars.iter_mut().find(|b| b.id == id)
    }

    /// The bars in the order vanilla lists them.
    pub fn list(&self) -> Vec<&Bar> {
        let mut bars: Vec<(usize, usize, &Bar)> = self
            .bars
            .iter()
            .enumerate()
            .map(|(i, b)| (bucket(&b.id, self.capacity), i, b))
            .collect();
        bars.sort_by_key(|&(bucket, i, _)| (bucket, i));
        bars.into_iter().map(|(_, _, b)| b).collect()
    }

    /// The bars a player joining sees.
    pub fn join_packets(&self, uuid: u128) -> Vec<BossEvent> {
        self.bars
            .iter()
            .filter(|b| b.visible && b.players.contains(&uuid))
            .map(Bar::add_packet)
            .collect()
    }

    pub fn save(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let mut data = Compound::new();
        for b in self.list() {
            let mut c = Compound::new();
            c.insert("Name", b.name.clone());
            if b.value != 0 {
                c.insert("Value", Tag::Int(b.value));
            }
            if b.max != 100 {
                c.insert("Max", Tag::Int(b.max));
            }
            c.insert("Visible", Tag::Byte(i8::from(b.visible)));
            if b.color != 6 {
                c.insert("Color", COLORS[b.color].0);
            }
            if b.style != 0 {
                c.insert("Overlay", STYLES[b.style]);
            }
            for (bit, key) in ["DarkenScreen", "PlayBossMusic", "CreateWorldFog"]
                .iter()
                .enumerate()
            {
                if b.flags >> bit & 1 == 1 {
                    c.insert(*key, Tag::Byte(1));
                }
            }
            if !b.players.is_empty() {
                let players = b
                    .players
                    .iter()
                    .map(|u| Tag::IntArray((0..4).map(|i| (u >> (96 - 32 * i)) as i32).collect()))
                    .collect();
                c.insert("Players", Tag::List(players));
            }
            data.insert(b.id.as_str(), Tag::Compound(c));
        }
        let mut root = Compound::new();
        root.insert("data", Tag::Compound(data));
        root.insert("DataVersion", Tag::Int(self.data_version));
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = crate::world::write_nbt_gz(file, &root) {
            tracing::warn!("saving boss bars: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_in_vanillas_order() {
        let mut bars = BossBars::load(None, 0);
        for id in ["minecraft:b", "minecraft:c", "minecraft:r", "minecraft:d"] {
            let bar = bars.create(id, Tag::from(id));
            bars.insert(bar);
        }
        let ids: Vec<&str> = bars.list().iter().map(|b| b.id.as_str()).collect();
        assert_eq!(
            ids,
            ["minecraft:b", "minecraft:r", "minecraft:d", "minecraft:c"]
        );
    }
}
