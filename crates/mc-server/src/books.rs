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
