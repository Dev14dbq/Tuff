//! Completions as Brigadier gives them: the node before the cursor is
//! found in the parsed line, and every child of it lists what may follow
//! (literals by name, arguments by their type), sorted.

use mc_nbt::Tag;

use crate::args::{Env, full_id};
use crate::dispatch::{self, Parse};
use crate::text::translate;
use crate::tree::{Kind, ROOT, Tree};

/// What the server knows that completions need beyond the registries.
pub trait Known {
    /// Names of the players online.
    fn players(&self) -> Vec<String>;
    /// Completions for an `ask_server` argument at `path`.
    fn ask(&self, path: &[String], typed: &str) -> Vec<String>;
}

/// Completions for `input` (without `/`): where they start and the texts
/// with their tooltips.
pub fn complete(
    tree: &Tree,
    input: &str,
    level: u8,
    env: &dyn Env,
    data: &crate::env::DataEnv,
    known: &dyn Known,
) -> (usize, Vec<(String, Option<Tag>)>) {
    let parse = dispatch::parse(tree, input, level, env);
    let cursor = input.len();
    let (parent, start) = context(tree, &parse, cursor);
    let typed = &input[start.min(input.len())..];
    let mut out: Vec<(usize, String, Option<Tag>)> = Vec::new();
    for &c in &tree.nodes[parent].children {
        let n = &tree.nodes[c];
        if n.level > level {
            continue;
        }
        match &n.kind {
            Kind::Literal => {
                if n.name.to_lowercase().starts_with(&typed.to_lowercase()) {
                    out.push((start, n.name.clone(), None));
                }
            }
            Kind::Argument { parser, properties } => {
                let mut path = tree.names(parent);
                path.push(n.name.clone());
                let found = if n.suggestions.as_deref() == Some("minecraft:ask_server") {
                    known
                        .ask(&path, typed)
                        .into_iter()
                        .filter(|s| ids_match(s, typed))
                        .map(|s| (0, s, None))
                        .collect()
                } else {
                    argument(
                        parser,
                        properties,
                        n.suggestions.as_deref(),
                        typed,
                        data,
                        known,
                    )
                };
                out.extend(found.into_iter().map(|(off, s, t)| (start + off, s, t)));
            }
            Kind::Root => {}
        }
    }
    // A suggestion of exactly what is typed is none.
    out.retain(|(s, text, _)| input.get(*s..) != Some(text.as_str()));
    if out.is_empty() {
        return (0, Vec::new());
    }
    let first = out.iter().map(|o| o.0).min().unwrap_or(start);
    // Merged as Brigadier merges: everything from the earliest start.
    let mut list: Vec<(String, Option<Tag>)> = out
        .into_iter()
        .map(|(s, text, tip)| (format!("{}{}", &input[first..s], text), tip))
        .collect();
    list.sort_by_key(|a| a.0.to_lowercase());
    list.dedup_by(|a, b| a.0 == b.0);
    (first, list)
}

/// The node whose children may follow the cursor, and where the text
/// they complete starts.
fn context(tree: &Tree, parse: &Parse, cursor: usize) -> (usize, usize) {
    let mut root = ROOT;
    let mut after = 0;
    for (i, c) in parse.chain.iter().enumerate() {
        let Some(&(_, start, _)) = c.nodes.first() else {
            return (root, after);
        };
        let &(last_node, _, end) = c.nodes.last().expect("a node");
        if end < cursor {
            if i + 1 < parse.chain.len() {
                // The rest is in the redirected context.
                root = tree.nodes[last_node].redirect.unwrap_or(ROOT);
                after = end + 1;
                continue;
            }
            return (last_node, end + 1);
        }
        let mut prev = root;
        for &(n, s, e) in &c.nodes {
            if s <= cursor && cursor <= e {
                return (prev, s);
            }
            prev = n;
        }
        return (prev, start);
    }
    (root, after)
}

/// Vanilla's id matching: the whole id, or its path without `minecraft:`,
/// or any part after a `_` or `/`... (only the first two here).
fn ids_match(id: &str, typed: &str) -> bool {
    if id.starts_with(typed) {
        return true;
    }
    match id.split_once(':') {
        Some((_, path)) if !typed.contains(':') => path.starts_with(typed),
        _ => false,
    }
}

fn ids(list: Vec<String>, typed: &str) -> Vec<(usize, String, Option<Tag>)> {
    list.into_iter()
        .filter(|s| ids_match(s, typed))
        .map(|s| (0, s, None))
        .collect()
}

