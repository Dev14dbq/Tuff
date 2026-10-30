//! SNBT, the text form of NBT used in commands (minecraft.wiki, "NBT
//! format#SNBT format"): `{key: value, ...}`, `[a, b]`, typed arrays
//! `[B; 1b, 2b]`, `[I; ...]`, `[L; ...]`, quoted strings (`"..."` or
//! `'...'`), numbers with an optional type suffix (`b`, `s`, `l`, `f`, `d`;
//! none means int, or double with a decimal point), `true`/`false` as bytes,
//! and bare words as strings.

use crate::{Compound, Tag};

/// Parses one SNBT value; the whole text must be used.
pub fn parse_snbt(text: &str) -> Result<Tag, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let tag = p.value()?;
    p.skip_ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected `{}` at {}", &text[p.i..], p.i));
    }
    Ok(tag)
}

/// Parses SNBT at the start of `text`; returns the value and the bytes used.
pub fn parse_snbt_prefix(text: &str) -> Result<(Tag, usize), String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let tag = p.value()?;
    Ok((tag, p.i))
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    /// Lists and compounds open now; at most [`crate::MAX_DEPTH`], as for
    /// binary NBT, so deep nesting is an error and not a stack overflow.
    depth: usize,
}

fn bare(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'+')
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.s.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected `{}` at {}", c as char, self.i))
        }
    }

    fn value(&mut self) -> Result<Tag, String> {
        match self.peek() {
            Some(b'{' | b'[') if self.depth >= crate::MAX_DEPTH => Err(format!(
                "nested deeper than {} at {}",
                crate::MAX_DEPTH,
                self.i
            )),
            Some(b'{') => self.nested(|p| p.compound().map(Tag::Compound)),
            Some(b'[') => self.nested(Self::list),
            Some(b'"' | b'\'') => self.quoted().map(Tag::String),
            Some(_) => {
                let word = self.word()?;
                Ok(scalar(&word))
            }
            None => Err("unexpected end".into()),
        }
    }

    fn nested(
        &mut self,
        read: impl FnOnce(&mut Self) -> Result<Tag, String>,
    ) -> Result<Tag, String> {
        self.depth += 1;
        let tag = read(self);
        self.depth -= 1;
        tag
    }

    fn word(&mut self) -> Result<String, String> {
        self.skip_ws();
        let start = self.i;
        while self.i < self.s.len() && bare(self.s[self.i]) {
            self.i += 1;
        }
        if start == self.i {
            return Err(format!("expected a value at {start}"));
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn quoted(&mut self) -> Result<String, String> {
        let quote = self.s[self.i];
        self.i += 1;
        let mut out = Vec::new();
        while self.i < self.s.len() {
            let c = self.s[self.i];
            self.i += 1;
            if c == quote {
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
            if c == b'\\' && self.i < self.s.len() {
                let e = self.s[self.i];
                self.i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    other => out.push(other),
                }
            } else {
                out.push(c);
            }
        }
        Err("unterminated string".into())
    }

    fn compound(&mut self) -> Result<Compound, String> {
        self.expect(b'{')?;
        let mut c = Compound::new();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(c);
        }
        loop {
            let key = match self.peek() {
                Some(b'"' | b'\'') => self.quoted()?,
                _ => self.word()?,
            };
            self.expect(b':')?;
            let value = self.value()?;
            c.insert(&key, value);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(c);
                }
                _ => return Err(format!("expected `,` or `}}` at {}", self.i)),
            }
        }
    }

    fn list(&mut self) -> Result<Tag, String> {
        self.expect(b'[')?;
        // Typed arrays: [B; ...], [I; ...], [L; ...].
        self.skip_ws();
        if self.i + 1 < self.s.len()
            && matches!(self.s[self.i], b'B' | b'I' | b'L')
            && self.s[self.i + 1] == b';'
        {
            let kind = self.s[self.i];
            self.i += 2;
            let mut values = Vec::new();
            if self.peek() != Some(b']') {
                loop {
                    let word = self.word()?;
                    let digits = word.trim_end_matches(['b', 'B', 'l', 'L']);
                    let v: i64 = digits
                        .parse()
                        .map_err(|_| format!("bad array number `{word}`"))?;
                    values.push(v);
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => break,
                        _ => return Err(format!("expected `,` or `]` at {}", self.i)),
                    }
                }
            }
            self.i += 1;
            return Ok(match kind {
                b'B' => Tag::ByteArray(values.into_iter().map(|v| v as i8).collect()),
                b'I' => Tag::IntArray(values.into_iter().map(|v| v as i32).collect()),
                _ => Tag::LongArray(values),
            });
        }
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Tag::List(items));
        }
        loop {
            items.push(self.value()?);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Tag::List(items));
                }
                _ => return Err(format!("expected `,` or `]` at {}", self.i)),
            }
        }
    }
}

