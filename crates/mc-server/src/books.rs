//! Books: writing in a book and quill and signing it (`edit_book`).

use mc_nbt::{Compound, Tag};
use mc_protocol::filter::FilteredText;
use mc_protocol::packets::play::ItemStack;

use crate::context::ServerContext;

/// A page or title as the book stores it: what was written and, when the
/// text filter changed it, what others see. An editor who has text filtering
/// on only gets the filtered text.
fn filterable(text: &FilteredText, editor_filtering: bool) -> Tag {
    let mut c = Compound::new();
    if editor_filtering {
        c.insert("raw", Tag::String(text.filtered_or_empty()));
    } else {
        c.insert("raw", Tag::String(text.raw.clone()));
        if text.is_filtered() {
            c.insert("filtered", Tag::String(text.filtered_or_empty()));
        }
    }
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
        new.insert(
            "pages",
            Tag::List(pages.iter().map(&resolve_page).collect()),
        );
    }
    new.insert("resolved", Tag::Byte(1));
    stack
        .components
        .set("written_book_content", Tag::Compound(new));
    true
}

/// A book and quill holds at most this many pages, and each page this many
/// characters. A signed title is at most 32 characters.
const MAX_PAGES: usize = 100;
const MAX_PAGE_CHARS: usize = 1024;
const MAX_TITLE_CHARS: usize = 32;

