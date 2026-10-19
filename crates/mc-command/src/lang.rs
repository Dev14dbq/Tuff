//! Text components as the console prints them: in English, from the
//! server's `assets/minecraft/lang/en_us.json`.

use std::collections::HashMap;
use std::path::Path;

use mc_nbt::Tag;

#[derive(Debug, Default)]
pub struct Lang {
    strings: HashMap<String, String>,
}

impl Lang {
    /// `en_us.json` under the data folder; empty (keys shown as they are)
    /// when it is missing.
    pub fn load(path: &Path) -> Self {
        let strings = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str::<HashMap<String, String>>(&t).ok())
            .unwrap_or_default();
        Self { strings }
    }

    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings.get(key).map_or(key, String::as_str)
    }

    pub fn render(&self, tag: &Tag) -> String {
        let mut out = String::new();
        self.write(tag, &mut out);
        out
    }

    fn write(&self, tag: &Tag, out: &mut String) {
        match tag {
            Tag::String(s) => out.push_str(s),
            Tag::Byte(v) => out.push_str(&v.to_string()),
            Tag::Short(v) => out.push_str(&v.to_string()),
            Tag::Int(v) => out.push_str(&v.to_string()),
            Tag::Long(v) => out.push_str(&v.to_string()),
            Tag::Float(v) => out.push_str(&java_float(f64::from(*v))),
            Tag::Double(v) => out.push_str(&java_float(*v)),
            Tag::List(items) => {
                for t in items {
                    self.write(t, out);
                }
            }
            Tag::Compound(c) => {
                if let Some(v) = c.get("") {
                    self.write(v, out);
                } else if let Some(Tag::String(s)) = c.get("text") {
                    out.push_str(s);
                } else if let Some(Tag::String(key)) = c.get("translate") {
                    let args: Vec<String> = match c.get("with") {
                        Some(Tag::List(l)) => l.iter().map(|a| self.render(a)).collect(),
                        _ => Vec::new(),
                    };
                    let format = match (self.strings.get(key), c.get("fallback")) {
                        (Some(f), _) => f.as_str(),
                        (None, Some(Tag::String(f))) => f.as_str(),
                        _ => key.as_str(),
                    };
                    out.push_str(&format_args(format, &args));
                } else if let Some(Tag::String(s)) = c.get("selector") {
                    out.push_str(s);
                } else if let Some(Tag::String(s)) = c.get("keybind") {
                    out.push_str(s);
                }
                if let Some(Tag::List(extra)) = c.get("extra") {
                    for t in extra {
                        self.write(t, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// How Java prints a float or double (`1.0`, `0.5`).
fn java_float(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e7 {
        format!("{v:.1}")
    } else {
        v.to_string()
    }
}

/// `%s`, `%1$s` and `%%` filled from `args`, as translated text is.
pub fn format_args(format: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut next = 0;
    let mut chars = format.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(&(_, d)) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(d);
                chars.next();
            } else {
                break;
            }
        }
        match chars.peek().map(|&(_, d)| d) {
            Some('%') if digits.is_empty() => {
                chars.next();
                out.push('%');
            }
            Some('$') if !digits.is_empty() => {
                chars.next();
                if chars.peek().map(|&(_, d)| d) == Some('s') {
                    chars.next();
                }
                let i: usize = digits.parse().unwrap_or(1);
                out.push_str(args.get(i.saturating_sub(1)).map_or("", String::as_str));
            }
            Some('s' | 'd') if digits.is_empty() => {
                chars.next();
                out.push_str(args.get(next).map_or("", String::as_str));
                next += 1;
            }
            _ => {
                out.push('%');
                out.push_str(&digits);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let a = vec!["x".to_owned(), "y".to_owned()];
        assert_eq!(format_args("%s and %s", &a), "x and y");
        assert_eq!(format_args("%2$s %1$s 100%%", &a), "y x 100%");
    }
}
