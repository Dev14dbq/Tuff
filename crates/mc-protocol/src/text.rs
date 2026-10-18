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
        compound.insert("with", Tag::List(args));
    }
    Tag::Compound(compound)
}

/// The same with a color.
pub fn translate_colored(key: &str, args: Vec<Tag>, color: &str) -> Tag {
    let mut t = translate(key, args);
    if let Tag::Compound(c) = &mut t {
        c.insert("color", color);
    }
    t
}
