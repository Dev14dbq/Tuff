//! Which characters of a text an external filter removed, and the text it
//! was applied to. Positions count UTF-16 code units, as the filter services
//! (written against Java strings) report them.

use std::collections::BTreeSet;

use mc_nbt::{Compound, Tag};

use crate::text;

/// A filter's verdict on one text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterMask {
    /// Nothing was removed.
    PassThrough,
    /// The whole text is hidden.
    FullyFiltered,
    /// The characters at these positions are shown as `#`.
    Partially(BTreeSet<u32>),
}

impl FilterMask {
    /// A partial mask with nothing marked yet.
    pub fn partial() -> Self {
        FilterMask::Partially(BTreeSet::new())
    }

    /// Marks position `index`; a negative one is an error, as it is for a
    /// bit set. Does nothing on masks that are not partial.
    pub fn set_filtered(&mut self, index: i64) -> Result<(), String> {
        let index = u32::try_from(index).map_err(|_| format!("bit index {index} out of range"))?;
        if let FilterMask::Partially(bits) = self {
            bits.insert(index);
        }
        Ok(())
    }

    /// The text with removed characters replaced by `#`; `None` when all of
    /// it is hidden.
    pub fn apply(&self, text: &str) -> Option<String> {
        match self {
            FilterMask::PassThrough => Some(text.to_owned()),
            FilterMask::FullyFiltered => None,
            FilterMask::Partially(bits) => {
                let mut units: Vec<u16> = text.encode_utf16().collect();
                for &i in bits {
                    if let Some(unit) = units.get_mut(i as usize) {
                        *unit = u16::from(b'#');
                    }
                }
                Some(String::from_utf16_lossy(&units))
            }
        }
    }

    /// The same as a chat component: the removed runs are dark gray `#`
    /// with a hover saying the text was filtered. `None` when all of it is
    /// hidden.
    pub fn apply_with_formatting(&self, text: &str) -> Option<Tag> {
        match self {
            FilterMask::PassThrough => Some(text::plain(text)),
            FilterMask::FullyFiltered => None,
            FilterMask::Partially(bits) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                let mut parts = Vec::new();
                let mut start = 0;
                while start < units.len() {
                    let filtered = bits.contains(&(start as u32));
                    let mut end = start;
                    while end < units.len() && bits.contains(&(end as u32)) == filtered {
                        end += 1;
                    }
                    if filtered {
                        parts.push(filtered_run(end - start));
                    } else {
                        parts.push(Tag::String(String::from_utf16_lossy(&units[start..end])));
                    }
                    start = end;
                }
                let mut out = Compound::new();
                out.insert("text", "");
                if !parts.is_empty() {
                    out.insert("extra", text::list(parts));
                }
                Some(Tag::Compound(out))
            }
        }
    }

    /// Nothing was removed.
    pub fn is_empty(&self) -> bool {
        matches!(self, FilterMask::PassThrough)
    }

    pub fn is_fully_filtered(&self) -> bool {
        matches!(self, FilterMask::FullyFiltered)
    }
}

fn filtered_run(len: usize) -> Tag {
    let mut hover = Compound::new();
    hover.insert("action", "show_text");
    hover.insert("value", text::translate("chat.filtered", vec![]));
    let mut run = Compound::new();
    run.insert("text", "#".repeat(len).as_str());
    run.insert("color", "dark_gray");
    run.insert("hover_event", Tag::Compound(hover));
    Tag::Compound(run)
}

/// A text as the player wrote it and what the filter made of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilteredText {
    pub raw: String,
    pub mask: FilterMask,
}

impl FilteredText {
    pub fn pass_through(message: &str) -> Self {
        Self {
            raw: message.to_owned(),
            mask: FilterMask::PassThrough,
        }
    }

    pub fn fully_filtered(message: &str) -> Self {
        Self {
            raw: message.to_owned(),
            mask: FilterMask::FullyFiltered,
        }
    }

    /// The text with the removed characters hidden; `None` when all of it is.
    pub fn filtered(&self) -> Option<String> {
        self.mask.apply(&self.raw)
    }

    pub fn filtered_or_empty(&self) -> String {
        self.filtered().unwrap_or_default()
    }

    /// The filter changed something.
    pub fn is_filtered(&self) -> bool {
        !self.mask.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(bits: &[i64]) -> FilterMask {
        let mut m = FilterMask::partial();
        for &b in bits {
            m.set_filtered(b).unwrap();
        }
        m
    }

    #[test]
    fn removed_characters_become_hashes() {
        let m = mask(&[0, 1, 5]);
        assert_eq!(m.apply("badword!").as_deref(), Some("##dwo#d!"));
        assert_eq!(FilterMask::PassThrough.apply("hi").as_deref(), Some("hi"));
        assert_eq!(FilterMask::FullyFiltered.apply("hi"), None);
        // Positions past the end do nothing.
        assert_eq!(mask(&[9]).apply("hi").as_deref(), Some("hi"));
    }

    #[test]
    fn positions_count_utf16_units() {
        // The emoji is two units, so unit 3 is the letter after it.
        assert_eq!(
            mask(&[3]).apply("a\u{1F600}b").as_deref(),
            Some("a\u{1F600}#")
        );
    }

    #[test]
    fn a_negative_position_is_an_error() {
        assert!(FilterMask::partial().set_filtered(-1).is_err());
    }

    #[test]
    fn formatting_marks_each_removed_run() {
        let m = mask(&[2, 3]);
        let Some(Tag::Compound(c)) = m.apply_with_formatting("abcdef") else {
            panic!("expected a component");
        };
        assert_eq!(c.get("text"), Some(&Tag::from("")));
        let Some(Tag::List(extra)) = c.get("extra") else {
            panic!("expected siblings");
        };
        assert_eq!(extra.len(), 3);
        let Tag::Compound(run) = &extra[1] else {
            panic!("expected the removed run");
        };
        assert_eq!(run.get("text"), Some(&Tag::from("##")));
        assert_eq!(run.get("color"), Some(&Tag::from("dark_gray")));
        assert!(run.get("hover_event").is_some());
        assert_eq!(FilterMask::FullyFiltered.apply_with_formatting("x"), None);
    }
}
