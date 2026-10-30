//! Finds the things a fix is about (entities, block entities, item stacks,
//! component maps, block states, text) anywhere in a saved document.
//!
//! Vanilla knows where each of these lives from its type schemas. We go by the
//! shape of the data instead: a compound with a string `id` and a count is an
//! item stack, one under an entity key (`Passengers`, `SpawnData.entity`,
//! `EntityTag`...) or with a `Pos` is an entity, one under a block entity key
//! or with `x`/`y`/`z` is a block entity. Plugin data (`PublicBukkitValues`)
//! and leftover item data (`minecraft:custom_data`) are opaque to vanilla and
//! are not looked into.

use mc_nbt::{Compound, Tag};

use super::nbt::{compound_mut, id_of, list_mut};
use super::{Context, Kind, Saved};

/// What a fix changes. Each kind of thing is found by [`run`].
#[derive(Clone, Copy)]
pub(super) enum Apply {
    /// A region chunk's root.
    Chunk(fn(&mut Compound, &Context)),
    /// A `poi` region chunk's root.
    Poi(fn(&mut Compound)),
    /// level.dat's `Data`.
    Level(fn(&mut Compound)),
    /// A player's root (player files and level.dat's `Player`).
    Player(fn(&mut Compound)),
    /// An entity by namespaced id; `players` also passes players as
    /// `minecraft:player`.
    Entity {
        players: bool,
        f: fn(&str, &mut Compound),
    },
    BlockEntity(fn(&str, &mut Compound)),
    /// An item stack by namespaced id (old `Count`/`tag` or new
    /// `count`/`components` form).
    Item(fn(&str, &mut Compound)),
    /// A component map; the id is the item's or block entity's.
    Components(fn(&str, &mut Compound)),
    /// A block state (`Name`/`Properties`) in a palette or on an entity.
    BlockState(fn(&mut Compound)),
    /// A text component.
    Text(fn(&mut Tag)),
    /// A saved data file's `data`.
    Saved(Saved, fn(&mut Compound)),
}

impl Apply {
    /// Whether the fix looks inside the document, not only at its root.
    fn reaches_inside(self) -> bool {
        matches!(
            self,
            Apply::Entity { .. }
                | Apply::BlockEntity(_)
                | Apply::Item(_)
                | Apply::Components(_)
                | Apply::BlockState(_)
                | Apply::Text(_)
        )
    }
}

/// Keys that hold no game data a fix touches, or data vanilla treats as
/// opaque.
const SKIP: [&str; 9] = [
    "PublicBukkitValues",
    "BukkitValues",
    "ChunkBukkitValues",
    "minecraft:custom_data",
    "Heightmaps",
    "sections",
    "structures",
    "PostProcessing",
    "blending_data",
];

const ENTITY_KEYS: [&str; 9] = [
    "Passengers",
    "Entities",
    "entities",
    "entity",
    "EntityTag",
    "minecraft:entity_data",
    "EntityData",
    "entity_data",
    "Riding",
];

const BLOCK_ENTITY_KEYS: [&str; 4] = [
    "block_entities",
    "BlockEntityTag",
    "minecraft:block_entity_data",
    "TileEntityData",
];

/// Keys on entities that hold a block state.
const ENTITY_BLOCK_STATES: [&str; 4] = [
    "BlockState",
    "carriedBlockState",
    "DisplayState",
    "block_state",
];

#[derive(Clone, PartialEq, Eq, Debug)]
enum Role {
    Other,
    Entity(String),
    BlockEntity(String),
    Item(String),
    Components(String),
}

