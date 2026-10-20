//! `cargo xtask similarity`: the clean-room guard. Compares our Rust sources
//! with the decompiled vanilla sources in `reference/<version>/server-src`
//! by winnowing fingerprints of normalised token n-grams and fails when a
//! pair of files shares too much.
//!
//! Tokens are made language-agnostic so Rust can be compared with Java:
//! comments, punctuation and keywords of both languages are dropped,
//! identifiers are lowercased without underscores (`getBlockState` and
//! `get_block_state` are one token), numbers are written in one canonical
//! form (`0x10`, `16L`, `16_i64` → `16`, `1.0F` → `1`) and every string or
//! char literal becomes one placeholder (they are mostly protocol and data
//! names, which we may share). Type names (capitalised words) are dropped
//! as well: the languages put them in different places. What is left is the sequence of names and
//! constants, which copied code keeps however it is rewritten.
//!
//! Only file paths, line ranges and scores are printed, never vanilla text:
//! the writers of our code must not see it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Tokens per n-gram.
const K: usize = 12;
/// Winnowing window: every shared run of at least `K + W - 1` tokens is found.
const W: usize = 8;
/// A fingerprint found in more vanilla files than this is boilerplate.
///
/// Tuning (2026-10-20): a 40-line vanilla piece respelled as Rust shares a
/// median of 11 fingerprints with its file (`--self-test`: p25 9, p90 19);
/// our code at most 5 (the public gradient table of improved noise). The
/// default `--fail` of 12 sits between.
const COMMON: usize = 4;

const USAGE: &str = "similarity [--reference <dir>] [--fail <n>] [--top <n>] [--self-test]";

