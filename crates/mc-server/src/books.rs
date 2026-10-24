//! Books: writing in a book and quill and signing it (`edit_book`).

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::ItemStack;

use crate::context::ServerContext;

fn raw(text: &str) -> Tag {
    let mut c = Compound::new();
    c.insert("raw", Tag::String(text.to_owned()));
    Tag::Compound(c)
}

/// Resolves the text of a written book that has not been (selectors and
/// scores), as it is for whoever opens it, and marks it resolved
/// (minecraft.wiki, "Written Book"). Whether the stack changed.
pub fn resolve(ctx: &ServerContext, uuid: u128, stack: &mut ItemStack) -> bool {
    let Some(Tag::Compound(content)) = stack.components.get("written_book_content") else {
        return false;
    };
    if matches!(content.get("resolved"), Some(Tag::Byte(1))) {
        return false;
    }
    let Some((name, pose)) = crate::tick::lock(&ctx.players)
        .get(&uuid)
        .and_then(|p| Some((p.profile.name.clone(), p.pose?)))
    else {
        return false;
    };
    let src = crate::commands::Src::for_book(ctx, uuid, &name, pose);
    let resolve_page = |page: &Tag| -> Tag {
        match page {
            Tag::Compound(p) if p.get("raw").is_some() => {
                let mut out = Compound::new();
                for (k, v) in p.iter() {
                    let v = if k == "raw" || k == "filtered" {
                        crate::commands::resolve_text(ctx, &src, v)
                    } else {
                        v.clone()
                    };
                    out.insert(k, v);
                }
                Tag::Compound(out)
            }
            other => crate::commands::resolve_text(ctx, &src, other),
        }
    };
    let mut new = content.clone();
    if let Some(Tag::List(pages)) = content.get("pages") {
        new.insert("pages", Tag::List(pages.iter().map(&resolve_page).collect()));
    }
    new.insert("resolved", Tag::Byte(1));
    stack
        .components
        .set("written_book_content", Tag::Compound(new));
    true
}

/// Writes `pages` into a book and quill, or signs it as a written book
/// when `title` is given (by `author`). Whether the stack changed.
pub fn edit(
    ctx: &ServerContext,
    stack: &mut ItemStack,
    author: &str,
    pages: Vec<String>,
    title: Option<String>,
) -> bool {
    if ctx.items.name(stack.item) != Some("minecraft:writable_book") {
        return false;
    }
    let pages: Vec<Tag> = pages.iter().map(|p| raw(p)).collect();
    match title {
        Some(title) => {
            let Some(written) = ctx.items.id("minecraft:written_book") else {
                return false;
            };
            stack.item = written;
            stack.components.reset("writable_book_content");
            let mut content = Compound::new();
            content.insert("title", raw(title.trim()));
            content.insert("author", Tag::String(author.to_owned()));
            if !pages.is_empty() {
                content.insert("pages", Tag::List(pages));
            }
            content.insert("resolved", Tag::Byte(1));
            stack
                .components
                .set("written_book_content", Tag::Compound(content));
        }
        None if pages.is_empty() => {
            stack.components.reset("writable_book_content");
        }
        None => {
            let mut content = Compound::new();
            content.insert("pages", Tag::List(pages));
            stack
                .components
                .set("writable_book_content", Tag::Compound(content));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Source, Src};

    fn context() -> Option<std::sync::Arc<ServerContext>> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../cache/mcdata/26.3");
        let data = mc_data::McData::load(&dir).ok()?;
        ServerContext::new(crate::Config::new(dir), data).ok()
    }

    #[test]
    fn scores_in_a_page_are_filled_in() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        crate::tick::lock(&ctx.scoreboard).score_mut("Steve", "kills").value = 7;
        let mut page = Compound::new();
        let mut score = Compound::new();
        score.insert("name", Tag::from("Steve"));
        score.insert("objective", Tag::from("kills"));
        page.insert("score", Tag::Compound(score));
        let src = Src::of(&ctx, &Source::Console);
        let out = crate::commands::resolve_text(&ctx, &src, &Tag::Compound(page));
        assert_eq!(out, Tag::from("7"));
        // Plain text is left alone.
        let plain = Tag::from("hello");
        assert_eq!(crate::commands::resolve_text(&ctx, &src, &plain), plain);
    }

    #[test]
    fn nbt_parts_read_storage_blocks_and_separate() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut data = Compound::new();
        data.insert("list", Tag::List(vec![Tag::Int(1), Tag::Int(2), Tag::Int(3)]));
        data.insert("name", Tag::from("Alex"));
        crate::tick::lock(&ctx.storage).set("minecraft:book_test", data);
        let src = Src::of(&ctx, &Source::Console);
        let part = |extra: &[(&str, &str)]| {
            let mut c = Compound::new();
            c.insert("nbt", Tag::from("list[]"));
            c.insert("storage", Tag::from("minecraft:book_test"));
            for (k, v) in extra {
                c.insert(*k, Tag::from(*v));
            }
            crate::commands::resolve_text(&ctx, &src, &Tag::Compound(c))
        };
        let Tag::Compound(out) = part(&[]) else {
            panic!("a component");
        };
        let Some(Tag::List(items)) = out.get("extra") else {
            panic!("parts");
        };
        assert_eq!(items.len(), 5, "three numbers and two separators");
        assert_eq!(items[1], Tag::from(", "));
        let Tag::Compound(out) = part(&[("separator", "|")]) else {
            panic!("a component");
        };
        let Some(Tag::List(items)) = out.get("extra") else {
            panic!("parts");
        };
        assert_eq!(items[1], Tag::from("|"));
        // A path that finds nothing shows nothing.
        let mut c = Compound::new();
        c.insert("nbt", Tag::from("missing"));
        c.insert("storage", Tag::from("minecraft:book_test"));
        let Tag::Compound(out) = crate::commands::resolve_text(&ctx, &src, &Tag::Compound(c)) else {
            panic!("a component");
        };
        assert_eq!(out.get("extra"), Some(&Tag::List(Vec::new())));
    }
}
