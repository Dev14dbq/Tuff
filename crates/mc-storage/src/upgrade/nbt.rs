//! Small editing helpers the data fixes share.

use mc_nbt::{Compound, Tag};

/// `id` with the `minecraft:` namespace when it has none.
pub(super) fn ns(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

pub(super) fn string<'a>(c: &'a Compound, key: &str) -> Option<&'a str> {
    match c.get(key) {
        Some(Tag::String(s)) => Some(s),
        _ => None,
    }
}

/// The namespaced `id` of an entity, block entity or item.
pub(super) fn id_of(c: &Compound) -> Option<String> {
    string(c, "id").map(ns)
}

pub(super) fn number(tag: &Tag) -> Option<f64> {
    Some(match *tag {
        Tag::Byte(v) => f64::from(v),
        Tag::Short(v) => f64::from(v),
        Tag::Int(v) => f64::from(v),
        Tag::Long(v) => v as f64,
        Tag::Float(v) => f64::from(v),
        Tag::Double(v) => v,
        _ => return None,
    })
}

pub(super) fn num(c: &Compound, key: &str) -> Option<f64> {
    c.get(key).and_then(number)
}

/// A whole number field (any numeric tag, truncated like Java's `intValue`).
pub(super) fn int(c: &Compound, key: &str) -> Option<i32> {
    match c.get(key)? {
        Tag::Long(v) => Some(*v as i32),
        tag => number(tag).map(|v| v as i32),
    }
}

pub(super) fn long(c: &Compound, key: &str) -> Option<i64> {
    match c.get(key)? {
        Tag::Long(v) => Some(*v),
        tag => number(tag).map(|v| v as i64),
    }
}

/// A boolean the way codecs read one: a number (non-zero is true) or, in old
/// game rules, the strings `true` / `false`.
pub(super) fn boolean(c: &Compound, key: &str) -> Option<bool> {
    match c.get(key)? {
        Tag::String(s) => Some(s == "true"),
        tag => number(tag).map(|v| v != 0.0),
    }
}

pub(super) fn flag(value: bool) -> Tag {
    Tag::Byte(i8::from(value))
}

/// Moves `from` to `to` when it is there.
pub(super) fn rename(c: &mut Compound, from: &str, to: &str) {
    if let Some(value) = c.remove(from) {
        c.insert(to, value);
    }
}

/// Moves `from` to `to`, changing the value on the way.
pub(super) fn rename_with(c: &mut Compound, from: &str, to: &str, f: impl FnOnce(Tag) -> Tag) {
    if let Some(value) = c.remove(from) {
        c.insert(to, f(value));
    }
}

pub(super) fn compound_mut<'a>(c: &'a mut Compound, key: &str) -> Option<&'a mut Compound> {
    match c.get_mut(key) {
        Some(Tag::Compound(inner)) => Some(inner),
        _ => None,
    }
}

pub(super) fn compound<'a>(c: &'a Compound, key: &str) -> Option<&'a Compound> {
    match c.get(key) {
        Some(Tag::Compound(inner)) => Some(inner),
        _ => None,
    }
}

pub(super) fn list_mut<'a>(c: &'a mut Compound, key: &str) -> Option<&'a mut Vec<Tag>> {
    match c.get_mut(key) {
        Some(Tag::List(list)) => Some(list),
        _ => None,
    }
}

pub(super) fn list<'a>(c: &'a Compound, key: &str) -> &'a [Tag] {
    match c.get(key) {
        Some(Tag::List(list)) => list,
        _ => &[],
    }
}

/// The compound at `key`, made empty first when there is none.
pub(super) fn child<'a>(c: &'a mut Compound, key: &str) -> &'a mut Compound {
    if !matches!(c.get(key), Some(Tag::Compound(_))) {
        c.insert(key, Compound::new());
    }
    match c.get_mut(key) {
        Some(Tag::Compound(inner)) => inner,
        _ => unreachable!("inserted just above"),
    }
}