pub fn run(args: &[String]) -> Result<()> {
    let root = crate::workspace_root();
    let mut reference = std::env::var_os("MC_REFERENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("reference/26.3/server-src"));
    let mut fail = 12usize;
    let mut top = 15usize;
    let mut self_test = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .with_context(|| format!("{a} needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--reference" => reference = PathBuf::from(value()?),
            "--fail" => fail = value()?.parse()?,
            "--top" => top = value()?.parse()?,
            "--self-test" => self_test = true,
            other => bail!("unknown option `{other}`\n{USAGE}"),
        }
    }
    if !reference.is_dir() {
        println!(
            "similarity: {} not found, check skipped",
            reference.display()
        );
        return Ok(());
    }

    let mut ours = Vec::new();
    for dir in ["crates", "xtask"] {
        collect(&root.join(dir), "rs", &mut ours)?;
    }
    let mut vanilla = Vec::new();
    collect(&reference, "java", &mut vanilla)?;
    ensure_some(&vanilla, &reference)?;

    let vanilla_prints = fingerprint_all(&vanilla);
    // Fingerprint → vanilla files (and a line in each) holding it.
    let mut index: HashMap<u64, Vec<(u32, u32)>> = HashMap::new();
    for (file, prints) in vanilla_prints.iter().enumerate() {
        let mut seen = HashSet::new();
        for &(hash, line) in prints {
            if seen.insert(hash) {
                index.entry(hash).or_default().push((file as u32, line));
            }
        }
    }
    index.retain(|_, files| files.len() <= COMMON);
    if self_test {
        return run_self_test(&vanilla, &index);
    }

    let our_prints = fingerprint_all(&ours);
    let mut pairs = Vec::new();
    for (file, prints) in our_prints.iter().enumerate() {
        let distinct: HashSet<u64> = prints.iter().map(|p| p.0).collect();
        // Vanilla file → (our lines, vanilla lines, shared fingerprints).
        let mut hits: HashMap<u32, Hit> = HashMap::new();
        for &(hash, line) in prints {
            for &(v, vline) in index.get(&hash).into_iter().flatten() {
                let hit = hits.entry(v).or_default();
                hit.0.push(line);
                hit.1.push(vline);
                hit.2.insert(hash);
            }
        }
        for (v, (lines, vlines, shared)) in hits {
            pairs.push(Pair {
                ours: file,
                vanilla: v as usize,
                shared: shared.len(),
                total: distinct.len(),
                lines,
                vlines,
            });
        }
    }
    pairs.sort_by_key(|p| std::cmp::Reverse(p.shared));

    println!(
        "similarity: {} Rust files vs {} vanilla files (k={K}, w={W}); top pairs:",
        ours.len(),
        vanilla.len()
    );
    let rel = |p: &Path, base: &Path| p.strip_prefix(base).unwrap_or(p).display().to_string();
    let mut failed = 0;
    for (n, pair) in pairs.iter().enumerate() {
        let over = pair.shared >= fail;
        failed += usize::from(over);
        if n >= top && !over {
            continue;
        }
        println!(
            "{:>4} shared ({:>5.1}% of ours){}  {} {}  ~  {} {}",
            pair.shared,
            100.0 * pair.shared as f64 / pair.total.max(1) as f64,
            if over { "  FAIL" } else { "" },
            rel(&ours[pair.ours], &root),
            ranges(&pair.lines),
            rel(&vanilla[pair.vanilla], &reference),
            ranges(&pair.vlines),
        );
    }
    if failed > 0 {
        bail!("{failed} file pair(s) share {fail} or more fingerprints with vanilla");
    }
    println!("similarity: no pair reaches {fail} shared fingerprints");
    Ok(())
}

/// Checks that copying is caught: 40-line pieces of vanilla files are
/// rewritten the way a port to Rust would spell them (snake_case names,
/// typed number literals, `let`) and looked up in the index. Prints only
/// counts.
fn run_self_test(vanilla: &[PathBuf], index: &HashMap<u64, Vec<(u32, u32)>>) -> Result<()> {
    let mut found = Vec::new();
    for (file, path) in vanilla.iter().enumerate().step_by(25) {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < 80 {
            continue;
        }
        let piece = rustify(&lines[20..60].join("\n"));
        let prints: HashSet<u64> = winnow(&tokens(&piece)).iter().map(|p| p.0).collect();
        let shared = prints
            .iter()
            .filter(|h| {
                index
                    .get(h)
                    .is_some_and(|f| f.iter().any(|v| v.0 as usize == file))
            })
            .count();
        found.push(shared);
    }
    ensure_some(vanilla, Path::new("."))?;
    found.sort_unstable();
    let at = |q: f64| found[((found.len() - 1) as f64 * q) as usize];
    println!(
        "self-test: {} rewritten 40-line pieces; shared fingerprints p10 {} p25 {} median {} p90 {}",
        found.len(),
        at(0.1),
        at(0.25),
        at(0.5),
        at(0.9)
    );
    Ok(())
}

/// A crude Java → Rust spelling: camelCase → snake_case, `16` → `16_i32`,
/// `2.0F` → `2.0_f32`, `int x =` → `let x: i32 =`.
fn rustify(java: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let snake = word.starts_with(|c: char| c.is_ascii_lowercase());
        for c in word.chars() {
            if snake && c.is_ascii_uppercase() {
                out.push('_');
                out.push(c.to_ascii_lowercase());
            } else {
                out.push(c);
            }
        }
        if word.starts_with(|c: char| c.is_ascii_digit()) && !word.contains(['.', 'x', 'L']) {
            out.push_str("_i32");
        }
        word.clear();
    };
    for c in java.chars() {
        if c.is_ascii_alphanumeric()
            || c == '_'
            || c == '.' && word.starts_with(|c: char| c.is_ascii_digit())
        {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out.replace("int ", "let ").replace("F)", "_f32)")
}

/// Our lines, vanilla lines and the fingerprints a file pair shares.
type Hit = (Vec<u32>, Vec<u32>, HashSet<u64>);

struct Pair {
    ours: usize,
    vanilla: usize,
    shared: usize,
    total: usize,
    lines: Vec<u32>,
    vlines: Vec<u32>,
}

fn ensure_some(files: &[PathBuf], dir: &Path) -> Result<()> {
    if files.is_empty() {
        bail!("no .java files under {}", dir.display());
    }
    Ok(())
}

fn collect(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| dir.display().to_string())? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                collect(&path, ext, out)?;
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// Fingerprints of every file, in parallel.
fn fingerprint_all(files: &[PathBuf]) -> Vec<Vec<(u64, u32)>> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = files.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|f| {
                            let text = std::fs::read(f).unwrap_or_default();
                            winnow(&tokens(&String::from_utf8_lossy(&text)))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("fingerprint thread"))
            .collect()
    })
}

