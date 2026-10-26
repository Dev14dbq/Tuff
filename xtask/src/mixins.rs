//! `cargo xtask mixins`: which vanilla methods mods patch. Takes the most
//! downloaded open-source mods on Modrinth that have a build for 26.x (where
//! the game is no longer obfuscated, so Mixin targets are Mojang names),
//! downloads that build into `tools/modrinth/`, reads the Mixin annotations
//! from the class files (jar-in-jar included) and ranks vanilla classes and
//! methods by how many mods patch them.
//!
//! Only names leave the jars: the ranking is written to `cache/mixins/`
//! (`methods.tsv`, `classes.tsv`, `ranking.md`).
//!
//! A mod is open source when its Modrinth license is an SPDX id (not
//! `LicenseRef-…`). A Modrinth project counts once per target however many
//! of its mixins hit it; a bundled library (jar-in-jar, by the mod id in
//! `fabric.mod.json` / `neoforge.mods.toml`) counts only for the first
//! project it is found in.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

const GAME_VERSIONS: [&str; 5] = ["26.3", "26.2", "26.1.2", "26.1.1", "26.1"];
const LOADERS: [&str; 3] = ["fabric", "neoforge", "quilt"];
const USER_AGENT: &str = "minecraft-rewrite xtask mixins";

pub fn run(args: &[String]) -> Result<()> {
    let mut top = 500usize;
    let mut show = 100usize;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--top" => top = value()?.parse()?,
            "--show" => show = value()?.parse()?,
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = crate::workspace_root();
    let jars = root.join("tools/modrinth");
    std::fs::create_dir_all(&jars)?;
    let mods = top_mods(top)?;
    eprintln!("mixins: {} open-source mods", mods.len());

    let mut census = Census::default();
    for (i, m) in mods.iter().enumerate() {
        let jar = match download(&jars, m) {
            Ok(Some(jar)) => jar,
            Ok(None) => {
                eprintln!("mixins: {}: no 26.x file", m.slug);
                continue;
            }
            Err(err) => {
                eprintln!("mixins: {}: {err:#}", m.slug);
                continue;
            }
        };
        let bytes = std::fs::read(&jar)?;
        let before = census.targets.len();
        if let Err(err) = census.scan_jar(&bytes, &m.slug, 0) {
            eprintln!("mixins: {}: {err:#}", m.slug);
        }
        census.scanned.insert(m.slug.clone());
        eprintln!(
            "mixins: [{}/{}] {} (+{} targets)",
            i + 1,
            mods.len(),
            m.slug,
            census.targets.len() - before
        );
    }
    census.write(&root.join("cache/mixins"), show)
}

struct Mod {
    slug: String,
    id: String,
}

/// The `top` most downloaded open-source mods with a 26.x build.
fn top_mods(top: usize) -> Result<Vec<Mod>> {
    let versions: Vec<String> = GAME_VERSIONS
        .iter()
        .map(|v| format!("\"versions:{v}\""))
        .collect();
    let facets = format!("[[\"project_type:mod\"],[{}]]", versions.join(","));
    let mut mods = Vec::new();
    let mut offset = 0;
    while mods.len() < top {
        let url = format!(
            "https://api.modrinth.com/v2/search?index=downloads&limit=100&offset={offset}&facets={}",
            encode(&facets)
        );
        let page: serde_json::Value = serde_json::from_slice(&get(&url)?)?;
        let hits = page["hits"].as_array().context("no hits")?;
        if hits.is_empty() {
            break;
        }
        for hit in hits {
            let license = hit["license"].as_str().unwrap_or("");
            if license.is_empty() || license.starts_with("LicenseRef-") {
                continue;
            }
            mods.push(Mod {
                slug: hit["slug"].as_str().unwrap_or("").to_owned(),
                id: hit["project_id"].as_str().unwrap_or("").to_owned(),
            });
            if mods.len() == top {
                break;
            }
        }
        offset += hits.len();
    }
    Ok(mods)
}

