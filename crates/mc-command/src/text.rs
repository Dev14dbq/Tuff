//! Message parts.

use mc_nbt::Tag;

pub use mc_protocol::text::{list, translate};

pub fn string(s: &str) -> Tag {
    Tag::from(s)
}

pub fn int(n: i64) -> Tag {
    match i32::try_from(n) {
        Ok(v) => Tag::Int(v),
        Err(_) => Tag::Long(n),
    }
}

/// A component as JSON, the way `observe-commands` records vanilla's.
pub fn to_json(tag: &Tag) -> serde_json::Value {
    use serde_json::{Value, json};
    match tag {
        Tag::String(s) => Value::String(s.clone()),
        Tag::Byte(b) => json!(b),
        Tag::Short(v) => json!(v),
        Tag::Int(v) => json!(v),
        Tag::Long(v) => json!(v),
        Tag::Float(v) => json!(v),
        Tag::Double(v) => json!(v),
        Tag::List(l) => Value::Array(l.iter().map(to_json).collect()),
        Tag::Compound(c) => {
            Value::Object(c.iter().map(|(k, v)| (k.to_owned(), to_json(v))).collect())
        }
        Tag::ByteArray(a) => json!(a),
        Tag::IntArray(a) => json!(a),
        Tag::LongArray(a) => json!(a),
    }
}

/// A list of components: NBT lists hold one tag type, so when the types
/// differ every part that is not a compound is wrapped as `{"": value}`, as
/// vanilla writes them.
pub fn list_of(items: Vec<Tag>) -> Tag {
    let mixed = items.windows(2).any(|w| w[0].type_id() != w[1].type_id());
    if !mixed {
        return Tag::List(items);
    }
    Tag::List(
        items
            .into_iter()
            .map(|t| match t {
                Tag::Compound(_) => t,
                other => {
                    let mut c = mc_nbt::Compound::new();
                    c.insert("", other);
                    Tag::Compound(c)
                }
            })
            .collect(),
    )
}