/// Selects the smallest k-gram hash of every window of `W` k-grams, with the
/// line its first token is on.
fn winnow(tokens: &[(u64, u32)]) -> Vec<(u64, u32)> {
    if tokens.len() < K {
        return Vec::new();
    }
    let grams: Vec<(u64, u32)> = tokens
        .windows(K)
        .map(|w| {
            let hash = w.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, t| {
                (h ^ t.0).wrapping_mul(0x0100_0000_01b3).rotate_left(5)
            });
            (hash, w[0].1)
        })
        .collect();
    let mut out: Vec<(u64, u32)> = Vec::new();
    let mut last = usize::MAX;
    for start in 0..grams.len().saturating_sub(W - 1).max(1) {
        let window = &grams[start..(start + W).min(grams.len())];
        // The rightmost minimum, as in the winnowing paper.
        let (i, _) = window
            .iter()
            .enumerate()
            .rev()
            .min_by_key(|(_, g)| g.0)
            .expect("window is not empty");
        if start + i != last {
            last = start + i;
            out.push(grams[last]);
        }
    }
    out
}

const KEYWORDS: &[&str] = &[
    // Shared and Java
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "true",
    "try",
    "void",
    "volatile",
    "while",
    "var",
    "record",
    "sealed",
    "permits",
    "yield",
    "override",
    "nullable",
    "string",
    "object",
    // Rust
    "as",
    "async",
    "await",
    "crate",
    "dyn",
    "fn",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "self",
    "struct",
    "trait",
    "type",
    "unsafe",
    "use",
    "where",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "f32",
    "f64",
    "bool",
    "str",
    "some",
    "none",
    "ok",
    "err",
    "vec",
    "option",
    "result",
    "box",
];

