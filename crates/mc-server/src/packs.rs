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
use crate::packconfig::{self, Candidate, InitialPacks, LevelPacks, VANILLA_FLAG};

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
    /// Feature flags the pack needs (`features.enabled` in `pack.mcmeta`).
    pub features: Vec<String>,
    /// Overlay folders of the pack that apply to this game version.
    pub overlays: Vec<String>,
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
    /// Feature flags this world has on: `minecraft:vanilla`, plus any
    /// stored with the world. Empty until [`Packs::load`].
    world_flags: Vec<String>,
    /// Built-in feature packs the world has selected (they carry no data
    /// here, but level.dat keeps them).
    feature_selected: Vec<String>,
    /// Flags level.dat keeps whatever the packs ask for: those it was
    /// saved with.
    forced: Vec<String>,
}

/// Why `/datapack enable` or `disable` is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToggleError {
    Unknown,
    /// Already enabled (when enabling) or already disabled (when disabling).
    State,
    /// Flags the world lacks, joined for the message.
    NoFlags(String),
}

fn vanilla() -> Pack {
    Pack {
        id: "vanilla".into(),
        title: "vanilla".into(),
        description: Tag::String(String::new()),
        dir: None,
        features: Vec::new(),
        overlays: Vec::new(),
    }
}

impl Packs {
    pub fn load(world: Option<&Path>, commands: &Commands) -> Self {
        Self::load_with(world, commands, None)
    }

    /// Opens the packs of `world`. A world with a level.dat runs the packs it
    /// stored; one without it is new and starts from `initial`, if given.
    pub fn load_with(
        world: Option<&Path>,
        commands: &Commands,
        initial: Option<&InitialPacks>,
    ) -> Self {
        let mut p = Packs {
            folder: world.map(|w| w.join("datapacks")),
            world_flags: world_flags(world),
            ..Packs::default()
        };
        p.scan();
        let candidates = p.candidates();
        let stored = world.and_then(stored_config);
        let selection = match (&stored, initial, world) {
            (None, Some(init), Some(_)) => {
                let s =
                    packconfig::configure(&candidates, &init.enabled, &init.disabled, None, &[]);
                for flag in &s.features {
                    push_flag(&mut p.world_flags, flag);
                }
                s
            }
            _ => {
                let default = InitialPacks::default();
                let (enabled, disabled, flags) = stored.unwrap_or((
                    default.enabled,
                    default.disabled,
                    vec![VANILLA_FLAG.to_owned()],
                ));
                p.forced = flags;
                packconfig::configure(
                    &candidates,
                    &enabled,
                    &disabled,
                    Some(&p.world_flags),
                    &p.forced,
                )
            }
        };
        p.feature_selected = selection
            .enabled
            .iter()
            .filter(|id| FEATURE_PACKS.contains(&id.as_str()))
            .cloned()
            .collect();
        p.enabled = selection
            .enabled
            .into_iter()
            .filter(|id| p.all.iter().any(|k| &k.id == id))
            .collect();
        p.reload(commands);
        p
    }

