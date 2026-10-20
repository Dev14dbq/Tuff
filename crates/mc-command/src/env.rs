//! Registries, tags and block properties from the extracted game data
//! (`reports/registries.json`, `reports/blocks.json`, the data pack).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::args::Env;

#[derive(Debug, Default)]
pub struct DataEnv {
    registries: HashMap<String, HashSet<String>>,
    blocks: HashMap<String, Vec<(String, Vec<String>)>>,
    /// The data pack root (`.../data`).
    pack: PathBuf,
}

fn split(id: &str) -> (&str, &str) {
    id.split_once(':').unwrap_or(("minecraft", id))
}

impl DataEnv {
    pub fn load(data: &Path) -> Self {
        let read = |p: &str| -> Value {
            std::fs::read_to_string(data.join(p))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null)
        };
        let mut registries = HashMap::new();
        if let Some(o) = read("reports/registries.json").as_object() {
            for (name, r) in o {
                let set = r["entries"]
                    .as_object()
                    .map(|e| e.keys().cloned().collect())
                    .unwrap_or_default();
                registries.insert(name.clone(), set);
            }
        }
        let mut blocks = HashMap::new();
        if let Some(o) = read("reports/blocks.json").as_object() {
            for (name, b) in o {
                let props = b["properties"]
                    .as_object()
                    .map(|p| {
                        p.iter()
                            .map(|(k, v)| {
                                let values = v
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|x| x.as_str().map(str::to_owned))
                                    .collect();
                                (k.clone(), values)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                blocks.insert(name.clone(), props);
            }
        }
        Self {
            registries,
            blocks,
            pack: data.join("data"),
        }
    }

    /// Ids of a registry from the reports, or the data pack's files.
    pub fn ids(&self, registry: &str) -> Vec<String> {
        if let Some(set) = self.registries.get(registry) {
            let mut v: Vec<String> = set.iter().cloned().collect();
            v.sort();
            return v;
        }
        let (_, path) = split(registry);
        let mut out = Vec::new();
        let Ok(namespaces) = std::fs::read_dir(&self.pack) else {
            return out;
        };
        for ns in namespaces.flatten() {
            let root = ns.path().join(path);
            let name = ns.file_name().to_string_lossy().into_owned();
            collect(&root, &root, &name, &mut out);
        }
        out.sort();
        out
    }

    /// The ids in a tag (`#` members followed).
    pub fn tag(&self, registry: &str, tag: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.tag_into(registry, tag, &mut out, 0);
        out
    }

    fn tag_into(&self, registry: &str, tag: &str, out: &mut Vec<String>, depth: usize) {
        if depth > 16 {
            return;
        }
        let Some(v) = self.tag_file(registry, tag) else {
            return;
        };
        for e in v["values"].as_array().into_iter().flatten() {
            let id = e.as_str().or_else(|| e["id"].as_str()).unwrap_or("");
            if let Some(t) = id.strip_prefix('#') {
                self.tag_into(registry, t, out, depth + 1);
            } else if !id.is_empty() {
                out.push(crate::args::full_id(id));
            }
        }
    }

    fn tag_file(&self, registry: &str, tag: &str) -> Option<Value> {
        let (_, reg) = split(registry);
        let (ns, path) = split(tag);
        let p = self
            .pack
            .join(ns)
            .join("tags")
            .join(reg)
            .join(format!("{path}.json"));
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    }

    /// Every tag name of a registry.
    pub fn tags(&self, registry: &str) -> Vec<String> {
        let (_, reg) = split(registry);
        let mut out = Vec::new();
        let Ok(namespaces) = std::fs::read_dir(&self.pack) else {
            return out;
        };
        for ns in namespaces.flatten() {
            let root = ns.path().join("tags").join(reg);
            let name = ns.file_name().to_string_lossy().into_owned();
            collect(&root, &root, &name, &mut out);
        }
        out.sort();
        out
    }
}

fn collect(root: &Path, dir: &Path, ns: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(root, &p, ns, out);
        } else if p.extension().is_some_and(|x| x == "json")
            && let Ok(rel) = p.strip_prefix(root)
        {
            let id = rel.with_extension("").to_string_lossy().replace('\\', "/");
            out.push(format!("{ns}:{id}"));
        }
    }
}

impl Env for DataEnv {
    fn entries(&self, registry: &str) -> Option<Vec<String>> {
        let mut all: Vec<String> = if let Some(set) = self.registries.get(registry) {
            set.iter().cloned().collect()
        } else {
            let (_, reg) = split(registry);
            let mut out = Vec::new();
            for ns in std::fs::read_dir(&self.pack).ok()?.flatten() {
                let root = ns.path().join(reg);
                let name = ns.file_name().to_string_lossy().into_owned();
                collect(&root, &root, &name, &mut out);
            }
            out
        };
        all.sort();
        Some(all)
    }

    fn exists(&self, registry: &str, id: &str) -> bool {
        if registry == "minecraft:dimension" {
            return matches!(
                id,
                "minecraft:overworld" | "minecraft:the_nether" | "minecraft:the_end"
            );
        }
        if let Some(set) = self.registries.get(registry) {
            return set.contains(id);
        }
        let (_, reg) = split(registry);
        let (ns, path) = split(id);
        self.pack
            .join(ns)
            .join(reg)
            .join(format!("{path}.json"))
            .is_file()
    }

    fn tag_exists(&self, registry: &str, tag: &str) -> bool {
        self.tag_file(registry, tag).is_some()
    }

    fn block_properties(&self, block: &str) -> Option<Vec<(String, Vec<String>)>> {
        self.blocks.get(block).cloned()
    }
}