/// Calls `f` on every compound in the list at `key`.
pub(super) fn each_compound(c: &mut Compound, key: &str, mut f: impl FnMut(&mut Compound)) {
    if let Some(list) = list_mut(c, key) {
        for tag in list {
            if let Tag::Compound(inner) = tag {
                f(inner);
            }
        }
    }
}

pub(super) fn int_array(x: i32, y: i32, z: i32) -> Tag {
    Tag::IntArray(vec![x, y, z])
}

/// `{X, Y, Z}` → `[I; x, y, z]`; anything else is kept.
pub(super) fn block_pos(tag: Tag) -> Tag {
    if let Tag::Compound(c) = &tag
        && let (Some(x), Some(y), Some(z)) = (int(c, "X"), int(c, "Y"), int(c, "Z"))
    {
        return int_array(x, y, z);
    }
    tag
}

/// Three loose coordinate fields → one `[I; x, y, z]` field.
pub(super) fn inline_pos(c: &mut Compound, x: &str, y: &str, z: &str, to: &str) {
    if let (Some(px), Some(py), Some(pz)) = (int(c, x), int(c, y), int(c, z)) {
        c.remove(x);
        c.remove(y);
        c.remove(z);
        c.insert(to, int_array(px, py, pz));
    }
}

/// Dye colour by its old numeric id.
pub(super) fn dye(id: i32) -> &'static str {
    const NAMES: [&str; 16] = [
        "white",
        "orange",
        "magenta",
        "light_blue",
        "yellow",
        "lime",
        "pink",
        "gray",
        "light_gray",
        "cyan",
        "purple",
        "blue",
        "brown",
        "green",
        "red",
        "black",
    ];
    usize::try_from(id)
        .ok()
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("white")
}

/// Renames a namespaced string at `key` through `table` (`old`, `new`).
pub(super) fn rename_value(c: &mut Compound, key: &str, table: &[(&str, &str)]) {
    if let Some(Tag::String(value)) = c.get_mut(key) {
        let full = ns(value);
        if let Some((_, to)) = table.iter().find(|(from, _)| *from == full) {
            *value = (*to).to_owned();
        }
    }
}

/// A text component as old saves wrote it: a JSON string.
pub(super) fn json_text(json: &str) -> Tag {
    Tag::String(json.to_owned())
}

/// `{"translate": key}` as a JSON string, with Gson's stable key order.
pub(super) fn json_translate(key: &str) -> Tag {
    json_text(&format!(
        "{{\"translate\":{}}}",
        serde_json::Value::String(key.to_owned())
    ))
}

/// The translation key of a text component that is only `{"translate": ..}`,
/// whether stored as a JSON string or as NBT.
pub(super) fn plain_translation(tag: &Tag) -> Option<String> {
    match tag {
        Tag::String(json) => {
            let value: serde_json::Value = serde_json::from_str(json).ok()?;
            value.get("translate")?.as_str().map(str::to_owned)
        }
        Tag::Compound(c) if c.len() == 1 => string(c, "translate").map(str::to_owned),
        _ => None,
    }
}

/// The `translate` key of a JSON text component, if it has one.
pub(super) fn json_translation(tag: &Tag) -> Option<String> {
    let Tag::String(json) = tag else {
        return None;
    };
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    value
        .as_object()?
        .get("translate")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_positions_become_int_arrays() {
        let mut pos = Compound::new();
        pos.insert("X", Tag::Int(1));
        pos.insert("Y", Tag::Int(-2));
        pos.insert("Z", Tag::Int(3));
        assert_eq!(block_pos(Tag::Compound(pos)), int_array(1, -2, 3));
        assert_eq!(block_pos(Tag::Int(4)), Tag::Int(4));
    }

    #[test]
    fn translations_are_found_in_both_forms() {
        let json = json_translate("block.minecraft.ominous_banner");
        assert_eq!(
            plain_translation(&json).as_deref(),
            Some("block.minecraft.ominous_banner")
        );
        let mut c = Compound::new();
        c.insert("translate", Tag::String("a.b".into()));
        assert_eq!(plain_translation(&Tag::Compound(c)).as_deref(), Some("a.b"));
        assert_eq!(dye(14), "red");
        assert_eq!(dye(99), "white");
    }
}