/// The normalised tokens of a Java or Rust source, hashed, with their lines.
fn tokens(src: &str) -> Vec<(u64, u32)> {
    let keywords: HashSet<&str> = KEYWORDS.iter().copied().collect();
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut line = 1u32;
    let mut i = 0;
    let push = |out: &mut Vec<(u64, u32)>, s: &str, line: u32| out.push((hash(s), line));
    while i < b.len() {
        let c = b[i];
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    line += u32::from(b[i] == b'\n');
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                let start = line;
                i = skip_string(b, i, 0, &mut line);
                push(&mut out, "\"", start);
            }
            b'\'' => {
                // A char literal, or a Rust lifetime (no closing quote).
                let close = if b.get(i + 1) == Some(&b'\\') {
                    (i + 3..(i + 12).min(b.len())).find(|&j| b[j] == b'\'')
                } else {
                    let len = b.get(i + 1).map_or(1, |&c| match c {
                        0xf0.. => 4,
                        0xe0.. => 3,
                        0xc0.. => 2,
                        _ => 1,
                    });
                    Some(i + 1 + len).filter(|&j| b.get(j) == Some(&b'\''))
                };
                match close {
                    Some(j) if !b[i + 1..j].contains(&b'\n') => {
                        push(&mut out, "\"", line);
                        i = j + 1;
                    }
                    _ => {
                        i += 1;
                        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                            i += 1;
                        }
                    }
                }
            }
            c if c.is_ascii_digit()
                || c == b'.'
                    && b.get(i + 1).is_some_and(u8::is_ascii_digit)
                    && (i == 0 || b[i - 1] != b'.') =>
            {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.')
                    || i < b.len()
                        && (b[i] == b'+' || b[i] == b'-')
                        && matches!(b[i - 1], b'e' | b'E')
                        && !src[start..i].starts_with("0x")
                {
                    // `1..10` is a range, not a number.
                    if b[i] == b'.' && b.get(i + 1) == Some(&b'.') {
                        break;
                    }
                    i += 1;
                }
                push(&mut out, &number(&src[start..i]), line);
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80)
                {
                    i += 1;
                }
                let word = &src[start..i];
                // Rust raw and byte strings: r"..", r#".."#, b"..", br#".."#.
                if matches!(word, "r" | "b" | "br") && matches!(b.get(i), Some(b'"' | b'#')) {
                    let hashes = b[i..].iter().take_while(|&&c| c == b'#').count();
                    if b.get(i + hashes) == Some(&b'"') {
                        let start = line;
                        i = skip_string(b, i + hashes, hashes, &mut line);
                        push(&mut out, "\"", start);
                        continue;
                    }
                }
                let norm: String = word
                    .chars()
                    .filter(|&c| c != '_')
                    .flat_map(char::to_lowercase)
                    .collect();
                // Type names (`BlockPos`) sit in different places in the two
                // languages (`BlockPos pos` / `pos: BlockPos`) and are often
                // left out in Rust, so they are dropped too.
                let type_name = word.starts_with(|c: char| c.is_ascii_uppercase())
                    && word.contains(|c: char| c.is_ascii_lowercase());
                if !type_name && !norm.is_empty() && !keywords.contains(norm.as_str()) {
                    push(&mut out, &norm, line);
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Skips a string literal starting at the quote `b[i]`, closed by a quote and
/// `hashes` `#`s (Rust raw strings, no escapes) or a Java text block.
fn skip_string(b: &[u8], mut i: usize, hashes: usize, line: &mut u32) -> usize {
    let block = hashes == 0 && b[i..].starts_with(b"\"\"\"");
    i += if block { 3 } else { 1 };
    while i < b.len() {
        match b[i] {
            b'\\' if hashes == 0 => {
                *line += u32::from(b.get(i + 1) == Some(&b'\n'));
                i += 2;
                continue;
            }
            b'\n' => *line += 1,
            b'"' if block => {
                if b[i..].starts_with(b"\"\"\"") {
                    return i + 3;
                }
            }
            b'"' if b[i + 1..]
                .iter()
                .take(hashes)
                .filter(|&&c| c == b'#')
                .count()
                == hashes =>
            {
                return i + 1 + hashes;
            }
            _ => {}
        }
        i += 1;
    }
    i
}

/// One spelling for a number literal in either language.
fn number(raw: &str) -> String {
    let s: String = raw.chars().filter(|&c| c != '_').collect();
    let lower = s.to_ascii_lowercase();
    for (prefix, radix) in [("0x", 16), ("0b", 2), ("0o", 8)] {
        if let Some(digits) = lower.strip_prefix(prefix) {
            let digits = digits.trim_end_matches(|c: char| !c.is_digit(radix));
            let digits = strip_int_suffix(digits);
            return u128::from_str_radix(digits, radix).map_or(lower.clone(), |v| v.to_string());
        }
    }
    let body = strip_int_suffix(&lower);
    let body = ["f32", "f64"]
        .iter()
        .find_map(|t| body.strip_suffix(t))
        .unwrap_or(body);
    let body = body.trim_end_matches(['l', 'f', 'd']);
    match body.parse::<f64>() {
        Ok(v) if v.fract() == 0.0 && v.abs() < 1e18 => format!("{}", v as i128),
        Ok(v) => format!("{v}"),
        Err(_) => body.to_owned(),
    }
}

fn strip_int_suffix(s: &str) -> &str {
    for t in [
        "i128", "u128", "isize", "usize", "i16", "i32", "i64", "u16", "u32", "u64", "i8", "u8",
    ] {
        if let Some(rest) = s.strip_suffix(t) {
            return rest;
        }
    }
    s
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
        (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Lines as merged ranges (`12-30,88`), at most three, the longest first.
fn ranges(lines: &[u32]) -> String {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for l in sorted {
        match spans.last_mut() {
            Some((_, end)) if l <= *end + 8 => *end = l,
            _ => spans.push((l, l)),
        }
    }
    let count = spans.len();
    spans.sort_by_key(|(a, b)| std::cmp::Reverse(b - a));
    let mut parts: Vec<String> = spans
        .iter()
        .take(3)
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect();
    if count > 3 {
        parts.push(format!("+{}", count - 3));
    }
    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(src: &str) -> Vec<u64> {
        tokens(src).into_iter().map(|t| t.0).collect()
    }

    #[test]
    fn java_and_rust_spellings_agree() {
        let java = "public int getBlockState(int x) { return lookup(x, 0x10, 2.0F, \"a\"); }";
        let rust = "pub fn get_block_state(x: i32) -> i32 { lookup(x, 16_i32, 2.0, r#\"b\"#) }";
        assert_eq!(norm(java), norm(rust));
    }

    #[test]
    fn comments_lifetimes_and_ranges() {
        assert_eq!(norm("a /* x\ny */ b // z"), norm("a b"));
        assert_eq!(norm("fn f<'a>(x: &'a T) {}"), norm("f x t"));
        assert_eq!(norm("for i in 0..10 {}"), norm("i 0 10"));
        assert_eq!(norm("let c = 'x'; d"), norm("c \"x\" d"));
    }

    #[test]
    fn shared_runs_are_found() {
        let body: String = (0..40).map(|i| format!("name{i} ")).collect();
        let a = winnow(&tokens(&format!("alpha beta {body}")));
        let b = winnow(&tokens(&format!("{body} gamma")));
        let a: HashSet<u64> = a.iter().map(|p| p.0).collect();
        assert!(b.iter().filter(|p| a.contains(&p.0)).count() >= 3);
    }

    #[test]
    fn ranges_merge_near_lines() {
        assert_eq!(ranges(&[5, 3, 9, 40, 41, 100]), "3-9,40-41,100");
    }
}
