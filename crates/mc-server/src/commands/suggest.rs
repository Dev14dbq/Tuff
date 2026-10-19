//! Completions the client asks the server for: arguments marked
//! `minecraft:ask_server` in the tree (time markers, players for `/op`,
//! banned addresses, structure templates...).

use mc_command::tree::{Kind, ROOT};
use mc_protocol::packets::play::CommandSuggestions;

use super::Source;
use crate::context::ServerContext;
use crate::tick::lock;

/// Whether `candidate` completes `typed` as vanilla matches ids: the whole
/// id, or its path when no namespace was typed.
fn completes(candidate: &str, typed: &str) -> bool {
    if candidate.starts_with(typed) {
        return true;
    }
    match candidate.split_once(':') {
        Some(("minecraft", path)) if !typed.contains(':') => path.starts_with(typed),
        _ => false,
    }
}

fn data_ids(ctx: &ServerContext, folder: &str) -> Vec<String> {
    ctx.commands.env.ids(&format!("minecraft:{folder}"))
}

fn time_markers(ctx: &ServerContext, clock: &str) -> Vec<String> {
    let dir = ctx.config.data_dir.join("data/minecraft/timeline");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Some(v) = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        else {
            continue;
        };
        if v["clock"].as_str() != Some(clock) {
            continue;
        }
        for (name, m) in v["time_markers"].as_object().into_iter().flatten() {
            if m["show_in_commands"].as_bool() == Some(true) {
                out.push(name.clone());
            }
        }
    }
    out.sort();
    out
}

/// What may go in the argument at `path` (names from the root).
fn candidates(ctx: &ServerContext, path: &[String]) -> Vec<String> {
    let p: Vec<&str> = path.iter().map(String::as_str).collect();
    let online: Vec<String> = lock(&ctx.players)
        .values()
        .map(|p| p.profile.name.clone())
        .collect();
    let lists = lock(&ctx.lists);
    let mut out: Vec<String> = match p.as_slice() {
        ["time", "set", "timemarker"] | ["time", "of", "clock", "set", "timemarker"] => {
            drop(lists);
            return time_markers(ctx, "minecraft:overworld");
        }
        ["op", ..] => online
            .into_iter()
            .filter(|n| !lists.ops.iter().any(|o| o.name.eq_ignore_ascii_case(n)))
            .collect(),
        ["deop", ..] => lists.ops.iter().map(|o| o.name.clone()).collect(),
        ["pardon", ..] => lists
            .banned_players
            .iter()
            .map(|b| b.name.clone())
            .collect(),
        ["pardon-ip", ..] => lists.banned_ips.iter().map(|b| b.name.clone()).collect(),
        ["ban", ..] => online,
        ["whitelist", "add", ..] => online
            .into_iter()
            .filter(|n| {
                !lists
                    .whitelist
                    .iter()
                    .any(|w| w.name.eq_ignore_ascii_case(n))
            })
            .collect(),
        ["whitelist", "remove", ..] => lists.whitelist.iter().map(|w| w.name.clone()).collect(),
        ["place", "template", ..] => {
            drop(lists);
            return data_ids(ctx, "structure");
        }
        [.., "score_holder" | "targets" | "target" | "source"] => {
            let mut names = online;
            names.push("*".into());
            names
        }
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

/// The answer to a Command Suggestions Request for `text` (with its `/`).
pub fn suggest(
    ctx: &ServerContext,
    source: &Source,
    id: i32,
    text: &str,
) -> Option<CommandSuggestions> {
    let commands = &ctx.commands;
    let input = text.strip_prefix('/').unwrap_or(text);
    let offset = text.len() - input.len();
    let level = match source {
        Source::Console => 4,
        Source::Player { uuid, name, .. } => super::level(ctx, *uuid, name),
    };
    // The completed part up to the last space, and the word being typed.
    let split = input.rfind(' ');
    let (done, word) = match split {
        Some(i) => (&input[..i], &input[i + 1..]),
        None => ("", input),
    };
    let tree = &commands.tree;
    let node = if done.is_empty() {
        ROOT
    } else {
        let parse = mc_command::dispatch::parse(tree, done, level, &commands.env);
        if parse.cursor < done.len() {
            return Some(CommandSuggestions {
                id,
                start: (offset + input.len()) as i32,
                length: 0,
                matches: Vec::new(),
            });
        }
        let last = parse.chain.last()?;
        match last.nodes.last() {
            Some(&(n, _, _)) => tree.nodes[n].redirect.unwrap_or(n),
            None => ROOT,
        }
    };
    let mut matches = Vec::new();
    for &c in &tree.nodes[node].children {
        let n = &tree.nodes[c];
        if n.level > level || n.suggestions.as_deref() != Some("minecraft:ask_server") {
            continue;
        }
        if !matches!(n.kind, Kind::Argument { .. }) {
            continue;
        }
        let mut path = tree.names(node);
        path.push(n.name.clone());
        for cand in candidates(ctx, &path) {
            if completes(&cand, word) && !matches.iter().any(|(m, _)| *m == cand) {
                matches.push((cand, None));
            }
        }
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0));
    let start = offset + split.map_or(0, |i| i + 1);
    Some(CommandSuggestions {
        id,
        start: start as i32,
        length: word.len() as i32,
        matches,
    })
}
