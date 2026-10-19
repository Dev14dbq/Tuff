//! Command errors as vanilla shows them: a translatable message, and for
//! errors found while parsing the input, where in it they are.

use mc_nbt::{Compound, Tag};

use crate::text;

/// An error with its message (`key` with `args`); `at` is the input and the
/// cursor when it was found while parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandError {
    pub message: Tag,
    pub at: Option<(String, usize)>,
}

impl CommandError {
    pub fn new(key: &str, args: Vec<Tag>) -> Self {
        Self {
            message: text::translate(key, args),
            at: None,
        }
    }

    /// An error with an already built message.
    pub fn message(message: Tag) -> Self {
        Self { message, at: None }
    }

    /// The same error found at `cursor` of `input`.
    pub fn at(mut self, input: &str, cursor: usize) -> Self {
        self.at = Some((input.to_owned(), cursor));
        self
    }

    /// The lines sent to whoever ran the command: the message in red, then
    /// (for parse errors) the input up to the error, clickable to retry.
    pub fn lines(&self) -> Vec<Tag> {
        let mut out = vec![failure(self.message.clone())];
        if let Some((input, cursor)) = &self.at {
            out.push(context(input, *cursor));
        }
        out
    }

    /// One line for the console: `message`, or brigadier's
    /// `message at position n: ...text<--[HERE]`.
    pub fn console(&self, lang: &crate::lang::Lang) -> String {
        let message = lang.render(&self.message);
        match &self.at {
            None => message,
            Some((input, cursor)) => {
                let cursor = (*cursor).min(input.len());
                let mut s = format!("{message} at position {cursor}: ");
                if cursor > 10 {
                    s.push_str("...");
                }
                s.push_str(&input[floor(input, cursor.saturating_sub(10))..cursor]);
                s.push_str("<--[HERE]");
                s
            }
        }
    }
}

/// The nearest char boundary at or below `i`.
fn floor(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A failure message: red.
pub fn failure(message: Tag) -> Tag {
    let mut c = Compound::new();
    c.insert("text", "");
    c.insert("extra", Tag::List(vec![message]));
    c.insert("color", "red");
    Tag::Compound(c)
}

/// The input up to the error, with the rest underlined.
fn context(input: &str, cursor: usize) -> Tag {
    let cursor = floor(input, cursor.min(input.len()));
    let mut parts = Vec::new();
    if cursor > 10 {
        parts.push(Tag::from("..."));
    }
    let before = &input[floor(input, cursor.saturating_sub(10))..cursor];
    if !before.is_empty() {
        parts.push(Tag::from(before));
    }
    if cursor < input.len() {
        let mut rest = Compound::new();
        rest.insert("text", &input[cursor..]);
        rest.insert("color", "red");
        rest.insert("underlined", Tag::Byte(1));
        parts.push(Tag::Compound(rest));
    }
    let mut here = Compound::new();
    here.insert("translate", "command.context.here");
    here.insert("color", "red");
    here.insert("italic", Tag::Byte(1));
    parts.push(Tag::Compound(here));
    let mut click = Compound::new();
    click.insert("action", "suggest_command");
    click.insert("command", Tag::String(format!("/{input}")));
    let mut line = Compound::new();
    line.insert("text", "");
    line.insert("extra", text::list(parts));
    line.insert("color", "gray");
    line.insert("click_event", Tag::Compound(click));
    failure(Tag::Compound(line))
}

/// Brigadier's built-in errors.
pub mod builtin {
    use super::CommandError;
    use crate::text::{int, string};

    pub fn expected(key: &str) -> CommandError {
        CommandError::new(key, Vec::new())
    }

    pub fn invalid(key: &str, found: &str) -> CommandError {
        CommandError::new(key, vec![string(found)])
    }

    pub fn bound(key: &str, limit: mc_nbt::Tag, found: mc_nbt::Tag) -> CommandError {
        CommandError::new(key, vec![limit, found])
    }

    pub fn expected_char(c: char) -> CommandError {
        CommandError::new("parsing.expected", vec![string(&c.to_string())])
    }

    pub fn literal(name: &str) -> CommandError {
        CommandError::new("argument.literal.incorrect", vec![string(name)])
    }

    pub fn unknown_command() -> CommandError {
        expected("command.unknown.command")
    }

    pub fn unknown_argument() -> CommandError {
        expected("command.unknown.argument")
    }

    pub fn separator() -> CommandError {
        expected("command.expected.separator")
    }

    pub fn integer(key: &str, n: i64) -> CommandError {
        CommandError::new(key, vec![int(n)])
    }
}
