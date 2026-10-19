//! Minimal text components, encoded as network NBT.
//! A plain string is a String tag; anything styled is a Compound.

use mc_nbt::{Compound, Tag};

pub fn plain(text: &str) -> Tag {
    Tag::from(text)
}

/// `color` is a named color (`"gold"`) or `#RRGGBB`.
pub fn colored(text: &str, color: &str) -> Tag {
    let mut compound = Compound::new();
    compound.insert("text", text).insert("color", color);
    Tag::Compound(compound)
}

/// A translatable message (`translate` key with `with` arguments), shown in
/// the player's language.
pub fn translate(key: &str, args: Vec<Tag>) -> Tag {
    let mut compound = Compound::new();
    compound.insert("translate", key);
    if !args.is_empty() {
        compound.insert("with", list(args));
    }
    Tag::Compound(compound)
}

/// A list of components or arguments. NBT lists hold one tag type, so when
/// the types differ every element that is not a compound is wrapped as
/// `{"": value}`, as vanilla writes them.
pub fn list(items: Vec<Tag>) -> Tag {
    let same = items.windows(2).all(|w| w[0].type_id() == w[1].type_id());
    if same {
        return Tag::List(items);
    }
    Tag::List(
        items
            .into_iter()
            .map(|t| match t {
                Tag::Compound(_) => t,
                other => {
                    let mut c = Compound::new();
                    c.insert("", other);
                    Tag::Compound(c)
                }
            })
            .collect(),
    )
}

/// The same with a color.
pub fn translate_colored(key: &str, args: Vec<Tag>, color: &str) -> Tag {
    let mut t = translate(key, args);
    if let Tag::Compound(c) = &mut t {
        c.insert("color", color);
    }
    t
}
