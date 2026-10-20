//! Data packs in the world's `datapacks` folder and what the server takes
//! from them: functions (`data/<ns>/function/**.mcfunction`) and function
//! tags (`data/<ns>/tags/function/**.json`), plus functions waiting on
//! `/schedule`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mc_command::dispatch::{self, Context};
use mc_nbt::Tag;
use tracing::warn;

use crate::commands::Commands;

/// Vanilla's feature packs, which need flags the world does not have.
pub const FEATURE_PACKS: [&str; 3] = [
    "minecart_improvements",
    "redstone_experiments",
    "trade_rebalance",
];

/// The permission level function lines are checked and run at.
pub const FUNCTION_LEVEL: u8 = 2;

#[derive(Clone, Debug)]
pub struct Pack {
    /// `vanilla` or `file/<folder>`.
    pub id: String,
    /// The folder name (shown with the description).
    pub title: String,
    pub description: Tag,
    pub dir: Option<PathBuf>,
}

/// One line of a function.
#[derive(Clone, Debug)]
pub enum Line {
    Parsed(Vec<Context>),
    /// A `$` line: text with `$(name)` holes, parsed once filled in.
    Macro(Vec<Part>),
}

#[derive(Clone, Debug)]
pub enum Part {
    Text(String),
    Var(String),
}

#[derive(Debug)]
pub struct Function {
    pub id: String,
    pub lines: Vec<Line>,
}

impl Function {
    pub fn is_macro(&self) -> bool {
        self.lines.iter().any(|l| matches!(l, Line::Macro(_)))
    }

    /// The macro arguments in the order they first appear.
    pub fn vars(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for l in &self.lines {
            if let Line::Macro(parts) = l {
                for p in parts {
                    if let Part::Var(v) = p
                        && !out.contains(&v.as_str())
                    {
                        out.push(v);
                    }
                }
            }
        }
        out
    }
}

/// A tag entry: an id or (`#`) another tag, and whether it must exist.
#[derive(Clone, Debug)]
struct Entry {
    id: String,
    required: bool,
}

#[derive(Clone, Debug)]
pub struct Scheduled {
    /// A function id, or a tag id starting with `#`.
    pub id: String,
    pub at: i64,
}

#[derive(Debug, Default)]
pub struct Packs {
    /// Every pack found, enabled or not.
    pub all: Vec<Pack>,
    /// Enabled pack ids, lowest priority first.
    pub enabled: Vec<String>,
    pub functions: BTreeMap<String, Arc<Function>>,
    tags: BTreeMap<String, Vec<Entry>>,
    pub scheduled: Vec<Scheduled>,
    /// `#minecraft:load` still has to run.
    pub load_pending: bool,
    folder: Option<PathBuf>,
}

fn vanilla() -> Pack {
    Pack {
        id: "vanilla".into(),
        title: "vanilla".into(),
        description: Tag::String(String::new()),
        dir: None,
    }
}

impl Packs {
    pub fn load(world: Option<&Path>, commands: &Commands) -> Self {
        let mut p = Packs {
            folder: world.map(|w| w.join("datapacks")),
            ..Packs::default()
        };
        p.scan();
        p.enabled = p.all.iter().map(|k| k.id.clone()).collect();
        p.reload(commands);
        p
    }

    /// Finds the packs in the folder again (new ones stay disabled).
    pub fn scan(&mut self) {
        let mut all = vec![vanilla()];
        if let Some(dir) = &self.folder
            && let Ok(read) = std::fs::read_dir(dir)
        {
            let mut found: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
            found.sort();
            for path in found {
                let Some(meta) = std::fs::read_to_string(path.join("pack.mcmeta"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                else {
                    continue;
                };
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let description = match &meta["pack"]["description"] {
                    serde_json::Value::String(s) => Tag::String(s.clone()),
                    serde_json::Value::Null => Tag::String(String::new()),
                    other => Tag::String(other.to_string()),
                };
                all.push(Pack {
                    id: format!("file/{name}"),
                    title: name,
                    description,
                    dir: Some(path),
                });
            }
        }
        self.all = all;
    }

    pub fn pack(&self, id: &str) -> Option<&Pack> {
        self.all.iter().find(|p| p.id == id)
    }

    /// Reads functions and tags from the enabled packs again.
    pub fn reload(&mut self, commands: &Commands) {
        self.scan();
        self.enabled
            .retain(|id| self.all.iter().any(|p| &p.id == id));
        let mut sources: HashMap<String, String> = HashMap::new();
        let mut tags: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        for id in &self.enabled {
            let Some(dir) = self.pack(id).and_then(|p| p.dir.clone()) else {
                continue;
            };
            let Ok(namespaces) = std::fs::read_dir(dir.join("data")) else {
                continue;
            };
            for ns in namespaces.flatten() {
                let ns_name = ns.file_name().to_string_lossy().into_owned();
                for (path, rel) in files(&ns.path().join("function"), "mcfunction") {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        sources.insert(format!("{ns_name}:{rel}"), text);
                    }
                }
                for (path, rel) in files(&ns.path().join("tags/function"), "json") {
                    let Some(json) = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    else {
                        continue;
                    };
                    let entries = tags.entry(format!("{ns_name}:{rel}")).or_default();
                    if json["replace"].as_bool() == Some(true) {
                        entries.clear();
                    }
                    for v in json["values"].as_array().into_iter().flatten() {
                        let (id, required) = match v {
                            serde_json::Value::String(s) => (s.clone(), true),
                            o => (
                                o["id"].as_str().unwrap_or_default().to_owned(),
                                o["required"].as_bool().unwrap_or(true),
                            ),
                        };
                        entries.push(Entry { id, required });
                    }
                }
            }
        }
        let mut functions = BTreeMap::new();
        for (id, text) in sources {
            match compile(&id, &text, commands) {
                Ok(f) => {
                    functions.insert(id, Arc::new(f));
                }
                Err(e) => warn!("Failed to load function {id}: {e}"),
            }
        }
        self.functions = functions;
        self.tags = tags;
        self.load_pending = true;
    }

    pub fn function(&self, id: &str) -> Option<Arc<Function>> {
        self.functions.get(id).cloned()
    }

    /// The functions of a tag, in order and without repeats; `None` if
    /// there is no such tag.
    pub fn tag(&self, id: &str) -> Option<Vec<Arc<Function>>> {
        self.tags.get(id)?;
        let mut out: Vec<Arc<Function>> = Vec::new();
        let mut seen = Vec::new();
        self.collect(id, &mut out, &mut seen);
        Some(out)
    }

    fn collect(&self, id: &str, out: &mut Vec<Arc<Function>>, seen: &mut Vec<String>) {
        if seen.iter().any(|s| s == id) {
            return;
        }
        seen.push(id.to_owned());
        for e in self.tags.get(id).into_iter().flatten() {
            if let Some(tag) = e.id.strip_prefix('#') {
                self.collect(&mc_command::args::full_id(tag), out, seen);
            } else if let Some(f) = self.function(&mc_command::args::full_id(&e.id)) {
                if !out.iter().any(|o| o.id == f.id) {
                    out.push(f);
                }
            } else if e.required {
                warn!("function tag {id} names a missing function {}", e.id);
            }
        }
    }

    pub fn tag_ids(&self) -> impl Iterator<Item = &String> {
        self.tags.keys()
    }

    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }
}

