//! The command tree: every node vanilla registers, in its order, with the
//! parser, suggestion type and permission level of each.
//!
//! It is read from what a vanilla server sends an operator
//! (`observed/commands.json`, `cargo xtask observe-commands`), with the
//! permission levels from the data report (`reports/commands.json`).

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use mc_protocol::Write as _;
use serde_json::Value;

pub const ROOT: usize = 0;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Root,
    Literal,
    Argument { parser: String, properties: Value },
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub kind: Kind,
    pub executable: bool,
    pub children: Vec<usize>,
    pub redirect: Option<usize>,
    pub suggestions: Option<String>,
    /// The permission level needed (0 for everyone).
    pub level: u8,
    /// Whether it is marked as needing permission (flag 0x20).
    pub restricted: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// Argument type network ids by name.
    types: HashMap<String, i32>,
}

fn level_of(v: &Value) -> u8 {
    match v["permission"]["level"].as_str() {
        Some("moderators") => 1,
        Some("gamemasters") => 2,
        Some("admins") => 3,
        Some("owners") => 4,
        _ => 0,
    }
}

impl Tree {
    /// From the data folder (`cache/mcdata/<version>`); empty if the
    /// observation is missing.
    pub fn load(data: &Path) -> Result<Self, String> {
        let read = |p: &Path| -> Result<Value, String> {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
        };
        let observed = read(&data.join("observed/commands.json"))?;
        let report = read(&data.join("reports/commands.json"))?;
        let registries = read(&data.join("reports/registries.json"))?;
        let types: HashMap<String, i32> = registries["minecraft:command_argument_type"]["entries"]
            .as_object()
            .map(|e| {
                e.iter()
                    .map(|(k, v)| (k.clone(), v["protocol_id"].as_i64().unwrap_or(0) as i32))
                    .collect()
            })
            .unwrap_or_default();
        let raw = observed["tree"]["nodes"]
            .as_array()
            .ok_or("no tree nodes")?;
        let root = observed["tree"]["root"].as_u64().unwrap_or(0) as usize;
        // Renumber so the root is 0 and children come in the order sent.
        let mut index = vec![usize::MAX; raw.len()];
        let mut order = Vec::new();
        let mut queue = VecDeque::from([root]);
        while let Some(i) = queue.pop_front() {
            if i >= raw.len() || index[i] != usize::MAX {
                continue;
            }
            index[i] = order.len();
            order.push(i);
            for c in raw[i]["children"].as_array().into_iter().flatten() {
                queue.push_back(c.as_u64().unwrap_or(0) as usize);
            }
            if let Some(r) = raw[i]["redirect"].as_u64() {
                queue.push_back(r as usize);
            }
        }
        let mut nodes = Vec::with_capacity(order.len());
        for &i in &order {
            let n = &raw[i];
            let flags = n["flags"].as_u64().unwrap_or(0) as u8;
            let kind = match flags & 3 {
                0 => Kind::Root,
                1 => Kind::Literal,
                _ => Kind::Argument {
                    parser: n["parser"].as_str().unwrap_or("").to_owned(),
                    properties: n["properties"].clone(),
                },
            };
            nodes.push(Node {
                name: n["name"].as_str().unwrap_or("").to_owned(),
                kind,
                executable: flags & 4 != 0,
                children: n["children"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| index.get(c.as_u64()? as usize).copied())
                    .collect(),
                redirect: n["redirect"]
                    .as_u64()
                    .and_then(|r| index.get(r as usize).copied()),
                suggestions: n["suggestions"].as_str().map(str::to_owned),
                level: 0,
                restricted: flags & 0x20 != 0,
            });
        }
        let mut tree = Tree { nodes, types };
        // Permission levels by path from the report.
        let mut stack = vec![(ROOT, &report)];
        while let Some((i, rep)) = stack.pop() {
            if let Some(p) = rep.get("permissions") {
                tree.nodes[i].level = level_of(p);
            }
            for &c in &tree.nodes[i].children.clone() {
                if let Some(r) = rep["children"].get(&tree.nodes[c].name) {
                    stack.push((c, r));
                }
            }
        }
        Ok(tree)
    }