/// Downloads the newest 26.x file of `m` (preferring Fabric) unless it is
/// already in `dir`.
fn download(dir: &Path, m: &Mod) -> Result<Option<PathBuf>> {
    let versions: Vec<String> = GAME_VERSIONS.iter().map(|v| format!("\"{v}\"")).collect();
    let url = format!(
        "https://api.modrinth.com/v2/project/{}/version?game_versions={}",
        m.id,
        encode(&format!("[{}]", versions.join(",")))
    );
    let list: serde_json::Value = serde_json::from_slice(&get(&url)?)?;
    let list = list.as_array().context("versions")?;
    let loader_rank = |v: &serde_json::Value| {
        let loaders = v["loaders"].as_array().cloned().unwrap_or_default();
        LOADERS
            .iter()
            .position(|l| loaders.iter().any(|x| x == l))
            .unwrap_or(LOADERS.len())
    };
    // The list is newest first; take the newest build of the best loader.
    let Some(version) = list
        .iter()
        .filter(|v| loader_rank(v) < LOADERS.len())
        .min_by_key(|v| loader_rank(v))
    else {
        return Ok(None);
    };
    let files = version["files"].as_array().context("files")?;
    let Some(file) = files
        .iter()
        .find(|f| f["primary"].as_bool() == Some(true))
        .or_else(|| files.first())
    else {
        return Ok(None);
    };
    let name = file["filename"].as_str().context("filename")?;
    if !name.ends_with(".jar") {
        return Ok(None);
    }
    let path = dir.join(format!("{}--{name}", m.slug));
    if !path.is_file() {
        let bytes = get(file["url"].as_str().context("url")?)?;
        let sha512 = file["hashes"]["sha512"].as_str().unwrap_or("");
        if !sha512.is_empty() && sha512_hex(&bytes)? != sha512 {
            bail!("sha512 mismatch for {name}");
        }
        std::fs::write(&path, bytes)?;
    }
    Ok(Some(path))
}

fn get(url: &str) -> Result<Vec<u8>> {
    let out = std::process::Command::new("curl")
        .args(["-sfL", "--retry", "3", "-A", USER_AGENT, url])
        .output()
        .context("running curl")?;
    if !out.status.success() {
        bail!("GET {url}: curl exit {}", out.status);
    }
    Ok(out.stdout)
}

