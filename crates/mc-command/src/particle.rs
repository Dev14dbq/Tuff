//! Particle options (`dust{color:[1.0,0.0,0.0],scale:1.0}`): which keys
//! each particle type needs, with the messages vanilla's codecs give when
//! they are wrong (seen on a vanilla 26.3 server).

use mc_nbt::{Compound, Tag};

/// The keys a particle type must have, as vanilla reports them missing
/// (last declared first). Types not listed take no options or have
/// defaults for all.
fn required(kind: &str) -> &'static [&'static str] {
    match kind {
        "block" | "block_crumble" | "block_marker" | "dust_pillar" | "falling_dust" => {
            &["block_state"]
        }
        "dust" => &["scale", "color"],
        "dust_color_transition" => &["scale", "to_color", "from_color"],
        "entity_effect" | "flash" | "tinted_leaves" => &["color"],
        "geyser" | "geyser_plume" => &["water_blocks"],
        "geyser_base" | "geyser_poof" => &["burst_impulse_base", "water_blocks"],
        "item" => &["item"],
        "sculk_charge" => &["roll"],
        "shriek" => &["delay"],
        "trail" => &["duration", "color", "target"],
        "vibration" => &["arrival_in_ticks", "destination"],
        _ => &[],
    }
}

/// Checks the options of particle `id`; the message vanilla gives if they
/// do not do.
pub fn check(id: &str, options: &Compound) -> Result<(), String> {
    let kind = id.strip_prefix("minecraft:").unwrap_or(id);
    let shown = format!("MapLike[{}]", snbt(&Tag::Compound(options.clone())));
    let mut errors: Vec<String> = Vec::new();
    for key in required(kind) {
        match options.get(key) {
            None => errors.push(format!("No key {key} in {shown}")),
            Some(v) if *key == "block_state" => {
                if let Some(why) = block_state_error(v) {
                    errors.push(why);
                }
            }
            Some(v) => {
                let colour = match (kind, *key) {
                    ("dust", "color") | ("dust_color_transition", "from_color" | "to_color") => {
                        Some(3)
                    }
                    ("entity_effect" | "flash" | "tinted_leaves", "color") => Some(4),
                    _ => None,
                };
                if let Some(n) = colour
                    && color(v, n).is_none()
                {
                    errors.push(format!(
                        "Failed to parse either. First: Not a number; Second: Input is not a \
                         list of {n} elements"
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// What vanilla says of a `block_state` that is neither a block id nor a
/// map with one (`{id:..., Properties:{...}}`).
fn block_state_error(v: &Tag) -> Option<String> {
    match v {
        Tag::String(s) => {
            let path = s.split_once(':').map_or(s.as_str(), |(_, p)| p);
            if path
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "/._-".contains(c))
            {
                return None;
            }
            let bad = format!(
                "Not a valid resource location: {s} Non [a-z0-9/._-] character in path of \
                 location: {s}"
            );
            Some(format!(
                "Failed to parse either. First: Failed to parse either. First: {bad}; Second: \
                 Not a map: \"{s}\"; Second: {bad}"
            ))
        }
        Tag::Compound(c) if matches!(c.get("id"), Some(Tag::String(_))) => None,
        other => Some(format!(
            "Failed to parse either. First: Failed to parse either. First: Not a string; \
             Second: No key id in MapLike[{}]; Second: Not a string",
            snbt(other)
        )),
    }
}

fn number(t: &Tag) -> Option<f64> {
    Some(match t {
        Tag::Byte(v) => f64::from(*v),
        Tag::Short(v) => f64::from(*v),
        Tag::Int(v) => f64::from(*v),
        Tag::Long(v) => *v as f64,
        Tag::Float(v) => f64::from(*v),
        Tag::Double(v) => *v,
        _ => return None,
    })
}

/// A colour option as an ARGB int: an int as it is, or a list of `n`
/// (3: RGB, opaque; 4: RGBA) numbers from 0 to 1.
pub fn color(t: &Tag, n: usize) -> Option<i32> {
    if let Tag::Int(v) = t {
        return Some(*v);
    }
    let list: Vec<f64> = match t {
        Tag::List(l) => l.iter().map(number).collect::<Option<_>>()?,
        Tag::IntArray(a) => a.iter().map(|v| f64::from(*v)).collect(),
        _ => return None,
    };
    if list.len() != n {
        return None;
    }
    let c = |v: f64| ((v.clamp(0.0, 1.0) * 255.0) as i32) & 0xff;
    let a = if n == 4 { c(list[3]) } else { 255 };
    Some(a << 24 | c(list[0]) << 16 | c(list[1]) << 8 | c(list[2]))
}

/// A number option, or `default`.
pub fn float(options: &Compound, key: &str, default: f32) -> f32 {
    options
        .get(key)
        .and_then(number)
        .map_or(default, |v| v as f32)
}

/// A tag as vanilla prints it (`{color:[1.0d,0.0d]}`).
pub fn snbt(t: &Tag) -> String {
    fn key(k: &str) -> String {
        if !k.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.+".contains(c))
        {
            k.to_owned()
        } else {
            quote(k)
        }
    }
    fn quote(s: &str) -> String {
        let q = if s.contains('"') && !s.contains('\'') {
            '\''
        } else {
            '"'
        };
        let mut out = String::from(q);
        for c in s.chars() {
            if c == q || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push(q);
        out
    }
    fn join<T>(items: &[T], f: impl Fn(&T) -> String) -> String {
        items.iter().map(f).collect::<Vec<_>>().join(",")
    }
    match t {
        Tag::Byte(v) => format!("{v}b"),
        Tag::Short(v) => format!("{v}s"),
        Tag::Int(v) => v.to_string(),
        Tag::Long(v) => format!("{v}L"),
        Tag::Float(v) => format!("{v:?}f"),
        Tag::Double(v) => format!("{v:?}d"),
        Tag::String(s) => quote(s),
        Tag::List(l) => format!("[{}]", join(l, snbt)),
        Tag::Compound(c) => format!(
            "{{{}}}",
            c.iter()
                .map(|(k, v)| format!("{}:{}", key(k), snbt(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Tag::ByteArray(a) => format!("[B;{}]", join(a, |v| format!("{v}B"))),
        Tag::IntArray(a) => format!("[I;{}]", join(a, |v| v.to_string())),
        Tag::LongArray(a) => format!("[L;{}]", join(a, |v| format!("{v}L"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_as_vanilla() {
        assert_eq!(
            check("minecraft:dust", &Compound::new()).unwrap_err(),
            "No key scale in MapLike[{}]; No key color in MapLike[{}]"
        );
        let mut c = Compound::new();
        c.insert("color", Tag::List(vec![Tag::Double(1.0), Tag::Double(0.0)]));
        assert_eq!(
            check("minecraft:dust", &c).unwrap_err(),
            "No key scale in MapLike[{color:[1.0d,0.0d]}]; Failed to parse either. First: Not \
             a number; Second: Input is not a list of 3 elements"
        );
        assert!(check("minecraft:flame", &c).is_ok());
    }

    #[test]
    fn colours() {
        let red = Tag::List(vec![Tag::Double(1.0), Tag::Double(0.0), Tag::Double(0.0)]);
        assert_eq!(color(&red, 3), Some(0xffff0000u32 as i32));
        assert_eq!(color(&Tag::Int(16711680), 3), Some(0x00ff0000));
    }
}