    /// A command of our own, at the root, for operators of `level`.
    pub fn add_literal(&mut self, name: &str, level: u8) {
        if self.nodes.is_empty() {
            return;
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            name: name.to_owned(),
            kind: Kind::Literal,
            executable: true,
            children: Vec::new(),
            redirect: None,
            suggestions: None,
            level,
            restricted: true,
        });
        self.nodes[ROOT].children.push(id);
    }

    pub fn child(&self, node: usize, name: &str) -> Option<usize> {
        self.nodes[node]
            .children
            .iter()
            .copied()
            .find(|&c| self.nodes[c].name == name)
    }

    /// The node at a path of names from the root (`["time", "set", "time"]`).
    pub fn path(&self, names: &[&str]) -> Option<usize> {
        names.iter().try_fold(ROOT, |n, name| self.child(n, name))
    }

    /// The names from the root to `node` (following children, not redirects).
    pub fn names(&self, node: usize) -> Vec<String> {
        fn find(t: &Tree, at: usize, want: usize, path: &mut Vec<usize>) -> bool {
            if at == want {
                return true;
            }
            for &c in &t.nodes[at].children {
                if path.contains(&c) {
                    continue;
                }
                path.push(c);
                if find(t, c, want, path) {
                    return true;
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        find(self, ROOT, node, &mut path);
        path.iter().map(|&i| self.nodes[i].name.clone()).collect()
    }

    /// The Commands packet for someone with permission `level`: the nodes
    /// they may use, in the order the game numbers them.
    pub fn packet(&self, level: u8) -> Vec<u8> {
        let usable = |i: usize| self.nodes[i].level <= level;
        let mut index: HashMap<usize, i32> = HashMap::new();
        let mut order = Vec::new();
        let mut queue = VecDeque::from([ROOT]);
        while let Some(i) = queue.pop_front() {
            if index.contains_key(&i) {
                continue;
            }
            index.insert(i, order.len() as i32);
            order.push(i);
            for &c in &self.nodes[i].children {
                if usable(c) {
                    queue.push_back(c);
                }
            }
            if let Some(r) = self.nodes[i].redirect {
                queue.push_back(r);
            }
        }
        let mut out = Vec::new();
        out.put_varint(order.len() as i32);
        for &i in &order {
            let n = &self.nodes[i];
            let mut flags = match n.kind {
                Kind::Root => 0u8,
                Kind::Literal => 1,
                Kind::Argument { .. } => 2,
            };
            if n.executable {
                flags |= 0x04;
            }
            if n.redirect.is_some() {
                flags |= 0x08;
            }
            if n.suggestions.is_some() {
                flags |= 0x10;
            }
            if n.restricted {
                flags |= 0x20;
            }
            out.push(flags);
            let children: Vec<i32> = n
                .children
                .iter()
                .filter(|&&c| usable(c))
                .map(|c| index[c])
                .collect();
            out.put_varint(children.len() as i32);
            for c in children {
                out.put_varint(c);
            }
            if let Some(r) = n.redirect {
                out.put_varint(index[&r]);
            }
            if n.kind != Kind::Root {
                out.put_string(&n.name);
            }
            if let Kind::Argument { parser, properties } = &n.kind {
                out.put_varint(self.types.get(parser).copied().unwrap_or(0));
                put_properties(&mut out, parser, properties);
            }
            if let Some(s) = &n.suggestions {
                out.put_string(s);
            }
        }
        out.put_varint(0);
        out
    }
}

fn put_properties(out: &mut Vec<u8>, parser: &str, p: &Value) {
    match parser {
        "brigadier:float" | "brigadier:double" | "brigadier:integer" | "brigadier:long" => {
            let (min, max) = (&p["min"], &p["max"]);
            out.push(u8::from(!min.is_null()) | (u8::from(!max.is_null()) << 1));
            for v in [min, max] {
                if v.is_null() {
                    continue;
                }
                match parser {
                    "brigadier:float" => out.put_f32(v.as_f64().unwrap_or(0.0) as f32),
                    "brigadier:double" => out.put_f64(v.as_f64().unwrap_or(0.0)),
                    "brigadier:integer" => out.put_i32(v.as_i64().unwrap_or(0) as i32),
                    _ => out.put_i64(v.as_i64().unwrap_or(0)),
                }
            }
        }
        "brigadier:string" => out.put_varint(p["type"].as_i64().unwrap_or(0) as i32),
        "minecraft:entity" | "minecraft:score_holder" => {
            out.push(p["flags"].as_u64().unwrap_or(0) as u8);
        }
        "minecraft:time" => out.put_i32(p["min"].as_i64().unwrap_or(0) as i32),
        "minecraft:resource_or_tag"
        | "minecraft:resource_or_tag_key"
        | "minecraft:resource"
        | "minecraft:resource_key"
        | "minecraft:resource_selector" => out.put_string(p["registry"].as_str().unwrap_or("")),
        _ => {}
    }
}