/// Files under `dir` with extension `ext`, with their path relative to
/// `dir` without the extension (`sub/deep`).
fn files(dir: &Path, ext: &str) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == ext)
                && let Ok(rel) = p.with_extension("").strip_prefix(dir)
            {
                out.push((p.clone(), rel.to_string_lossy().replace('\\', "/")));
            }
        }
    }
    out
}

/// The lines of a function file: comments and blank lines dropped, lines
/// ending in `\` joined with the next.
fn source_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        let mut line = l.trim().to_owned();
        while line.ends_with('\\') {
            line.pop();
            match lines.next() {
                Some(next) => line.push_str(next.trim()),
                None => break,
            }
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push(line);
    }
    out
}

/// Parses one command line of a function, as vanilla's error text.
pub fn parse_line(line: &str, commands: &Commands) -> Result<Vec<Context>, String> {
    let parse = dispatch::parse(&commands.tree, line, FUNCTION_LEVEL, &commands.env);
    if let Some(e) = parse.error(line) {
        return Err(e.console(&commands.lang));
    }
    if parse.chain.last().is_none_or(|c| c.command.is_none()) {
        return Err(mc_command::error::builtin::unknown_command().console(&commands.lang));
    }
    Ok(parse.chain)
}

fn compile(id: &str, text: &str, commands: &Commands) -> Result<Function, String> {
    let mut lines = Vec::new();
    for (n, line) in source_lines(text).into_iter().enumerate() {
        if let Some(m) = line.strip_prefix('$') {
            lines.push(Line::Macro(macro_parts(m)));
            continue;
        }
        let chain = parse_line(&line, commands).map_err(|e| format!("line {}: {e}", n + 1))?;
        lines.push(Line::Parsed(chain));
    }
    Ok(Function {
        id: id.to_owned(),
        lines,
    })
}

fn macro_parts(line: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("$(") {
        let Some(len) = rest[start + 2..].find(')') else {
            break;
        };
        if start > 0 {
            out.push(Part::Text(rest[..start].to_owned()));
        }
        out.push(Part::Var(rest[start + 2..start + 2 + len].to_owned()));
        rest = &rest[start + 3 + len..];
    }
    if !rest.is_empty() {
        out.push(Part::Text(rest.to_owned()));
    }
    out
}

/// A macro argument as it goes into the line: strings as they are,
/// numbers without their suffix, the rest as SNBT.
pub fn macro_value(t: &Tag) -> String {
    match t {
        Tag::String(s) => s.clone(),
        Tag::Byte(v) => v.to_string(),
        Tag::Short(v) => v.to_string(),
        Tag::Int(v) => v.to_string(),
        Tag::Long(v) => v.to_string(),
        Tag::Float(v) => mc_command::text::java_float(*v),
        Tag::Double(v) => mc_command::text::java_double(*v),
        other => mc_command::particle::snbt(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_join_and_skip_comments() {
        let l = source_lines("# c\n\ntellraw @s \\\n  \"x\"\n  say hi \n");
        assert_eq!(l, vec!["tellraw @s \"x\"", "say hi"]);
    }

    #[test]
    fn macro_holes() {
        let p = macro_parts("tellraw @s \"$(a) and $(b)\"");
        assert_eq!(p.len(), 5);
        assert!(matches!(&p[1], Part::Var(v) if v == "a"));
    }
}
