//! Worldgen JSON across the vanilla data folder and datapack layers.
//!
//! Each root is a `data` directory. A file lives at
//! `<root>/<namespace>/worldgen/<kind>/<path>.json`. Roots are lowest
//! priority first; a later file replaces the same `namespace:path` id.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Vanilla `data` folder, then datapack `data` folders, lowest priority first.
pub fn roots(data_root: &Path, layers: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(1 + layers.len());
    roots.push(data_root.to_path_buf());
    roots.extend(layers.iter().cloned());
    roots
}

/// `namespace:path`, or `minecraft:` when the id has no namespace.
pub fn full_id(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// `(namespace, path)` of a registry id. A missing namespace is `minecraft`.
pub fn split_id(id: &str) -> (&str, &str) {
    match id.split_once(':') {
        Some((ns, path)) if !ns.is_empty() && !path.is_empty() => (ns, path),
        _ => ("minecraft", id),
    }
}

/// Every JSON file of one worldgen kind. Later roots replace the same id.
pub fn read_kind(roots: &[PathBuf], kind: &str) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for_each(roots, kind, |ns, rel, value| {
        out.insert(format!("{ns}:{rel}"), value);
    });
    out
}

/// One worldgen file. The last root that has it wins.
pub fn read_id(roots: &[PathBuf], kind: &str, id: &str) -> Option<Value> {
    let (ns, path) = split_id(id);
    for root in roots.iter().rev() {
        let file = root
            .join(ns)
            .join("worldgen")
            .join(kind)
            .join(format!("{path}.json"));
        if let Some(value) = read_file(&file) {
            return Some(value);
        }
    }
    None
}

/// A biome tag file (`tags/worldgen/biome`), later roots first.
pub fn tag_file(roots: &[PathBuf], tag: &str) -> Option<PathBuf> {
    let tag = tag.strip_prefix('#').unwrap_or(tag);
    let (ns, path) = split_id(tag);
    for root in roots.iter().rev() {
        let file = root
            .join(ns)
            .join("tags")
            .join("worldgen")
            .join("biome")
            .join(format!("{path}.json"));
        if file.is_file() {
            return Some(file);
        }
    }
    None
}

/// A structure template (`<namespace>/structure/<path>.nbt`), later roots first.
pub fn template_file(roots: &[PathBuf], id: &str) -> Option<PathBuf> {
    let (ns, path) = split_id(id);
    for root in roots.iter().rev() {
        let file = root.join(ns).join("structure").join(format!("{path}.nbt"));
        if file.is_file() {
            return Some(file);
        }
    }
    None
}

/// Visit `(namespace, relative path, json)` of `worldgen/<kind>`, vanilla first.
pub fn for_each(roots: &[PathBuf], kind: &str, mut visit: impl FnMut(&str, &str, Value)) {
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let ns = entry.file_name().to_string_lossy().into_owned();
            let dir = path.join("worldgen").join(kind);
            walk_json(&dir, &mut |rel, value| visit(&ns, &rel, value));
        }
    }
}

fn read_file(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn walk_json(dir: &Path, visit: &mut impl FnMut(String, Value)) {
    fn walk(base: &Path, dir: &Path, visit: &mut impl FnMut(String, Value)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, visit);
            } else if path.extension().is_some_and(|ext| ext == "json")
                && let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(value) = serde_json::from_str(&text)
            {
                let rel = path.strip_prefix(base).unwrap_or(&path).with_extension("");
                let rel = rel.to_string_lossy().replace('\\', "/");
                visit(rel, value);
            }
        }
    }
    walk(dir, dir, visit);
}
