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

/// A number as Java's `Double.toString` writes it: plain from 0.001 to
/// 10⁷ (`5.0`), scientific outside (`1.0E10`).
pub fn java_double(v: f64) -> String {
    java_number(v, format!("{v:e}"), format!("{v}"))
}

/// The same for `float`s (`Float.toString`).
pub fn java_float(v: f32) -> String {
    java_number(f64::from(v), format!("{v:e}"), format!("{v}"))
}

fn java_number(v: f64, sci: String, plain: String) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let a = v.abs();
    if a == 0.0 || (1e-3..1e7).contains(&a) {
        return if plain.contains('.') {
            plain
        } else {
            format!("{plain}.0")
        };
    }
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let mantissa = if mantissa.contains('.') {
        mantissa.to_owned()
    } else {
        format!("{mantissa}.0")
    };
    format!("{mantissa}E{exp}")
}

/// A tag as `/data get` shows it: coloured, keys in vanilla's order.
pub fn pretty(tag: &Tag) -> Tag {
    let mut parts = Vec::new();
    pretty_into(tag, &mut parts);
    let mut c = mc_nbt::Compound::new();
    c.insert("text", "");
    c.insert("extra", list_of(parts));
    Tag::Compound(c)
}

fn colored(text: &str, color: &str) -> Tag {
    let mut c = mc_nbt::Compound::new();
    c.insert("color", color);
    c.insert("text", text);
    Tag::Compound(c)
}

fn number_parts(out: &mut Vec<Tag>, n: String, suffix: &str) {
    out.push(colored(&n, "gold"));
    if !suffix.is_empty() {
        out.push(colored(suffix, "red"));
    }
}

/// `s` quoted as SNBT quotes it: the quote and what goes between.
fn quoted(s: &str) -> (char, String) {
    let q = if s.contains('"') && !s.contains('\'') {
        '\''
    } else {
        '"'
    };
    let mut inner = String::new();
    for c in s.chars() {
        match c {
            '\n' => inner.push_str("\\n"),
            '\t' => inner.push_str("\\t"),
            '\r' => inner.push_str("\\r"),
            _ => {
                if c == q || c == '\\' {
                    inner.push('\\');
                }
                inner.push(c);
            }
        }
    }
    (q, inner)
}

fn pretty_into(tag: &Tag, out: &mut Vec<Tag>) {
    let s = |t: &str| Tag::String(t.to_owned());
    match tag {
        Tag::Byte(v) => number_parts(out, v.to_string(), "b"),
        Tag::Short(v) => number_parts(out, v.to_string(), "s"),
        Tag::Int(v) => number_parts(out, v.to_string(), ""),
        Tag::Long(v) => number_parts(out, v.to_string(), "L"),
        Tag::Float(v) => number_parts(out, java_float(*v), "f"),
        Tag::Double(v) => number_parts(out, java_double(*v), "d"),
        Tag::String(v) => {
            let (q, inner) = quoted(v);
            out.push(s(&q.to_string()));
            out.push(colored(&inner, "green"));
            out.push(s(&q.to_string()));
        }
        Tag::List(l) => {
            out.push(s("["));
            // Lists holding a compound or list get an (empty) indent per element.
            let inline = !l
                .iter()
                .any(|e| matches!(e, Tag::Compound(_) | Tag::List(_)));
            for (i, e) in l.iter().enumerate() {
                if i > 0 {
                    out.push(s(","));
                    out.push(s(" "));
                }
                if !inline {
                    out.push(s(""));
                }
                pretty_into(e, out);
            }
            out.push(s("]"));
        }
        Tag::ByteArray(a) => array(out, "B", a.iter().map(|v| (v.to_string(), "b"))),
        Tag::IntArray(a) => array(out, "I", a.iter().map(|v| (v.to_string(), ""))),
        Tag::LongArray(a) => array(out, "L", a.iter().map(|v| (v.to_string(), "L"))),
        Tag::Compound(c) => {
            out.push(s("{"));
            for (i, (k, v)) in mc_nbt::java_order(c).into_iter().enumerate() {
                if i > 0 {
                    out.push(s(","));
                    out.push(s(" "));
                }
                out.push(s(""));
                let simple = !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
                if simple {
                    out.push(colored(k, "aqua"));
                } else {
                    let (q, inner) = quoted(k);
                    let mut key = mc_nbt::Compound::new();
                    key.insert("text", Tag::String(q.to_string()));
                    key.insert(
                        "extra",
                        list_of(vec![colored(&inner, "aqua"), s(&q.to_string())]),
                    );
                    out.push(Tag::Compound(key));
                }
                out.push(s(":"));
                out.push(s(" "));
                pretty_into(v, out);
            }
            out.push(s("}"));
        }
    }
}

fn array(out: &mut Vec<Tag>, kind: &str, items: impl Iterator<Item = (String, &'static str)>) {
    out.push(Tag::String("[".into()));
    out.push(colored(kind, "red"));
    out.push(Tag::String(";".into()));
    for (i, (n, suffix)) in items.enumerate() {
        out.push(Tag::String(if i == 0 { " " } else { "," }.into()));
        if i > 0 {
            out.push(Tag::String(" ".into()));
        }
        number_parts(out, n, suffix);
    }
    out.push(Tag::String("]".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_as_java_writes_them() {
        assert_eq!(java_double(1.0e10), "1.0E10");
        assert_eq!(java_double(-3.25), "-3.25");
        assert_eq!(java_float(5.0), "5.0");
        assert_eq!(java_float(0.1), "0.1");
        assert_eq!(java_double(-0.0784000015258789), "-0.0784000015258789");
        assert_eq!(java_double(1.0e-4), "1.0E-4");
    }
}
