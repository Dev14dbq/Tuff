//! Books: writing in a book and quill and signing it (`edit_book`).

use mc_nbt::{Compound, Tag};
use mc_protocol::packets::play::ItemStack;

use crate::context::ServerContext;

fn raw(text: &str) -> Tag {
    let mut c = Compound::new();
    c.insert("raw", Tag::String(text.to_owned()));
    Tag::Compound(c)
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