fn classify(parent: &Role, key: &str, c: &Compound) -> Role {
    if key == "components" {
        let owner = match parent {
            Role::Item(id) | Role::BlockEntity(id) => id.clone(),
            _ => String::new(),
        };
        return Role::Components(owner);
    }
    let id = id_of(c);
    if BLOCK_ENTITY_KEYS.contains(&key) {
        return Role::BlockEntity(id.unwrap_or_default());
    }
    let Some(id) = id else {
        return Role::Other;
    };
    if c.get("Count").is_some() || c.get("count").is_some() {
        return Role::Item(id);
    }
    if ENTITY_KEYS.contains(&key) || c.get("Pos").is_some() {
        return Role::Entity(id);
    }
    if ["x", "y", "z"]
        .iter()
        .all(|k| matches!(c.get(k), Some(Tag::Int(_))))
    {
        return Role::BlockEntity(id);
    }
    Role::Other
}

/// Applies one fix to a whole document of `kind`.
pub(super) fn run(root: &mut Compound, kind: Kind, apply: Apply, ctx: &Context) {
    match kind {
        Kind::Chunk => {
            if let Apply::Chunk(f) = apply {
                f(root, ctx);
            }
            if let Apply::BlockState(f) = apply {
                palettes(root, f);
            }
        }
        Kind::Poi => {
            if let Apply::Poi(f) = apply {
                f(root);
            }
            return;
        }
        Kind::EntityChunk => {}
        Kind::Player => {
            player(root, apply);
            return;
        }
        Kind::Level => {
            let Some(data) = compound_mut(root, "Data") else {
                return;
            };
            if let Apply::Level(f) = apply {
                f(data);
            }
            if let Some(p) = compound_mut(data, "Player") {
                player(p, apply);
            }
            if let Apply::Text(f) = apply {
                boss_bar_names(data, "CustomBossEvents", f);
            }
            return;
        }
        Kind::Saved(saved) => {
            let Some(data) = compound_mut(root, "data") else {
                return;
            };
            if let Apply::Saved(which, f) = apply
                && which == saved
            {
                f(data);
            }
            if let Apply::Text(f) = apply {
                saved_texts(data, saved, f);
            }
            if apply.reaches_inside() {
                children(data, &Role::Other, apply);
            }
            return;
        }
    }
    if apply.reaches_inside() {
        children(root, &Role::Other, apply);
    }
}

fn player(root: &mut Compound, apply: Apply) {
    match apply {
        Apply::Player(f) => f(root),
        Apply::Entity { players: true, f } => f("minecraft:player", root),
        Apply::Text(f) => texts(root, &["CustomName"], f),
        _ => {}
    }
    if apply.reaches_inside() {
        children(root, &Role::Entity("minecraft:player".into()), apply);
    }
}

/// Block palettes of a chunk's sections.
fn palettes(root: &mut Compound, f: fn(&mut Compound)) {
    let Some(sections) = list_mut(root, "sections") else {
        return;
    };
    for section in sections {
        let Tag::Compound(section) = section else {
            continue;
        };
        let Some(states) = compound_mut(section, "block_states") else {
            continue;
        };
        if let Some(palette) = list_mut(states, "palette") {
            for entry in palette {
                if let Tag::Compound(entry) = entry {
                    f(entry);
                }
            }
        }
    }
}

fn children(c: &mut Compound, role: &Role, apply: Apply) {
    for (key, value) in c.iter_mut() {
        if SKIP.contains(&key) {
            continue;
        }
        match value {
            Tag::Compound(inner) => visit(role, key, inner, apply),
            Tag::List(list) => {
                for tag in list {
                    if let Tag::Compound(inner) = tag {
                        visit(role, key, inner, apply);
                    }
                }
            }
            _ => {}
        }
    }
}