fn sha512_hex(bytes: &[u8]) -> Result<String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("shasum")
        .args(["-a", "512"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("running shasum")?;
    child.stdin.take().unwrap().write_all(bytes)?;
    let out = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_owned())
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What one mod's mixin does to one vanilla target.
#[derive(Default)]
struct Target {
    /// Modrinth project → injector kinds it uses here.
    mods: BTreeMap<String, BTreeSet<String>>,
    /// Some mod patches it in a client-only mixin config.
    client: bool,
    /// Some mod patches it in a common or server config.
    common: bool,
}

#[derive(Default)]
struct Census {
    /// `net/minecraft/…/Class.method` (or `…/Class` for the class itself).
    targets: HashMap<String, Target>,
    classes: HashMap<String, Target>,
    /// Mod ids whose jars were already scanned (bundled libraries).
    seen_ids: BTreeSet<String>,
    scanned: BTreeSet<String>,
}

impl Census {
    fn scan_jar(&mut self, bytes: &[u8], slug: &str, depth: usize) -> Result<()> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let read = |zip: &mut zip::ZipArchive<_>, name: &str| -> Option<Vec<u8>> {
            let mut entry = zip.by_name(name).ok()?;
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).ok()?;
            Some(buf)
        };
        let names: Vec<String> = (0..zip.len())
            .filter_map(|i| Some(zip.by_index(i).ok()?.name().ok()?.into_owned()))
            .collect();
        let mod_id = read(&mut zip, "fabric.mod.json")
            .or_else(|| read(&mut zip, "quilt.mod.json"))
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|j| {
                j["id"]
                    .as_str()
                    .or_else(|| j["quilt_loader"]["id"].as_str())
                    .map(str::to_owned)
            })
            .or_else(|| {
                let toml = read(&mut zip, "META-INF/neoforge.mods.toml")?;
                let toml = String::from_utf8_lossy(&toml);
                let line = toml.lines().find(|l| l.trim_start().starts_with("modId"))?;
                Some(line.split('"').nth(1)?.to_owned())
            })
            .unwrap_or_else(|| slug.to_owned());
        if !self.seen_ids.insert(mod_id.clone()) {
            return Ok(());
        }
        for name in &names {
            if depth < 3
                && name.starts_with("META-INF/jars/")
                && name.ends_with(".jar")
                && let Some(inner) = read(&mut zip, name)
                && let Err(err) = self.scan_jar(&inner, slug, depth + 1)
            {
                eprintln!("mixins: {slug}: {name}: {err:#}");
            }
        }
        // Mixin configs: top-level JSON with a `package` and mixin lists.
        for name in &names {
            if name.contains('/') || !name.ends_with(".json") {
                continue;
            }
            let Some(config) = read(&mut zip, name)
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            else {
                continue;
            };
            let Some(package) = config["package"].as_str() else {
                continue;
            };
            for (list, client) in [("mixins", false), ("server", false), ("client", true)] {
                for mixin in config[list].as_array().into_iter().flatten() {
                    let Some(mixin) = mixin.as_str() else {
                        continue;
                    };
                    let path = format!("{package}.{mixin}").replace('.', "/") + ".class";
                    let Some(class) = read(&mut zip, &path) else {
                        continue;
                    };
                    match parse_class(&class) {
                        Ok(info) => self.add(slug, &info, client),
                        Err(err) => eprintln!("mixins: {slug}: {path}: {err:#}"),
                    }
                }
            }
        }
        Ok(())
    }

    fn add(&mut self, mod_id: &str, info: &MixinInfo, client: bool) {
        let note = |t: &mut Target, kind: &str| {
            t.mods
                .entry(mod_id.to_owned())
                .or_default()
                .insert(kind.to_owned());
            if client {
                t.client = true;
            } else {
                t.common = true;
            }
        };
        for class in info.targets.iter().filter(|c| vanilla(c)) {
            note(self.classes.entry(class.clone()).or_default(), "mixin");
            for (kind, method) in &info.injections {
                let (owner, name) = match method.split_once(';') {
                    Some((owner, rest)) if owner.starts_with('L') => {
                        (owner[1..].to_owned(), rest.to_owned())
                    }
                    _ => (class.clone(), method.clone()),
                };
                if !vanilla(&owner) {
                    continue;
                }
                let name = name
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('*')
                    .to_owned();
                if name.is_empty() {
                    continue;
                }
                note(
                    self.targets.entry(format!("{owner}.{name}")).or_default(),
                    kind,
                );
            }
        }
    }

    fn write(&self, dir: &Path, show: usize) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let side = |t: &Target| match (t.common, t.client) {
            (true, true) => "both",
            (true, false) => "common",
            _ => "client",
        };
        let kinds = |t: &Target| {
            let mut count: BTreeMap<&str, usize> = BTreeMap::new();
            for kinds in t.mods.values() {
                for k in kinds {
                    *count.entry(k).or_default() += 1;
                }
            }
            count
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let methods = ranked(&self.targets);
        let classes = ranked(&self.classes);
        let tsv = |rows: &[(&String, &Target)]| {
            let mut s = String::from("target\tmods\tside\tkinds\n");
            for (name, t) in rows {
                s += &format!("{name}\t{}\t{}\t{}\n", t.mods.len(), side(t), kinds(t));
            }
            s
        };
        std::fs::write(dir.join("methods.tsv"), tsv(&methods))?;
        std::fs::write(dir.join("classes.tsv"), tsv(&classes))?;
        let short = |name: &str| name.trim_start_matches("net/minecraft/").replace('/', ".");
        let mut md = format!(
            "{} mods scanned ({} mod ids with mixins or bundled), {} vanilla classes and {} methods patched.\n\n",
            self.scanned.len(),
            self.seen_ids.len(),
            classes.len(),
            methods.len()
        );
        md += "| # | method | mods | side | kinds |\n|---|---|---|---|---|\n";
        for (i, (name, t)) in methods.iter().take(show).enumerate() {
            md += &format!(
                "| {} | `{}` | {} | {} | {} |\n",
                i + 1,
                short(name),
                t.mods.len(),
                side(t),
                kinds(t)
            );
        }
        md += "\n| # | class | mods | side |\n|---|---|---|---|\n";
        for (i, (name, t)) in classes.iter().take(show / 2).enumerate() {
            md += &format!(
                "| {} | `{}` | {} | {} |\n",
                i + 1,
                short(name),
                t.mods.len(),
                side(t)
            );
        }
        std::fs::write(dir.join("ranking.md"), &md)?;
        println!("{md}");
        Ok(())
    }
}