/// Writes `pages` into a book and quill, or signs it as a written book
/// when `title` is given (by `author`). Whether the stack changed. A book
/// past the page, length or title limits is left as it was.
pub fn edit(
    ctx: &ServerContext,
    stack: &mut ItemStack,
    author: &str,
    pages: &[FilteredText],
    title: Option<&FilteredText>,
    editor_filtering: bool,
) -> bool {
    if ctx.items.name(stack.item) != Some("minecraft:writable_book") {
        return false;
    }
    if pages.len() > MAX_PAGES || pages.iter().any(|p| p.raw.chars().count() > MAX_PAGE_CHARS) {
        return false;
    }
    if title.is_some_and(|t| {
        let n = t.raw.trim().chars().count();
        n == 0 || n > MAX_TITLE_CHARS
    }) {
        return false;
    }
    let pages: Vec<Tag> = pages
        .iter()
        .map(|p| filterable(p, editor_filtering))
        .collect();
    match title {
        Some(title) => {
            let Some(written) = ctx.items.id("minecraft:written_book") else {
                return false;
            };
            stack.item = written;
            stack.components.reset("writable_book_content");
            let mut content = Compound::new();
            content.insert("title", filterable(title, editor_filtering));
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

    fn text(s: &str) -> FilteredText {
        FilteredText::pass_through(s)
    }

    fn masked(raw: &str, bits: &[i64]) -> FilteredText {
        let mut mask = mc_protocol::filter::FilterMask::partial();
        for &b in bits {
            mask.set_filtered(b).unwrap();
        }
        FilteredText {
            raw: raw.to_owned(),
            mask,
        }
    }

    fn blank_book(ctx: &ServerContext) -> ItemStack {
        ItemStack {
            count: 1,
            item: ctx.items.id("minecraft:writable_book").unwrap(),
            damage: 0,
            components: Default::default(),
        }
    }

    fn pages_of(stack: &ItemStack, component: &str) -> Vec<Compound> {
        let Some(Tag::Compound(content)) = stack.components.get(component) else {
            panic!("no {component}");
        };
        let Some(Tag::List(pages)) = content.get("pages") else {
            panic!("no pages");
        };
        pages
            .iter()
            .map(|p| match p {
                Tag::Compound(c) => c.clone(),
                other => panic!("page {other:?}"),
            })
            .collect()
    }

    #[test]
    fn filtered_pages_keep_what_was_written_and_what_others_see() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let pages = [masked("bad page", &[0, 1, 2]), text("fine")];
        let mut stack = blank_book(&ctx);
        assert!(edit(&ctx, &mut stack, "Steve", &pages, None, false));
        let stored = pages_of(&stack, "writable_book_content");
        assert_eq!(stored[0].get("raw"), Some(&Tag::from("bad page")));
        assert_eq!(stored[0].get("filtered"), Some(&Tag::from("### page")));
        // Nothing changed, nothing extra stored.
        assert_eq!(stored[1].get("raw"), Some(&Tag::from("fine")));
        assert!(stored[1].get("filtered").is_none());

        // An editor with text filtering on never keeps the raw text.
        let mut stack = blank_book(&ctx);
        assert!(edit(&ctx, &mut stack, "Steve", &pages, None, true));
        let stored = pages_of(&stack, "writable_book_content");
        assert_eq!(stored[0].get("raw"), Some(&Tag::from("### page")));
        assert!(stored[0].get("filtered").is_none());

        // A fully filtered page is empty for them.
        let hidden = [FilteredText::fully_filtered("bad")];
        let mut stack = blank_book(&ctx);
        assert!(edit(&ctx, &mut stack, "Steve", &hidden, None, true));
        assert_eq!(
            pages_of(&stack, "writable_book_content")[0].get("raw"),
            Some(&Tag::from(""))
        );
        let mut stack = blank_book(&ctx);
        assert!(edit(&ctx, &mut stack, "Steve", &hidden, None, false));
        let page = &pages_of(&stack, "writable_book_content")[0];
        assert_eq!(page.get("raw"), Some(&Tag::from("bad")));
        assert_eq!(page.get("filtered"), Some(&Tag::from("")));
    }

    #[test]
    fn a_signed_title_is_filtered_too() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let mut stack = blank_book(&ctx);
        let title = masked("Badtitle", &[0, 1, 2]);
        assert!(edit(
            &ctx,
            &mut stack,
            "Steve",
            &[text("x")],
            Some(&title),
            false
        ));
        let Some(Tag::Compound(content)) = stack.components.get("written_book_content") else {
            panic!("not signed");
        };
        let Some(Tag::Compound(t)) = content.get("title") else {
            panic!("no title");
        };
        assert_eq!(t.get("raw"), Some(&Tag::from("Badtitle")));
        assert_eq!(t.get("filtered"), Some(&Tag::from("###title")));
    }

    #[test]
    fn a_book_past_the_limits_is_left_alone() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        let book = ctx.items.id("minecraft:writable_book").unwrap();
        let mut stack = ItemStack {
            count: 1,
            item: book,
            damage: 0,
            components: Default::default(),
        };
        let page = text(&"a".repeat(MAX_PAGE_CHARS + 1));
        assert!(!edit(&ctx, &mut stack, "Steve", &[page], None, false));
        assert!(stack.components.get("writable_book_content").is_none());
        let ok = vec![text("ok"); MAX_PAGES + 1];
        assert!(!edit(&ctx, &mut stack, "Steve", &ok, None, false));
        let long = text(&"t".repeat(MAX_TITLE_CHARS + 1));
        assert!(!edit(
            &ctx,
            &mut stack,
            "Steve",
            &[text("hi")],
            Some(&long),
            false
        ));
        assert_eq!(stack.item, book);
        assert!(edit(
            &ctx,
            &mut stack,
            "Steve",
            &[text("hi")],
            Some(&text("Diary")),
            false
        ));
        assert_eq!(ctx.items.name(stack.item), Some("minecraft:written_book"));
    }

    #[test]
    fn scores_in_a_page_are_filled_in() {
        let Some(ctx) = context() else {
            eprintln!("skipped: no game data");
            return;
        };
        crate::tick::lock(&ctx.scoreboard)
            .score_mut("Steve", "kills")
            .value = 7;
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
        data.insert(
            "list",
            Tag::List(vec![Tag::Int(1), Tag::Int(2), Tag::Int(3)]),
        );
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
        let Tag::Compound(out) = crate::commands::resolve_text(&ctx, &src, &Tag::Compound(c))
        else {
            panic!("a component");
        };
        assert_eq!(out.get("extra"), Some(&Tag::List(Vec::new())));
    }
}