/// A bare word: a number if it reads as one, a boolean, else a string.
fn scalar(word: &str) -> Tag {
    match word {
        "true" => return Tag::Byte(1),
        "false" => return Tag::Byte(0),
        _ => {}
    }
    let (body, suffix) = match word.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&word[..i], Some(c.to_ascii_lowercase())),
        _ => (word, None),
    };
    let number = |b: &str| {
        b.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
    };
    if !body.is_empty() && number(body) {
        let parsed = match suffix {
            Some('b') => body.parse::<i8>().ok().map(Tag::Byte),
            Some('s') => body.parse::<i16>().ok().map(Tag::Short),
            Some('l') => body.parse::<i64>().ok().map(Tag::Long),
            Some('f') => body.parse::<f32>().ok().map(Tag::Float),
            Some('d') => body.parse::<f64>().ok().map(Tag::Double),
            None if body.contains(['.', 'e', 'E']) => body.parse::<f64>().ok().map(Tag::Double),
            None => body.parse::<i32>().ok().map(Tag::Int),
            _ => None,
        };
        if let Some(tag) = parsed {
            return tag;
        }
    }
    Tag::String(word.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_items_and_numbers() {
        let tag = parse_snbt(
            r#"{Items:[{Slot:0b,id:"minecraft:stone",count:64}],Name:'a b',x:1.5,f:2f,on:true,B:[B;1b,2b],w:word}"#,
        )
        .unwrap();
        let Tag::Compound(c) = tag else { panic!() };
        let Some(Tag::List(items)) = c.get("Items") else {
            panic!()
        };
        let Tag::Compound(item) = &items[0] else {
            panic!()
        };
        assert_eq!(item.get("Slot"), Some(&Tag::Byte(0)));
        assert_eq!(item.get("count"), Some(&Tag::Int(64)));
        assert_eq!(item.get("id"), Some(&Tag::String("minecraft:stone".into())));
        assert_eq!(c.get("Name"), Some(&Tag::String("a b".into())));
        assert_eq!(c.get("x"), Some(&Tag::Double(1.5)));
        assert_eq!(c.get("f"), Some(&Tag::Float(2.0)));
        assert_eq!(c.get("on"), Some(&Tag::Byte(1)));
        assert_eq!(c.get("B"), Some(&Tag::ByteArray(vec![1, 2])));
        assert_eq!(c.get("w"), Some(&Tag::String("word".into())));
    }

    #[test]
    fn deep_nesting_is_an_error() {
        let depth = crate::MAX_DEPTH;
        let ok = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        assert!(parse_snbt(&ok).is_ok());
        let deep = format!("{}{}", "[".repeat(depth + 1), "]".repeat(depth + 1));
        assert!(parse_snbt(&deep).is_err());
        // A whole command's worth of `{a:` must not overflow a tokio worker's 2 MiB stack.
        let hostile = "{a:".repeat(32767 / 3);
        let parsed = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || parse_snbt(&hostile).is_err())
            .unwrap()
            .join();
        assert_eq!(parsed.ok(), Some(true));
    }

    #[test]
    fn prefix_stops_after_the_value() {
        let (tag, used) = parse_snbt_prefix("{a:1} rest").unwrap();
        assert!(matches!(tag, Tag::Compound(_)));
        assert_eq!(used, 5);
    }
}
