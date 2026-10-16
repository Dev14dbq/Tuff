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