/// Most patched first.
fn ranked(map: &HashMap<String, Target>) -> Vec<(&String, &Target)> {
    let mut v: Vec<(&String, &Target)> = map.iter().collect();
    v.sort_by(|a, b| b.1.mods.len().cmp(&a.1.mods.len()).then(a.0.cmp(b.0)));
    v
}

fn vanilla(class: &str) -> bool {
    class.starts_with("net/minecraft/") || class.starts_with("com/mojang/")
}

/// The targets of one `@Mixin` class and what its methods inject into.
#[derive(Default)]
struct MixinInfo {
    targets: Vec<String>,
    /// (injector kind, method selector).
    injections: Vec<(String, String)>,
}

/// Reads the `@Mixin` targets and the injector annotations of a class file.
fn parse_class(bytes: &[u8]) -> Result<MixinInfo> {
    let mut r = Reader { b: bytes, at: 0 };
    if r.u4()? != 0xCAFE_BABE {
        bail!("not a class file");
    }
    r.skip(4)?;
    let count = r.u2()? as usize;
    let mut pool: Vec<Option<String>> = vec![None; count];
    let mut i = 1;
    while i < count {
        match r.u1()? {
            1 => {
                let len = r.u2()? as usize;
                pool[i] = Some(String::from_utf8_lossy(r.take(len)?).into_owned());
            }
            7 | 8 | 16 | 19 | 20 => r.skip(2)?,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => r.skip(4)?,
            5 | 6 => {
                r.skip(8)?;
                i += 1;
            }
            15 => r.skip(3)?,
            tag => bail!("constant pool tag {tag}"),
        }
        i += 1;
    }
    let utf8 =
        |pool: &[Option<String>], i: usize| pool.get(i).cloned().flatten().unwrap_or_default();
    let mut info = MixinInfo::default();
    r.skip(6)?;
    let interfaces = r.u2()? as usize;
    r.skip(interfaces * 2)?;
    // Fields: skip.
    for _ in 0..r.u2()? {
        r.skip(6)?;
        skip_attributes(&mut r)?;
    }
    for _ in 0..r.u2()? {
        r.skip(2)?;
        let name = utf8(&pool, r.u2()? as usize);
        r.skip(2)?;
        for _ in 0..r.u2()? {
            let attr = utf8(&pool, r.u2()? as usize);
            let len = r.u4()? as usize;
            let body = r.take(len)?;
            if plain_annotations(&attr) {
                for a in annotations(body, &pool)? {
                    method_annotation(&a, &name, &mut info);
                }
            }
        }
    }
    for _ in 0..r.u2()? {
        let attr = utf8(&pool, r.u2()? as usize);
        let len = r.u4()? as usize;
        let body = r.take(len)?;
        if plain_annotations(&attr) {
            for a in annotations(body, &pool)? {
                if a.kind == "Lorg/spongepowered/asm/mixin/Mixin;" {
                    for v in a.get("value").into_iter().chain(a.get("targets")) {
                        for s in v.strings() {
                            let s = s.trim_start_matches('L').trim_end_matches(';');
                            info.targets.push(s.replace('.', "/"));
                        }
                    }
                }
            }
        }
    }
    Ok(info)
}

