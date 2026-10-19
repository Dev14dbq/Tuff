//! Brigadier's string reader: a cursor over the command with the readers
//! for numbers, words, quoted strings and booleans, failing with the same
//! messages.

use crate::error::{CommandError, builtin};

#[derive(Clone, Debug)]
pub struct Reader<'a> {
    pub input: &'a str,
    pub cursor: usize,
}

pub type Result<T> = std::result::Result<T, CommandError>;

pub fn is_quote(c: char) -> bool {
    c == '"' || c == '\''
}

/// Characters of an unquoted string.
pub fn is_unquoted(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+')
}

fn is_number(c: char) -> bool {
    c.is_ascii_digit() || c == '.' || c == '-'
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }

    pub fn at(input: &'a str, cursor: usize) -> Self {
        Self { input, cursor }
    }

    pub fn remaining(&self) -> &'a str {
        &self.input[self.cursor..]
    }

    pub fn read(&self) -> &'a str {
        &self.input[..self.cursor]
    }

    pub fn can_read(&self) -> bool {
        self.cursor < self.input.len()
    }

    pub fn can_read_n(&self, n: usize) -> bool {
        self.remaining().chars().count() >= n
    }

    pub fn peek(&self) -> char {
        self.remaining().chars().next().unwrap_or('\0')
    }

    pub fn peek_at(&self, offset: usize) -> char {
        self.remaining().chars().nth(offset).unwrap_or('\0')
    }

    pub fn read_char(&mut self) -> char {
        let c = self.peek();
        self.skip();
        c
    }

    pub fn skip(&mut self) {
        if let Some(c) = self.remaining().chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub fn skip_whitespace(&mut self) {
        while self.can_read() && self.peek().is_whitespace() {
            self.skip();
        }
    }

    /// The error, found here.
    pub fn error(&self, e: CommandError) -> CommandError {
        e.at(self.input, self.cursor)
    }

    /// The error, found at `cursor`.
    pub fn error_at(&self, e: CommandError, cursor: usize) -> CommandError {
        e.at(self.input, cursor)
    }

    fn read_number<T: std::str::FromStr>(&mut self, kind: &str) -> Result<T> {
        let start = self.cursor;
        while self.can_read() && is_number(self.peek()) {
            self.skip();
        }
        let text = &self.input[start..self.cursor];
        if text.is_empty() {
            return Err(self.error(builtin::expected(&format!("parsing.{kind}.expected"))));
        }
        text.parse::<T>().map_err(|_| {
            self.cursor = start;
            self.error(builtin::invalid(&format!("parsing.{kind}.invalid"), text))
        })
    }

    pub fn read_int(&mut self) -> Result<i32> {
        self.read_number("int")
    }

    pub fn read_long(&mut self) -> Result<i64> {
        self.read_number("long")
    }

    pub fn read_double(&mut self) -> Result<f64> {
        self.read_number("double")
    }

    pub fn read_float(&mut self) -> Result<f32> {
        self.read_number("float")
    }

    pub fn read_unquoted(&mut self) -> &'a str {
        let start = self.cursor;
        while self.can_read() && is_unquoted(self.peek()) {
            self.skip();
        }
        &self.input[start..self.cursor]
    }

    pub fn read_quoted(&mut self) -> Result<String> {
        if !self.can_read() {
            return Ok(String::new());
        }
        let next = self.peek();
        if !is_quote(next) {
            return Err(self.error(builtin::expected("parsing.quote.expected.start")));
        }
        self.skip();
        self.read_until(next)
    }

    /// The rest of a quoted string up to `terminator`, with `\` escapes.
    pub fn read_until(&mut self, terminator: char) -> Result<String> {
        let mut out = String::new();
        let mut escaped = false;
        while self.can_read() {
            let c = self.read_char();
            if escaped {
                if c == terminator || c == '\\' {
                    out.push(c);
                    escaped = false;
                } else {
                    self.cursor -= c.len_utf8();
                    return Err(
                        self.error(builtin::invalid("parsing.quote.escape", &c.to_string()))
                    );
                }
            } else if c == '\\' {
                escaped = true;
            } else if c == terminator {
                return Ok(out);
            } else {
                out.push(c);
            }
        }
        Err(self.error(builtin::expected("parsing.quote.expected.end")))
    }

    pub fn read_string(&mut self) -> Result<String> {
        if !self.can_read() {
            return Ok(String::new());
        }
        let next = self.peek();
        if is_quote(next) {
            self.skip();
            return self.read_until(next);
        }
        Ok(self.read_unquoted().to_owned())
    }

    pub fn read_bool(&mut self) -> Result<bool> {
        let start = self.cursor;
        let value = self.read_string()?;
        if value.is_empty() {
            return Err(self.error(builtin::expected("parsing.bool.expected")));
        }
        match value.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => {
                self.cursor = start;
                Err(self.error(builtin::invalid("parsing.bool.invalid", &value)))
            }
        }
    }

    pub fn expect(&mut self, c: char) -> Result<()> {
        if !self.can_read() || self.peek() != c {
            return Err(self.error(builtin::expected_char(c)));
        }
        self.skip();
        Ok(())
    }

    /// A word up to the next space (what a literal is matched against).
    pub fn word(&self) -> &'a str {
        let rest = self.remaining();
        &rest[..rest.find(' ').unwrap_or(rest.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(Reader::new("12 x").read_int().unwrap(), 12);
        let mut r = Reader::new("1.5");
        let e = r.read_int().unwrap_err();
        assert_eq!(r.cursor, 0);
        assert_eq!(e.at, Some(("1.5".into(), 0)));
        assert!(Reader::new("abc").read_int().is_err());
        assert_eq!(Reader::new("-.5").read_double().unwrap(), -0.5);
    }

    #[test]
    fn strings() {
        assert_eq!(
            Reader::new("\"a \\\"b\\\"\" c").read_string().unwrap(),
            "a \"b\""
        );
        assert_eq!(Reader::new("word next").read_string().unwrap(), "word");
        assert!(Reader::new("\"open").read_string().is_err());
        assert!(Reader::new("yes").read_bool().is_err());
    }
}
