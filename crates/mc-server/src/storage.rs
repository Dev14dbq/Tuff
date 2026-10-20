//! Command storage (`/data ... storage <id>`), kept as vanilla keeps it:
//! one `data/minecraft/command_storage_<namespace>.dat` per namespace.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mc_nbt::{Compound, Tag};

#[derive(Debug, Default)]
pub struct Storage {
    /// By full id (`t:s`).
    pub contents: BTreeMap<String, Compound>,
    dir: Option<PathBuf>,
    data_version: i32,
}

/// Lists holding tags of different types, as vanilla stores them: each
/// element that is not a compound wrapped as `{"": value}`.
pub fn wrap_lists(t: &Tag) -> Tag {
    match t {
        Tag::Compound(c) => {
            let mut out = Compound::new();
            for (k, v) in c.iter() {
                out.insert(k, wrap_lists(v));
            }
            Tag::Compound(out)
        }
        Tag::List(l) => {
            let items: Vec<Tag> = l.iter().map(wrap_lists).collect();
            let mixed = items.windows(2).any(|w| w[0].type_id() != w[1].type_id());
            Tag::List(if mixed {
                items
                    .into_iter()
                    .map(|t| match t {
                        Tag::Compound(_) => t,
                        other => {
                            let mut c = Compound::new();
                            c.insert("", other);
                            Tag::Compound(c)
                        }
                    })
                    .collect()
            } else {
                items
            })
        }
        other => other.clone(),
    }
}

/// The other way round: wrapped elements back as they were.
pub fn unwrap_lists(t: &Tag) -> Tag {
    match t {
        Tag::Compound(c) => {
            let mut out = Compound::new();
            for (k, v) in c.iter() {
                out.insert(k, unwrap_lists(v));
            }
            Tag::Compound(out)
        }
        Tag::List(l) => Tag::List(
            l.iter()
                .map(|e| match e {
                    Tag::Compound(c) if c.len() == 1 && c.get("").is_some() => {
                        unwrap_lists(c.get("").unwrap_or(e))
                    }
                    other => unwrap_lists(other),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

impl Storage {
    pub fn load(world: Option<&Path>, data_version: i32) -> Self {
        let dir = world.map(|w| w.join("data/minecraft"));
        let mut contents = BTreeMap::new();
        if let Some(entries) = dir.as_ref().and_then(|d| std::fs::read_dir(d).ok()) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(ns) = name
                    .strip_prefix("command_storage_")
                    .and_then(|n| n.strip_suffix(".dat"))
                else {
                    continue;
                };
                let Ok(root) = crate::world::read_nbt_gz_compound(&e.path()) else {
                    continue;
                };
                let Some(Tag::Compound(data)) = root.get("data") else {
                    continue;
                };
                let Some(Tag::Compound(all)) = data.get("contents") else {
                    continue;
                };
                for (path, v) in all.iter() {
                    if let Tag::Compound(c) = unwrap_lists(v) {
                        contents.insert(format!("{ns}:{path}"), c);
                    }
                }
            }
        }
        Storage {
            contents,
            dir,
            data_version,
        }
    }

    pub fn get(&self, id: &str) -> Compound {
        self.contents.get(id).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, id: &str, c: Compound) {
        self.contents.insert(id.to_owned(), c);
    }

    pub fn save(&self) {
        let Some(dir) = &self.dir else { return };
        let mut by_ns: BTreeMap<&str, Compound> = BTreeMap::new();
        for (id, c) in &self.contents {
            let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
            by_ns
                .entry(ns)
                .or_default()
                .insert(path, wrap_lists(&Tag::Compound(c.clone())));
        }
        let _ = std::fs::create_dir_all(dir);
        for (ns, contents) in by_ns {
            let mut data = Compound::new();
            data.insert("contents", Tag::Compound(contents));
            let mut root = Compound::new();
            root.insert("data", Tag::Compound(data));
            root.insert("DataVersion", Tag::Int(self.data_version));
            let file = dir.join(format!("command_storage_{ns}.dat"));
            if let Err(e) = crate::world::write_nbt_gz(&file, &root) {
                tracing::warn!("saving command storage {ns}: {e:#}");
            }
        }
    }
}