/// `Runtime(In)VisibleAnnotations` (not the parameter or type ones, which
/// have another layout).
fn plain_annotations(attr: &str) -> bool {
    matches!(
        attr,
        "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations"
    )
}

fn method_annotation(a: &Annotation, method: &str, info: &mut MixinInfo) {
    let kind = a.kind.trim_start_matches('L').trim_end_matches(';');
    let short = kind.rsplit('/').next().unwrap_or(kind);
    if kind == "org/spongepowered/asm/mixin/Overwrite" {
        info.injections
            .push(("Overwrite".into(), method.to_owned()));
        return;
    }
    if kind.starts_with("org/spongepowered/asm/mixin/gen/") {
        // @Accessor / @Invoker: access, not a patch.
        return;
    }
    if !(kind.starts_with("org/spongepowered/asm/mixin/injection/")
        || kind.starts_with("com/llamalad7/mixinextras/"))
    {
        return;
    }
    if let Some(v) = a.get("method") {
        for s in v.strings() {
            info.injections.push((short.to_owned(), s));
        }
    }
}

struct Annotation {
    kind: String,
    elements: Vec<(String, Value)>,
}

impl Annotation {
    fn get(&self, name: &str) -> Option<&Value> {
        self.elements
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }
}

enum Value {
    Str(String),
    Other,
    Array(Vec<Value>),
}

impl Value {
    fn strings(&self) -> Vec<String> {
        match self {
            Value::Str(s) => vec![s.clone()],
            Value::Array(v) => v.iter().flat_map(Value::strings).collect(),
            Value::Other => Vec::new(),
        }
    }
}

fn annotations(body: &[u8], pool: &[Option<String>]) -> Result<Vec<Annotation>> {
    let mut r = Reader { b: body, at: 0 };
    let n = r.u2()?;
    (0..n).map(|_| annotation(&mut r, pool)).collect()
}

fn annotation(r: &mut Reader, pool: &[Option<String>]) -> Result<Annotation> {
    let utf8 = |i: u16| pool.get(i as usize).cloned().flatten().unwrap_or_default();
    let kind = utf8(r.u2()?);
    let mut elements = Vec::new();
    for _ in 0..r.u2()? {
        let name = utf8(r.u2()?);
        elements.push((name, value(r, pool)?));
    }
    Ok(Annotation { kind, elements })
}

fn value(r: &mut Reader, pool: &[Option<String>]) -> Result<Value> {
    let utf8 = |i: u16| pool.get(i as usize).cloned().flatten().unwrap_or_default();
    Ok(match r.u1()? {
        b's' | b'c' => Value::Str(utf8(r.u2()?)),
        b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z' => {
            r.skip(2)?;
            Value::Other
        }
        b'e' => {
            r.skip(4)?;
            Value::Other
        }
        b'@' => {
            annotation(r, pool)?;
            Value::Other
        }
        b'[' => {
            let n = r.u2()?;
            Value::Array((0..n).map(|_| value(r, pool)).collect::<Result<_>>()?)
        }
        tag => bail!("element value tag {tag}"),
    })
}

fn skip_attributes(r: &mut Reader) -> Result<()> {
    for _ in 0..r.u2()? {
        r.skip(2)?;
        let len = r.u4()? as usize;
        r.skip(len)?;
    }
    Ok(())
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .b
            .get(self.at..self.at + n)
            .context("truncated class file")?;
        self.at += n;
        Ok(s)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
    fn u1(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u2(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u4(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
}