fn words(list: &[&str], typed: &str) -> Vec<(usize, String, Option<Tag>)> {
    list.iter()
        .filter(|w| w.starts_with(typed))
        .map(|w| (0, (*w).to_owned(), None))
        .collect()
}

/// The slot names `item_slot` takes: each numbered group with its `.*`.
fn slot_names() -> Vec<String> {
    let mut out: Vec<String> = [
        "armor.body",
        "armor.chest",
        "armor.feet",
        "armor.head",
        "armor.legs",
        "armor.*",
        "contents",
        "horse.chest",
        "player.cursor",
        "saddle",
        "weapon",
        "weapon.*",
        "weapon.mainhand",
        "weapon.offhand",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for (group, count) in [
        ("container", 54),
        ("enderchest", 27),
        ("horse", 15),
        ("hotbar", 9),
        ("inventory", 27),
        ("mob.inventory", 8),
        ("player.crafting", 4),
    ] {
        out.push(format!("{group}.*"));
        out.extend((0..count).map(|i| format!("{group}.{i}")));
    }
    out
}

const SELECTORS: [(&str, &str); 6] = [
    ("@a", "allPlayers"),
    ("@e", "allEntities"),
    ("@n", "nearestEntity"),
    ("@p", "nearestPlayer"),
    ("@r", "randomPlayer"),
    ("@s", "self"),
];

const OPTIONS: [&str; 21] = [
    "advancements",
    "distance",
    "dx",
    "dy",
    "dz",
    "gamemode",
    "level",
    "limit",
    "name",
    "nbt",
    "predicate",
    "scores",
    "sort",
    "tag",
    "team",
    "type",
    "x",
    "x_rotation",
    "y",
    "y_rotation",
    "z",
];

fn tip(key: &str) -> Option<Tag> {
    Some(translate(key, Vec::new()))
}

fn selector(
    typed: &str,
    data: &crate::env::DataEnv,
    known: &dyn Known,
) -> Vec<(usize, String, Option<Tag>)> {
    // Inside the options: `@e[`, `@e[type=`.
    if let Some(open) = typed.find('[') {
        let inside = &typed[open + 1..];
        let part_start = inside.rfind(',').map_or(0, |i| i + 1);
        let part = &inside[part_start..];
        let base = open + 1 + part_start;
        return match part.split_once('=') {
            None => OPTIONS
                .iter()
                .filter(|o| o.starts_with(part))
                .map(|o| {
                    let key = format!("argument.entity.options.{o}.description");
                    (base, format!("{o}="), tip(&key))
                })
                .collect(),
            Some(("type", value)) => {
                let off = base + 5;
                let neg = value.starts_with('!');
                let v = value.trim_start_matches('!');
                let mut all = Vec::new();
                let mut push = |s: String| {
                    for p in ["!", ""] {
                        let full = format!("{p}{s}");
                        if (neg && full.starts_with('!') && ids_match(&s, v))
                            || (!neg && full.starts_with(v))
                        {
                            all.push((off, full, None));
                        }
                    }
                };
                for t in data.tags("minecraft:entity_type") {
                    push(format!("#{t}"));
                }
                for id in data.ids("minecraft:entity_type") {
                    push(id);
                }
                all
            }
            Some(("gamemode", v)) => words(&["adventure", "creative", "spectator", "survival"], v)
                .into_iter()
                .map(|(_, s, t)| (base + 9, s, t))
                .collect(),
            Some(("sort", v)) => words(&["arbitrary", "furthest", "nearest", "random"], v)
                .into_iter()
                .map(|(_, s, t)| (base + 5, s, t))
                .collect(),
            _ => Vec::new(),
        };
    }
    let mut out: Vec<(usize, String, Option<Tag>)> = Vec::new();
    for (s, key) in SELECTORS {
        if s.starts_with(typed) {
            out.push((
                0,
                s.to_owned(),
                tip(&format!("argument.entity.selector.{key}")),
            ));
        }
    }
    if !typed.starts_with('@') {
        for p in known.players() {
            if p.to_lowercase().starts_with(&typed.to_lowercase()) {
                out.push((0, p, None));
            }
        }
    }
    out
}

fn block_state(
    typed: &str,
    data: &crate::env::DataEnv,
    tags: bool,
) -> Vec<(usize, String, Option<Tag>)> {
    use crate::args::Env as _;
    let Some(open) = typed.find('[') else {
        let mut list = data.ids("minecraft:block");
        if tags {
            list.extend(
                data.tags("minecraft:block")
                    .into_iter()
                    .map(|t| format!("#{t}")),
            );
        }
        return ids(list, typed);
    };
    let id = full_id(&typed[..open]);
    let Some(props) = data.block_properties(&id) else {
        return Vec::new();
    };
    let inside = &typed[open + 1..];
    let part_start = inside.rfind(',').map_or(0, |i| i + 1);
    let part = &inside[part_start..];
    let base = open + 1 + part_start;
    match part.split_once('=') {
        None => {
            let set: Vec<String> = inside
                .split(',')
                .filter_map(|p| p.split_once('=').map(|(k, _)| k.to_owned()))
                .collect();
            let mut out: Vec<(usize, String, Option<Tag>)> = props
                .iter()
                .filter(|(k, _)| !set.contains(k) && k.starts_with(part))
                .map(|(k, _)| (base, format!("{k}="), None))
                .collect();
            if part.is_empty() {
                out.push((base, "]".into(), None));
            }
            out
        }
        Some((k, v)) => props
            .iter()
            .find(|(name, _)| name == k)
            .map(|(_, values)| {
                values
                    .iter()
                    .filter(|x| x.starts_with(v))
                    .map(|x| (base + k.len() + 1, x.clone(), None))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// What an argument of `parser` suggests on its own.
fn argument(
    parser: &str,
    props: &serde_json::Value,
    custom: Option<&str>,
    typed: &str,
    data: &crate::env::DataEnv,
    known: &dyn Known,
) -> Vec<(usize, String, Option<Tag>)> {
    let registry = props["registry"].as_str().unwrap_or("");
    match custom {
        Some("minecraft:summonable_entities") => {
            return data
                .ids("minecraft:entity_type")
                .into_iter()
                .filter(|id| id != "minecraft:player" && id != "minecraft:fishing_bobber")
                .filter(|s| ids_match(s, typed))
                .map(|s| {
                    let key = format!("entity.{}", s.replace(':', "."));
                    (0, s, tip(&key))
                })
                .collect();
        }
        Some("minecraft:available_sounds") => return ids(data.ids("minecraft:sound_event"), typed),
        _ => {}
    }
    match parser {
        "brigadier:bool" => words(&["false", "true"], typed),
        "minecraft:gamemode" => words(&["adventure", "creative", "spectator", "survival"], typed),
        "minecraft:entity" | "minecraft:game_profile" | "minecraft:score_holder" => {
            selector(typed, data, known)
        }
        "minecraft:item_stack" => ids(data.ids("minecraft:item"), typed),
        "minecraft:item_slot" | "minecraft:slot_source" => {
            let mut list = slot_names();
            list.retain(|w| w.starts_with(typed));
            list.sort();
            list.into_iter().map(|w| (0, w, None)).collect()
        }
        "minecraft:item_predicate" => {
            let mut list = data.ids("minecraft:item");
            list.extend(
                data.tags("minecraft:item")
                    .into_iter()
                    .map(|t| format!("#{t}")),
            );
            ids(list, typed)
        }
        "minecraft:block_state" => block_state(typed, data, false),
        "minecraft:block_predicate" => block_state(typed, data, true),
        "minecraft:resource" | "minecraft:resource_key" => ids(data.ids(registry), typed),
        "minecraft:resource_or_tag" | "minecraft:resource_or_tag_key" => {
            let mut list: Vec<String> = data
                .tags(registry)
                .into_iter()
                .map(|t| format!("#{t}"))
                .collect();
            list.extend(data.ids(registry));
            ids(list, typed)
        }
        "minecraft:dimension" => ids(
            vec![
                "minecraft:overworld".into(),
                "minecraft:the_end".into(),
                "minecraft:the_nether".into(),
            ],
            typed,
        ),
        "minecraft:particle" => ids(data.ids("minecraft:particle_type"), typed),
        "minecraft:entity_anchor" => words(&["eyes", "feet"], typed),
        "minecraft:heightmap" => words(
            &[
                "motion_blocking",
                "motion_blocking_no_leaves",
                "ocean_floor",
                "world_surface",
            ],
            typed,
        ),
        "minecraft:template_mirror" => words(&["front_back", "left_right", "none"], typed),
        "minecraft:template_rotation" => words(
            &["180", "clockwise_90", "counterclockwise_90", "none"],
            typed,
        ),
        "minecraft:team_color" => words(
            &[
                "aqua",
                "black",
                "blue",
                "dark_aqua",
                "dark_blue",
                "dark_gray",
                "dark_green",
                "dark_purple",
                "dark_red",
                "gold",
                "gray",
                "green",
                "light_purple",
                "red",
                "reset",
                "white",
                "yellow",
            ],
            typed,
        ),
        "minecraft:operation" => words(&["%=", "*=", "+=", "-=", "/=", "<", "=", ">", "><"], typed),
        _ => Vec::new(),
    }
}
