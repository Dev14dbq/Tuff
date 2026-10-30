//! Text components: JSON strings until 25w02a (4290), NBT since; the event
//! fields changed twice right after.

use mc_nbt::{Compound, Tag};

use super::nbt::{list_mut, number, string};

/// 4290: a JSON string becomes the NBT it describes. Text that is not JSON
/// stays a plain string, which reads as literal text.
pub(super) fn unflatten(tag: &mut Tag) {
    let Tag::String(json) = tag else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return;
    };
    if let Some(parsed) = mc_data::json_to_nbt(&value) {
        *tag = parsed;
    }
}

/// Every component in `tag`: itself, its `extra`, `separator`, `with` and a
/// `show_text` hover's text.
fn each(tag: &mut Tag, f: &mut impl FnMut(&mut Compound)) {
    match tag {
        Tag::List(list) => list.iter_mut().for_each(|t| each(t, f)),
        Tag::Compound(c) => {
            // Heterogeneous lists store non-compound entries as `{"": v}`.
            if c.len() == 1
                && let Some(inner) = c.get_mut("")
            {
                each(inner, f);
                return;
            }
            f(c);
            for key in ["extra", "with"] {
                if let Some(list) = list_mut(c, key) {
                    list.iter_mut().for_each(|t| each(t, f));
                }
            }
            if let Some(sep) = c.get_mut("separator") {
                each(sep, f);
            }
            for hover in ["hoverEvent", "hover_event"] {
                if let Some(Tag::Compound(h)) = c.get_mut(hover)
                    && string(h, "action") == Some("show_text")
                {
                    for key in ["contents", "value"] {
                        if let Some(inner) = h.get_mut(key) {
                            each(inner, f);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// 4291: style flags stored as `"true"` strings become booleans, and a
/// legacy hover event (`value`) becomes a modern one: `show_text` keeps its
/// text, anything else is replaced by a text saying what it was.
pub(super) fn flags_and_legacy_hover(tag: &mut Tag) {
    each(tag, &mut |c| {
        for key in [
            "bold",
            "italic",
            "underlined",
            "strikethrough",
            "obfuscated",
        ] {
            if let Some(Tag::String(s)) = c.get(key) {
                let value = s.parse::<bool>().unwrap_or(false);
                c.insert(key, Tag::Byte(i8::from(value)));
            }
        }
        let Some(Tag::Compound(hover)) = c.get_mut("hoverEvent") else {
            return;
        };
        let Some(value) = hover.get("value").cloned() else {
            return;
        };
        if string(hover, "action") == Some("show_text") {
            hover.remove("value");
            hover.insert("contents", value);
        } else {
            let mut text = Compound::new();
            text.insert(
                "text",
                Tag::String(format!(
                    "Legacy hoverEvent: {}",
                    stable_json(&Tag::Compound(hover.clone()))
                )),
            );
            let mut placeholder = Compound::new();
            placeholder.insert("action", Tag::String("show_text".into()));
            placeholder.insert("contents", Tag::Compound(text));
            *hover = placeholder;
        }
    });
}

/// 4292: `hoverEvent` / `clickEvent` become `hover_event` / `click_event`
/// with their values in named fields. Click events that would not be
/// accepted any more (non-http links, commands with control characters,
/// unreadable page numbers) are dropped.
pub(super) fn events(tag: &mut Tag) {
    each(tag, &mut |c| {
        if let Some(Tag::Compound(mut hover)) = c.remove("hoverEvent") {
            fix_hover(&mut hover);
            c.insert("hover_event", Tag::Compound(hover));
        }
        if let Some(Tag::Compound(click)) = c.remove("clickEvent")
            && let Some(click) = fix_click(click)
        {
            c.insert("click_event", Tag::Compound(click));
        }
    });
}

fn fix_hover(hover: &mut Compound) {
    match string(hover, "action") {
        Some("show_text") => super::nbt::rename(hover, "contents", "value"),
        Some("show_item") => {
            if let Some(Tag::Compound(contents)) = hover.remove("contents") {
                for key in ["id", "count", "components"] {
                    if let Some(v) = contents.get(key) {
                        hover.insert(key, v.clone());
                    }
                }
            }
        }
        Some("show_entity") => {
            if let Some(Tag::Compound(contents)) = hover.remove("contents") {
                if let Some(v) = contents.get("id") {
                    hover.insert("uuid", v.clone());
                }
                if let Some(v) = contents.get("type") {
                    hover.insert("id", v.clone());
                }
                if let Some(v) = contents.get("name") {
                    hover.insert("name", v.clone());
                }
            }
        }
        _ => {}
    }
}

fn fix_click(mut click: Compound) -> Option<Compound> {
    let action = string(&click, "action").unwrap_or("").to_owned();
    let value = string(&click, "value").unwrap_or("").to_owned();
    match action.as_str() {
        "open_url" => {
            let scheme = value.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
            if !matches!(scheme.as_deref(), Some("http" | "https")) {
                return None;
            }
            super::nbt::rename(&mut click, "value", "url");
        }
        "open_file" => super::nbt::rename(&mut click, "value", "path"),
        "run_command" | "suggest_command" => {
            if value
                .chars()
                .any(|ch| ch == '\u{a7}' || ch < ' ' || ch == '\u{7f}')
            {
                return None;
            }
            super::nbt::rename(&mut click, "value", "command");
        }
        "change_page" => {
            let page = match click.get("value") {
                Some(Tag::String(s)) => s.trim().parse::<i32>().ok(),
                Some(other) => number(other).map(|v| v as i32),
                None => None,
            }?;
            click.remove("value");
            click.insert("page", Tag::Int(page.max(1)));
        }
        _ => {}
    }
    Some(click)
}

/// JSON with sorted keys, the way Gson's stable writer prints it.
fn stable_json(tag: &Tag) -> String {
    match tag {
        Tag::Compound(c) => {
            let mut entries: Vec<_> = c.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let body: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}:{}",
                        serde_json::Value::String((*k).to_owned()),
                        stable_json(v)
                    )
                })
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Tag::List(list) => {
            let body: Vec<String> = list.iter().map(stable_json).collect();
            format!("[{}]", body.join(","))
        }
        Tag::String(s) => serde_json::Value::String(s.clone()).to_string(),
        Tag::ByteArray(v) => format!("{v:?}").replace(' ', ""),
        Tag::IntArray(v) => format!("{v:?}").replace(' ', ""),
        Tag::LongArray(v) => format!("{v:?}").replace(' ', ""),
        other => number(other).map_or_else(String::new, |n| {
            if n.fract() == 0.0 {
                format!("{}", n as i64)
            } else {
                format!("{n}")
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(json: &str) -> Tag {
        let mut tag = Tag::String(json.to_owned());
        unflatten(&mut tag);
        flags_and_legacy_hover(&mut tag);
        events(&mut tag);
        tag
    }

    #[test]
    fn json_becomes_nbt_with_modern_events() {
        let tag = all(
            r#"{"text":"hi","bold":"true","extra":[{"text":"x","clickEvent":{"action":"run_command","value":"/say a"}}],"hoverEvent":{"action":"show_text","value":"tip"}}"#,
        );
        let Tag::Compound(c) = tag else {
            panic!("not a compound");
        };
        assert_eq!(c.get("bold"), Some(&Tag::Byte(1)));
        let Some(Tag::Compound(hover)) = c.get("hover_event") else {
            panic!("no hover_event");
        };
        assert_eq!(hover.get("value"), Some(&Tag::String("tip".into())));
        let Some(Tag::List(extra)) = c.get("extra") else {
            panic!("no extra");
        };
        let Tag::Compound(first) = &extra[0] else {
            panic!("extra is not compounds");
        };
        let Some(Tag::Compound(click)) = first.get("click_event") else {
            panic!("no click_event");
        };
        assert_eq!(click.get("command"), Some(&Tag::String("/say a".into())));
    }

    #[test]
    fn plain_text_and_bad_links_are_handled() {
        assert_eq!(all("not json"), Tag::String("not json".into()));
        assert_eq!(all(r#""quoted""#), Tag::String("quoted".into()));
        let Tag::Compound(c) =
            all(r#"{"text":"a","clickEvent":{"action":"open_url","value":"file:///x"}}"#)
        else {
            panic!("not a compound");
        };
        assert!(c.get("click_event").is_none());
        let Tag::Compound(c) =
            all(r#"{"text":"a","hoverEvent":{"action":"show_item","value":"{id:stone}"}}"#)
        else {
            panic!("not a compound");
        };
        let Some(Tag::Compound(hover)) = c.get("hover_event") else {
            panic!("no hover_event");
        };
        assert_eq!(hover.get("action"), Some(&Tag::String("show_text".into())));
    }
}