fn visit(parent: &Role, key: &str, c: &mut Compound, apply: Apply) {
    let role = classify(parent, key, c);
    match (&role, apply) {
        (Role::Entity(id), Apply::Entity { f, .. }) if id != "minecraft:player" => f(id, c),
        (Role::Entity(_), Apply::Text(f)) => texts(c, &["CustomName", "text"], f),
        (Role::Entity(_), Apply::BlockState(f)) => {
            for k in ENTITY_BLOCK_STATES {
                if let Some(state) = compound_mut(c, k) {
                    f(state);
                }
            }
        }
        (Role::BlockEntity(id), Apply::BlockEntity(f)) => f(id, c),
        (Role::BlockEntity(_), Apply::Text(f)) => {
            texts(c, &["CustomName"], f);
            for side in ["front_text", "back_text"] {
                if let Some(text) = compound_mut(c, side) {
                    text_lists(text, &["messages", "filtered_messages"], f);
                }
            }
        }
        (Role::Item(id), Apply::Item(f)) => f(id, c),
        (Role::Components(owner), Apply::Components(f)) => f(owner, c),
        (Role::Components(_), Apply::Text(f)) => {
            texts(c, &["minecraft:custom_name", "minecraft:item_name"], f);
            text_lists(c, &["minecraft:lore"], f);
            if let Some(book) = compound_mut(c, "minecraft:written_book_content")
                && let Some(pages) = list_mut(book, "pages")
            {
                for page in pages {
                    if let Tag::Compound(page) = page {
                        texts(page, &["raw", "filtered"], f);
                    }
                }
            }
        }
        _ => {}
    }
    // A fix can turn an old item into a new one; look at what it is now.
    let role = match role {
        Role::Item(_) | Role::Entity(_) | Role::BlockEntity(_) => classify(parent, key, c),
        other => other,
    };
    children(c, &role, apply);
}

fn texts(c: &mut Compound, keys: &[&str], f: fn(&mut Tag)) {
    for key in keys {
        if let Some(tag) = c.get_mut(key) {
            f(tag);
        }
    }
}

fn text_lists(c: &mut Compound, keys: &[&str], f: fn(&mut Tag)) {
    for key in keys {
        if let Some(list) = list_mut(c, key) {
            list.iter_mut().for_each(f);
        }
    }
}

fn boss_bar_names(c: &mut Compound, key: &str, f: fn(&mut Tag)) {
    if let Some(Tag::Compound(bars)) = c.get_mut(key) {
        for (_, bar) in bars.iter_mut() {
            if let Tag::Compound(bar) = bar {
                texts(bar, &["Name"], f);
            }
        }
    }
}

fn saved_texts(data: &mut Compound, saved: Saved, f: fn(&mut Tag)) {
    match saved {
        Saved::Scoreboard => {
            if let Some(list) = list_mut(data, "Objectives") {
                for tag in list {
                    if let Tag::Compound(o) = tag {
                        texts(o, &["DisplayName"], f);
                    }
                }
            }
            if let Some(list) = list_mut(data, "Teams") {
                for tag in list {
                    if let Tag::Compound(t) = tag {
                        texts(
                            t,
                            &["DisplayName", "MemberNamePrefix", "MemberNameSuffix"],
                            f,
                        );
                    }
                }
            }
        }
        Saved::BossEvents => {
            for (_, bar) in data.iter_mut() {
                if let Tag::Compound(bar) = bar {
                    texts(bar, &["Name"], f);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(pairs: &[(&str, Tag)]) -> Compound {
        let mut c = Compound::new();
        for (k, v) in pairs {
            c.insert(*k, v.clone());
        }
        c
    }

    #[test]
    fn shapes_are_told_apart() {
        let item = with(&[("id", "stone".into()), ("Count", Tag::Byte(1))]);
        assert_eq!(
            classify(&Role::Other, "Item", &item),
            Role::Item("minecraft:stone".into())
        );
        let rider = with(&[("id", "minecraft:pig".into())]);
        assert_eq!(
            classify(&Role::Other, "Passengers", &rider),
            Role::Entity("minecraft:pig".into())
        );
        let chest = with(&[
            ("id", "minecraft:chest".into()),
            ("x", Tag::Int(0)),
            ("y", Tag::Int(0)),
            ("z", Tag::Int(0)),
        ]);
        assert_eq!(
            classify(&Role::Other, "whatever", &chest),
            Role::BlockEntity("minecraft:chest".into())
        );
        assert_eq!(
            classify(
                &Role::Item("minecraft:chest".into()),
                "components",
                &Compound::new()
            ),
            Role::Components("minecraft:chest".into())
        );
    }
}
