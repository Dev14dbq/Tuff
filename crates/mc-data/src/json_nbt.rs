//! JSON → NBT, for sending data-pack entries to clients as NBT (Registry Data
//! without known packs). Follows the documented generic conversion rules:
//!
//! - strings → String, booleans → Byte, objects → Compound, `null` is dropped;
//! - numbers → the narrowest of Byte, Short, Int, Long (when integral), else
//!   Float when exact, else Double;
//! - arrays of one element type → ByteArray / IntArray / LongArray / List.
//!
//! The documented converter rejects mixed arrays; we widen mixed numbers to a
//! common type and wrap other mixed elements as `{"": value}`, which is how
//! heterogeneous lists are stored in NBT since 1.21.5.

use mc_nbt::{Compound, Tag, tag_type};
use serde_json::{Number, Value};

pub fn json_to_nbt(value: &Value) -> Option<Tag> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(b) => Tag::Byte(i8::from(*b)),
        Value::Number(n) => number(n),
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => array(items.iter().filter_map(json_to_nbt).collect()),
        Value::Object(map) => {
            let mut compound = Compound::new();
            for (key, value) in map {
                if let Some(tag) = json_to_nbt(value) {
                    compound.insert(key.clone(), tag);
                }
            }
            Tag::Compound(compound)
        }
    })
}

fn integer(i: i64) -> Tag {
    if let Ok(v) = i8::try_from(i) {
        Tag::Byte(v)
    } else if let Ok(v) = i16::try_from(i) {
        Tag::Short(v)
    } else if let Ok(v) = i32::try_from(i) {
        Tag::Int(v)
    } else {
        Tag::Long(i)
    }
}

fn number(n: &Number) -> Tag {
    if let Some(i) = n.as_i64() {
        return integer(i);
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64 {
        return integer(f as i64);
    }
    if f64::from(f as f32) == f {
        Tag::Float(f as f32)
    } else {
        Tag::Double(f)
    }
}

/// Rank of numeric tag types for widening; `None` for non-numbers.
fn numeric_rank(tag: &Tag) -> Option<u8> {
    Some(match tag {
        Tag::Byte(_) => 0,
        Tag::Short(_) => 1,
        Tag::Int(_) => 2,
        Tag::Long(_) => 3,
        Tag::Float(_) => 4,
        Tag::Double(_) => 5,
        _ => return None,
    })
}

fn as_f64(tag: &Tag) -> f64 {
    match *tag {
        Tag::Byte(v) => f64::from(v),
        Tag::Short(v) => f64::from(v),
        Tag::Int(v) => f64::from(v),
        Tag::Long(v) => v as f64,
        Tag::Float(v) => f64::from(v),
        Tag::Double(v) => v,
        _ => unreachable!("only numbers are widened"),
    }
}

fn as_i64(tag: &Tag) -> i64 {
    match *tag {
        Tag::Byte(v) => i64::from(v),
        Tag::Short(v) => i64::from(v),
        Tag::Int(v) => i64::from(v),
        Tag::Long(v) => v,
        _ => unreachable!("only integers are widened to integers"),
    }
}

fn widen(tag: &Tag, rank: u8) -> Tag {
    match rank {
        0 => Tag::Byte(as_i64(tag) as i8),
        1 => Tag::Short(as_i64(tag) as i16),
        2 => Tag::Int(as_i64(tag) as i32),
        3 => Tag::Long(as_i64(tag)),
        // An integer widened to Float may lose precision; use Double then.
        4 if f64::from(as_f64(tag) as f32) == as_f64(tag) => Tag::Float(as_f64(tag) as f32),
        _ => Tag::Double(as_f64(tag)),
    }
}

fn array(items: Vec<Tag>) -> Tag {
    let Some(first) = items.first() else {
        return Tag::List(items);
    };
    let first_type = first.type_id();
    if items.iter().all(|t| t.type_id() == first_type) {
        return match first_type {
            tag_type::BYTE => Tag::ByteArray(items.iter().map(|t| as_i64(t) as i8).collect()),
            tag_type::INT => Tag::IntArray(items.iter().map(|t| as_i64(t) as i32).collect()),
            tag_type::LONG => Tag::LongArray(items.iter().map(as_i64).collect()),
            _ => Tag::List(items),
        };
    }
    if let Some(rank) = items
        .iter()
        .map(numeric_rank)
        .collect::<Option<Vec<_>>>()
        .and_then(|r| r.into_iter().max())
    {
        let widened: Vec<Tag> = items.iter().map(|t| widen(t, rank)).collect();
        // Float widening can fall back to Double per element; settle on one type.
        if widened.iter().all(|t| t.type_id() == widened[0].type_id()) {
            return Tag::List(widened);
        }
        return Tag::List(items.iter().map(|t| Tag::Double(as_f64(t))).collect());
    }
    Tag::List(
        items
            .into_iter()
            .map(|t| {
                let mut wrapper = Compound::new();
                wrapper.insert("", t);
                Tag::Compound(wrapper)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nbt(v: Value) -> Tag {
        json_to_nbt(&v).unwrap()
    }

    #[test]
    fn numbers_take_the_narrowest_type() {
        // Examples from the documented conversion rules.
        assert_eq!(nbt(json!(0)), Tag::Byte(0));
        assert_eq!(nbt(json!(1.0)), Tag::Byte(1));
        assert_eq!(nbt(json!(1.27e2)), Tag::Byte(127));
        assert_eq!(nbt(json!(128)), Tag::Short(128));
        assert_eq!(nbt(json!(12345678.0)), Tag::Int(12345678));
        assert_eq!(nbt(json!(-1.23e8)), Tag::Int(-123000000));
        assert_eq!(nbt(json!(2147483649i64)), Tag::Long(2147483649));
        assert_eq!(nbt(json!(0.5)), Tag::Float(0.5));
        assert_eq!(nbt(json!(31.75)), Tag::Float(31.75));
        assert_eq!(nbt(json!(0.6)), Tag::Double(0.6));
        assert_eq!(nbt(json!(true)), Tag::Byte(1));
    }

    #[test]
    fn arrays() {
        assert_eq!(nbt(json!([true, 127])), Tag::ByteArray(vec![1, 127]));
        assert_eq!(
            nbt(json!([1000000, 2000000])),
            Tag::IntArray(vec![1000000, 2000000])
        );
        assert_eq!(
            nbt(json!(["a", "b"])),
            Tag::List(vec![Tag::from("a"), Tag::from("b")])
        );
        assert_eq!(nbt(json!([])), Tag::List(vec![]));
        // Mixed numbers widen instead of failing.
        assert_eq!(
            nbt(json!([0, 1, 128])),
            Tag::List(vec![Tag::Short(0), Tag::Short(1), Tag::Short(128)])
        );
        assert_eq!(
            nbt(json!([0.5, 0.6])),
            Tag::List(vec![Tag::Double(0.5), Tag::Double(0.6)])
        );
        // Mixed kinds are wrapped.
        let mixed = nbt(json!(["a", 1]));
        let Tag::List(items) = mixed else {
            panic!("list")
        };
        assert!(
            items
                .iter()
                .all(|t| matches!(t, Tag::Compound(c) if c.get("").is_some()))
        );
    }

    #[test]
    fn objects_drop_nulls() {
        let tag = nbt(json!({"a": 1, "b": null, "c": {"d": "e"}}));
        let Tag::Compound(c) = tag else {
            panic!("compound")
        };
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("a"), Some(&Tag::Byte(1)));
        // Every converted tree must be encodable (no mixed lists).
        mc_nbt::write_network(&Tag::Compound(c), &mut Vec::new()).unwrap();
    }
}