    /// Every pack the world could run, by id: vanilla, the built-in feature
    /// packs and those in the world folder.
    fn candidates(&self) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = self
            .all
            .iter()
            .map(|k| Candidate {
                id: k.id.clone(),
                flags: if k.id == "vanilla" {
                    vec![VANILLA_FLAG.to_owned()]
                } else {
                    k.features.clone()
                },
                auto: k.id != "vanilla",
            })
            .collect();
        for name in FEATURE_PACKS {
            out.push(Candidate {
                id: name.to_owned(),
                flags: vec![format!("minecraft:{name}")],
                auto: false,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// What level.dat should say about the packs now.
    pub fn level_packs(&self) -> LevelPacks {
        let mut enabled = self.enabled.clone();
        let at = enabled
            .iter()
            .position(|i| i == "vanilla")
            .map_or(0, |i| i + 1);
        for (k, name) in self.feature_selected.iter().enumerate() {
            enabled.insert((at + k).min(enabled.len()), name.clone());
        }
        let candidates = self.candidates();
        let mut features = self.forced.clone();
        for id in &enabled {
            if let Some(c) = candidates.iter().find(|c| &c.id == id) {
                for f in &c.flags {
                    if !features.contains(f) {
                        features.push(f.clone());
                    }
                }
            }
        }
        features.sort();
        LevelPacks {
            disabled: candidates
                .iter()
                .filter(|c| !enabled.contains(&c.id))
                .map(|c| c.id.clone())
                .collect(),
            enabled,
            features,
        }
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
                let parsed = match mc_data::packmeta::parse(&meta) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        warn!("Failed to read pack file/{name} metadata: {e}");
                        continue;
                    }
                };
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
                    features: parsed.features,
                    overlays: parsed.overlays,
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
        self.refuse_missing_flags();
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

    /// A pack whose `pack.mcmeta` asks for a flag this world does not have
    /// stays out of the enabled set. Optional packs are skipped; a pack
    /// marked required (or one of vanilla's feature packs, which are
    /// already present but disabled) is left disabled as well, so startup
    /// continues.
    fn refuse_missing_flags(&mut self) {
        if self.world_flags.is_empty() {
            return;
        }
        let mut deny = Vec::new();
        for id in &self.enabled {
            let Some(pack) = self.pack(id) else {
                continue;
            };
            let missing = self.missing_flags(pack);
            if missing.is_empty() {
                continue;
            }
            warn!(
                "Pack {id} requires features {} that are not enabled for this world, disabling pack.",
                missing.join(", ")
            );
            deny.push(id.clone());
        }
        self.enabled.retain(|id| !deny.iter().any(|d| d == id));
    }

    /// The pack `/datapack enable` or `disable` is about, checked in
    /// vanilla's order: unknown, then already in the wanted state, then (only
    /// when enabling) missing feature flags.
    pub fn check_toggle(&self, name: &str, enabling: bool) -> Result<&Pack, ToggleError> {
        let enabled = self.enabled.iter().any(|id| id == name);
        let Some(pack) = self.pack(name) else {
            // Vanilla's built-in feature packs exist but are off here.
            if FEATURE_PACKS.contains(&name) {
                return Err(if enabling {
                    ToggleError::NoFlags(format!("minecraft:{name}"))
                } else {
                    ToggleError::State
                });
            }
            return Err(ToggleError::Unknown);
        };
        if enabling == enabled {
            return Err(ToggleError::State);
        }
        let missing = self.missing_flags(pack);
        if enabling && !missing.is_empty() {
            return Err(ToggleError::NoFlags(missing.join(", ")));
        }
        Ok(pack)
    }

    /// Flags the pack needs that this world does not have, as vanilla
    /// prints them.
    pub fn missing_flags(&self, pack: &Pack) -> Vec<String> {
        pack.features
            .iter()
            .filter(|f| !self.world_flags.iter().any(|w| w == *f))
            .cloned()
            .collect()
    }

    /// The flags a connecting client is told: the world's own, plus what
    /// the enabled packs ask for.
    pub fn enabled_features(&self) -> Vec<String> {
        let mut flags = self.world_flags.clone();
        if flags.is_empty() {
            flags.push("minecraft:vanilla".to_owned());
        }
        for pack in self.enabled.iter().filter_map(|id| self.pack(id)) {
            for f in &pack.features {
                if !flags.contains(f) {
                    flags.push(f.clone());
                }
            }
        }
        flags
    }

    /// `data` folders of the enabled packs, lowest priority first. A pack's
    /// overlays that apply to this version come right after the pack, later
    /// overlays winning over earlier ones.
    pub fn layer_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for pack in self.enabled.iter().filter_map(|id| self.pack(id)) {
            let Some(dir) = pack.dir.as_ref() else {
                continue;
            };
            let folders = std::iter::once(dir.clone())
                .chain(pack.overlays.iter().map(|overlay| dir.join(overlay)));
            for folder in folders {
                let data = folder.join("data");
                if data.is_dir() {
                    roots.push(data);
                }
            }
        }
        roots
    }
}

/// level.dat's choice of packs: enabled ids, disabled ids and the flags the
/// world was saved with (`minecraft:vanilla` when it names none). `None`
/// without a readable level.dat.
fn stored_config(world: &Path) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let root = crate::world::read_nbt_gz(&world.join("level.dat")).ok()?;
    let Some(Tag::Compound(data)) = root.get("Data") else {
        return None;
    };
    let strings = |tag: Option<&Tag>| -> Option<Vec<String>> {
        match tag {
            Some(Tag::List(list)) => Some(
                list.iter()
                    .filter_map(|t| match t {
                        Tag::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        }
    };
    let (mut enabled, mut disabled) = (vec!["vanilla".to_owned()], Vec::new());
    if let Some(Tag::Compound(packs)) = data.get("DataPacks") {
        enabled = strings(packs.get("Enabled")).unwrap_or(enabled);
        disabled = strings(packs.get("Disabled")).unwrap_or(disabled);
    }
    let flags = strings(data.get("enabled_features"))
        .map(|l| l.iter().map(|f| flag_id(f)).collect())
        .unwrap_or_else(|| vec![VANILLA_FLAG.to_owned()]);
    Some((enabled, disabled, flags))
}

/// `minecraft:vanilla`, plus flags recorded on the world.
fn world_flags(world: Option<&Path>) -> Vec<String> {
    let mut flags = vec!["minecraft:vanilla".to_owned()];
    let Some(world) = world else {
        return flags;
    };
    let Ok(root) = crate::world::read_nbt_gz(&world.join("level.dat")) else {
        return flags;
    };
    let Some(Tag::Compound(data)) = root.get("Data") else {
        return flags;
    };
    if let Some(Tag::List(list)) = data.get("enabled_features") {
        for tag in list {
            if let Tag::String(s) = tag {
                push_flag(&mut flags, s);
            }
        }
    }
    if let Some(Tag::Compound(packs)) = data.get("DataPacks")
        && let Some(Tag::List(enabled)) = packs.get("Enabled")
    {
        for tag in enabled {
            let Tag::String(id) = tag else {
                continue;
            };
            let name = id.rsplit(['/', ':']).next().unwrap_or(id.as_str());
            if name == "vanilla" || FEATURE_PACKS.contains(&name) {
                push_flag(&mut flags, name);
            }
        }
    }
    flags
}

fn flag_id(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("minecraft:{raw}")
    }
}

fn push_flag(out: &mut Vec<String>, raw: &str) {
    let id = flag_id(raw);
    if !id.is_empty() && !out.iter().any(|f| f == &id) {
        out.push(id);
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
    fn enabled_packs_become_layers_in_order() {
        let tmp = std::env::temp_dir().join(format!("mc-pack-layers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let a = tmp.join("a/data");
        let b = tmp.join("b/data");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let packs = Packs {
            all: vec![
                Pack {
                    id: "file/a".into(),
                    title: "a".into(),
                    description: Tag::String(String::new()),
                    dir: Some(tmp.join("a")),
                    features: Vec::new(),
                    overlays: Vec::new(),
                },
                Pack {
                    id: "file/b".into(),
                    title: "b".into(),
                    description: Tag::String(String::new()),
                    dir: Some(tmp.join("b")),
                    features: Vec::new(),
                    overlays: Vec::new(),
                },
            ],
            enabled: vec!["file/b".into(), "file/a".into()],
            ..Packs::default()
        };
        assert_eq!(packs.layer_roots(), vec![b, a]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn overlays_for_this_version_are_layered_above_the_pack() {
        let tmp = std::env::temp_dir().join(format!("mc-pack-overlay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pack = tmp.join("datapacks/over");
        for d in ["data", "new/data", "old/data"] {
            std::fs::create_dir_all(pack.join(d)).unwrap();
        }
        std::fs::write(
            pack.join("pack.mcmeta"),
            r#"{"pack":{"description":"o","min_format":1,"max_format":200},"overlays":{"entries":[{"directory":"old","min_format":122,"max_format":130},{"directory":"new","min_format":121,"max_format":121}]}}"#,
        )
        .unwrap();
        std::fs::write(tmp.join("datapacks/bad.mcmeta"), "").unwrap();
        let broken = tmp.join("datapacks/broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(
            broken.join("pack.mcmeta"),
            r#"{"pack":{},"features":{"enabled":["minecraft:no_such_flag"]}}"#,
        )
        .unwrap();
        let commands = crate::commands::Commands::load(&tmp.join("no-data"));
        let packs = Packs::load(Some(&tmp), &commands);
        assert_eq!(
            packs.layer_roots(),
            vec![pack.join("data"), pack.join("new/data")]
        );
        assert!(
            packs.pack("file/broken").is_none(),
            "unknown flag: unreadable"
        );
        assert_eq!(packs.enabled_features(), vec!["minecraft:vanilla"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn toggling_checks_state_before_flags() {
        let tmp = std::env::temp_dir().join(format!("mc-pack-toggle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pack = tmp.join("datapacks/rebalance");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("pack.mcmeta"),
            r#"{"pack":{"description":"r"},"features":{"enabled":["minecraft:trade_rebalance"]}}"#,
        )
        .unwrap();
        let commands = crate::commands::Commands::load(&tmp.join("no-data"));
        let packs = Packs::load(Some(&tmp), &commands);
        // Not enabled and missing a flag: disabling is "already disabled".
        assert_eq!(
            packs.check_toggle("file/rebalance", false).err(),
            Some(ToggleError::State)
        );
        assert_eq!(
            packs.check_toggle("file/rebalance", true).err(),
            Some(ToggleError::NoFlags("minecraft:trade_rebalance".into()))
        );
        assert_eq!(
            packs.check_toggle("vanilla", true).err(),
            Some(ToggleError::State)
        );
        assert!(packs.check_toggle("vanilla", false).is_ok());
        assert_eq!(
            packs.check_toggle("nope", false).err(),
            Some(ToggleError::Unknown)
        );
        assert_eq!(
            packs.check_toggle("trade_rebalance", false).err(),
            Some(ToggleError::State)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

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

    #[test]
    fn pack_requiring_trade_rebalance_is_not_enabled() {
        let tmp = std::env::temp_dir().join(format!("mc-pack-flags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pack = tmp.join("datapacks/rebalance");
        let plain = tmp.join("datapacks/plain");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(
            pack.join("pack.mcmeta"),
            r#"{"pack":{"description":"rebalance","min_format":1,"max_format":100},"features":{"enabled":["minecraft:trade_rebalance"]}}"#,
        )
        .unwrap();
        std::fs::write(
            plain.join("pack.mcmeta"),
            r#"{"pack":{"description":"plain","min_format":1,"max_format":100}}"#,
        )
        .unwrap();
        let commands = crate::commands::Commands::load(&tmp.join("no-data"));
        let packs = Packs::load(Some(&tmp), &commands);
        assert!(
            packs.all.iter().any(|p| p.id == "file/rebalance"),
            "the pack is found"
        );
        assert!(
            !packs.enabled.iter().any(|id| id == "file/rebalance"),
            "a world with only minecraft:vanilla does not enable it"
        );
        assert!(packs.enabled.iter().any(|id| id == "file/plain"));
        assert!(packs.enabled.iter().any(|id| id == "vanilla"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_new_world_starts_from_the_initial_packs_and_level_dat_follows_them() {
        let tmp = std::env::temp_dir().join(format!("mc-pack-initial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pack = tmp.join("datapacks/rebalance");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(
            pack.join("pack.mcmeta"),
            r#"{"pack":{"description":"r"},"features":{"enabled":["minecraft:trade_rebalance"]}}"#,
        )
        .unwrap();
        let commands = crate::commands::Commands::load(&tmp.join("no-data"));
        // A new world may use any flag, so the pack is found and its flag kept.
        let initial = InitialPacks::default();
        let packs = Packs::load_with(Some(&tmp), &commands, Some(&initial));
        assert!(packs.enabled.iter().any(|id| id == "file/rebalance"));
        let level = packs.level_packs();
        assert_eq!(level.enabled, ["vanilla", "file/rebalance"]);
        assert_eq!(
            level.disabled,
            [
                "minecart_improvements",
                "redstone_experiments",
                "trade_rebalance"
            ]
        );
        assert_eq!(
            level.features,
            ["minecraft:trade_rebalance", "minecraft:vanilla"]
        );
        // The same world told to leave the pack out.
        let initial = InitialPacks::from_properties("vanilla", "file/rebalance");
        let packs = Packs::load_with(Some(&tmp), &commands, Some(&initial));
        let level = packs.level_packs();
        assert_eq!(level.enabled, ["vanilla"]);
        assert!(level.disabled.contains(&"file/rebalance".to_owned()));
        assert_eq!(level.features, ["minecraft:vanilla"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
